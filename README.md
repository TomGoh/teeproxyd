# teeproxyd

Android system daemon for managing the TEE virtual machine and secret-proxy CA lifecycle. Part of the OpenClaw security enhanced project.

## Overview

`teeproxyd` is a single static Rust binary (aarch64-unknown-linux-musl, ~1.4MB) that:

- Starts at boot via Android `init.rc` (`on property:sys.boot_completed=1`)
- Auto-launches the x-kernel pKVM virtual machine (`pvm-manage` + `crosvm`)
- Launches `secret_proxy_ca` (HTTP server on `:19030`) after vsock is ready
- Monitors health via TCP probe, restarts CA on failure
- Exposes a Unix socket IPC at `/dev/socket/teeproxyd` for control
- Handles graceful shutdown and crash recovery with exponential backoff

Replaces the previous `root-helper.sh` shell script + `tee-proxy-app` APK model.

## Building

Requires the `aarch64-unknown-linux-musl` target and a musl cross linker.

```bash
rustup target add aarch64-unknown-linux-musl
bash build.sh
# → target/aarch64-unknown-linux-musl/release/teeproxyd
```

On macOS you need `musl-cross`:

```bash
brew install filosottile/musl-cross/musl-cross
```

## Deployment

See `teeproxyd.rc` for the Android init.rc service definition.

### Runtime layout on device

```
/system/bin/teeproxyd                  # daemon binary
/system/etc/init/teeproxyd.rc          # init.rc service
/data/teeproxy/                        # data root (root:system 0750)
  ├── vm/                              # VM artifacts
  │   ├── pvm-manage, crosvm, custom_pvmfw, kernel.bin, disk.img
  │   └── .pvm_instance/
  ├── bin/secret_proxy_ca              # CA binary (NDK Bionic)
  ├── logs/{pvm,ca,daemon}.log
  └── teeproxyd.conf                   # optional JSON config
/dev/socket/teeproxyd                  # init-created Unix socket
```

### Configuration

Optional JSON config at `/data/teeproxy/teeproxyd.conf`. All fields have defaults — see `teeproxyd.conf.example`.

## IPC

Line-delimited JSON over Unix socket:

```json
→ {"cmd":"status"}
← {"ok":true,"vm":"ready","ca":"ready","startup_phase":"Complete","vm_cid":103,"ca_port":19030,"uptime_secs":3600}

→ {"cmd":"start_all"}  / {"cmd":"stop_all"}
→ {"cmd":"start_vm"}   / {"cmd":"stop_vm"}
→ {"cmd":"start_ca"}   / {"cmd":"stop_ca"}
→ {"cmd":"tail_log","source":"ca","lines":200}
→ {"cmd":"ping"}
```

## Architecture

- **Single-threaded** `nix::poll`-based event loop (no tokio/async)
- **Self-pipe** signal handling for SIGTERM/SIGINT/SIGCHLD
- **Non-blocking startup state machine**: `VmStarting → WaitingVsock → CaStarting → WaitingCaPort → Complete`
- **Health monitoring**: TCP probe with separate `ConnectionRefused` and `TimedOut` counters
- **Crash recovery**: per-process `crash_count` with 5-strike threshold, reset after 60s stable

## Design decisions

- **musl static**: single binary, no dynamic linker dependency on Android
- **CA stays NDK/Bionic**: CA needs DNS (`getaddrinfo` → `dnsproxyd`), musl can't do DNS on Android
- **Key provisioning NOT handled by teeproxyd**: openclaw provisions keys directly via CA's HTTP admin API (`POST /admin/keys/provision` with `X-Admin-Token`). This avoids key exposure in `/proc/pid/cmdline`.

## AOSP Integration (Vendor Module)

This repo is structured as a drop-in AOSP vendor module:

```
vendor/kylin/teeproxyd/
├── Android.bp              # cc_prebuilt_binary module
├── teeproxyd.rc            # init.rc service definition
├── prebuilts/teeproxyd     # static musl binary (aarch64, 1.4MB)
└── src/                    # Rust source (for audit/rebuild)
```

Integration steps:

1. Copy this directory into AOSP tree: `vendor/kylin/teeproxyd/`
2. Add to product makefile (`device/kylin/<product>/device.mk`):
   ```make
   PRODUCT_PACKAGES += teeproxyd
   ```
3. Build ROM. `teeproxyd` will be installed to `/system/bin/teeproxyd` and `teeproxyd.rc` to `/system/etc/init/teeproxyd.rc`.

Runtime dependencies (VM artifacts + CA binary) are deployed separately to `/data/teeproxy/` via `adb push` or OTA.
