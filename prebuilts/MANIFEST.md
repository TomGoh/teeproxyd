# teeproxyd prebuilts — ROM integration deliverable

Self-contained drop-in for integrating the teeproxyd + VM + CA stack
into an Android ROM for the Phytium pd2508 platform (or any Android
arm64 build with pKVM support).

Version: **pvm-manage with `/data/teeproxy/` for both scratch dir and
control socket** (replaces `/tmp/pvm-composite` and `/tmp/pvm.socket`
— both hit the same "Android has no standard /tmp" failure mode).

---

## Layout

```
prebuilts/
├── MANIFEST.md                        ← this file
├── CHECKSUMS.sha256                   ← SHA256 of every binary below
│
├── teeproxyd                          ← supervisor daemon (ELF arm64, musl-static)
├── bin/
│   └── secret_proxy_ca                ← CA binary (ELF arm64, Bionic-linked)
├── vm/
│   ├── crosvm                         ← VMM
│   ├── custom_pvmfw                   ← protected-VM firmware
│   ├── kernel.bin                     ← guest kernel image
│   ├── disk.img                       ← guest rootfs
│   └── pvm-manage                     ← VM manager (UPDATED — new composite path)
│
├── init/
│   ├── teeproxyd.no-sepolicy.rc       ← userdebug ROMs, NO teeproxyd.te deployed (use this for dev/exhibition)
│   └── teeproxyd.with-sepolicy.rc     ← production, requires teeproxyd.te deployed into system/sepolicy/private/
│
└── sepolicy/
    ├── teeproxyd.te                           ← domain + allow rules
    ├── file_contexts.fragment                 ← append into system/sepolicy/private/file_contexts
    └── file_contexts_test_data.fragment       ← append into system/sepolicy/contexts/file_contexts_test_data
```

All binaries are **aarch64 Linux ELF**. `teeproxyd` and `pvm-manage`
are statically linked against musl (no libc dependency on the device).
`secret_proxy_ca` links Bionic (required — it uses Android's DNS
resolver path via dnsproxyd → netd).

---

## Deployment paths on device

| Prebuilt file | Target path on device |
|---|---|
| `teeproxyd` | `/system/bin/teeproxyd` *or* `/apex/com.android.virt/bin/teeproxyd` (choose one) |
| `bin/secret_proxy_ca` | `/data/teeproxy/bin/secret_proxy_ca` |
| `vm/crosvm` | `/data/teeproxy/vm/crosvm` |
| `vm/custom_pvmfw` | `/data/teeproxy/vm/custom_pvmfw` |
| `vm/kernel.bin` | `/data/teeproxy/vm/kernel.bin` |
| `vm/disk.img` | `/data/teeproxy/vm/disk.img` |
| `vm/pvm-manage` | `/data/teeproxy/vm/pvm-manage` |
| `init/teeproxyd.no-sepolicy.rc` **or** `init/teeproxyd.with-sepolicy.rc` | `/system/etc/init/teeproxyd.rc` (rename during install; see "Which .rc to use" below) |
| `sepolicy/teeproxyd.te` | `system/sepolicy/private/teeproxyd.te` (build-tree) |

**Two options** for binary staging — the `teeproxyd.rc` supports both:

**Option A: /vendor → /data copy at post-fs-data**
- Ship binaries into `/vendor/etc/teeproxy/` via the vendor partition.
- `init` copies to `/data/teeproxy/` at boot (uncomment the `copy`/`chmod`/`chown`
  block in `init/teeproxyd.rc`).

**Option B: install directly (simpler for dev/exhibition)**
- `adb push` the binaries to their `/data/teeproxy/...` paths once.
- Leave the `copy` block commented out in `init/teeproxyd.rc`.

---

## Which .rc to use

| ROM type | teeproxyd.te deployed? | Use |
|---|---|---|
| userdebug, dev/exhibition | NO | `init/teeproxyd.no-sepolicy.rc` |
| userdebug, full integration | YES | `init/teeproxyd.with-sepolicy.rc` |
| user build (production) | **YES (required)** | `init/teeproxyd.with-sepolicy.rc` |

The difference is one line: `.no-sepolicy.rc` has `seclabel u:r:su:s0`
which forces the service into the universally permissive `su` domain.
`su` only exists on userdebug builds, so the no-sepolicy variant will
fail to start on user builds.

Rename to `teeproxyd.rc` when installing — init only loads files ending
in `.rc` regardless of prefix, but convention expects the canonical name.

---

## ROM integration checklist

### 1. Binaries

Install into the correct `PRODUCT_COPY_FILES` / `PRODUCT_PACKAGES` entries
(or the APEX `prebuilts` block if going the `com.android.virt` route).

### 2. SEPolicy

```bash
# In the AOSP tree:
cp prebuilts/sepolicy/teeproxyd.te \
   system/sepolicy/private/teeproxyd.te

# Pick the binary-location option (A or B) at the top of each fragment,
# then append to the target files:
cat prebuilts/sepolicy/file_contexts.fragment \
    >> system/sepolicy/private/file_contexts

cat prebuilts/sepolicy/file_contexts_test_data.fragment \
    >> system/sepolicy/contexts/file_contexts_test_data
```

**Do not** put `teeproxyd.te` under `device/*/sepolicy/` — that tree
is vendor policy, and `kvm_device`, `dnsproxyd_socket`, `netd`,
`mdnsd_socket` are all private system types invisible to vendor policy.
We tried that path and the build fails on "unknown type".

### 3. Init

```bash
cp prebuilts/init/teeproxyd.rc \
   /<ROM staging>/system/etc/init/teeproxyd.rc
```

Or add as a `prebuilt_etc` Soong module for proper packaging.

### 4. Verify checksums on device after push/flash

```bash
adb shell sha256sum /system/bin/teeproxyd \
                     /data/teeproxy/vm/pvm-manage \
                     /data/teeproxy/bin/secret_proxy_ca

# Compare with CHECKSUMS.sha256 in this dir.
```

---

## What changed in this prebuilt (vs. the previous drop)

| Component | Change |
|---|---|
| `vm/pvm-manage` | Two hardcoded paths moved off `/tmp`: `VM_TEMP_DIR` (`/tmp/pvm-composite` → `/data/teeproxy/composite`) and `PVM_SOCKET` (`/tmp/pvm.socket` → `/data/teeproxy/pvm.socket`). Fixes boot-time "Permission denied" on ROMs where `/tmp` is absent or has restrictive DAC. `PVM_SOCKET` is host-side only (pvm-manage ↔ crosvm control plane); zero impact on CA/TA which use vsock. |
| `init/teeproxyd.rc` | Added `mkdir /data/teeproxy/composite 0770 root system`. Capabilities set to `SYS_ADMIN SYS_NICE NET_RAW IPC_LOCK` — **IPC_LOCK is critical**: without it, `crosvm` fails at vcpu init with ENOMEM because Android's default RLIMIT_MEMLOCK is 64 KiB and pKVM's vcpu state mlock exceeds that. `CAP_IPC_LOCK` bypasses the rlimit. Confirmed by reproduction on pd2508 2026-04-18: shell-started chain worked (full caps), init-started failed (narrow caps), adding IPC_LOCK to the service .rc fixed init-started path. |
| `sepolicy/teeproxyd.te` | Removed `teeproxyd_tmp_file` type + all tmpfs-related allow rules (no longer needed). Removed `sock_file_type` attribute from `teeproxyd_socket` declaration (not declared on all vendor policy trees; functionally unneeded). |

---

## Host-side build provenance

`vm/pvm-manage` was built on:

- **Host**: Ubuntu 24.04 arm64 (via orb)
- **Rust target**: `aarch64-unknown-linux-musl`
- **Linker**: `/usr/local/bin/aarch64-linux-musl-gcc`
- **Flags**: `RUSTFLAGS="-C target-feature=+crt-static"`
- **Profile**: release (optimized, debug symbols stripped via `strip`)
- **Size**: 645K
- **SHA256**: `f72ead8f392b38b3251a18a6a13ccdcfff07d47af303e0a65e477a929fbd40a4`
- **Embedded paths** (verified via `strings`): `/data/teeproxy/composite`, `/data/teeproxy/pvm.socket`. No `/tmp/*` paths remain in the binary.

Other binaries (`teeproxyd`, `crosvm`, `custom_pvmfw`, `kernel.bin`,
`disk.img`, `bin/secret_proxy_ca`) are carried over unchanged from the
previous prebuilts drop — their checksums in `CHECKSUMS.sha256` match
what was shipped last time.
