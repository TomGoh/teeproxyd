#!/usr/bin/env bash
#
# deploy.sh — Deploy teeproxyd and VM artifacts to an Android device.
#
# This script uses `adb` to push everything in this repo to the standard
# device paths (/data/teeproxy/). It is intended for development/testing
# (manual push); for production, the daemon + init.rc should be integrated
# into the ROM via Android.bp, and VM artifacts deployed via OTA.
#
# Usage:
#   ./deploy.sh                         # deploy to default device
#   ./deploy.sh --device 10.218.64.6    # deploy to specific device IP
#   ./deploy.sh --start                 # also start teeproxyd after deploy
#   ./deploy.sh --status                # check status on device
#   ./deploy.sh --stop                  # stop teeproxyd + VM + CA
#
# Prerequisites:
#   - adb connected to target device (userdebug + root)
#   - Device must have /dev/kvm and pKVM support
#   - secret_proxy_ca binary deployed separately (NDK Bionic build,
#     placed at /data/teeproxy/bin/secret_proxy_ca)

set -euo pipefail
cd "$(dirname "$0")"
REPO_DIR="$(pwd)"
REMOTE="/data/teeproxy"

DEVICE_IP="${DEVICE_IP:-}"
START=false
STATUS_ONLY=false
STOP_ONLY=false

while [[ $# -gt 0 ]]; do
    case "$1" in
        --device) shift; DEVICE_IP="$1" ;;
        --start)  START=true ;;
        --status) STATUS_ONLY=true ;;
        --stop)   STOP_ONLY=true ;;
        -h|--help)
            sed -n '2,25p' "$0"
            exit 0
            ;;
        *) echo "unknown option: $1" >&2; exit 1 ;;
    esac
    shift
done

if [[ -n "$DEVICE_IP" ]]; then
    DEV_ADB="${DEVICE_IP}:5555"
    adb connect "$DEV_ADB" >/dev/null 2>&1 || true
    ADB=("adb" "-s" "$DEV_ADB")
else
    ADB=("adb")
fi

GREEN='\033[0;32m'; CYAN='\033[0;36m'; RED='\033[0;31m'; NC='\033[0m'
info()  { echo -e "${CYAN}[INFO]${NC} $*"; }
ok()    { echo -e "${GREEN}[ OK ]${NC} $*"; }
fail()  { echo -e "${RED}[FAIL]${NC} $*"; exit 1; }

# --- Status mode ---
if $STATUS_ONLY; then
    info "teeproxyd processes:"
    "${ADB[@]}" shell "su 0 ps -ef | grep -E 'teeproxyd|pvm-manage|crosvm|secret_proxy_ca' | grep -v grep" || echo "  (none running)"
    echo
    info "Last 20 daemon log lines:"
    "${ADB[@]}" shell "su 0 tail -20 $REMOTE/logs/daemon.log 2>/dev/null" || echo "  (no log)"
    exit 0
fi

# --- Stop mode ---
if $STOP_ONLY; then
    info "Stopping teeproxyd and children..."
    "${ADB[@]}" shell "su 0 setprop ctl.stop teeproxyd" 2>/dev/null || true
    "${ADB[@]}" shell "su 0 pkill -9 teeproxyd; su 0 pkill -9 crosvm; su 0 pkill -9 pvm-manage; su 0 pkill -9 -f secret_proxy_ca" 2>/dev/null || true
    sleep 1
    ok "Stopped"
    exit 0
fi

echo
echo "==============================================="
echo "  teeproxyd deploy"
echo "==============================================="
echo

# --- Verify prebuilts present ---
for f in prebuilts/teeproxyd prebuilts/vm/crosvm prebuilts/vm/pvm-manage \
         prebuilts/vm/custom_pvmfw prebuilts/vm/kernel.bin prebuilts/vm/disk.img \
         prebuilts/bin/secret_proxy_ca; do
    [[ -f "$f" ]] || fail "missing prebuilt: $f"
done
ok "all prebuilts present"

# --- Stop existing processes ---
info "Stopping existing services..."
"${ADB[@]}" shell "su 0 setprop ctl.stop teeproxyd" 2>/dev/null || true
"${ADB[@]}" shell "su 0 pkill -9 teeproxyd; su 0 pkill -9 crosvm; su 0 pkill -9 pvm-manage; su 0 pkill -9 -f secret_proxy_ca" 2>/dev/null || true
sleep 2

# --- Create dirs ---
info "Creating /data/teeproxy/..."
"${ADB[@]}" shell "su 0 mkdir -p $REMOTE/vm/.pvm_instance $REMOTE/bin $REMOTE/logs"
"${ADB[@]}" shell "su 0 chown -R root:system $REMOTE; su 0 chmod 750 $REMOTE"

# --- Push via /data/local/tmp staging ---
# adb push can't write directly to root-owned dirs, so stage then cp.
STAGING="/data/local/tmp/teeproxyd_staging"
info "Pushing to staging at $STAGING..."
"${ADB[@]}" shell "mkdir -p $STAGING"

info "  teeproxyd..."
"${ADB[@]}" push prebuilts/teeproxyd "$STAGING/" 2>&1 | tail -1

info "  VM artifacts..."
for f in crosvm custom_pvmfw pvm-manage kernel.bin disk.img; do
    "${ADB[@]}" push "prebuilts/vm/$f" "$STAGING/" 2>&1 | tail -1
done

info "  CA binary (NDK)..."
"${ADB[@]}" push prebuilts/bin/secret_proxy_ca "$STAGING/" 2>&1 | tail -1

info "Moving to $REMOTE/..."
"${ADB[@]}" shell "su 0 cp $STAGING/teeproxyd $REMOTE/bin/ && su 0 chmod 755 $REMOTE/bin/teeproxyd"
"${ADB[@]}" shell "su 0 cp $STAGING/secret_proxy_ca $REMOTE/bin/ && su 0 chmod 755 $REMOTE/bin/secret_proxy_ca"
"${ADB[@]}" shell "su 0 cp $STAGING/crosvm $STAGING/custom_pvmfw $STAGING/pvm-manage $STAGING/kernel.bin $STAGING/disk.img $REMOTE/vm/"
"${ADB[@]}" shell "su 0 chmod 755 $REMOTE/vm/crosvm $REMOTE/vm/pvm-manage"
"${ADB[@]}" shell "su 0 chown -R root:system $REMOTE"

info "Cleaning up staging..."
"${ADB[@]}" shell "rm -rf $STAGING"

# --- Optional: push teeproxyd.rc to /system (requires remount) ---
info "Attempting to install teeproxyd.rc (for init.rc auto-start)..."
if "${ADB[@]}" shell "su 0 mount -o remount,rw /system" 2>/dev/null; then
    "${ADB[@]}" push teeproxyd.rc "$STAGING.rc" 2>&1 | tail -1
    "${ADB[@]}" shell "su 0 cp $STAGING.rc /system/etc/init/teeproxyd.rc && rm $STAGING.rc"
    "${ADB[@]}" shell "su 0 cp $REMOTE/bin/teeproxyd /system/bin/teeproxyd"
    ok "  teeproxyd.rc installed — will auto-start on next boot"
else
    info "  /system is read-only (verified boot?). Use manual start:"
    info "    $0 --start"
fi

# --- Clear old instance.img (DICE measurement may change after kernel update) ---
info "Clearing stale instance.img..."
"${ADB[@]}" shell "su 0 rm -f $REMOTE/vm/.pvm_instance/instance.img" 2>/dev/null || true

echo
ok "Deploy complete"
echo
info "Files deployed:"
"${ADB[@]}" shell "su 0 ls -la $REMOTE/bin/ $REMOTE/vm/"
echo

# --- Optional start ---
if $START; then
    echo
    info "Starting teeproxyd..."
    if "${ADB[@]}" shell "su 0 setprop ctl.start teeproxyd" 2>/dev/null; then
        ok "Started via init (ctl.start)"
    else
        # Fallback: run directly
        "${ADB[@]}" shell "su 0 sh -c 'RUST_LOG=info nohup $REMOTE/bin/teeproxyd > $REMOTE/logs/daemon.log 2>&1 &'"
        ok "Started manually"
    fi
    sleep 3
    "${ADB[@]}" shell "su 0 ps -ef | grep teeproxyd | grep -v grep" || true
fi

echo
info "Next steps:"
info "  - Watch daemon log:  $0 --status"
info "  - Start daemon:      $0 --start"
info "  - Stop everything:   $0 --stop"
