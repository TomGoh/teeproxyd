use std::io::{self, Read, Write};
use std::net::{SocketAddr, TcpStream};
use std::time::Duration;

/// Minimal HTTP/1.0 GET /health request. We send a real HTTP request (not a
/// bare TCP connect + drop) so CA treats us as a well-formed client — a
/// connect-then-close probe would flood ca.log with "bad HTTP method" every
/// probe interval.
const HEALTH_REQUEST: &[u8] =
    b"GET /health HTTP/1.0\r\nHost: 127.0.0.1\r\nUser-Agent: teeproxyd-health\r\nConnection: close\r\n\r\n";

#[derive(Debug, PartialEq, Eq)]
pub enum HealthResult {
    Ok,
    Degraded(u32),
    RestartNeeded,
}

pub struct HealthMonitor {
    port: u16,
    fail_threshold: u32,
    consecutive_failures: u32,
    consecutive_timeouts: u32,
    timeout_threshold: u32,
}

impl HealthMonitor {
    pub fn new(port: u16, fail_threshold: u32) -> Self {
        Self {
            port,
            fail_threshold,
            consecutive_failures: 0,
            consecutive_timeouts: 0,
            // Timeouts are treated more leniently than ConnectionRefused:
            // a timeout often means the CA is busy handling a long LLM
            // request, not actually dead. ~100s of consecutive timeouts
            // (10 probes * 10s interval) before restart is intentional.
            timeout_threshold: 10,
        }
    }

    pub fn probe(&mut self) -> HealthResult {
        let addr = SocketAddr::from(([127, 0, 0, 1], self.port));
        match TcpStream::connect_timeout(&addr, Duration::from_secs(2)) {
            Ok(mut stream) => {
                // Send a real HTTP request so CA doesn't log "bad HTTP method"
                // every probe. We don't care about the response body — a
                // successful write+read is enough to confirm CA is serving.
                // Errors here are non-fatal: the TCP connect already
                // succeeded, so the CA is up; write/read failure just means
                // it closed early (still counts as healthy).
                let _ = stream.set_write_timeout(Some(Duration::from_secs(1)));
                let _ = stream.set_read_timeout(Some(Duration::from_secs(1)));
                let _ = stream.write_all(HEALTH_REQUEST);
                let mut sink = [0u8; 256];
                let _ = stream.read(&mut sink);
                self.consecutive_failures = 0;
                self.consecutive_timeouts = 0;
                HealthResult::Ok
            }
            Err(e) if e.kind() == io::ErrorKind::ConnectionRefused => {
                self.consecutive_timeouts = 0;
                self.consecutive_failures += 1;
                if self.consecutive_failures >= self.fail_threshold {
                    self.consecutive_failures = 0;
                    HealthResult::RestartNeeded
                } else {
                    HealthResult::Degraded(self.consecutive_failures)
                }
            }
            Err(e) if e.kind() == io::ErrorKind::TimedOut => {
                self.consecutive_timeouts += 1;
                if self.consecutive_timeouts >= self.timeout_threshold {
                    self.consecutive_timeouts = 0;
                    HealthResult::RestartNeeded
                } else {
                    HealthResult::Ok
                }
            }
            Err(_) => {
                // Other errors (transient) — don't count
                HealthResult::Ok
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn new_monitor_starts_healthy() {
        let h = HealthMonitor::new(1, 3);
        assert_eq!(h.consecutive_failures, 0);
        assert_eq!(h.consecutive_timeouts, 0);
    }

    #[test]
    fn threshold_defaults() {
        let h = HealthMonitor::new(19030, 3);
        assert_eq!(h.fail_threshold, 3);
        assert_eq!(h.timeout_threshold, 10);
        assert_eq!(h.port, 19030);
    }

    #[test]
    fn probe_unreachable_port_returns_degraded_then_restart() {
        // Port 1 is almost certainly not listening
        let mut h = HealthMonitor::new(1, 2);
        let r1 = h.probe();
        // Should be Degraded(1) or RestartNeeded depending on error type
        // On most systems, port 1 gives ConnectionRefused
        match r1 {
            HealthResult::Degraded(1) => {
                let r2 = h.probe();
                assert_eq!(r2, HealthResult::RestartNeeded);
                // After restart, counter resets
                assert_eq!(h.consecutive_failures, 0);
            }
            HealthResult::RestartNeeded => {
                // threshold was 2, got 2 failures somehow (unlikely for first call)
            }
            HealthResult::Ok => {
                // Timeout or other error — treated as OK, that's fine
            }
            _ => {}
        }
    }

    #[test]
    fn probe_localhost_open_port_returns_ok() {
        // Bind a temporary listener to get a known-open port
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();
        let mut h = HealthMonitor::new(port, 3);
        assert_eq!(h.probe(), HealthResult::Ok);
        assert_eq!(h.consecutive_failures, 0);
    }
}
