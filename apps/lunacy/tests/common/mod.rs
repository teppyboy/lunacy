//! Shared helper: a throwaway project with a bare origin and a working clone.

use std::path::{Path, PathBuf};
use std::process::Command;

pub struct Fixture {
    _dir: tempfile::TempDir,
    pub bare: PathBuf,
    pub work: PathBuf,
}

impl Fixture {
    /// `bare` is the origin; `work` is a clone with `origin` pointing at it.
    pub fn new() -> Fixture {
        let dir = tempfile::tempdir().expect("tempdir");
        let root = dir.path().to_path_buf();
        let bare = root.join("origin.git");
        let work = root.join("work");
        run(
            root.as_path(),
            &["init", "--bare", "-b", "master", bare.to_str().unwrap()],
        );
        run(
            root.as_path(),
            &["clone", bare.to_str().unwrap(), work.to_str().unwrap()],
        );
        // The clone must not inherit the host's user identity.
        run(&work, &["config", "user.email", "lunacy@example.invalid"]);
        run(&work, &["config", "user.name", "lunacy tests"]);
        Fixture {
            _dir: dir,
            bare,
            work,
        }
    }

    /// Commit a change to `file` and push it to `master`.
    pub fn commit(&self, file: &str, contents: &str, message: &str) -> String {
        let path = self.work.join(file);
        if let Some(dir) = path.parent() {
            std::fs::create_dir_all(dir).expect("create parent dirs");
        }
        std::fs::write(&path, contents).expect("write file");
        run(&self.work, &["add", file]);
        run(&self.work, &["commit", "-q", "-m", message]);
        run(&self.work, &["push", "-q", "origin", "HEAD:master"]);
        self.tip()
    }

    /// The sha `master` points at in the bare origin.
    pub fn tip(&self) -> String {
        let out = Command::new("git")
            .arg("-C")
            .arg(&self.bare)
            .args(["rev-parse", "master"])
            .output()
            .expect("rev-parse");
        assert!(out.status.success(), "rev-parse failed");
        String::from_utf8_lossy(&out.stdout).trim().to_string()
    }
}

pub fn run(dir: &Path, args: &[&str]) {
    let out = Command::new("git")
        .arg("-C")
        .arg(dir)
        .args(args)
        .output()
        .unwrap_or_else(|e| panic!("git {args:?}: {e}"));
    assert!(
        out.status.success(),
        "git {args:?} failed: {}{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
}
