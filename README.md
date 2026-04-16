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

## Repository Layout

```
.
├── Android.bp                   # AOSP vendor module definition
├── teeproxyd.rc                 # init.rc service (auto-start at boot)
├── teeproxyd.conf.example       # optional config (JSON)
├── deploy.sh                    # adb deploy script (dev/testing)
├── build.sh                     # build teeproxyd from source
├── Cargo.toml, Cargo.lock       # Rust crate metadata
├── src/                         # Rust source (8 modules)
└── prebuilts/
    ├── teeproxyd                # static musl aarch64 (1.4MB)
    └── vm/                      # runtime VM artifacts (~135MB)
        ├── crosvm               # crosvm-android hypervisor (14MB)
        ├── custom_pvmfw         # pVM firmware (1.2MB)
        ├── pvm-manage           # VM process manager (900KB)
        ├── kernel.bin           # SM2-signed x-kernel (55MB)
        └── disk.img             # TEE rootfs with secret_proxy_ta (64MB)
```

## AOSP Integration (Vendor Module)

**See [INTEGRATION.md](INTEGRATION.md) for the full step-by-step guide.**

Quick version:

```bash
git clone https://github.com/TomGoh/teeproxyd.git vendor/kylin/teeproxyd
# Then in device/kylin/<product>/device.mk:
PRODUCT_PACKAGES += teeproxyd_all
# Then: m
```

`teeproxyd_all` pulls in:
- `teeproxyd` daemon → `/system/bin/teeproxyd` + `/system/etc/init/teeproxyd.rc`
- VM artifacts (crosvm, pvmfw, pvm-manage, kernel.bin, disk.img) → `/vendor/etc/teeproxy/vm/`
- `secret_proxy_ca` → `/vendor/etc/teeproxy/bin/`

At boot, `teeproxyd.rc` (`on post-fs-data`) copies everything from `/vendor/etc/teeproxy/` to the writable `/data/teeproxy/` tree, then init starts the daemon on `sys.boot_completed=1`.

## Quick Deploy (adb, dev only)

```bash
./deploy.sh --device 10.218.64.6 --start     # push everything + start daemon
./deploy.sh --status --device 10.218.64.6    # check running state
./deploy.sh --stop --device 10.218.64.6      # stop everything
```

Use `deploy.sh` for iterative development on a device that already has a working TEE stack. For first-time bring-up, integrate via ROM (see INTEGRATION.md).
