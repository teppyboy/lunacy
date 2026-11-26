use std::path::{Path, PathBuf};
use std::process::Command;

fn main() {
    println!("cargo:rerun-if-env-changed=LUNACY_SHA");
    emit_git_rerun_hints();

    let sha = std::env::var("LUNACY_SHA")
        .ok()
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
        .or_else(git_head_sha)
        .unwrap_or_else(|| "dev".to_string());

    let short_sha: String = if sha == "dev" {
        "dev".to_string()
    } else {
        sha.chars().take(7).collect()
    };

    let target = std::env::var("TARGET").unwrap_or_else(|_| "unknown".to_string());

    println!("cargo:rustc-env=LUNACY_BUILD_SHA={sha}");
    println!("cargo:rustc-env=LUNACY_BUILD_SHORT_SHA={short_sha}");
    println!("cargo:rustc-env=LUNACY_BUILD_TARGET={target}");

    if std::env::var("CARGO_CFG_TARGET_OS").as_deref() == Ok("windows") {
        embed_windows_resources();
    }
}

enum ResourceCompiler {
    /// `llvm-rc`: supports `/no-preprocess /FO <out.res> <in.rc>`.
    LlvmRc(PathBuf),
    /// Microsoft `rc.exe` (or compatible): `/nologo /fo <out.res> <in.rc>`.
    MsRc(PathBuf),
    /// MinGW / LLVM `windres`: `-O coff -i <in.rc> -o <out.o>`.
    Windres(PathBuf),
}

fn embed_windows_resources() {
    println!("cargo:rerun-if-changed=../../assets/icons/lunacy.ico");
    println!("cargo:rerun-if-env-changed=CARGO_PKG_VERSION");
    println!("cargo:rerun-if-env-changed=RC");
    println!("cargo:rerun-if-env-changed=WINDRES");

    let manifest_dir = PathBuf::from(
        std::env::var("CARGO_MANIFEST_DIR").expect("CARGO_MANIFEST_DIR is always set by Cargo"),
    );
    // `assets/` sits at the workspace root, shared with the daemon.
    let ico_path = manifest_dir
        .join("../../assets")
        .join("icons")
        .join("lunacy.ico");
    assert!(
        ico_path.is_file(),
        "Windows icon asset missing at {}",
        ico_path.display()
    );

    let out_dir = PathBuf::from(std::env::var("OUT_DIR").expect("OUT_DIR is always set by Cargo"));
    let rc_path = out_dir.join("lunacy.rc");

    let pkg_version = std::env::var("CARGO_PKG_VERSION").unwrap_or_else(|_| "0.0.0".to_string());
    let (major, minor, patch, build) = parse_pkg_version(&pkg_version);
    let ico_rc_path = ico_path
        .to_string_lossy()
        .replace('\\', "/")
        .replace('"', "\"\"");

    // Use raw numeric constants for VERSIONINFO flags so no Windows SDK headers (<winver.h>)
    // are needed during cross-compilation with llvm-rc.
    let rc_contents = format!(
        r#"1 ICON "{ico_rc_path}"

1 VERSIONINFO
FILEVERSION {major},{minor},{patch},{build}
PRODUCTVERSION {major},{minor},{patch},{build}
FILEFLAGSMASK 0x3f
FILEFLAGS 0x0
FILEOS 0x40004
FILETYPE 0x1
FILESUBTYPE 0x0
BEGIN
  BLOCK "StringFileInfo"
  BEGIN
    BLOCK "040904b0"
    BEGIN
      VALUE "ProductName", "Lunacy\0"
      VALUE "FileDescription", "Lunacy\0"
      VALUE "InternalName", "lunacy-ui\0"
      VALUE "OriginalFilename", "lunacy-ui.exe\0"
      VALUE "FileVersion", "{pkg_version}\0"
      VALUE "ProductVersion", "{pkg_version}\0"
    END
  END
  BLOCK "VarFileInfo"
  BEGIN
    VALUE "Translation", 0x0409, 1200
  END
END
"#
    );

    std::fs::write(&rc_path, rc_contents).unwrap_or_else(|e| {
        panic!(
            "failed to write Windows resource script {}: {e}",
            rc_path.display()
        );
    });

    let target_env = std::env::var("CARGO_CFG_TARGET_ENV").unwrap_or_default();
    let target_arch = std::env::var("CARGO_CFG_TARGET_ARCH").unwrap_or_default();
    let compiler = find_resource_compiler(&target_env, &target_arch);

    let (res_path, mut cmd) = match &compiler {
        ResourceCompiler::LlvmRc(tool) => {
            let out = out_dir.join("lunacy.res");
            let mut c = Command::new(tool);
            c.arg("/no-preprocess").arg("/FO").arg(&out).arg(&rc_path);
            (out, c)
        }
        ResourceCompiler::MsRc(tool) => {
            let out = out_dir.join("lunacy.res");
            let mut c = Command::new(tool);
            c.arg("/nologo").arg("/fo").arg(&out).arg(&rc_path);
            (out, c)
        }
        ResourceCompiler::Windres(tool) => {
            let out = out_dir.join("lunacy.res.o");
            let mut c = Command::new(tool);
            c.args(["-O", "coff", "-i"])
                .arg(&rc_path)
                .arg("-o")
                .arg(&out);
            (out, c)
        }
    };

    let output = cmd.output().unwrap_or_else(|e| {
        panic!("failed to execute Windows resource compiler: {e}");
    });
    if !output.status.success() {
        let stdout = String::from_utf8_lossy(&output.stdout);
        let stderr = String::from_utf8_lossy(&output.stderr);
        panic!(
            "Windows resource compiler failed ({}):\nstdout:\n{stdout}\nstderr:\n{stderr}",
            output.status
        );
    }

    println!("cargo:rustc-link-arg-bin=lunacy-ui={}", res_path.display());
}

fn parse_pkg_version(version: &str) -> (u16, u16, u16, u16) {
    let core = version.split(['-', '+']).next().unwrap_or("0.0.0");
    let mut parts = core.split('.').map(|p| p.parse::<u16>().unwrap_or(0));
    let major = parts.next().unwrap_or(0);
    let minor = parts.next().unwrap_or(0);
    let patch = parts.next().unwrap_or(0);
    let build = parts.next().unwrap_or(0);
    (major, minor, patch, build)
}

fn classify_rc(path: PathBuf) -> ResourceCompiler {
    let stem = path
        .file_stem()
        .and_then(|s| s.to_str())
        .unwrap_or_default()
        .to_ascii_lowercase();
    if stem.contains("windres") {
        ResourceCompiler::Windres(path)
    } else if stem.contains("llvm-rc") {
        ResourceCompiler::LlvmRc(path)
    } else {
        ResourceCompiler::MsRc(path)
    }
}

fn find_resource_compiler(target_env: &str, target_arch: &str) -> ResourceCompiler {
    if let Ok(rc_env) = std::env::var("RC") {
        let rc_env = rc_env.trim();
        if !rc_env.is_empty() {
            let resolved = find_executable(rc_env).unwrap_or_else(|| {
                panic!(
                    "RC environment variable is set to `{rc_env}`, but that executable was not found"
                );
            });
            return classify_rc(resolved);
        }
    }

    if target_env == "gnu" {
        if let Ok(windres_env) = std::env::var("WINDRES") {
            let windres_env = windres_env.trim();
            if !windres_env.is_empty() {
                let resolved = find_executable(windres_env).unwrap_or_else(|| {
                    panic!(
                        "WINDRES environment variable is set to `{windres_env}`, but that executable was not found"
                    );
                });
                return ResourceCompiler::Windres(resolved);
            }
        }

        let prefixed = match target_arch {
            "x86_64" => Some("x86_64-w64-mingw32-windres"),
            "x86" => Some("i686-w64-mingw32-windres"),
            "aarch64" => Some("aarch64-w64-mingw32-windres"),
            _ => None,
        };
        for candidate in prefixed.into_iter().chain(["llvm-windres", "windres"]) {
            if let Some(found) = find_executable(candidate) {
                return ResourceCompiler::Windres(found);
            }
        }
    } else {
        if let Some(found) = find_executable("llvm-rc") {
            return ResourceCompiler::LlvmRc(found);
        }
        if let Some(found) = find_executable("rc.exe") {
            return ResourceCompiler::MsRc(found);
        }
        // Only probe bare `rc` on Windows hosts (on Unix `rc` may be the Plan 9 / OpenRC shell).
        if cfg!(windows) {
            if let Some(found) = find_executable("rc") {
                return ResourceCompiler::MsRc(found);
            }
        }
        if let Some(found) = find_windows_kits_rc() {
            return ResourceCompiler::MsRc(found);
        }
    }

    panic!(
        "failed to embed Windows icon/version resources for `lunacy-ui`: \
         no resource compiler found (target_env=`{target_env}`). \
         Install `llvm-rc` (e.g. via LLVM for cross-compilation), the Windows SDK `rc.exe`, \
         or MinGW `windres`, or set the `RC` environment variable."
    );
}

fn find_executable(name: &str) -> Option<PathBuf> {
    let candidate = Path::new(name);
    if candidate.components().count() > 1 {
        return candidate.is_file().then(|| candidate.to_path_buf());
    }
    let path_var = std::env::var_os("PATH")?;
    for dir in std::env::split_paths(&path_var) {
        let full = dir.join(name);
        if full.is_file() {
            return Some(full);
        }
        #[cfg(windows)]
        if candidate.extension().is_none() {
            let with_exe = dir.join(format!("{name}.exe"));
            if with_exe.is_file() {
                return Some(with_exe);
            }
        }
    }
    None
}

fn find_windows_kits_rc() -> Option<PathBuf> {
    let arch_dirs: &[&str] = match std::env::consts::ARCH {
        "aarch64" => &["arm64", "x64", "x86"],
        "x86" => &["x86", "x64"],
        _ => &["x64", "x86"],
    };

    if let (Some(sdk_dir), Some(sdk_ver)) = (
        std::env::var_os("WindowsSdkDir"),
        std::env::var_os("WindowsSDKVersion"),
    ) {
        let base = PathBuf::from(sdk_dir).join("bin").join(sdk_ver);
        for arch in arch_dirs {
            let candidate = base.join(arch).join("rc.exe");
            if candidate.is_file() {
                return Some(candidate);
            }
        }
    }

    for env_key in ["ProgramFiles(x86)", "ProgramFiles"] {
        let Some(root) = std::env::var_os(env_key) else {
            continue;
        };
        let bin_dir = PathBuf::from(root)
            .join("Windows Kits")
            .join("10")
            .join("bin");
        let Ok(entries) = std::fs::read_dir(&bin_dir) else {
            continue;
        };
        let mut versions: Vec<PathBuf> = entries
            .filter_map(|e| e.ok().map(|e| e.path()))
            .filter(|p| p.is_dir())
            .collect();
        versions.sort_by_key(|a| std::cmp::Reverse(version_dir_key(a)));
        for ver_dir in versions {
            for arch in arch_dirs {
                let candidate = ver_dir.join(arch).join("rc.exe");
                if candidate.is_file() {
                    return Some(candidate);
                }
            }
        }
    }

    None
}

fn version_dir_key(path: &Path) -> Vec<u32> {
    path.file_name()
        .and_then(|n| n.to_str())
        .map(|s| {
            s.split('.')
                .map(|part| part.parse::<u32>().unwrap_or(0))
                .collect()
        })
        .unwrap_or_default()
}

fn emit_git_rerun_hints() {
    let mut emitted: Vec<PathBuf> = Vec::new();
    let mut watch = |rel: &str| {
        if let Some(path) = git_path(rel) {
            if path.exists() {
                let resolved = std::fs::canonicalize(&path).unwrap_or(path);
                if !emitted.contains(&resolved) {
                    println!("cargo:rerun-if-changed={}", resolved.display());
                    emitted.push(resolved);
                }
            }
        }
    };

    watch("HEAD");
    watch("logs/HEAD");
    watch("packed-refs");

    // Also watch the active branch ref file so commits trigger rebuilds even when reflogs are off.
    if let Some(head_path) = git_path("HEAD") {
        if let Ok(head_contents) = std::fs::read_to_string(head_path) {
            if let Some(ref_name) = head_contents.trim().strip_prefix("ref:") {
                let ref_name = ref_name.trim();
                if !ref_name.is_empty() {
                    watch(ref_name);
                }
            }
        }
    }
}

fn git_path(rel: &str) -> Option<PathBuf> {
    let out = Command::new("git")
        .args(["rev-parse", "--git-path", rel])
        .output()
        .ok()?;
    if !out.status.success() {
        return None;
    }
    let raw = String::from_utf8(out.stdout).ok()?.trim().to_string();
    if raw.is_empty() {
        None
    } else {
        Some(PathBuf::from(raw))
    }
}

fn git_head_sha() -> Option<String> {
    let out = Command::new("git")
        .args(["rev-parse", "HEAD"])
        .output()
        .ok()?;
    if !out.status.success() {
        return None;
    }
    let sha = String::from_utf8(out.stdout).ok()?.trim().to_string();
    if sha.len() >= 7 && sha.chars().all(|c| c.is_ascii_hexdigit()) {
        Some(sha)
    } else {
        None
    }
}
