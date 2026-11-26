//! Thin `git` wrappers. Everything shells out; no libgit2 dependency.

use anyhow::{bail, Context, Result};
use std::io::Read;
use std::path::Path;
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

fn git(repo: &Path, args: &[&str]) -> Result<String> {
    let out = Command::new("git")
        .arg("-C")
        .arg(repo)
        .args(args)
        .output()
        .with_context(|| format!("running git {} in {}", args.join(" "), repo.display()))?;
    if !out.status.success() {
        let stderr = String::from_utf8_lossy(&out.stderr);
        bail!(
            "git {} failed: {}",
            args.join(" "),
            stderr.trim().lines().last().unwrap_or("(no output)")
        );
    }
    Ok(String::from_utf8_lossy(&out.stdout).trim().to_string())
}

/// How long one network git command (`fetch`, `ls-remote`) may take. A hung credential helper
/// or network otherwise stalls the project's worker for good (seen: a `gh auth git-credential`
/// child stuck for 46 minutes).
const FETCH_TIMEOUT: Duration = Duration::from_secs(120);

fn git_bounded(repo: &Path, args: &[&str], timeout: Duration) -> Result<String> {
    let mut child = Command::new("git")
        .arg("-C")
        .arg(repo)
        .args(args)
        // Never wait on a terminal prompt for credentials: there is no one to answer it.
        .env("GIT_TERMINAL_PROMPT", "0")
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .with_context(|| format!("running git {} in {}", args.join(" "), repo.display()))?;

    let stdout_handle = child.stdout.take().map(|mut out| {
        std::thread::spawn(move || {
            let mut buf = Vec::new();
            let _ = out.read_to_end(&mut buf);
            buf
        })
    });
    let stderr_handle = child.stderr.take().map(|mut err| {
        std::thread::spawn(move || {
            let mut buf = Vec::new();
            let _ = err.read_to_end(&mut buf);
            buf
        })
    });

    let started = Instant::now();
    let status = loop {
        if let Some(status) = child.try_wait()? {
            break status;
        }
        if started.elapsed() >= timeout {
            let _ = child.kill();
            let _ = child.wait();
            if let Some(h) = stdout_handle {
                let _ = h.join();
            }
            if let Some(h) = stderr_handle {
                let _ = h.join();
            }
            bail!(
                "git {} timed out after {}s",
                args.join(" "),
                timeout.as_secs()
            );
        }
        std::thread::sleep(Duration::from_millis(50));
    };

    let stdout_bytes = stdout_handle
        .and_then(|h| h.join().ok())
        .unwrap_or_default();
    let stderr_bytes = stderr_handle
        .and_then(|h| h.join().ok())
        .unwrap_or_default();

    if !status.success() {
        let stderr = String::from_utf8_lossy(&stderr_bytes);
        bail!(
            "git {} failed: {}",
            args.join(" "),
            stderr.trim().lines().last().unwrap_or("(no output)")
        );
    }
    Ok(String::from_utf8_lossy(&stdout_bytes).trim().to_string())
}

/// True when `s` is a valid commit SHA (7..=64 ASCII hex characters).
pub fn is_valid_sha(s: &str) -> bool {
    (7..=64).contains(&s.len()) && s.bytes().all(|b| b.is_ascii_hexdigit())
}

/// True when `s` is a safe git ref/branch/tag/remote name (no leading `-`,
/// no `..`, no backslashes, no control/whitespace chars, no rev-spec metachars).
pub fn is_valid_git_ref(s: &str) -> bool {
    !s.is_empty()
        && !s.starts_with('-')
        && !s.contains("..")
        && !s.contains('\\')
        && !s.contains("@{")
        && !s.chars().any(|c| {
            c.is_ascii_control()
                || c.is_control()
                || c.is_whitespace()
                || matches!(c, '~' | '^' | ':' | '?' | '*' | '[')
        })
}

fn ensure_valid_sha(sha: &str) -> Result<()> {
    if !is_valid_sha(sha) {
        bail!("invalid commit sha: {sha:?}");
    }
    Ok(())
}

fn ensure_valid_ref(value: &str, label: &str) -> Result<()> {
    if !is_valid_git_ref(value) {
        bail!("invalid {label}: {value:?}");
    }
    Ok(())
}

fn ensure_valid_rev(rev: &str) -> Result<()> {
    let base = rev.strip_suffix('^').unwrap_or(rev);
    if !is_valid_git_ref(base) {
        bail!("invalid git revision: {rev:?}");
    }
    Ok(())
}

pub fn fetch(repo: &Path, remote: &str, branch: &str) -> Result<()> {
    ensure_valid_ref(remote, "git remote")?;
    ensure_valid_ref(branch, "git branch")?;
    let refspec = format!("{branch}:refs/remotes/{remote}/{branch}");
    git_bounded(
        repo,
        &["fetch", "--quiet", "--", remote, &refspec],
        FETCH_TIMEOUT,
    )?;
    Ok(())
}

/// Fetch a pull request head commit (bounded 120s, non-interactive).
/// Tries `refs/pull/<pr_number>/head` first, falling back to fetching `<sha>` directly.
pub fn fetch_pr(repo: &Path, remote: &str, pr_number: u64, sha: &str) -> Result<()> {
    ensure_valid_ref(remote, "git remote")?;
    if !sha.is_empty() {
        ensure_valid_sha(sha)?;
    }
    let pr_ref = format!("refs/pull/{pr_number}/head");
    let first = git_bounded(
        repo,
        &["fetch", "--quiet", "--", remote, &pr_ref],
        FETCH_TIMEOUT,
    );
    if first.is_ok() && (sha.is_empty() || rev_parse(repo, sha).is_ok()) {
        return Ok(());
    }
    if !sha.is_empty()
        && git_bounded(
            repo,
            &["fetch", "--quiet", "--", remote, sha],
            FETCH_TIMEOUT,
        )
        .is_ok()
    {
        return Ok(());
    }
    first.map(|_| ())
}

/// List remote tags as `(tag_name, sha)` pairs via `git ls-remote --tags --refs <remote>`.
pub fn list_remote_tags(repo: &Path, remote: &str) -> Result<Vec<(String, String)>> {
    ensure_valid_ref(remote, "git remote")?;
    let out = git_bounded(
        repo,
        &["ls-remote", "--tags", "--refs", "--", remote],
        FETCH_TIMEOUT,
    )?;
    let mut tags = Vec::new();
    for line in out.lines() {
        let line = line.trim();
        if line.is_empty() {
            continue;
        }
        let Some((sha, raw_ref)) = line.split_once('\t') else {
            continue;
        };
        if let Some(tag) = raw_ref.strip_prefix("refs/tags/") {
            if !tag.is_empty() && !sha.is_empty() {
                tags.push((tag.to_string(), sha.to_string()));
            }
        }
    }
    Ok(tags)
}

/// Fetch a single tag from `remote` (bounded 120s, non-interactive).
pub fn fetch_tag(repo: &Path, remote: &str, tag: &str) -> Result<()> {
    ensure_valid_ref(remote, "git remote")?;
    ensure_valid_ref(tag, "git tag")?;
    let short = tag
        .strip_prefix("refs/tags/")
        .or_else(|| tag.strip_prefix("tags/"))
        .unwrap_or(tag);
    ensure_valid_ref(short, "git tag")?;
    let refspec = format!("refs/tags/{short}:refs/tags/{short}");
    git_bounded(
        repo,
        &["fetch", "--quiet", "--force", "--", remote, &refspec],
        FETCH_TIMEOUT,
    )?;
    Ok(())
}

pub fn rev_parse(repo: &Path, rev: &str) -> Result<String> {
    ensure_valid_rev(rev)?;
    git(
        repo,
        &["rev-parse", "--verify", &format!("{rev}^{{commit}}")],
    )
}

pub fn upstream_sha(repo: &Path, remote: &str, branch: &str) -> Result<String> {
    ensure_valid_ref(remote, "git remote")?;
    ensure_valid_ref(branch, "git branch")?;
    rev_parse(repo, &format!("refs/remotes/{remote}/{branch}"))
}

/// `(message, author name, author email)` of one commit; empty parts on failure.
pub fn commit_details(repo: &Path, sha: &str) -> (String, String, String) {
    if !is_valid_sha(sha) {
        return (String::new(), String::new(), String::new());
    }
    let raw = git(repo, &["log", "-1", "--format=%B%x1f%an%x1f%ae", sha, "--"]).unwrap_or_default();
    let mut parts = raw.split('\u{1f}');
    let message = parts.next().unwrap_or_default().trim_end().to_string();
    let name = parts.next().unwrap_or_default().trim().to_string();
    let email = parts.next().unwrap_or_default().trim().to_string();
    (message, name, email)
}

pub fn subject(repo: &Path, sha: &str) -> String {
    if !is_valid_sha(sha) {
        return String::new();
    }
    git(repo, &["log", "-1", "--format=%s", sha, "--"]).unwrap_or_default()
}

pub fn remote_url(repo: &Path, remote: &str) -> Option<String> {
    if !is_valid_git_ref(remote) {
        return None;
    }
    git(repo, &["config", "--get", &format!("remote.{remote}.url")]).ok()
}

pub fn is_dirty(repo: &Path) -> Result<bool> {
    let out = git(repo, &["status", "--porcelain", "--untracked-files=normal"])?;
    Ok(!out.is_empty())
}

pub fn is_ancestor(repo: &Path, ancestor: &str, descendant: &str) -> Result<bool> {
    ensure_valid_rev(ancestor)?;
    ensure_valid_rev(descendant)?;
    let out = Command::new("git")
        .arg("-C")
        .arg(repo)
        .args(["merge-base", "--is-ancestor", ancestor, descendant])
        .output()?;
    match out.status.code() {
        Some(0) => Ok(true),
        Some(1) => Ok(false),
        _ => bail!(
            "git merge-base --is-ancestor failed: {}",
            String::from_utf8_lossy(&out.stderr).trim()
        ),
    }
}

pub fn current_branch(repo: &Path) -> Option<String> {
    git(repo, &["rev-parse", "--abbrev-ref", "HEAD"]).ok()
}

/// Fast-forward the checkout at `repo` to `sha`; fails if that is not a fast-forward.
pub fn fast_forward(repo: &Path, remote: &str, branch: &str, sha: &str) -> Result<()> {
    ensure_valid_ref(remote, "git remote")?;
    ensure_valid_ref(branch, "git branch")?;
    ensure_valid_sha(sha)?;
    if is_dirty(repo)? {
        bail!(
            "worktree at {} is dirty; refusing to fast-forward",
            repo.display()
        );
    }
    let head = rev_parse(repo, "HEAD")?;
    if head == sha {
        return Ok(());
    }
    if !is_ancestor(repo, &head, sha)? {
        bail!(
            "local HEAD {} is not an ancestor of {sha}; refusing a non-fast-forward update",
            crate::model::short_sha(&head)
        );
    }
    let on_branch = current_branch(repo).as_deref() == Some(branch);
    if !on_branch {
        git(repo, &["checkout", "--quiet", branch])
            .or_else(|_| git(repo, &["checkout", "--quiet", "-B", branch, sha]))?;
    }
    let _ = remote;
    git(repo, &["merge", "--ff-only", "--quiet", sha])?;
    Ok(())
}

/// `git worktree add --detach <dest> <sha>`; caller removes with [`remove_worktree`].
pub fn add_worktree(repo: &Path, dest: &Path, sha: &str) -> Result<()> {
    ensure_valid_sha(sha)?;
    if dest.exists() {
        bail!("worktree destination {} already exists", dest.display());
    }
    git(
        repo,
        &[
            "worktree",
            "add",
            "--detach",
            "--force",
            "--",
            &dest.to_string_lossy(),
            sha,
        ],
    )?;
    Ok(())
}

pub fn remove_worktree(repo: &Path, dest: &Path) {
    let _ = git(
        repo,
        &[
            "worktree",
            "remove",
            "--force",
            "--",
            &dest.to_string_lossy(),
        ],
    );
    if dest.exists() {
        let _ = std::fs::remove_dir_all(dest);
    }
    let _ = git(repo, &["worktree", "prune"]);
}

/// Fresh clone of `sha` from `repo` (act needs a real `.git`, not a linked worktree).
pub fn clone_at(repo: &Path, dest: &Path, remote: &str, sha: &str) -> Result<()> {
    ensure_valid_ref(remote, "git remote")?;
    ensure_valid_sha(sha)?;
    let out = Command::new("git")
        .args(["clone", "--quiet", "--no-checkout", "--"])
        .arg(repo)
        .arg(dest)
        .output()
        .context("running git clone")?;
    if !out.status.success() {
        bail!(
            "git clone failed: {}",
            String::from_utf8_lossy(&out.stderr).trim()
        );
    }
    let source = remote_url(repo, remote).unwrap_or_else(|| repo.to_string_lossy().to_string());
    let _ = Command::new("git")
        .arg("-C")
        .arg(dest)
        .args(["fetch", "--quiet", "--", &source, sha])
        .output();
    git(dest, &["checkout", "--quiet", "--detach", sha])
        .with_context(|| format!("checking out {sha} in {}", dest.display()))?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sha_validation() {
        assert!(is_valid_sha("0123456"));
        assert!(is_valid_sha("0123456789abcdef0123456789abcdef01234567"));
        assert!(is_valid_sha(
            "0123456789ABCDEF0123456789abcdef0123456789abcdef0123456789abcdef"
        ));
        assert!(!is_valid_sha(""));
        assert!(!is_valid_sha("012345")); // < 7
        assert!(!is_valid_sha(&"a".repeat(65))); // > 64
        assert!(!is_valid_sha("-abcdef1"));
        assert!(!is_valid_sha("--upload-pack=id"));
        assert!(!is_valid_sha("012345g"));
        assert!(!is_valid_sha("0123456 "));
    }

    #[test]
    fn git_ref_validation() {
        assert!(is_valid_git_ref("master"));
        assert!(is_valid_git_ref("feat/pr-42"));
        assert!(is_valid_git_ref("refs/heads/main"));
        assert!(is_valid_git_ref("refs/tags/v1.2.3"));
        assert!(is_valid_git_ref("tags/v1.0.0-rc.1"));
        assert!(is_valid_git_ref("HEAD"));

        for bad in [
            "",
            "-foo",
            "--upload-pack=id",
            "a..b",
            "a\\b",
            "a b",
            "a\nb",
            "a\0b",
            "a~1",
            "a^",
            "a:b",
            "a?b",
            "a*b",
            "a[0]",
            "a@{0}",
        ] {
            assert!(!is_valid_git_ref(bad), "expected invalid ref: {bad:?}");
        }
    }

    #[test]
    fn git_helpers_reject_option_injection() {
        let dir = tempfile::tempdir().unwrap();
        let repo = dir.path();
        assert!(fetch(repo, "--upload-pack=id", "master").is_err());
        assert!(fetch(repo, "origin", "--upload-pack=id").is_err());
        assert!(fetch_pr(repo, "origin", 1, "--upload-pack=id").is_err());
        assert!(fetch_pr(repo, "-origin", 1, "").is_err());
        assert!(fetch_tag(repo, "origin", "--force").is_err());
        assert!(rev_parse(repo, "--all").is_err());
        assert!(add_worktree(repo, &repo.join("wt"), "--orphan").is_err());
        assert!(clone_at(repo, &repo.join("cl"), "origin", "--detach").is_err());
        assert!(fast_forward(repo, "origin", "-master", "01234567").is_err());
        assert!(fast_forward(repo, "origin", "master", "--ff").is_err());
        assert_eq!(subject(repo, "--format=%H"), "");
        assert_eq!(
            commit_details(repo, "--format=%H"),
            (String::new(), String::new(), String::new())
        );
    }
}
