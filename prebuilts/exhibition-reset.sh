#!/usr/bin/env bash
#
# exhibition-reset.sh —— 展会/演示设备的一键重置脚本
#
# 用途: factory reset(恢复出厂设置)之后,把 teeproxyd 整套 stack 重新部署
#       到设备的 /data 目录下。假设 ROM 里已经包含了 /system/bin/teeproxyd
#       和 /system/etc/init/teeproxyd.rc(要么走 ROM 集成,要么之前手动推过
#       /system),这个脚本只处理会被 factory reset 清掉的 /data 那一侧。
#
# 用法:
#   ./exhibition-reset.sh <设备 IP> [选项]
#
# 选项:
#   --skip-verify-checksums  不做 SHA256 校验(更快,用于重复测试)
#   -h / --help              打印这个帮助
#
# 主机端前提:
#   - adb 在 PATH 里,设备能 `adb connect`
#   - 这个脚本必须在 kit 目录里跑(同目录下要有 bin/ vm/ CHECKSUMS.sha256)
#
# 设备端前提:
#   - userdebug 版本(`su root` 能用)
#   - /system/bin/teeproxyd 已经装好(随 ROM 刷入)
#   - /system/etc/init/teeproxyd.rc 已经装好(随 ROM 刷入)
#   - 内核编译时打开了 pKVM(`kvm-arm.mode=protected` 加进 cmdline)
#
# 脚本做什么:
#   1. 预检(adb + root + ROM 侧文件齐全)
#   2. 停掉正在跑的服务
#   3. 推 VM + CA 二进制到 /data/teeproxy/{vm,bin}/
#   4. SHA256 校验
#   5. 启服务 + 等 /health 返回 200
#
# 脚本不做什么:
#   - 密钥 provision —— kit 里的 disk.img 出厂 TEE 存储是空的。API key 由
#     openclaw-termux 首次启动的 configure 流程自己注入。这样密钥不会出现
#     在脚本参数、shell history 或日志里。
#
# 退出码:
#   0   成功
#   1   用法错误
#   2   preflight 检查失败(adb/设备/root 不对)
#   3   ROM 侧文件缺失(先刷 ROM)
#   4   推送二进制失败
#   5   SHA256 校验不匹配
#   6   服务起不来
#
# 幂等: 反复跑安全,每一步动作前会先看当前状态。

set -euo pipefail

# ─── 参数解析 ─────────────────────────────────────────────────────────────
SCRIPT_DIR="$(cd "$(dirname "$0")" && pwd)"
KIT_DIR="$SCRIPT_DIR"

DEVICE=""
SKIP_VERIFY=false

while [[ $# -gt 0 ]]; do
    case "$1" in
        -h|--help)
            sed -n '2,38p' "$0"; exit 0 ;;
        --skip-verify-checksums)
            SKIP_VERIFY=true ;;
        -*)
            echo "未知选项: $1" >&2; exit 1 ;;
        *)
            if [[ -z "$DEVICE" ]]; then
                DEVICE="$1"
            else
                echo "多余参数: $1" >&2; exit 1
            fi
            ;;
    esac
    shift
done

[[ -n "$DEVICE" ]] || { echo "用法: $0 <设备 IP> [选项]"; exit 1; }

ADB_DEV="${DEVICE}:5555"

# ─── 彩色输出工具 ────────────────────────────────────────────────────────
R=$'\e[31m'; G=$'\e[32m'; Y=$'\e[33m'; C=$'\e[36m'; B=$'\e[1m'; NC=$'\e[0m'
info()  { echo "${C}[信息]${NC} $*"; }
ok()    { echo "${G}[成功]${NC} $*"; }
warn()  { echo "${Y}[警告]${NC} $*"; }
die()   { local code="${2:-1}"; echo "${R}[失败]${NC} $1" >&2; exit "$code"; }

ashell() { adb -s "$ADB_DEV" shell "$@"; }
apush()  { adb -s "$ADB_DEV" push "$@"; }

# ─── Step 0: 预检 ────────────────────────────────────────────────────────
echo "${B}━━━ 展会重置 :: $DEVICE ━━━${NC}"
info "执行 preflight 预检..."

adb start-server >/dev/null 2>&1
adb connect "$ADB_DEV" >/dev/null 2>&1 || true
sleep 1
adb -s "$ADB_DEV" wait-for-device 2>/dev/null || die "adb 连不上 $ADB_DEV" 2

# 确认是 root
uid_line=$(ashell 'su root id' 2>&1 || true)
echo "$uid_line" | grep -q 'uid=0(root)' || die "'su root' 不 work —— 这是 userdebug 版本吗?" 2
ok "设备已连接,root 可用"

# 确认 ROM 侧文件在
ashell 'ls /system/bin/teeproxyd' >/dev/null 2>&1 \
    || die "/system/bin/teeproxyd 不存在 —— 先刷带 teeproxyd 的 ROM" 3
ashell 'ls /system/etc/init/teeproxyd.rc' >/dev/null 2>&1 \
    || die "/system/etc/init/teeproxyd.rc 不存在 —— 先刷带 init rc 的 ROM" 3
ok "ROM 侧文件齐全"

# 等 init post-fs-data 建好 /data/teeproxy 目录树
info "等待 init post-fs-data 建目录(最多 30s)..."
for i in $(seq 1 30); do
    if ashell 'su root ls -d /data/teeproxy/vm' >/dev/null 2>&1; then
        ok "/data/teeproxy 目录树已就绪"
        break
    fi
    sleep 1
    if [[ $i -eq 30 ]]; then
        die "/data/teeproxy 一直没出现 —— 查 init 日志: adb shell 'dmesg | grep teeproxy'" 3
    fi
done

# ─── Step 1: 停掉正在跑的服务(清空状态) ─────────────────────────────────
info "停掉 teeproxyd 及子进程(清空状态)..."
ashell 'su root setprop ctl.stop teeproxyd' 2>/dev/null || true
ashell 'su root sh -c "pkill -9 secret_proxy_ca 2>/dev/null; pkill -9 crosvm 2>/dev/null; pkill -9 pvm-manage 2>/dev/null; pkill -9 teeproxyd 2>/dev/null; true"' 2>/dev/null || true
sleep 3
ok "服务已停"

# ─── Step 2: 推送二进制 ──────────────────────────────────────────────────
push_bin() {
    # 推一个文件到设备,设置权限 + owner
    # $1=本地路径  $2=设备目标路径  $3=chmod 模式(默认 0755)
    local src="$1" dst="$2" mode="${3:-0755}"
    local tmp="/data/local/tmp/$(basename "$dst").stage"

    [[ -f "$src" ]] || die "kit 里缺文件: $src" 4

    apush "$src" "$tmp" >/dev/null 2>&1 \
        || die "推送失败: $src -> $tmp" 4
    ashell "su root sh -c 'mv $tmp $dst && chmod $mode $dst && chown root:system $dst'" \
        || die "安装失败: $dst" 4
}

info "推送 VM 相关二进制到 /data/teeproxy/vm/..."
push_bin "$KIT_DIR/vm/crosvm"        /data/teeproxy/vm/crosvm
push_bin "$KIT_DIR/vm/custom_pvmfw"  /data/teeproxy/vm/custom_pvmfw
push_bin "$KIT_DIR/vm/kernel.bin"    /data/teeproxy/vm/kernel.bin
push_bin "$KIT_DIR/vm/disk.img"      /data/teeproxy/vm/disk.img
push_bin "$KIT_DIR/vm/pvm-manage"    /data/teeproxy/vm/pvm-manage
ok "VM 二进制已推送"

info "推送 CA 二进制到 /data/teeproxy/bin/..."
push_bin "$KIT_DIR/bin/secret_proxy_ca" /data/teeproxy/bin/secret_proxy_ca
ok "CA 已推送"

# ─── Step 3: 校验 SHA256 ─────────────────────────────────────────────────
if ! $SKIP_VERIFY; then
    info "校验设备上 SHA256 是否跟 kit 一致..."
    [[ -f "$KIT_DIR/CHECKSUMS.sha256" ]] || die "kit 里缺 CHECKSUMS.sha256" 5

    # 设备上算一把
    remote=$(ashell 'su root sh -c "cd /data/teeproxy && sha256sum bin/secret_proxy_ca vm/crosvm vm/custom_pvmfw vm/kernel.bin vm/disk.img vm/pvm-manage"')

    while IFS= read -r line; do
        [[ -z "$line" ]] && continue
        expected=$(echo "$line" | awk '{print $1}')
        path=$(echo "$line" | awk '{print $2}')
        # 跳过 teeproxyd,它随 ROM 装进 /system,不在 /data/teeproxy 下
        [[ "$path" == "teeproxyd" ]] && continue
        actual=$(echo "$remote" | awk -v p="$path" '$2==p {print $1}')
        if [[ "$expected" != "$actual" ]]; then
            die "checksum 不匹配,文件 $path: 期望=$expected 实际=$actual" 5
        fi
    done < "$KIT_DIR/CHECKSUMS.sha256"
    ok "所有 checksum 都一致"
else
    warn "按 --skip-verify-checksums 跳过了校验"
fi

# ─── Step 4: 启动 teeproxyd ──────────────────────────────────────────────
info "通过 init 启动 teeproxyd..."
ashell 'su root setprop ctl.start teeproxyd'

info "等 /health 返回 200(最多 90s)..."
health_ok=false
for i in $(seq 1 90); do
    resp=$(ashell 'su root sh -c "printf \"GET /health HTTP/1.0\r\n\r\n\" | toybox nc -w 2 127.0.0.1 19030"' 2>/dev/null || true)
    if echo "$resp" | grep -q '"ok":true'; then
        ok "/health 用 ${i}s 返回 200 OK"
        health_ok=true
        break
    fi
    sleep 1
    # 每 5s 打一个点表示还在等
    if (( i % 5 == 0 )); then echo -n "."; fi
done
echo

if ! $health_ok; then
    warn "/health 在 90s 内没返回 200,dump 日志定位:"
    echo "--- daemon.log ---"
    ashell 'su root tail -20 /data/teeproxy/logs/daemon.log 2>&1' || true
    echo "--- pvm.log ---"
    ashell 'su root tail -30 /data/teeproxy/logs/pvm.log 2>&1' || true
    echo "--- ca.log ---"
    ashell 'su root tail -10 /data/teeproxy/logs/ca.log 2>&1' || true
    die "服务 90s 内没起来" 6
fi

# 确认进程树
info "进程树:"
ashell 'su root ps -A -o PID,PPID,USER,ARGS' 2>&1 | grep -E 'teeproxy|crosvm|pvm-manage|secret_proxy_ca' | grep -v grep || true

# ─── 最后再看一次 /health 作快照 ─────────────────────────────────────────
echo
info "最终 /health 快照(TEE 存储此时应为空 slot,等 openclaw 首次启动注入):"
ashell 'su root sh -c "printf \"GET /health HTTP/1.0\r\n\r\n\" | toybox nc -w 2 127.0.0.1 19030"' 2>/dev/null | tail -5 || true

echo
ok "${B}$DEVICE 展会重置完成 —— 现在打开 openclaw-termux 做 configure 即可${NC}"
