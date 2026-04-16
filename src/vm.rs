use crate::config::Config;
use crate::process;
use nix::unistd::Pid;
use std::fs;
use std::process::Command;

#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize)]
#[serde(rename_all = "snake_case")]
pub enum VmState {
    Idle,
    Starting,
    Booting,
    Ready,
    Stopping,
    Error,
}

pub struct VmManager<'a> {
    config: &'a Config,
    state: VmState,
    child_pid: Option<Pid>,
    pub crash_count: u32,
    ready_since: Option<std::time::Instant>,
}

impl<'a> VmManager<'a> {
    pub fn new(config: &'a Config) -> Self {
        Self {
            config,
            state: VmState::Idle,
            child_pid: None,
            crash_count: 0,
            ready_since: None,
        }
    }

    pub fn state(&self) -> VmState {
        self.state
    }

    pub fn set_state(&mut self, s: VmState) {
        self.state = s;
        if s == VmState::Ready {
            self.ready_since = Some(std::time::Instant::now());
            // Reset crash count after stable operation (60s)
        }
    }

    pub fn owns_pid(&self, pid: Pid) -> bool {
        self.child_pid == Some(pid)
    }

    pub fn is_dead(&self) -> bool {
        matches!(self.state, VmState::Idle | VmState::Error)
            && self.child_pid.is_some()
            && !self.child_pid.map_or(false, process::is_alive)
    }

    pub fn mark_dead(&mut self) {
        self.crash_count += 1;
        self.child_pid = None;
        self.state = VmState::Idle;
        self.ready_since = None;
    }

    /// Reset crash count if VM has been stable for 60s.
    pub fn maybe_reset_crash_count(&mut self) {
        if let Some(since) = self.ready_since {
            if since.elapsed() >= std::time::Duration::from_secs(60) {
                self.crash_count = 0;
            }
        }
    }

    pub fn validate_images(&self) -> Result<(), String> {
        let dir = &self.config.image_dir;
        let required = ["pvm-manage", "crosvm", "custom_pvmfw", "kernel.bin", "disk.img"];
        for name in &required {
            let path = dir.join(name);
            if !path.exists() {
                return Err(format!("{name} not found in {}", dir.display()));
            }
        }
        for name in &["kernel.bin", "disk.img"] {
            let meta = fs::metadata(dir.join(name))
                .map_err(|e| format!("stat {name}: {e}"))?;
            if meta.len() < 1_000_000 {
                return Err(format!("{name} too small ({}B)", meta.len()));
            }
        }
        Ok(())
    }

    pub fn start(&mut self) -> Result<(), String> {
        self.validate_images()?;
        self.state = VmState::Starting;

        let log_file = self.config.log_dir.join("pvm.log");
        let cpus = self.config.vm_cpus.to_string();
        let mem = format!("size={}", self.config.vm_mem_mb);
        let cid = self.config.vsock_cid.to_string();
        let home = self.config.image_dir.to_string_lossy().to_string();

        let child = process::spawn_with_log(
            &self.config.image_dir.join("pvm-manage").to_string_lossy(),
            &[
                "run", "--protected-vm-with-pvmfw", "--",
                "--cpus", &cpus,
                "--mem", &mem,
                "--block", "disk.img",
                "--cid", &cid,
                "kernel.bin",
            ],
            &[("HOME", &home)],
            &log_file,
            Some(&self.config.image_dir),
        ).map_err(|e| format!("spawn pvm-manage: {e}"))?;

        self.child_pid = Some(Pid::from_raw(child.id() as i32));
        self.state = VmState::Booting;
        log::info!("VM started (pid={})", child.id());
        Ok(())
    }

    pub fn stop(&mut self) -> Result<(), String> {
        self.state = VmState::Stopping;

        if let Some(pid) = self.child_pid.take() {
            log::info!("stopping VM (pid={pid})");
            process::graceful_kill(pid, self.config.shutdown_grace_secs);
            // Also kill crosvm children
            process::kill_tree(pid, nix::sys::signal::Signal::SIGKILL);
            // pkill crosvm as fallback
            let _ = Command::new("pkill").args(["-9", "-f", "crosvm"]).status();
        }

        self.state = VmState::Idle;
        self.ready_since = None;
        Ok(())
    }

    /// Probe vsock readiness using CA's vsock-test subcommand.
    pub fn probe_vsock(&self) -> bool {
        let output = Command::new(self.config.ca_binary.as_os_str())
            .args(["vsock-test", &self.config.vsock_cid.to_string(), "9999"])
            .env("VSOCK_CID", self.config.vsock_cid.to_string())
            .output();
        matches!(output, Ok(o) if o.status.success())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    fn test_config(dir: &std::path::Path) -> Config {
        Config {
            image_dir: dir.to_path_buf(),
            ca_binary: dir.join("secret_proxy_ca"),
            log_dir: dir.join("logs"),
            ..Config::default()
        }
    }

    #[test]
    fn validate_images_missing_dir() {
        let config = test_config(std::path::Path::new("/nonexistent"));
        let vm = VmManager::new(&config);
        assert!(vm.validate_images().is_err());
    }

    #[test]
    fn validate_images_missing_file() {
        let dir = std::env::temp_dir().join(format!("teeproxyd_vm_test_{}", std::process::id()));
        fs::create_dir_all(&dir).unwrap();
        // Create all except disk.img
        for name in &["pvm-manage", "crosvm", "custom_pvmfw", "kernel.bin"] {
            let path = dir.join(name);
            let mut f = fs::File::create(&path).unwrap();
            f.write_all(&vec![0u8; 2_000_000]).unwrap(); // > 1MB
        }
        let config = test_config(&dir);
        let vm = VmManager::new(&config);
        let err = vm.validate_images().unwrap_err();
        assert!(err.contains("disk.img"), "expected disk.img missing, got: {err}");
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn validate_images_too_small() {
        let dir = std::env::temp_dir().join(format!("teeproxyd_vm_small_{}", std::process::id()));
        fs::create_dir_all(&dir).unwrap();
        for name in &["pvm-manage", "crosvm", "custom_pvmfw"] {
            fs::File::create(dir.join(name)).unwrap();
        }
        // kernel.bin too small
        fs::write(dir.join("kernel.bin"), b"tiny").unwrap();
        let mut f = fs::File::create(dir.join("disk.img")).unwrap();
        f.write_all(&vec![0u8; 2_000_000]).unwrap();

        let config = test_config(&dir);
        let vm = VmManager::new(&config);
        let err = vm.validate_images().unwrap_err();
        assert!(err.contains("kernel.bin") && err.contains("too small"), "got: {err}");
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn validate_images_all_present() {
        let dir = std::env::temp_dir().join(format!("teeproxyd_vm_ok_{}", std::process::id()));
        fs::create_dir_all(&dir).unwrap();
        for name in &["pvm-manage", "crosvm", "custom_pvmfw"] {
            fs::File::create(dir.join(name)).unwrap();
        }
        for name in &["kernel.bin", "disk.img"] {
            let mut f = fs::File::create(dir.join(name)).unwrap();
            f.write_all(&vec![0u8; 2_000_000]).unwrap();
        }
        let config = test_config(&dir);
        let vm = VmManager::new(&config);
        assert!(vm.validate_images().is_ok());
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn state_transitions() {
        let dir = std::env::temp_dir().join("teeproxyd_state_test");
        let config = test_config(&dir);
        let mut vm = VmManager::new(&config);
        assert_eq!(vm.state(), VmState::Idle);
        vm.set_state(VmState::Ready);
        assert_eq!(vm.state(), VmState::Ready);
        assert_eq!(vm.crash_count, 0);
        vm.mark_dead();
        assert_eq!(vm.state(), VmState::Idle);
        assert_eq!(vm.crash_count, 1);
    }
}
