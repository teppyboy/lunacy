//! Desktop client auto-updater (`lunacy-ui` on native Linux & Windows).
//!
//! Checks the GitHub `prerelease` release for a newer commit SHA, verifies the matching platform
//! archive against `SHA256SUMS.txt`, extracts `lunacy-ui` (or `lunacy-ui.exe`), and atomically
//! swaps the running binary before relaunching on user confirmation.
//!
//! Compiled out to a lightweight stub on `wasm32`.

pub const BUILD_VERSION: &str = env!("CARGO_PKG_VERSION");
pub const BUILD_SHA: &str = env!("LUNACY_BUILD_SHA");
pub const BUILD_SHORT_SHA: &str = env!("LUNACY_BUILD_SHORT_SHA");
pub const BUILD_TARGET: &str = env!("LUNACY_BUILD_TARGET");
pub const DEFAULT_UPDATE_URL: &str =
    "https://api.github.com/repos/teppyboy/lunacy/releases/tags/prerelease";

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UpdateInfo {
    pub tag: String,
    pub title: String,
    pub sha: String,
    pub short_sha: String,
    pub html_url: String,
    pub published_at: String,
    pub notes: String,
    pub asset_name: Option<String>,
    pub asset_url: Option<String>,
    pub checksums_url: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub enum UpdateStatus {
    #[default]
    Idle,
    Checking,
    UpToDate {
        checked_sha: String,
    },
    Available(UpdateInfo),
    Installing {
        info: UpdateInfo,
        stage: String,
    },
    ReadyToRestart(UpdateInfo),
    Error(String),
}

impl UpdateStatus {
    pub fn is_busy(&self) -> bool {
        matches!(
            self,
            UpdateStatus::Checking | UpdateStatus::Installing { .. }
        )
    }

    pub fn info(&self) -> Option<&UpdateInfo> {
        match self {
            UpdateStatus::Available(info)
            | UpdateStatus::Installing { info, .. }
            | UpdateStatus::ReadyToRestart(info) => Some(info),
            _ => None,
        }
    }
}

/// Compares a local build SHA against a remote commit SHA, supporting 7+ char prefixes.
pub fn is_same_commit(local: &str, remote: &str) -> bool {
    let a = local.trim().to_ascii_lowercase();
    let b = remote.trim().to_ascii_lowercase();
    if a.is_empty() || b.is_empty() || a == "dev" || b == "dev" {
        return false;
    }
    if a == b {
        return true;
    }
    let (short, long) = if a.len() <= b.len() { (a, b) } else { (b, a) };
    short.len() >= 7 && long.starts_with(&short)
}

/// Extracts a 7..=40-char hex commit SHA from a release title like `"Pre-release (f5841e0)"`.
pub fn parse_sha_from_title(title: &str) -> Option<String> {
    for token in title.split(|c: char| !c.is_ascii_alphanumeric()) {
        if (7..=40).contains(&token.len()) && token.chars().all(|c| c.is_ascii_hexdigit()) {
            return Some(token.to_ascii_lowercase());
        }
    }
    None
}

/// Maps a Rust target triple to `(archive_asset_name, binary_file_name)`.
pub fn expected_asset_for_target(target: &str) -> Option<(&'static str, &'static str)> {
    match target {
        "x86_64-unknown-linux-gnu" => Some(("lunacy-linux-x86_64.tar.gz", "lunacy-ui")),
        "x86_64-pc-windows-msvc" | "x86_64-pc-windows-gnu" => {
            Some(("lunacy-windows-x86_64.zip", "lunacy-ui.exe"))
        }
        _ => None,
    }
}

/// Parses a `SHA256SUMS.txt` body and returns the lowercase 64-hex digest for `asset_name`.
pub fn parse_checksum_for_asset(text: &str, asset_name: &str) -> Option<String> {
    for line in text.lines() {
        let trimmed = line.trim();
        if trimmed.is_empty() || trimmed.starts_with('#') {
            continue;
        }
        let mut parts = trimmed.split_whitespace();
        let digest = parts.next()?;
        let raw_name = match parts.next() {
            Some(n) => n,
            None => continue,
        };
        if digest.len() != 64 || !digest.chars().all(|c| c.is_ascii_hexdigit()) {
            continue;
        }
        let cleaned = raw_name
            .trim_start_matches('*')
            .trim_start_matches("./")
            .rsplit(['/', '\\'])
            .next()
            .unwrap_or(raw_name);
        if cleaned == asset_name {
            return Some(digest.to_ascii_lowercase());
        }
    }
    None
}

/// Returns whether a startup auto-check should run automatically.
pub fn should_auto_check() -> bool {
    #[cfg(target_arch = "wasm32")]
    {
        false
    }
    #[cfg(not(target_arch = "wasm32"))]
    {
        if std::env::var("LUNACY_UPDATE_URL")
            .ok()
            .is_some_and(|v| !v.trim().is_empty())
        {
            return true;
        }
        if BUILD_SHA == "dev" || cfg!(debug_assertions) {
            return false;
        }
        // Never hit GitHub during headless screenshot / scripted runs.
        !std::env::args()
            .any(|a| matches!(a.as_str(), "--script" | "--snap" | "--scene" | "--tree"))
    }
}

// ---------------------------------------------------------------------------------------------
// Native implementation

#[cfg(not(target_arch = "wasm32"))]
mod native {
    use std::fs;
    use std::io::{Cursor, Read, Write};
    use std::path::{Path, PathBuf};
    use std::sync::mpsc::{self, Receiver, Sender};
    use std::sync::OnceLock;
    use std::time::Duration;

    use serde::Deserialize;
    use sha2::{Digest, Sha256};

    use super::{
        expected_asset_for_target, is_same_commit, parse_checksum_for_asset, parse_sha_from_title,
        UpdateInfo, UpdateStatus, BUILD_SHA, BUILD_TARGET, BUILD_VERSION, DEFAULT_UPDATE_URL,
    };

    #[derive(Debug, Deserialize)]
    struct ReleasePayload {
        #[serde(default)]
        tag_name: String,
        #[serde(default)]
        name: String,
        #[serde(default)]
        target_commitish: String,
        #[serde(default)]
        html_url: String,
        #[serde(default)]
        published_at: String,
        #[serde(default)]
        body: String,
        #[serde(default)]
        assets: Vec<ReleaseAsset>,
    }

    #[derive(Debug, Deserialize)]
    struct ReleaseAsset {
        #[serde(default)]
        name: String,
        #[serde(default)]
        browser_download_url: String,
    }

    pub struct Updater {
        tx: Sender<UpdateStatus>,
        rx: Receiver<UpdateStatus>,
        waker: Option<rune::Waker>,
    }

    impl Default for Updater {
        fn default() -> Self {
            let (tx, rx) = mpsc::channel();
            Updater {
                tx,
                rx,
                waker: None,
            }
        }
    }

    impl Updater {
        pub fn set_waker(&mut self, waker: rune::Waker) {
            self.waker = Some(waker);
        }

        pub fn drain(&self) -> Vec<UpdateStatus> {
            let mut out = Vec::new();
            while let Ok(ev) = self.rx.try_recv() {
                out.push(ev);
            }
            out
        }

        pub fn check_now(&self) {
            let tx = self.tx.clone();
            let waker = self.waker.clone();
            let url = update_url();
            let _ = std::thread::Builder::new()
                .name("lunacy-updater-check".into())
                .spawn(move || {
                    let result = fetch_update_info(&url, BUILD_TARGET, BUILD_SHA);
                    let status = match result {
                        Ok(CheckOutcome::UpToDate(sha)) => {
                            UpdateStatus::UpToDate { checked_sha: sha }
                        }
                        Ok(CheckOutcome::Available(info)) => UpdateStatus::Available(*info),
                        Err(msg) => UpdateStatus::Error(msg),
                    };
                    let _ = tx.send(status);
                    if let Some(w) = waker {
                        w();
                    }
                });
        }

        pub fn install(&self, info: UpdateInfo) {
            let tx = self.tx.clone();
            let waker = self.waker.clone();
            let _ = std::thread::Builder::new()
                .name("lunacy-updater-install".into())
                .spawn(move || {
                    let emit = |st: UpdateStatus| {
                        let _ = tx.send(st);
                        if let Some(w) = &waker {
                            w();
                        }
                    };
                    match run_install(&info, &emit) {
                        Ok(()) => emit(UpdateStatus::ReadyToRestart(info)),
                        Err(msg) => emit(UpdateStatus::Error(msg)),
                    }
                });
        }
    }

    enum CheckOutcome {
        UpToDate(String),
        Available(Box<UpdateInfo>),
    }

    fn update_url() -> String {
        std::env::var("LUNACY_UPDATE_URL")
            .ok()
            .map(|v| v.trim().to_string())
            .filter(|v| !v.is_empty())
            .unwrap_or_else(|| DEFAULT_UPDATE_URL.to_string())
    }

    fn http_agent(timeout_secs: u64) -> ureq::Agent {
        ureq::Agent::new_with_config(
            ureq::Agent::config_builder()
                .http_status_as_error(false)
                .timeout_global(Some(Duration::from_secs(timeout_secs)))
                .build(),
        )
    }

    fn user_agent() -> String {
        format!("lunacy-ui/{BUILD_VERSION}")
    }

    fn get_text(url: &str, timeout_secs: u64) -> Result<String, String> {
        let agent = http_agent(timeout_secs);
        let mut resp = agent
            .get(url)
            .header("User-Agent", &user_agent())
            .header(
                "Accept",
                "application/vnd.github+json, application/json, text/plain",
            )
            .call()
            .map_err(|e| format!("request failed: {e}"))?;
        let status = resp.status().as_u16();
        let text = resp
            .body_mut()
            .read_to_string()
            .map_err(|e| format!("failed reading response: {e}"))?;
        if !(200..300).contains(&status) {
            return Err(format!("HTTP {status} from update server"));
        }
        Ok(text)
    }

    fn get_bytes(url: &str, timeout_secs: u64) -> Result<Vec<u8>, String> {
        let agent = http_agent(timeout_secs);
        let mut resp = agent
            .get(url)
            .header("User-Agent", &user_agent())
            .call()
            .map_err(|e| format!("download failed: {e}"))?;
        let status = resp.status().as_u16();
        if !(200..300).contains(&status) {
            return Err(format!("HTTP {status} while downloading update"));
        }
        let mut buf = Vec::new();
        resp.body_mut()
            .as_reader()
            .read_to_end(&mut buf)
            .map_err(|e| format!("failed reading download body: {e}"))?;
        Ok(buf)
    }

    fn fetch_update_info(url: &str, target: &str, local_sha: &str) -> Result<CheckOutcome, String> {
        let text = get_text(url, 15)?;
        let payload: ReleasePayload =
            serde_json::from_str(&text).map_err(|e| format!("invalid release JSON: {e}"))?;

        let commitish = payload.target_commitish.trim();
        let remote_sha = if commitish.len() >= 7 && commitish.chars().all(|c| c.is_ascii_hexdigit())
        {
            commitish.to_ascii_lowercase()
        } else {
            parse_sha_from_title(&payload.name)
                .ok_or_else(|| "release payload did not include a commit SHA".to_string())?
        };
        let short_sha: String = remote_sha.chars().take(7).collect();

        if is_same_commit(local_sha, &remote_sha) {
            return Ok(CheckOutcome::UpToDate(short_sha));
        }

        let checksums_url = payload
            .assets
            .iter()
            .find(|a| a.name == "SHA256SUMS.txt")
            .map(|a| a.browser_download_url.clone())
            .filter(|u| !u.is_empty());

        let (asset_name, asset_url) = match expected_asset_for_target(target) {
            Some((want_name, _)) => match payload.assets.iter().find(|a| a.name == want_name) {
                Some(asset) if !asset.browser_download_url.is_empty() => (
                    Some(asset.name.clone()),
                    Some(asset.browser_download_url.clone()),
                ),
                _ => (None, None),
            },
            None => (None, None),
        };

        Ok(CheckOutcome::Available(Box::new(UpdateInfo {
            tag: payload.tag_name,
            title: payload.name,
            sha: remote_sha,
            short_sha,
            html_url: payload.html_url,
            published_at: payload.published_at,
            notes: payload.body,
            asset_name,
            asset_url,
            checksums_url,
        })))
    }

    fn run_install(info: &UpdateInfo, emit: &dyn Fn(UpdateStatus)) -> Result<(), String> {
        let asset_name = info
            .asset_name
            .as_deref()
            .ok_or_else(|| format!("no downloadable release asset for target {BUILD_TARGET}"))?;
        let asset_url = info
            .asset_url
            .as_deref()
            .ok_or_else(|| "release asset URL missing".to_string())?;
        let checksums_url = info
            .checksums_url
            .as_deref()
            .ok_or_else(|| "SHA256SUMS.txt missing from release assets".to_string())?;
        let (_, bin_name) = expected_asset_for_target(BUILD_TARGET)
            .ok_or_else(|| format!("self-update is not supported on target {BUILD_TARGET}"))?;

        let exe_path =
            resolved_current_exe().ok_or_else(|| "cannot locate current executable".to_string())?;
        let exe_dir = exe_path
            .parent()
            .ok_or_else(|| "current executable has no parent directory".to_string())?;

        emit(UpdateStatus::Installing {
            info: info.clone(),
            stage: "Downloading checksums…".into(),
        });
        let sums_text = get_text(checksums_url, 20)?;
        let expected_sha = parse_checksum_for_asset(&sums_text, asset_name)
            .ok_or_else(|| format!("SHA256SUMS.txt has no entry for {asset_name}"))?;

        emit(UpdateStatus::Installing {
            info: info.clone(),
            stage: format!("Downloading {asset_name}…"),
        });
        let archive_bytes = get_bytes(asset_url, 180)?;

        emit(UpdateStatus::Installing {
            info: info.clone(),
            stage: "Verifying SHA-256 checksum…".into(),
        });
        verify_sha256(&archive_bytes, &expected_sha)?;

        let stage_dir = exe_dir.join(format!(".lunacy-ui-update-{}", std::process::id()));
        let _ = fs::remove_dir_all(&stage_dir);
        fs::create_dir_all(&stage_dir)
            .map_err(|e| format!("cannot create staging directory: {e}"))?;

        let install_res = (|| -> Result<(), String> {
            emit(UpdateStatus::Installing {
                info: info.clone(),
                stage: "Extracting binary…".into(),
            });
            let staged_bin = stage_dir.join(bin_name);
            if asset_name.ends_with(".tar.gz") {
                extract_tar_gz_binary(&archive_bytes, bin_name, &staged_bin)?;
            } else if asset_name.ends_with(".zip") {
                extract_zip_binary(&archive_bytes, bin_name, &staged_bin)?;
            } else {
                return Err(format!("unsupported archive format: {asset_name}"));
            }

            emit(UpdateStatus::Installing {
                info: info.clone(),
                stage: "Replacing executable…".into(),
            });
            replace_binary(&exe_path, &staged_bin, cfg!(windows))
        })();

        let _ = fs::remove_dir_all(&stage_dir);
        install_res
    }

    pub(crate) fn verify_sha256(bytes: &[u8], expected_hex: &str) -> Result<(), String> {
        let mut hasher = Sha256::new();
        hasher.update(bytes);
        let digest = hasher.finalize();
        let actual: String = digest.iter().map(|b| format!("{b:02x}")).collect();
        let expected = expected_hex.trim().to_ascii_lowercase();
        if actual != expected {
            return Err(format!(
                "SHA-256 mismatch: expected {expected}, got {actual}"
            ));
        }
        Ok(())
    }

    pub(crate) fn extract_tar_gz_binary(
        archive_bytes: &[u8],
        bin_name: &str,
        dest_path: &Path,
    ) -> Result<(), String> {
        let gz = flate2::read::GzDecoder::new(Cursor::new(archive_bytes));
        let mut archive = tar::Archive::new(gz);
        let entries = archive
            .entries()
            .map_err(|e| format!("failed reading tar archive: {e}"))?;
        for entry in entries {
            let mut entry = entry.map_err(|e| format!("corrupt tar entry: {e}"))?;
            if !entry.header().entry_type().is_file() {
                continue;
            }
            let path = entry
                .path()
                .map_err(|e| format!("invalid tar entry path: {e}"))?;
            let normalized = path.to_string_lossy().replace('\\', "/");
            if normalized == bin_name || normalized.ends_with(&format!("/{bin_name}")) {
                let mut out = fs::File::create(dest_path)
                    .map_err(|e| format!("cannot create staged binary: {e}"))?;
                std::io::copy(&mut entry, &mut out)
                    .map_err(|e| format!("failed extracting {bin_name}: {e}"))?;
                out.flush()
                    .map_err(|e| format!("failed flushing {bin_name}: {e}"))?;
                return Ok(());
            }
        }
        Err(format!("archive did not contain {bin_name}"))
    }

    pub(crate) fn extract_zip_binary(
        archive_bytes: &[u8],
        bin_name: &str,
        dest_path: &Path,
    ) -> Result<(), String> {
        let mut archive = zip::ZipArchive::new(Cursor::new(archive_bytes))
            .map_err(|e| format!("failed reading zip archive: {e}"))?;
        for idx in 0..archive.len() {
            let mut entry = archive
                .by_index(idx)
                .map_err(|e| format!("corrupt zip entry: {e}"))?;
            if entry.is_dir() {
                continue;
            }
            let normalized = entry.name().replace('\\', "/");
            if normalized == bin_name || normalized.ends_with(&format!("/{bin_name}")) {
                let mut out = fs::File::create(dest_path)
                    .map_err(|e| format!("cannot create staged binary: {e}"))?;
                std::io::copy(&mut entry, &mut out)
                    .map_err(|e| format!("failed extracting {bin_name}: {e}"))?;
                out.flush()
                    .map_err(|e| format!("failed flushing {bin_name}: {e}"))?;
                return Ok(());
            }
        }
        Err(format!("zip archive did not contain {bin_name}"))
    }

    pub(crate) fn old_binary_path(exe_path: &Path) -> PathBuf {
        let mut os = exe_path.as_os_str().to_os_string();
        os.push(".old");
        PathBuf::from(os)
    }

    pub(crate) fn replace_binary(
        exe_path: &Path,
        staged_path: &Path,
        windows_swap: bool,
    ) -> Result<(), String> {
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let _ = fs::set_permissions(staged_path, fs::Permissions::from_mode(0o755));
        }

        if windows_swap {
            let old_path = old_binary_path(exe_path);
            let _ = fs::remove_file(&old_path);
            fs::rename(exe_path, &old_path)
                .map_err(|e| format!("cannot move current binary aside: {e}"))?;
            if let Err(e) = fs::rename(staged_path, exe_path) {
                let _ = fs::rename(&old_path, exe_path);
                return Err(format!("cannot move updated binary into place: {e}"));
            }
            Ok(())
        } else {
            fs::rename(staged_path, exe_path).map_err(|e| format!("cannot replace executable: {e}"))
        }
    }

    static INITIAL_EXE: OnceLock<Option<PathBuf>> = OnceLock::new();

    pub(crate) fn normalize_exe_path(path: PathBuf) -> PathBuf {
        #[cfg(target_os = "linux")]
        {
            use std::os::unix::ffi::{OsStrExt, OsStringExt};
            let bytes = path.as_os_str().as_bytes();
            if let Some(stripped) = bytes.strip_suffix(b" (deleted)") {
                if !stripped.is_empty() {
                    return PathBuf::from(std::ffi::OsString::from_vec(stripped.to_vec()));
                }
            }
            path
        }
        #[cfg(not(target_os = "linux"))]
        {
            path
        }
    }

    /// Returns the resolved executable path captured at startup, stripping any trailing
    /// `" (deleted)"` marker on Linux so self-update replacement and relaunch keep working
    /// after the running binary is atomically renamed over.
    pub fn resolved_current_exe() -> Option<PathBuf> {
        INITIAL_EXE
            .get_or_init(|| std::env::current_exe().ok().map(normalize_exe_path))
            .clone()
    }

    pub fn cleanup_old_binary() {
        if let Some(exe) = resolved_current_exe() {
            let old = old_binary_path(&exe);
            if old.exists() {
                let _ = fs::remove_file(old);
            }
        }
    }

    pub fn relaunch_current_exe() -> Result<(), String> {
        let exe =
            resolved_current_exe().ok_or_else(|| "cannot locate current executable".to_string())?;
        let args: Vec<std::ffi::OsString> = std::env::args_os().skip(1).collect();
        std::process::Command::new(&exe)
            .args(&args)
            .spawn()
            .map_err(|e| format!("failed to relaunch {}: {e}", exe.display()))?;
        Ok(())
    }
}

#[cfg(not(target_arch = "wasm32"))]
pub use native::{cleanup_old_binary, relaunch_current_exe, resolved_current_exe, Updater};

// ---------------------------------------------------------------------------------------------
// Web (wasm32) no-op stub

#[cfg(target_arch = "wasm32")]
#[derive(Default)]
pub struct Updater {
    _priv: (),
}

#[cfg(target_arch = "wasm32")]
impl Updater {
    pub fn drain(&self) -> Vec<UpdateStatus> {
        Vec::new()
    }

    pub fn check_now(&self) {}

    pub fn install(&self, _info: UpdateInfo) {}
}

#[cfg(target_arch = "wasm32")]
pub fn resolved_current_exe() -> Option<std::path::PathBuf> {
    None
}

#[cfg(target_arch = "wasm32")]
pub fn cleanup_old_binary() {}

#[cfg(target_arch = "wasm32")]
pub fn relaunch_current_exe() -> Result<(), String> {
    Err("self-update is not available in the web build".into())
}

// ---------------------------------------------------------------------------------------------
// Tests

#[cfg(all(test, not(target_arch = "wasm32")))]
mod tests {
    use std::io::Write;

    use sha2::{Digest, Sha256};

    use super::native::{
        extract_tar_gz_binary, extract_zip_binary, normalize_exe_path, old_binary_path,
        replace_binary, verify_sha256,
    };
    use super::*;

    fn temp_dir(name: &str) -> std::path::PathBuf {
        let dir =
            std::env::temp_dir().join(format!("lunacy-updater-test-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).expect("create tempdir");
        dir
    }

    fn sha256_hex(data: &[u8]) -> String {
        let mut h = Sha256::new();
        h.update(data);
        h.finalize().iter().map(|b| format!("{b:02x}")).collect()
    }

    #[test]
    fn commit_sha_comparison_handles_prefixes_and_dev() {
        assert!(is_same_commit(
            "f5841e0",
            "f5841e0123456789abcdef0123456789abcdef01"
        ));
        assert!(is_same_commit(
            "F5841E0123456789ABCDEF0123456789ABCDEF01",
            "f5841e0"
        ));
        assert!(!is_same_commit("f5841e0", "9a8b7c6d5e4f"));
        assert!(!is_same_commit("dev", "f5841e0123456789"));
        assert!(!is_same_commit("dev", "dev"));
        assert!(!is_same_commit("f5841", "f5841e0123456789"));
    }

    #[test]
    fn parses_sha_from_release_title() {
        assert_eq!(
            parse_sha_from_title("Pre-release (f5841e0)").as_deref(),
            Some("f5841e0")
        );
        assert_eq!(
            parse_sha_from_title("Build ABCDEF123456").as_deref(),
            Some("abcdef123456")
        );
        assert_eq!(parse_sha_from_title("No sha here"), None);
    }

    #[test]
    fn checksum_parser_and_verifier_accept_match_and_reject_mismatch() {
        let payload = b"fake-lunacy-ui-archive-bytes";
        let good = sha256_hex(payload);
        let sums = format!(
            "# release checksums\n\
             {good}  lunacy-linux-x86_64.tar.gz\n\
             0000000000000000000000000000000000000000000000000000000000000000 *dist/lunacy-windows-x86_64.zip\n"
        );
        let parsed_linux =
            parse_checksum_for_asset(&sums, "lunacy-linux-x86_64.tar.gz").expect("linux checksum");
        assert_eq!(parsed_linux, good);
        assert!(verify_sha256(payload, &parsed_linux).is_ok());

        let parsed_win =
            parse_checksum_for_asset(&sums, "lunacy-windows-x86_64.zip").expect("windows checksum");
        assert!(verify_sha256(payload, &parsed_win).is_err());
        assert!(parse_checksum_for_asset(&sums, "missing.tar.gz").is_none());
    }

    #[test]
    fn extracts_binary_from_tar_gz() {
        let dir = temp_dir("targz");
        let payload = b"#!/bin/sh\necho lunacy-ui\n";

        let mut tar_buf = Vec::new();
        {
            let gz = flate2::write::GzEncoder::new(&mut tar_buf, flate2::Compression::default());
            let mut builder = tar::Builder::new(gz);
            let mut header = tar::Header::new_gnu();
            header.set_size(payload.len() as u64);
            header.set_mode(0o755);
            header.set_cksum();
            builder
                .append_data(&mut header, "lunacy-linux-x86_64/lunacy-ui", &payload[..])
                .expect("append tar entry");
            builder
                .into_inner()
                .expect("finish tar")
                .finish()
                .expect("finish gz");
        }

        let dest = dir.join("lunacy-ui");
        extract_tar_gz_binary(&tar_buf, "lunacy-ui", &dest).expect("extract tar.gz");
        assert_eq!(std::fs::read(&dest).expect("read extracted"), payload);
        assert!(extract_tar_gz_binary(&tar_buf, "missing-bin", &dest).is_err());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn extracts_binary_from_zip() {
        let dir = temp_dir("zip");
        let payload = b"MZ-fake-windows-pe-binary";

        let mut zip_buf = std::io::Cursor::new(Vec::new());
        {
            let mut writer = zip::ZipWriter::new(&mut zip_buf);
            let opts = zip::write::SimpleFileOptions::default()
                .compression_method(zip::CompressionMethod::Deflated);
            writer
                .start_file("lunacy-windows-x86_64/lunacy-ui.exe", opts)
                .expect("start zip file");
            writer.write_all(payload).expect("write zip payload");
            writer.finish().expect("finish zip");
        }

        let dest = dir.join("lunacy-ui.exe");
        extract_zip_binary(&zip_buf.into_inner(), "lunacy-ui.exe", &dest).expect("extract zip");
        assert_eq!(std::fs::read(&dest).expect("read extracted"), payload);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn atomic_swap_and_windows_old_swap_work_and_rollback() {
        let dir = temp_dir("swap");

        // Unix atomic swap
        let exe_u = dir.join("lunacy-ui");
        let staged_u = dir.join("staged-u");
        std::fs::write(&exe_u, b"old-unix").unwrap();
        std::fs::write(&staged_u, b"new-unix").unwrap();
        replace_binary(&exe_u, &staged_u, false).expect("unix swap");
        assert_eq!(std::fs::read(&exe_u).unwrap(), b"new-unix");
        assert!(!staged_u.exists());

        // Windows-style .old swap
        let exe_w = dir.join("lunacy-ui.exe");
        let staged_w = dir.join("staged-w");
        let old_w = old_binary_path(&exe_w);
        std::fs::write(&exe_w, b"old-win").unwrap();
        std::fs::write(&staged_w, b"new-win").unwrap();
        replace_binary(&exe_w, &staged_w, true).expect("windows swap");
        assert_eq!(std::fs::read(&exe_w).unwrap(), b"new-win");
        assert_eq!(std::fs::read(&old_w).unwrap(), b"old-win");
        assert!(!staged_w.exists());

        // Rollback when staged file is missing
        let missing = dir.join("does-not-exist");
        assert!(replace_binary(&exe_w, &missing, true).is_err());
        assert_eq!(std::fs::read(&exe_w).unwrap(), b"new-win");

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn normalize_exe_path_strips_linux_deleted_suffix_and_caches_resolved_exe() {
        let normal = std::path::PathBuf::from("/usr/local/bin/lunacy-ui");
        assert_eq!(normalize_exe_path(normal.clone()), normal);

        let deleted = std::path::PathBuf::from("/usr/local/bin/lunacy-ui (deleted)");
        #[cfg(target_os = "linux")]
        assert_eq!(normalize_exe_path(deleted), normal);
        #[cfg(not(target_os = "linux"))]
        assert_eq!(normalize_exe_path(deleted.clone()), deleted);

        let first = resolved_current_exe().expect("resolved current exe");
        let second = resolved_current_exe().expect("cached current exe");
        assert_eq!(first, second);
        assert!(
            !first.to_string_lossy().ends_with(" (deleted)"),
            "resolved exe should not end with (deleted): {}",
            first.display()
        );
    }
}
