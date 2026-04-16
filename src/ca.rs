use crate::config::Config;
use crate::process;
use nix::unistd::Pid;
use std::net::{SocketAddr, TcpStream};
use std::time::Duration;

#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize)]
#[serde(rename_all = "snake_case")]
pub enum CaState {
    Idle,
    Starting,
    Running,
    Ready,
    Stopping,
    Error,
}

pub struct CaManager<'a> {
    config: &'a Config,
    state: CaState,
    child_pid: Option<Pid>,
    pub crash_count: u32,
    ready_since: Option<std::time::Instant>,
}

impl<'a> CaManager<'a> {
    pub fn new(config: &'a Config) -> Self {
        Self {
            config,
            state: CaState::Idle,
            child_pid: None,
            crash_count: 0,
            ready_since: None,
        }
    }

    pub fn state(&self) -> CaState {
        self.state
    }

    pub fn set_state(&mut self, s: CaState) {
        self.state = s;
        if s == CaState::Ready {
            self.ready_since = Some(std::time::Instant::now());
        }
    }

    pub fn owns_pid(&self, pid: Pid) -> bool {
        self.child_pid == Some(pid)
    }

    pub fn is_dead(&self) -> bool {
        matches!(self.state, CaState::Idle | CaState::Error)
            && self.child_pid.is_some()
            && !self.child_pid.map_or(false, process::is_alive)
    }

    pub fn mark_dead(&mut self) {
        self.crash_count += 1;
        self.child_pid = None;
        self.state = CaState::Idle;
        self.ready_since = None;
    }

    pub fn maybe_reset_crash_count(&mut self) {
        if let Some(since) = self.ready_since {
            if since.elapsed() >= Duration::from_secs(60) {
                self.crash_count = 0;
            }
        }
    }

    pub fn start(&mut self, vsock_cid: u32) -> Result<(), String> {
        if !self.config.ca_binary.exists() {
            return Err(format!("CA binary not found: {}", self.config.ca_binary.display()));
        }
        self.state = CaState::Starting;

        let log_file = self.config.log_dir.join("ca.log");
        let port_str = self.config.ca_port.to_string();
        let cid_str = vsock_cid.to_string();

        let child = process::spawn_with_log(
            &self.config.ca_binary.to_string_lossy(),
            &["serve", "--port", &port_str],
            &[
                ("VSOCK_CID", &cid_str),
                ("RUST_LOG", &self.config.ca_log_level),
                ("SECRET_PROXY_CA_ADMIN_TOKEN", &self.config.ca_admin_token),
            ],
            &log_file,
            None,
        ).map_err(|e| format!("spawn CA: {e}"))?;

        self.child_pid = Some(Pid::from_raw(child.id() as i32));
        self.state = CaState::Running;
        log::info!("CA started (pid={})", child.id());
        Ok(())
    }

    pub fn stop(&mut self) -> Result<(), String> {
        self.state = CaState::Stopping;

        if let Some(pid) = self.child_pid.take() {
            log::info!("stopping CA (pid={pid})");
            process::graceful_kill(pid, self.config.shutdown_grace_secs);
        }

        self.state = CaState::Idle;
        self.ready_since = None;
        Ok(())
    }

    /// Check if CA port is accepting TCP connections.
    pub fn is_port_ready(&self) -> bool {
        TcpStream::connect_timeout(
            &SocketAddr::from(([127, 0, 0, 1], self.config.ca_port)),
            Duration::from_secs(1),
        ).is_ok()
    }
}
