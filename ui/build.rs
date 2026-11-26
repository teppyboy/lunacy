use std::path::PathBuf;
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
