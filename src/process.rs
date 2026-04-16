use nix::sys::signal::{self, Signal};
use nix::unistd::Pid;
use std::fs::{self, OpenOptions};
use std::io;
use std::path::Path;
use std::process::{Child, Command, Stdio};
use std::thread;
use std::time::Duration;

/// Spawn a child process with stdout/stderr redirected to a log file.
pub fn spawn_with_log(
    cmd: &str,
    args: &[&str],
    env: &[(&str, &str)],
    log_file: &Path,
    cwd: Option<&Path>,
) -> io::Result<Child> {
    rotate_if_needed(log_file, 10 * 1024 * 1024); // 10MB

    let file = OpenOptions::new()
        .create(true)
        .append(true)
        .open(log_file)?;
    let stderr_file = file.try_clone()?;

    let mut command = Command::new(cmd);
    command
        .args(args)
        .stdout(Stdio::from(file))
        .stderr(Stdio::from(stderr_file));

    for (k, v) in env {
        command.env(k, v);
    }
    if let Some(dir) = cwd {
        command.current_dir(dir);
    }

    command.spawn()
}

/// Graceful kill: SIGTERM -> wait grace_secs -> SIGKILL.
/// Returns true if the process exited within the grace period.
pub fn graceful_kill(pid: Pid, grace_secs: u64) -> bool {
    if !is_alive(pid) {
        return true;
    }
    let _ = signal::kill(pid, Signal::SIGTERM);

    let deadline = std::time::Instant::now() + Duration::from_secs(grace_secs);
    while std::time::Instant::now() < deadline {
        if !is_alive(pid) {
            return true;
        }
        thread::sleep(Duration::from_millis(200));
    }

    // Still alive — force kill
    let _ = signal::kill(pid, Signal::SIGKILL);
    thread::sleep(Duration::from_millis(100));
    !is_alive(pid)
}

/// Check if a process is alive.
pub fn is_alive(pid: Pid) -> bool {
    signal::kill(pid, None).is_ok()
}

/// Kill process tree: kill the process and attempt to pkill children by parent pid.
pub fn kill_tree(pid: Pid, sig: Signal) {
    let _ = signal::kill(pid, sig);
    // Also kill children (best-effort via /proc traversal or pkill -P)
    let _ = Command::new("pkill")
        .args(["-P", &pid.as_raw().to_string()])
        .arg(format!("-{}", sig as i32))
        .status();
}

/// Rotate log file if it exceeds max_bytes. Renames to .log.1.
pub fn rotate_if_needed(log_path: &Path, max_bytes: u64) {
    if let Ok(meta) = fs::metadata(log_path) {
        if meta.len() > max_bytes {
            let rotated = log_path.with_extension("log.1");
            let _ = fs::rename(log_path, &rotated);
            log::info!("rotated log: {} -> {}", log_path.display(), rotated.display());
        }
    }
}
