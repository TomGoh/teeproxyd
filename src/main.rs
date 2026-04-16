mod ca;
mod config;
mod health;
mod ipc;
mod log_collector;
mod process;
mod startup;
mod vm;

use ca::CaManager;
use config::Config;
use health::{HealthMonitor, HealthResult};
use nix::poll::{PollFd, PollFlags, PollTimeout};
use nix::sys::signal::{sigaction, SaFlags, SigAction, SigHandler, SigSet, Signal};
use nix::sys::wait::{waitpid, WaitPidFlag, WaitStatus};
use nix::unistd::Pid;
use startup::StartupPhase;
use std::fs;
use std::os::fd::{AsFd, AsRawFd, FromRawFd, RawFd};
use std::os::unix::net::UnixListener;
use std::sync::atomic::{AtomicI32, Ordering};
use std::time::{Duration, Instant};
use vm::VmManager;

use log::{error, info, warn};

/// Self-pipe for signal notification. Signal handler writes 1 byte here
/// to wake the poll() loop.
static SIG_PIPE_FD: AtomicI32 = AtomicI32::new(-1);

extern "C" fn signal_handler(sig: libc::c_int) {
    let fd = SIG_PIPE_FD.load(Ordering::Relaxed);
    if fd >= 0 {
        // Write different bytes so the main loop can distinguish signals:
        // 'T' = SIGTERM/SIGINT (shutdown), 'C' = SIGCHLD (child died)
        let byte: u8 = if sig == libc::SIGCHLD { b'C' } else { b'T' };
        unsafe {
            libc::write(fd, &byte as *const u8 as *const _, 1);
        }
    }
}

fn setup_signal_handler(write_fd: RawFd) {
    SIG_PIPE_FD.store(write_fd, Ordering::Relaxed);
    let action = SigAction::new(
        SigHandler::Handler(signal_handler),
        SaFlags::SA_RESTART,
        SigSet::empty(),
    );
    unsafe {
        sigaction(Signal::SIGTERM, &action).expect("sigaction SIGTERM");
        sigaction(Signal::SIGINT, &action).expect("sigaction SIGINT");
        sigaction(Signal::SIGCHLD, &action).expect("sigaction SIGCHLD");
    }
}

/// Acquire the IPC listener. Prefer Android init socket fd inheritance
/// via ANDROID_SOCKET_teeproxyd env var; fall back to creating one.
fn acquire_listener(config: &Config) -> UnixListener {
    if let Ok(fd_str) = std::env::var("ANDROID_SOCKET_teeproxyd") {
        if let Ok(fd) = fd_str.parse::<RawFd>() {
            info!("using init-inherited socket fd={fd}");
            return unsafe { UnixListener::from_raw_fd(fd) };
        }
    }
    // Fallback: manual invocation / debugging
    let path = config.data_dir.join("teeproxyd.sock");
    let _ = fs::remove_file(&path);
    info!("creating socket at {}", path.display());
    UnixListener::bind(&path).expect("bind socket")
}

fn main() {
    env_logger::Builder::from_env(env_logger::Env::default().default_filter_or("info")).init();
    info!("teeproxyd v{} starting", env!("CARGO_PKG_VERSION"));

    let config = Config::load_with_warnings();
    info!("image_dir={} ca_binary={}", config.image_dir.display(), config.ca_binary.display());

    // Create directories
    let _ = fs::create_dir_all(&config.log_dir);
    let _ = fs::create_dir_all(&config.image_dir);

    // Self-pipe for signal notification
    let (sig_read, sig_write) = nix::unistd::pipe().expect("pipe");
    setup_signal_handler(sig_write.as_raw_fd());

    // Managers
    let mut vm = VmManager::new(&config);
    let mut ca = CaManager::new(&config);
    let mut health = HealthMonitor::new(config.ca_port, config.health_fail_threshold);
    let mut startup_phase = if config.auto_start {
        StartupPhase::VmStarting
    } else {
        StartupPhase::NotStarted
    };

    // IPC listener
    let listener = acquire_listener(&config);
    let started_at = Instant::now();
    let health_interval = Duration::from_secs(config.health_interval_secs);
    let mut last_health = Instant::now();

    info!("entering main loop (auto_start={})", config.auto_start);

    loop {
        // Compute poll timeout
        let until_health = health_interval.saturating_sub(last_health.elapsed());
        let poll_timeout_ms = match &startup_phase {
            StartupPhase::WaitingVsock { delay_done: false, .. } => 1000, // 1s during boot delay
            StartupPhase::WaitingVsock { .. } => 2000,   // 2s between vsock probes
            StartupPhase::WaitingCaPort { .. } => 500,    // 500ms between port probes
            _ => until_health.as_millis() as i32,
        };
        let poll_timeout = poll_timeout_ms.max(100).min(10_000); // clamp to 100ms..10s

        let mut poll_fds = [
            PollFd::new(listener.as_fd(), PollFlags::POLLIN),
            PollFd::new(sig_read.as_fd(), PollFlags::POLLIN),
        ];

        let _ = nix::poll::poll(&mut poll_fds, PollTimeout::from(poll_timeout as u16));

        // Check signal pipe
        if poll_fds[1]
            .revents()
            .map_or(false, |r| r.contains(PollFlags::POLLIN))
        {
            // Drain pipe and check signal type
            let mut buf = [0u8; 64];
            let n = nix::unistd::read(sig_read.as_raw_fd(), &mut buf).unwrap_or(0);

            let got_term = buf[..n].contains(&b'T');
            let got_chld = buf[..n].contains(&b'C');

            if got_chld {
                reap_children(&mut vm, &mut ca, &config);
            }
            if got_term {
                info!("received shutdown signal");
                break;
            }
        }

        // Accept IPC connection
        if poll_fds[0]
            .revents()
            .map_or(false, |r| r.contains(PollFlags::POLLIN))
        {
            if let Ok((stream, _)) = listener.accept() {
                let _ = stream.set_read_timeout(Some(Duration::from_secs(5)));
                ipc::handle_client(stream, &mut vm, &mut ca, &startup_phase, &config, started_at);
            }
        }

        // Advance startup state machine
        startup::advance_startup(&mut startup_phase, &mut vm, &mut ca, &config);

        // Health check
        if last_health.elapsed() >= health_interval {
            if matches!(ca.state(), ca::CaState::Ready | ca::CaState::Running) {
                match health.probe() {
                    HealthResult::RestartNeeded => {
                        warn!("health check: CA restart needed");
                        let _ = ca.stop();
                        let _ = ca.start(config.vsock_cid);
                    }
                    HealthResult::Degraded(n) => {
                        warn!("health check: CA degraded ({n}/{} failures)", config.health_fail_threshold);
                    }
                    HealthResult::Ok => {}
                }
            }
            last_health = Instant::now();
        }

        // Reset crash counts after stable operation
        vm.maybe_reset_crash_count();
        ca.maybe_reset_crash_count();
    }

    // Graceful shutdown
    info!("shutting down...");
    let _ = ca.stop();
    let _ = vm.stop();
    info!("teeproxyd exited cleanly");
}

/// Reap dead children via waitpid(WNOHANG). Returns true if any child was reaped.
fn reap_children(vm: &mut VmManager, ca: &mut CaManager, config: &Config) -> bool {
    let mut reaped_any = false;
    loop {
        match waitpid(None, Some(WaitPidFlag::WNOHANG)) {
            Ok(WaitStatus::Exited(pid, code)) => {
                reaped_any = true;
                handle_child_death(pid, &format!("exit code {code}"), vm, ca, config);
            }
            Ok(WaitStatus::Signaled(pid, sig, _)) => {
                reaped_any = true;
                handle_child_death(pid, &format!("signal {sig}"), vm, ca, config);
            }
            Ok(WaitStatus::StillAlive) | Err(_) => break,
            _ => break,
        }
    }
    reaped_any
}

fn handle_child_death(
    pid: Pid,
    reason: &str,
    vm: &mut VmManager,
    ca: &mut CaManager,
    config: &Config,
) {
    if vm.owns_pid(pid) {
        warn!("VM process (pid={pid}) died ({reason})");
        vm.mark_dead();
        if vm.crash_count >= 5 {
            error!("VM crashed 5 times, entering ERROR state");
            vm.set_state(vm::VmState::Error);
        }
        // Startup state machine will handle restart on next advance_startup()
    } else if ca.owns_pid(pid) {
        warn!("CA process (pid={pid}) died ({reason})");
        ca.mark_dead();
        if ca.crash_count < 5 {
            info!("restarting CA immediately (crash #{}/5)", ca.crash_count);
            let _ = ca.start(config.vsock_cid);
        } else {
            error!("CA crashed 5 times, entering ERROR state");
            ca.set_state(ca::CaState::Error);
        }
    } else {
        // Unknown child — ignore (could be pkill or a forked grandchild)
    }
}
