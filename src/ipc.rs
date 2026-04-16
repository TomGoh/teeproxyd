use crate::ca::CaManager;
use crate::config::Config;
use crate::log_collector;
use crate::startup::StartupPhase;
use crate::vm::VmManager;
use serde_json::json;
use std::io::{BufRead, BufReader, Read, Write};
use std::os::unix::net::UnixStream;
use std::time::Instant;

/// Per-request line cap. Any commands here are tiny JSON objects;
/// 64 KiB protects the daemon from a client that dumps garbage without a
/// newline and would otherwise grow our buffer without bound.
const MAX_REQUEST_LINE_BYTES: u64 = 64 * 1024;
/// Upper bound on lines the tail_log command will return, to prevent
/// a single request from blowing up RAM on a huge rotated file.
const MAX_TAIL_LINES: usize = 10_000;

/// Handle one IPC client connection. Reads line-delimited JSON commands,
/// dispatches, and writes JSON responses. The stream has a 5s read timeout
/// set by the caller to prevent blocking the main loop.
pub fn handle_client(
    stream: UnixStream,
    vm: &mut VmManager,
    ca: &mut CaManager,
    startup: &StartupPhase,
    config: &Config,
    started_at: Instant,
) {
    let mut reader = BufReader::new(&stream);
    loop {
        // Bound per-line reads so a misbehaving client can't OOM the daemon.
        let mut line = String::new();
        match reader.by_ref().take(MAX_REQUEST_LINE_BYTES).read_line(&mut line) {
            Ok(0) => break, // EOF
            Ok(_) => {}
            Err(_) => break, // timeout or I/O error
        }
        let line = line.trim_end_matches('\n').trim_end_matches('\r');
        if line.is_empty() {
            continue;
        }
        let req: serde_json::Value = match serde_json::from_str(line) {
            Ok(v) => v,
            Err(e) => {
                let _ = write_response(&stream, json!({"ok": false, "error": e.to_string()}));
                continue;
            }
        };
        let cmd = req["cmd"].as_str().unwrap_or("");
        let response = match cmd {
            "ping" => json!({"ok": true, "msg": "pong"}),
            "status" => handle_status(vm, ca, startup, config, started_at),
            "start_all" => handle_start_all(vm, ca, config),
            "stop_all" => handle_stop_all(vm, ca),
            "start_vm" => handle_start_vm(vm),
            "stop_vm" => handle_stop_vm(vm),
            "start_ca" => handle_start_ca(ca, config),
            "stop_ca" => handle_stop_ca(ca),
            "tail_log" => handle_tail_log(&req, config),
            _ => json!({"ok": false, "error": format!("unknown cmd: {cmd}")}),
        };
        if write_response(&stream, response).is_err() {
            break;
        }
    }
}

fn write_response(mut stream: &UnixStream, value: serde_json::Value) -> std::io::Result<()> {
    let mut s = serde_json::to_string(&value).unwrap_or_else(|_| r#"{"ok":false}"#.into());
    s.push('\n');
    stream.write_all(s.as_bytes())?;
    stream.flush()
}

fn handle_status(
    vm: &VmManager,
    ca: &CaManager,
    startup: &StartupPhase,
    config: &Config,
    started_at: Instant,
) -> serde_json::Value {
    json!({
        "ok": true,
        "vm": vm.state(),
        "ca": ca.state(),
        "startup_phase": format!("{startup:?}"),
        "vm_cid": config.vsock_cid,
        "ca_port": config.ca_port,
        "uptime_secs": started_at.elapsed().as_secs(),
    })
}

fn handle_start_all(vm: &mut VmManager, _ca: &mut CaManager, _config: &Config) -> serde_json::Value {
    if let Err(e) = vm.start() {
        return json!({"ok": false, "error": format!("VM: {e}")});
    }
    // CA will be started by the startup state machine after vsock is ready.
    json!({"ok": true, "msg": "starting"})
}

fn handle_stop_all(vm: &mut VmManager, ca: &mut CaManager) -> serde_json::Value {
    let _ = ca.stop();
    let _ = vm.stop();
    json!({"ok": true, "msg": "stopped"})
}

fn handle_start_vm(vm: &mut VmManager) -> serde_json::Value {
    match vm.start() {
        Ok(()) => json!({"ok": true, "msg": "vm starting"}),
        Err(e) => json!({"ok": false, "error": e}),
    }
}

fn handle_stop_vm(vm: &mut VmManager) -> serde_json::Value {
    let _ = vm.stop();
    json!({"ok": true, "msg": "vm stopped"})
}

fn handle_start_ca(ca: &mut CaManager, config: &Config) -> serde_json::Value {
    match ca.start(config.vsock_cid) {
        Ok(()) => json!({"ok": true, "msg": "ca starting"}),
        Err(e) => json!({"ok": false, "error": e}),
    }
}

fn handle_stop_ca(ca: &mut CaManager) -> serde_json::Value {
    let _ = ca.stop();
    json!({"ok": true, "msg": "ca stopped"})
}

fn handle_tail_log(req: &serde_json::Value, config: &Config) -> serde_json::Value {
    let source = req["source"].as_str().unwrap_or("daemon");
    let requested = req["lines"].as_u64().unwrap_or(200) as usize;
    let lines = requested.min(MAX_TAIL_LINES);
    let log_path = match source {
        "vm" | "pvm" => config.log_dir.join("pvm.log"),
        "ca" => config.log_dir.join("ca.log"),
        "daemon" => config.log_dir.join("daemon.log"),
        _ => return json!({"ok": false, "error": format!("unknown source: {source}")}),
    };
    let content = log_collector::tail_log(&log_path, lines);
    json!({"ok": true, "content": content})
}
