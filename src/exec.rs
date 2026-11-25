//! Running a child process with a hard timeout, streamed into a log file.
//!
//! Unix: the child gets its own process group and the timeout kills the group.
//! Windows: the child goes into a Job object and the timeout kills the tree.

use anyhow::{Context, Result};
use std::collections::BTreeMap;
use std::io::{BufRead, BufReader, Write};
use std::path::PathBuf;
use std::process::{Command, Stdio};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

/// Something that watches each output line as it arrives (act's `--json`).
pub type LineSink = Arc<dyn Fn(&str) + Send + Sync>;

#[derive(Debug, Clone)]
pub struct ExecSpec {
    pub program: String,
    pub args: Vec<String>,
    pub cwd: PathBuf,
    pub env: BTreeMap<String, String>,
    pub timeout: Duration,
    pub log_path: PathBuf,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Outcome {
    Success,
    Failed(i32),
    Timeout,
    Spawn,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ExecResult {
    pub outcome: Outcome,
}

pub fn exec(spec: &ExecSpec, sink: Option<LineSink>) -> Result<ExecResult> {
    if let Some(parent) = spec.log_path.parent() {
        std::fs::create_dir_all(parent)
            .with_context(|| format!("creating log dir {}", parent.display()))?;
    }
    let file = std::fs::File::create(&spec.log_path)
        .with_context(|| format!("creating log {}", spec.log_path.display()))?;
    let log = Arc::new(Mutex::new(file));

    let mut cmd = Command::new(&spec.program);
    cmd.args(&spec.args)
        .current_dir(&spec.cwd)
        .envs(&spec.env)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());

    let mut guard = setup_tree_kill(&mut cmd);

    let started = Instant::now();
    let mut child = match cmd.spawn() {
        Ok(c) => c,
        Err(e) => {
            // Log the failure instead of raising: a missing tool is a failed step.
            writeln!(
                log.lock().expect("log mutex"),
                "failed to start {}: {e}",
                spec.program
            )
            .ok();
            guard.disarm();
            return Ok(ExecResult {
                outcome: Outcome::Spawn,
            });
        }
    };
    guard.attach(&mut child);

    let out = child.stdout.take().expect("piped stdout");
    let err = child.stderr.take().expect("piped stderr");
    let pump_out = spawn_pump(out, log.clone(), sink.clone());
    let pump_err = spawn_pump(err, log.clone(), None);

    let mut timed_out = false;
    loop {
        match child.try_wait()? {
            Some(_) => break,
            None if started.elapsed() >= spec.timeout => {
                timed_out = true;
                guard.kill(&mut child);
                let _ = child.wait();
                break;
            }
            None => std::thread::sleep(Duration::from_millis(50)),
        }
    }

    // Let the pumps drain what the child already wrote.
    let drain_deadline = Instant::now() + Duration::from_secs(5);
    while Instant::now() < drain_deadline {
        if pump_out.is_finished() && pump_err.is_finished() {
            break;
        }
        std::thread::sleep(Duration::from_millis(20));
    }

    let status = child.wait().ok();
    let outcome = if timed_out {
        let mut f = log.lock().expect("log mutex");
        let _ = writeln!(
            f,
            "\n[lunacy] timed out after {}s; killed the process tree",
            spec.timeout.as_secs()
        );
        Outcome::Timeout
    } else {
        match status.map(|s| s.code()) {
            Some(Some(0)) => Outcome::Success,
            Some(Some(c)) => Outcome::Failed(c),
            _ => Outcome::Failed(-1),
        }
    };

    Ok(ExecResult { outcome })
}

fn spawn_pump<R: std::io::Read + Send + 'static>(
    reader: R,
    log: Arc<Mutex<std::fs::File>>,
    sink: Option<LineSink>,
) -> std::thread::JoinHandle<()> {
    std::thread::spawn(move || {
        let mut buf = BufReader::new(reader);
        let mut line = Vec::new();
        loop {
            line.clear();
            match buf.read_until(b'\n', &mut line) {
                Ok(0) => break,
                Ok(_) => {
                    let text = String::from_utf8_lossy(&line);
                    let text = text.trim_end_matches(['\n', '\r']);
                    if let Ok(mut f) = log.lock() {
                        let _ = writeln!(f, "{text}");
                    }
                    if let Some(sink) = &sink {
                        if !text.is_empty() {
                            sink(text);
                        }
                    }
                }
                Err(_) => break,
            }
        }
    })
}

// ---------------------------------------------------------------- unix

#[cfg(unix)]
struct TreeGuard;

#[cfg(unix)]
fn setup_tree_kill(cmd: &mut Command) -> TreeGuard {
    use std::os::unix::process::CommandExt;
    // A new process group keeps our kills away from the daemon itself and lets
    // us reap grandchildren (shell scripts, docker, act, ...) in one call.
    cmd.process_group(0);
    TreeGuard
}

#[cfg(unix)]
impl TreeGuard {
    fn attach(&mut self, _child: &mut std::process::Child) {}

    fn disarm(&mut self) {}

    fn kill(&mut self, child: &mut std::process::Child) {
        let pid = child.id() as libc::pid_t;
        // SIGTERM first so scripts can clean up, then SIGKILL the whole group.
        unsafe {
            libc::killpg(pid, libc::SIGTERM);
        }
        let deadline = Instant::now() + Duration::from_secs(5);
        while Instant::now() < deadline {
            if let Ok(Some(_)) = child.try_wait() {
                break;
            }
            std::thread::sleep(Duration::from_millis(50));
        }
        unsafe {
            libc::killpg(pid, libc::SIGKILL);
        }
    }
}

// ------------------------------------------------------------- windows

#[cfg(windows)]
struct TreeGuard {
    job: Option<windows_sys::Win32::Foundation::HANDLE>,
}

#[cfg(windows)]
fn setup_tree_kill(_cmd: &mut Command) -> TreeGuard {
    use windows_sys::Win32::System::JobObjects::{
        CreateJobObjectW, JobObjectExtendedLimitInformation, SetInformationJobObject,
        JOBOBJECT_EXTENDED_LIMIT_INFORMATION, JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE,
    };
    unsafe {
        let job = CreateJobObjectW(std::ptr::null(), std::ptr::null());
        if job.is_null() {
            return TreeGuard { job: None };
        }
        let mut info: JOBOBJECT_EXTENDED_LIMIT_INFORMATION = std::mem::zeroed();
        info.BasicLimitInformation.LimitFlags = JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE;
        SetInformationJobObject(
            job,
            JobObjectExtendedLimitInformation,
            &info as *const _ as *const std::ffi::c_void,
            std::mem::size_of::<JOBOBJECT_EXTENDED_LIMIT_INFORMATION>() as u32,
        );
        TreeGuard { job: Some(job) }
    }
}

#[cfg(windows)]
impl TreeGuard {
    /// Keep the job open so it is not killed on drop when spawn failed.
    fn disarm(&mut self) {
        self.job = None;
    }

    fn attach(&mut self, child: &mut std::process::Child) {
        use std::os::windows::io::AsRawHandle;
        use windows_sys::Win32::System::JobObjects::AssignProcessToJobObject;
        if let Some(job) = self.job {
            unsafe {
                AssignProcessToJobObject(job, child.as_raw_handle() as _);
            }
        }
    }

    fn kill(&mut self, child: &mut std::process::Child) {
        use windows_sys::Win32::System::JobObjects::TerminateJobObject;
        if let Some(job) = self.job {
            unsafe {
                TerminateJobObject(job, 1);
            }
        }
        // `taskkill` catches anything that escaped the job (rare, but cheap).
        let _ = Command::new("taskkill")
            .args(["/T", "/F", "/PID", &child.id().to_string()])
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status();
        let _ = child.kill();
    }
}

#[cfg(windows)]
impl Drop for TreeGuard {
    fn drop(&mut self) {
        use windows_sys::Win32::Foundation::CloseHandle;
        if let Some(job) = self.job.take() {
            unsafe {
                CloseHandle(job);
            }
        }
    }
}

/// Resolve `program` on PATH (for `/api/system` diagnostics).
pub fn which(program: &str) -> Option<PathBuf> {
    let path = std::env::var_os("PATH")?;
    let exts: Vec<String> = if cfg!(windows) {
        std::env::var("PATHEXT")
            .unwrap_or_else(|_| ".EXE;.CMD;.BAT".into())
            .split(';')
            .map(|s| s.to_string())
            .collect()
    } else {
        vec![String::new()]
    };
    for dir in std::env::split_paths(&path) {
        for ext in &exts {
            let candidate = dir.join(format!("{program}{ext}"));
            if candidate.is_file() {
                return Some(candidate);
            }
        }
    }
    None
}

pub fn first_line_of_stdout(program: &str, args: &[&str]) -> Option<String> {
    let out = Command::new(program)
        .args(args)
        .stdin(Stdio::null())
        .output()
        .ok()?;
    if !out.status.success() {
        return None;
    }
    let text = String::from_utf8_lossy(&out.stdout);
    let first = text.lines().next()?.trim().to_string();
    if first.is_empty() {
        None
    } else {
        Some(first)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::Path;

    fn spec(dir: &Path, program: &str, args: &[&str], timeout: Duration) -> ExecSpec {
        ExecSpec {
            program: program.to_string(),
            args: args.iter().map(|s| s.to_string()).collect(),
            cwd: dir.to_path_buf(),
            env: BTreeMap::new(),
            timeout,
            log_path: dir.join("log"),
        }
    }

    #[test]
    fn success_and_failure_exit_codes() {
        let dir = tempfile::tempdir().unwrap();
        let ok = exec(
            &spec(dir.path(), "true", &[], Duration::from_secs(10)),
            None,
        )
        .unwrap();
        assert_eq!(ok.outcome, Outcome::Success);
        let bad = exec(
            &spec(dir.path(), "false", &[], Duration::from_secs(10)),
            None,
        )
        .unwrap();
        assert_eq!(bad.outcome, Outcome::Failed(1));
    }

    #[test]
    fn output_is_captured_and_streamed() {
        let dir = tempfile::tempdir().unwrap();
        let seen = Arc::new(Mutex::new(Vec::<String>::new()));
        let sink_seen = seen.clone();
        let sink: LineSink = Arc::new(move |line: &str| {
            sink_seen.lock().unwrap().push(line.to_string());
        });
        let out = exec(
            &spec(
                dir.path(),
                "sh",
                &["-c", "echo one; echo two"],
                Duration::from_secs(10),
            ),
            Some(sink),
        )
        .unwrap();
        assert_eq!(out.outcome, Outcome::Success);
        let log = std::fs::read_to_string(dir.path().join("log")).unwrap();
        assert!(log.contains("one") && log.contains("two"), "{log}");
        assert_eq!(seen.lock().unwrap().len(), 2);
    }

    #[cfg(unix)]
    #[test]
    fn timeout_kills_the_whole_group() {
        let dir = tempfile::tempdir().unwrap();
        let marker = dir.path().join("survived");
        let script = format!(
            "( sleep 8; echo alive > {} ) & echo started; sleep 30",
            marker.display()
        );
        let started = Instant::now();
        let out = exec(
            &spec(
                dir.path(),
                "sh",
                &["-c", &script],
                Duration::from_millis(800),
            ),
            None,
        )
        .unwrap();
        assert_eq!(out.outcome, Outcome::Timeout);
        assert!(
            started.elapsed() < Duration::from_secs(20),
            "kill took too long"
        );
        // The background grandchild died with the group.
        std::thread::sleep(Duration::from_millis(300));
        assert!(!marker.exists(), "grandchild survived the group kill");
    }

    #[cfg(unix)]
    #[test]
    fn missing_program_is_a_spawn_failure() {
        let dir = tempfile::tempdir().unwrap();
        let out = exec(
            &spec(
                dir.path(),
                "definitely-not-a-real-binary-xyz",
                &[],
                Duration::from_secs(5),
            ),
            None,
        )
        .unwrap();
        assert_eq!(out.outcome, Outcome::Spawn);
        let log = std::fs::read_to_string(dir.path().join("log")).unwrap();
        assert!(log.contains("failed to start"), "{log}");
    }
}
