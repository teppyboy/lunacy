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

/// How long one `git fetch` may take. A hung credential helper or network otherwise stalls the
/// project's worker for good (seen: a `gh auth git-credential` child stuck for 46 minutes).
const FETCH_TIMEOUT: Duration = Duration::from_secs(120);

pub fn fetch(repo: &Path, remote: &str, branch: &str) -> Result<()> {
    let refspec = format!("{branch}:refs/remotes/{remote}/{branch}");
    let mut child = Command::new("git")
        .arg("-C")
        .arg(repo)
        .args(["fetch", "--quiet", remote, &refspec])
        // Never wait on a terminal prompt for credentials: there is no one to answer it.
        .env("GIT_TERMINAL_PROMPT", "0")
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::piped())
        .spawn()
        .with_context(|| format!("running git fetch in {}", repo.display()))?;
    let started = Instant::now();
    let status = loop {
        if let Some(status) = child.try_wait()? {
            break status;
        }
        if started.elapsed() >= FETCH_TIMEOUT {
            let _ = child.kill();
            let _ = child.wait();
            bail!(
                "git fetch {remote} {branch} timed out after {}s",
                FETCH_TIMEOUT.as_secs()
            );
        }
        std::thread::sleep(Duration::from_millis(100));
    };
    if !status.success() {
        let mut stderr = String::new();
        if let Some(mut e) = child.stderr.take() {
            let _ = e.read_to_string(&mut stderr);
        }
        bail!(
            "git fetch failed: {}",
            stderr.trim().lines().last().unwrap_or("(no output)")
        );
    }
    Ok(())
}

pub fn rev_parse(repo: &Path, rev: &str) -> Result<String> {
    git(
        repo,
        &["rev-parse", "--verify", &format!("{rev}^{{commit}}")],
    )
}

pub fn upstream_sha(repo: &Path, remote: &str, branch: &str) -> Result<String> {
    rev_parse(repo, &format!("refs/remotes/{remote}/{branch}"))
}

/// `(message, author name, author email)` of one commit; empty parts on failure.
pub fn commit_details(repo: &Path, sha: &str) -> (String, String, String) {
    let raw = git(repo, &["log", "-1", "--format=%B%x1f%an%x1f%ae", sha]).unwrap_or_default();
    let mut parts = raw.split('\u{1f}');
    let message = parts.next().unwrap_or_default().trim_end().to_string();
    let name = parts.next().unwrap_or_default().trim().to_string();
    let email = parts.next().unwrap_or_default().trim().to_string();
    (message, name, email)
}

pub fn subject(repo: &Path, sha: &str) -> String {
    git(repo, &["log", "-1", "--format=%s", sha]).unwrap_or_default()
}

pub fn remote_url(repo: &Path, remote: &str) -> Option<String> {
    git(repo, &["config", "--get", &format!("remote.{remote}.url")]).ok()
}

pub fn is_dirty(repo: &Path) -> Result<bool> {
    let out = git(repo, &["status", "--porcelain", "--untracked-files=normal"])?;
    Ok(!out.is_empty())
}

pub fn is_ancestor(repo: &Path, ancestor: &str, descendant: &str) -> Result<bool> {
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
            &dest.to_string_lossy(),
            sha,
        ],
    )?;
    Ok(())
}

pub fn remove_worktree(repo: &Path, dest: &Path) {
    let _ = git(
        repo,
        &["worktree", "remove", "--force", &dest.to_string_lossy()],
    );
    if dest.exists() {
        let _ = std::fs::remove_dir_all(dest);
    }
    let _ = git(repo, &["worktree", "prune"]);
}

/// Fresh clone of `sha` from `repo` (act needs a real `.git`, not a linked worktree).
pub fn clone_at(repo: &Path, dest: &Path, remote: &str, sha: &str) -> Result<()> {
    let out = Command::new("git")
        .args(["clone", "--quiet", "--no-checkout"])
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
        .args(["fetch", "--quiet", &source, sha])
        .output();
    git(dest, &["checkout", "--quiet", "--detach", sha])
        .with_context(|| format!("checking out {sha} in {}", dest.display()))?;
    Ok(())
}
