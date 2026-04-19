# AOSP 集成指南

本文档说明如何把 teeproxyd 及其所有运行时依赖集成进 AOSP 镜像。

---

## 0. 集成后的目标形态

在了解具体步骤前，先看清楚"集成完做成什么样"。

### 0.1 用户视角

设备出厂就自带 TEE 密钥代理能力，用户完全无感知：
- 开机即可用，无需安装任何额外 App、无需 adb、无需手动配置
- openclaw-termux 正常安装使用，开启「安全模式」时自动走 TEE 加密通道
- Factory reset 后照样可用——系统镜像里预装的东西会自动恢复

### 0.2 开机自启流程

```
设备上电
  ↓
Bootloader → Android init
  ↓
init 解析 /system/etc/init/teeproxyd.rc（由 Android.bp 构建时打入）
  ↓
on post-fs-data 阶段：
  - 创建 /data/teeproxy/{vm,bin,logs,composite}（root:system 0750/0770）
    · composite/ 是 pvm-manage 的 composite disk scratch 目录
      （原先走 /tmp/pvm-composite，因 Android /tmp 不可靠迁到此处）
  - copy /vendor/etc/teeproxy/vm/*    → /data/teeproxy/vm/
  - copy /vendor/etc/teeproxy/bin/*   → /data/teeproxy/bin/
  - chmod 0755 可执行文件
  - （可选）restorecon -R /data/teeproxy  # 仅当部署了 teeproxyd.te 时
  ↓
系统继续启动（Zygote、SystemServer…）
  ↓
on property:sys.boot_completed=1 触发：start teeproxyd
  ↓
teeproxyd 启动（由 init 管理生命周期）
  ├─ 读取可选配置 /data/teeproxy/teeproxyd.conf
  ├─ 打开 /dev/socket/teeproxyd（init 预创建的 Unix socket）
  └─ 执行启动状态机：VmStarting → WaitingVsock → CaStarting → Complete
  ↓
spawn pvm-manage → crosvm → 加载 SM2 签名的 kernel.bin + disk.img
  ↓
x-kernel pVM 启动 → secret_proxy_ta + teec_cc_bridge 就绪
  ↓
teeproxyd 探测到 vsock 可用 → spawn secret_proxy_ca serve --port 19030
  ↓
CA 端口 19030 就绪 → 全链路 Ready
  ↓
用户打开 openclaw-termux → HTTP POST 127.0.0.1:19030/... → TEE 代理生效
```

从开机到 CA 就绪约 30-40 秒，全程无需人工干预。

### 0.3 文件系统布局

```
/system/ （只读，随 ROM 出厂，dm-verity 保护）
├── bin/teeproxyd                              # 守护进程二进制
└── etc/init/teeproxyd.rc                      # init 服务定义

/vendor/ （只读，随 ROM 出厂）
└── etc/teeproxy/
    ├── vm/
    │   ├── crosvm                             # 虚拟机监控器
    │   ├── custom_pvmfw                       # pVM 固件
    │   ├── pvm-manage                         # VM 进程管理器
    │   ├── kernel.bin                         # SM2 签名的 x-kernel
    │   └── disk.img                           # TEE rootfs（含 TA）
    └── bin/
        └── secret_proxy_ca                    # CA 二进制（NDK Bionic）

/data/ （读写，factory reset 会清空，开机时由 init 重建目录树；
        /vendor 文件在 post-fs-data 阶段自动复制进来）
└── teeproxy/
    ├── vm/
    │   ├── crosvm, custom_pvmfw, pvm-manage   # 从 /vendor copy（可执行）
    │   ├── kernel.bin, disk.img               # 从 /vendor copy
    │   ├── .pvm_instance/instance.img         # pvm-manage 运行时生成（DICE 度量）
    │   └── .pvm_instance/overlay.dtbo         # 运行时生成（设备树 overlay）
    ├── bin/secret_proxy_ca                    # 从 /vendor copy
    ├── composite/                             # composite disk scratch（每次 VM boot
    │   │                                        由 pvm-manage 清空重建；0770）
    │   └── composite-instance{,.header,.footer,.filler}
    ├── pvm.socket                             # crosvm 控制套接字（Unix domain）
    ├── logs/{daemon,pvm,ca}.log               # 运行日志（pvm.log 10MB 自轮转）
    └── teeproxyd.conf                         # 可选：覆盖默认配置

/dev/socket/teeproxyd                          # init 创建的 Unix socket (0660 root:system)
```

**关键特性**：
- `/system` 和 `/vendor` 永久持久化，只能通过 OTA 更新
- `/data/teeproxy/` 可以被 factory reset 清空，但下次开机 init 会从 `/vendor` 自动重建
- 运行时持久化的 TEE 状态只有 `instance.img`（DICE 度量）和 TEE 密钥存储（加密在 disk.img 内部）

### 0.4 进程树

```
init (pid 1)
 └─ teeproxyd (init-managed)
     ├─ pvm-manage
     │   └─ crosvm
     │       └─ [pVM: x-kernel + TEE apps（pKVM 硬件隔离）]
     │             ├─ secret_proxy_ta
     │             └─ teec_cc_bridge (vsock:9999)
     └─ secret_proxy_ca (监听 127.0.0.1:19030)

openclaw-termux App (untrusted_app 域)
 └─ proot Ubuntu
     └─ openclaw gateway
         └─ HTTP POST → 127.0.0.1:19030 → CA → vsock → bridge → TA
```

### 0.5 运维特性

**日常升级（OTA）**：
- 通过 OTA 包替换 `/system/bin/teeproxyd` 和 `/vendor/etc/teeproxy/*`
- 开机时 `on post-fs-data` 重新 copy 新版本到 `/data/teeproxy/`
- 旧的 `instance.img` 因 DICE 度量不匹配会被 pvmfw 拒绝，自动失效 → 用户需重新 provision 密钥

**故障恢复**：
- teeproxyd 崩溃 → init 根据 .rc restart policy 自动重启
- CA 崩溃 → teeproxyd HealthMonitor 探测 TCP + SIGCHLD 触发重启
- VM 崩溃 → teeproxyd 检测到 pvm-manage 退出 → 状态机重启（指数退避，5 次上限）

**诊断**：
- 开发者 adb：`adb shell su 0 cat /data/teeproxy/logs/daemon.log`
- 生产远程：openclaw-termux 通过 `/dev/socket/teeproxyd` 或 CA 的 admin API 拉取日志

### 0.6 为什么必须 ROM 集成（不能只靠 adb）

我们用 `adb push` 把 `teeproxyd.rc` 推到 `/system/etc/init/` 做过测试：
- ✅ 文件写入成功，重启后依然存在
- ❌ **init 不加载这个 .rc**：`init.svc.teeproxyd` 属性不存在，`setprop ctl.start teeproxyd` 失败

Android 的安全设计：运行期加入 `/system/etc/init/` 的 `.rc` 文件不被 init 信任，**只有通过 AOSP 构建系统（Android.bp 的 `init_rc` 属性）打包的 .rc 才会被加载**。因此：

| 维度 | adb 部署（开发用） | ROM 集成（目标形态） |
|------|------------------|------------------|
| init 识别服务 | ❌ 不识别 | ✅ 识别 |
| 开机自启 | ❌ 需手动 `./deploy.sh --start` | ✅ sys.boot_completed=1 自动触发 |
| factory reset 后 | ❌ 需重新 adb 部署 | ✅ 从 /vendor 自动重建 |
| OTA 升级 | ❌ 不支持 | ✅ 标准 OTA 流程 |
| SELinux enforcing | ⚠️ 依赖 permissive | ✅ 有正式 teeproxyd.te |
| AVB 验证启动 | ❌ /system 改动破坏 AVB | ✅ AVB 重签后兼容 |

**adb 方式只是开发调试期的临时手段，生产设备必须走 ROM 集成。**

---

## TL;DR — 三步走

```bash
# 1. 克隆到 AOSP 树
git clone https://github.com/TomGoh/teeproxyd.git vendor/kylin/teeproxyd

# 2. 在 device/kylin/<product>/device.mk 中添加：
PRODUCT_PACKAGES += teeproxyd_all

# 3. 编译
m
```

完事。刷机后 teeproxyd 会开机自启，自动拉起 TEE VM 和 CA。

---

## 安装到设备上的文件清单

| 设备路径 | 源文件 | 用途 |
|----------|--------|------|
| `/system/bin/teeproxyd` | `prebuilts/teeproxyd` | 守护进程二进制（musl 静态，1.4MB） |
| `/system/etc/init/teeproxyd.rc` | `teeproxyd.rc` | init 服务定义 |
| `/vendor/etc/teeproxy/vm/crosvm` | `prebuilts/vm/crosvm` | crosvm 虚拟机监控器（14MB） |
| `/vendor/etc/teeproxy/vm/custom_pvmfw` | `prebuilts/vm/custom_pvmfw` | pVM 固件（1.2MB） |
| `/vendor/etc/teeproxy/vm/pvm-manage` | `prebuilts/vm/pvm-manage` | VM 进程管理器（900KB） |
| `/vendor/etc/teeproxy/vm/kernel.bin` | `prebuilts/vm/kernel.bin` | SM2 签名的 x-kernel（55MB） |
| `/vendor/etc/teeproxy/vm/disk.img` | `prebuilts/vm/disk.img` | 含 TA 的 TEE rootfs（64MB） |
| `/vendor/etc/teeproxy/bin/secret_proxy_ca` | `prebuilts/bin/secret_proxy_ca` | CA 二进制（NDK 编译，2.1MB） |

首次启动时，`teeproxyd.rc` 的 `on post-fs-data` 阶段会把 `/vendor/etc/teeproxy/*` 下的只读文件拷贝到可写的 `/data/teeproxy/*` 下（因为 pvm-manage 运行时需要写 `.pvm_instance/instance.img`）。

---

## 目标设备前置条件

- **pKVM 支持**：ARM64 + 用户空间可访问 `/dev/kvm`
- **Android 虚拟化框架 (AVF)**：非必需（我们自带 crosvm prebuilt），但建议有
- **userdebug 构建**：开发阶段需要 root（pvm-manage + crosvm 需要访问 `/dev/kvm`）

---

## 详细步骤

### 1. 克隆到 AOSP 树

方式 A — 直接 clone：
```bash
cd $AOSP_ROOT
git clone https://github.com/TomGoh/teeproxyd.git vendor/kylin/teeproxyd
```

方式 B — 通过 `repo` manifest（生产构建推荐）：
```xml
<project name="TomGoh/teeproxyd"
         path="vendor/kylin/teeproxyd"
         remote="github"
         revision="main"/>
```

### 2. 加入产品 makefile

在 `device/kylin/<product>/device.mk`（或对应的 vendor 文件）中加：

```makefile
PRODUCT_PACKAGES += teeproxyd_all
```

`teeproxyd_all` 是 `Android.bp` 中的 phony 模块，会拉入以下所有组件：
- `teeproxyd`（守护进程 + init.rc）
- `teeproxyd_vm_crosvm`、`teeproxyd_vm_pvmfw`、`teeproxyd_vm_pvm_manage`、`teeproxyd_vm_kernel`、`teeproxyd_vm_disk`
- `teeproxyd_ca`

如果想单独挑选某些组件安装，可以直接把它们加到 `PRODUCT_PACKAGES` 里。

### 3. SELinux 策略

开发期 permissive 可跳过此步。

设备是 enforcing 模式时，部署 `prebuilts/sepolicy/` 下的三个文件：

- `prebuilts/sepolicy/teeproxyd.te`                         — 完整 domain 策略
- `prebuilts/sepolicy/file_contexts.fragment`               — 要追加的 file_contexts 条目
- `prebuilts/sepolicy/file_contexts_test_data.fragment`     — 要追加的测试数据

#### 3.1 放在哪里 ─ 关键点：**system/sepolicy/private/，不是 vendor tree**

teeproxyd.te 引用了 `kvm_device`、`dnsproxyd_socket`、`netd`、`mdnsd_socket` 这些**系统私有类型**（private-system types）。按 AOSP Treble policy split 的规则，vendor 侧（`device/<vendor>/sepolicy/`、`BOARD_SEPOLICY_DIRS`）**看不见**这些类型，编译会报：

```
ERROR 'unknown type kvm_device' at token ';'
```

所以必须放在 plat 侧：

```bash
# 在 AOSP 源码树下（非本仓库）：
cp vendor/kylin/teeproxyd/prebuilts/sepolicy/teeproxyd.te \
   system/sepolicy/private/teeproxyd.te

cat vendor/kylin/teeproxyd/prebuilts/sepolicy/file_contexts.fragment \
    >> system/sepolicy/private/file_contexts

cat vendor/kylin/teeproxyd/prebuilts/sepolicy/file_contexts_test_data.fragment \
    >> system/sepolicy/contexts/file_contexts_test_data
```

**不要**再用 `BOARD_SEPOLICY_DIRS += device/kylin/<product>/sepolicy` 的方式加 teeproxyd.te —— 那个变量指的是 vendor policy 根。

#### 3.2 Capabilities 行 — **IPC_LOCK 必须有**

`teeproxyd.rc` 的 service block 里：

```
capabilities SYS_ADMIN SYS_NICE NET_RAW IPC_LOCK
```

这四个 cap 是**内核硬需求**，SEPolicy 替换不了：

| Cap | 为什么需要 |
|---|---|
| `SYS_ADMIN` | `KVM_CREATE_VM(KVM_VM_TYPE_ARM_PROTECTED_MASK)` |
| `SYS_NICE`  | crosvm 给 vcpu 线程设 `SCHED_FIFO` |
| `IPC_LOCK`  | pKVM vcpu 控制页 `mlock()`。**漏这个会 ENOMEM 于 vcpu 创建阶段**——Android 默认 `RLIMIT_MEMLOCK = 64KiB`，不够；`CAP_IPC_LOCK` 让 mlock 绕过 rlimit。不在任何官方文档里明确列出，但 pd2508 bring-up 实测必需 |
| `NET_RAW`   | 诊断用 raw socket（可选） |

#### 3.3 domain 过渡 vs userdebug fallback — 两种 `.rc` 变体

`prebuilts/init/` 下有两个 `.rc`，对应两种部署场景：

| 变体 | 用途 | 机制 |
|---|---|---|
| `teeproxyd.with-sepolicy.rc` | user build 或 enforcing 生产设备 | 依赖 `init_daemon_domain(teeproxyd)` 宏做 init→teeproxyd 域转换。前提是 teeproxyd.te 已部署 |
| `teeproxyd.no-sepolicy.rc` | userdebug 开发/展会设备 | `seclabel u:r:su:s0` 走 `su` 域（userdebug 独有、universally permissive）。不部署 teeproxyd.te 也能跑 |

选一个，安装时重命名为 `teeproxyd.rc`。`prebuilts/MANIFEST.md` 有选择矩阵。

#### 3.4 文件布局快速参考

```
vendor/kylin/teeproxyd/prebuilts/sepolicy/    ← 本仓库内，作为 source of truth
├── teeproxyd.te                               → system/sepolicy/private/teeproxyd.te
├── file_contexts.fragment                     → 追加到 system/sepolicy/private/file_contexts
└── file_contexts_test_data.fragment           → 追加到 system/sepolicy/contexts/file_contexts_test_data

vendor/kylin/teeproxyd/prebuilts/init/
├── teeproxyd.with-sepolicy.rc                 → /system/etc/init/teeproxyd.rc (生产)
└── teeproxyd.no-sepolicy.rc                   → /system/etc/init/teeproxyd.rc (dev)
```

详细步骤见 `prebuilts/MANIFEST.md`。

### 4. 编译并刷机

```bash
cd $AOSP_ROOT
source build/envsetup.sh
lunch <your_product>
m
```

刷入生成的镜像（`system.img`、`vendor.img` 等）。

---

## 刷机后的验证步骤

### 4.1 进程是否运行

```bash
adb shell ps -ef | grep teeproxyd
# 预期输出：
# root  XXXX  1  ... teeproxyd
# root  YYYY  XXXX  ... pvm-manage run --protected-vm-with-pvmfw ...
# root  ZZZZ  YYYY  ... crosvm ...
# root  WWWW  XXXX  ... secret_proxy_ca serve --port 19030
```

### 4.2 守护进程日志

```bash
adb shell "su 0 tail -30 /data/teeproxy/logs/daemon.log"
# 预期最后一行：
# CA ready (port 19030 accepting)
```

### 4.3 文件是否部署

```bash
adb shell "su 0 ls -la /vendor/etc/teeproxy/"
# 预期：bin/ 和 vm/ 两个子目录

adb shell "su 0 ls -la /data/teeproxy/vm/ /data/teeproxy/bin/"
# 预期：所有文件都以正确的权限被拷贝
```

### 4.4 IPC socket

```bash
adb shell "su 0 ls -l /dev/socket/teeproxyd"
# 预期：srw-rw---- root system /dev/socket/teeproxyd

adb shell "su 0 sh -c 'echo \"{\\\"cmd\\\":\\\"status\\\"}\" | nc -U /dev/socket/teeproxyd'"
# 预期 JSON：{"ok":true,"vm":"ready","ca":"ready",...}
```

---

## 配置（可选）

默认行为：开机自动启动 VM + CA，使用所有默认路径和端口。

如需覆盖，写 `/data/teeproxy/teeproxyd.conf`（JSON 格式）。示例见仓库根目录的 `teeproxyd.conf.example`。常见覆盖：

```json
{
    "ca_admin_token": "你自己的-32 字符以上的安全 token",
    "auto_start": true,
    "health_interval_secs": 10
}
```

**重要**：发布前务必修改 `ca_admin_token` 的默认值。openclaw 通过 HTTP 请求头 `X-Admin-Token` 使用这个 token 来注入 API 密钥。

---

## 更新产物

当 kernel.bin / disk.img / CA / teeproxyd 需要更新时：

1. 更新本仓库的 `prebuilts/`
2. 提交 + push
3. 在 AOSP 树中拉取新 commit：
   ```bash
   cd vendor/kylin/teeproxyd && git pull origin main
   ```
4. 重新编译：`m teeproxyd_all`（或完整 `m`）
5. OTA 或刷 vendor.img

---

## 常见问题排查

**teeproxyd 没有运行**
- 检查 init 是否失败：`adb shell dmesg | grep teeproxy`
- 手动启动：`adb shell su 0 setprop ctl.start teeproxyd`
- 查看日志：`adb shell su 0 cat /data/teeproxy/logs/daemon.log`

**VM 启动失败（pvmfw 验证失败）**
- kernel.bin 必须经过 SM2 签名。如果换了新 kernel 没重新签名，会验证失败。
- 清除过时的 instance.img：`adb shell su 0 rm /data/teeproxy/vm/.pvm_instance/instance.img` 重试。

**CA 端口 19030 不可连接**
- 查看 CA 日志：`adb shell su 0 cat /data/teeproxy/logs/ca.log`
- 通常是 VM 没启动好 —— 看 `/data/teeproxy/logs/pvm.log`

**/data/teeproxy/ 权限被拒**
- init.rc 的 `on post-fs-data` 没执行。检查 `teeproxyd.rc` 是否在 `/system/etc/init/`，语法是否正确。

**SELinux denial**
- `adb shell dmesg | grep avc` 查看被拒的操作
- 按上面步骤 3 更新 `teeproxyd.te` 策略，重新编译

**crosvm 报 `vcpu hit unknown error: Out of memory (os error 12)`**
- 这**不是** RAM 不够，是 `mlock()` 超过 `RLIMIT_MEMLOCK=64KiB`
- 检查 `getprop init.svc.teeproxyd=running` 时 `cat /proc/<pid>/status | grep CapEff` 位 14 (0x4000) 是否置位
- 缺位 = `.rc` 的 `capabilities` 行没加 `IPC_LOCK`。加上后重刷或 `adb push` + reboot
- 手动 `su root teeproxyd` 能跑而 init 起不来，99% 是这个

**composite disk `/tmp/pvm-composite: Permission denied`**
- 说明设备上跑的是**旧版 pvm-manage**（still hardcoding /tmp 路径）
- 更新二进制到本仓库 `prebuilts/vm/pvm-manage`（使用 `/data/teeproxy/composite`）
- SHA256 验证: `sha256sum /data/teeproxy/vm/pvm-manage` 对照 `prebuilts/CHECKSUMS.sha256`

**build 期间报 `ERROR 'unknown type kvm_device'`**
- teeproxyd.te 被错误地放在了 vendor policy 下（`device/<vendor>/sepolicy/` 或 `BOARD_SEPOLICY_DIRS`）
- 移到 `system/sepolicy/private/teeproxyd.te` —— `kvm_device` 是系统私有类型，vendor 侧看不见
- 同时 `file_contexts.fragment` 要加到 `system/sepolicy/private/file_contexts`，**不是** vendor 的 file_contexts

---

## 联系方式

集成过程中遇到问题，联系 OpenClaw TEE 安全团队或在主仓库提 issue。
