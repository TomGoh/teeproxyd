use crate::ca::CaManager;
use crate::config::Config;
use crate::vm::{VmManager, VmState};
use std::time::{Duration, Instant};

#[derive(Debug)]
pub enum StartupPhase {
    NotStarted,
    VmStarting,
    WaitingVsock {
        deadline: Instant,
        boot_start: Instant,
        delay_done: bool,
    },
    CaStarting,
    WaitingCaPort {
        deadline: Instant,
    },
    Complete,
    Failed(String),
}

/// Advance the startup state machine by one step (non-blocking).
/// Called on every main loop iteration.
pub fn advance_startup(
    startup: &mut StartupPhase,
    vm: &mut VmManager,
    ca: &mut CaManager,
    config: &Config,
) {
    // Synchronize with reap_children: if VM died mid-startup, reset
    if vm.is_dead() && !matches!(startup, StartupPhase::NotStarted | StartupPhase::Failed(_) | StartupPhase::Complete) {
        log::warn!("VM died during startup phase {:?}, resetting", startup);
        if vm.crash_count < 5 {
            *startup = StartupPhase::VmStarting;
        } else {
            *startup = StartupPhase::Failed("VM crashed too many times".into());
        }
        return;
    }
    if ca.is_dead() && matches!(startup, StartupPhase::WaitingCaPort { .. }) {
        log::warn!("CA died during startup, resetting to CaStarting");
        if ca.crash_count < 5 {
            *startup = StartupPhase::CaStarting;
        } else {
            *startup = StartupPhase::Failed("CA crashed too many times".into());
        }
        return;
    }

    match startup {
        StartupPhase::NotStarted | StartupPhase::Complete | StartupPhase::Failed(_) => {}

        StartupPhase::VmStarting => {
            match vm.start() {
                Ok(()) => {
                    let now = Instant::now();
                    let deadline = now
                        + Duration::from_secs(config.vsock_probe_delay_secs + config.vm_boot_timeout_secs);
                    *startup = StartupPhase::WaitingVsock {
                        deadline,
                        boot_start: now,
                        delay_done: false,
                    };
                    log::info!("VM started, waiting for vsock (delay {}s + timeout {}s)",
                        config.vsock_probe_delay_secs, config.vm_boot_timeout_secs);
                }
                Err(e) => {
                    log::error!("VM start failed: {e}");
                    *startup = StartupPhase::Failed(e);
                }
            }
        }

        StartupPhase::WaitingVsock { deadline, boot_start, delay_done } => {
            if !*delay_done {
                if boot_start.elapsed() >= Duration::from_secs(config.vsock_probe_delay_secs) {
                    *delay_done = true;
                    log::info!("boot delay elapsed, starting vsock probes");
                }
            } else if Instant::now() > *deadline {
                log::error!("vsock probe timed out");
                *startup = StartupPhase::Failed("vsock timeout".into());
            } else if vm.probe_vsock() {
                log::info!("VM ready (vsock responsive)");
                vm.set_state(VmState::Ready);
                *startup = StartupPhase::CaStarting;
            }
            // else: poll timeout will bring us back in ~2s
        }

        StartupPhase::CaStarting => {
            match ca.start(config.vsock_cid) {
                Ok(()) => {
                    let deadline = Instant::now() + Duration::from_secs(config.ca_ready_timeout_secs);
                    *startup = StartupPhase::WaitingCaPort { deadline };
                }
                Err(e) => {
                    log::error!("CA start failed: {e}");
                    *startup = StartupPhase::Failed(e);
                }
            }
        }

        StartupPhase::WaitingCaPort { deadline } => {
            if ca.is_port_ready() {
                log::info!("CA ready (port {} accepting)", config.ca_port);
                ca.set_state(crate::ca::CaState::Ready);
                *startup = StartupPhase::Complete;
            } else if Instant::now() > *deadline {
                log::warn!("CA port not ready within timeout, but process running");
                *startup = StartupPhase::Complete;
            }
        }
    }
}
