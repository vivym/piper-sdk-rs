# UDS Socket 权限问题分析报告

> **问题**：守护进程使用 `sudo` 运行（因 `detach_kernel_driver` 需要 root 权限），生成的 UDS `.sock` 文件普通用户无法连接
>
> **创建日期**：2024年
> **状态**：待实施

---

## 一、问题背景

### 1.1 问题描述

- **守护进程权限要求**：`gs_usb_daemon` 需要执行 `detach_kernel_driver()` 操作，该操作在 Linux/macOS 上需要 root 权限
- **运行方式**：守护进程使用 `sudo` 以 root 用户身份启动
- **Socket 文件权限**：当守护进程创建 UDS socket 文件（如 `/tmp/gs_usb_daemon.sock`）时，文件默认属于 root:root
- **权限问题**：普通用户客户端无法连接到 root 拥有的 socket 文件，报错 "Permission denied"

### 1.2 问题表现

```bash
# 守护进程以 root 运行
$ sudo ./target/release/gs_usb_daemon --uds /tmp/gs_usb_daemon.sock

# 普通用户客户端连接失败
$ ./client_app
Error: Permission denied (os error 13)
# 或
Error: connect: permission denied
```

### 1.3 当前实现

**代码位置**：`src/bin/gs_usb_daemon/daemon.rs:589-617`

```rust
fn init_sockets(&mut self) -> Result<(), DaemonError> {
    if let Some(ref uds_path) = self.config.uds_path {
        // 删除已存在的 socket 文件
        if std::path::Path::new(uds_path).exists() {
            std::fs::remove_file(uds_path)?;
        }

        // 创建 socket（当前未设置权限）
        let socket = std::os::unix::net::UnixDatagram::bind(uds_path)?;
        socket.set_nonblocking(true)?;
        self.socket_uds = Some(socket);
    }
    Ok(())
}
```

**问题**：创建 socket 后未设置文件权限和属组，导致：
- Socket 文件属主：`root:root`
- Socket 文件权限：默认 `srwx------` (0600) 或类似
- 结果：只有 root 用户可以连接

---

## 二、根本原因分析

### 2.1 权限模型

Unix Domain Socket 文件遵循标准文件权限模型：

1. **文件属主**：创建进程的有效用户 ID (EUID) 和组 ID (EGID)
2. **文件权限**：遵循 `umask` 设置（通常 022，即 0644），但 socket 文件可能有特殊处理
3. **访问控制**：Linux/Unix 检查：
   - 文件属主是否匹配
   - 文件属组是否匹配
   - 其他用户权限

### 2.2 为什么需要 root 权限

**`detach_kernel_driver()` 需要 root 权限的原因**：

- Linux：需要 `CAP_SYS_MODULE` 或 root 权限来卸载内核驱动
- macOS：需要管理员权限来操作 USB 设备驱动
- 这是 USB 子系统的安全限制，防止普通用户干扰系统驱动

### 2.3 权限冲突

| 需求 | 权限要求 | 冲突点 |
|------|---------|--------|
| USB 设备操作 | 需要 root | ✅ 满足（sudo 运行） |
| Socket 文件创建 | root 创建 → root:root 属主 | ❌ 普通用户无法访问 |
| 客户端连接 | 普通用户权限 | ❌ 无法连接 root 拥有的 socket |

---

## 三、解决方案分析

### 方案 A：代码中设置 Socket 文件权限和属组 ⭐ 推荐

**核心思路**：在创建 socket 后，立即修改文件权限和属组

#### 实现步骤

1. **创建用户组**（如 `gsusb_clients`）
2. **在代码中设置权限**：socket 创建后调用 `chown()` 和 `chmod()`

#### 优点

- ✅ **精确控制**：代码直接管理权限，不依赖外部配置
- ✅ **自包含**：不需要额外的启动脚本或 systemd 配置
- ✅ **灵活性**：可以通过配置参数指定组名和权限

#### 缺点

- ❌ **需要 root 权限**：必须保持守护进程以 root 运行
- ❌ **需要预先创建组**：用户组必须存在

#### 实现示例

```rust
use std::os::unix::fs::{chown, PermissionsExt};
use std::fs::set_permissions;
use nix::unistd::{Uid, Gid};

fn init_sockets(&mut self) -> Result<(), DaemonError> {
    if let Some(ref uds_path) = self.config.uds_path {
        // ... 删除已存在的 socket 文件 ...

        // 创建 socket
        let socket = std::os::unix::net::UnixDatagram::bind(uds_path)?;
        socket.set_nonblocking(true)?;

        // 设置 socket 文件权限和属组
        let group_name = self.config.socket_group.as_deref().unwrap_or("gsusb_clients");
        if let Ok(gid) = nix::unistd::Group::from_name(group_name)
            .and_then(|opt| opt.ok_or(()))
            .map(|g| g.gid)
        {
            // 设置属组：root:gsusb_clients
            chown(uds_path, Some(Uid::from_raw(0)), Some(gid))?;

            // 设置权限：srwxrw---- (0660)，属主和属组可读写
            let perms = std::fs::metadata(uds_path)?.permissions();
            let mut perms = perms.clone();
            perms.set_mode(0o660);
            set_permissions(uds_path, perms)?;

            eprintln!("[Socket] Set permissions: {}:{} 0660", "root", group_name);
        } else {
            eprintln!("[Socket] Warning: Group '{}' not found, using default permissions", group_name);
        }

        self.socket_uds = Some(socket);
    }
    Ok(())
}
```

#### 配置选项

```rust
#[derive(Parser, Debug)]
struct Args {
    // ... 其他选项 ...

    /// Socket 文件属组（允许连接的用户组）
    /// 默认: gsusb_clients
    #[arg(long, default_value = "gsusb_clients")]
    socket_group: String,

    /// Socket 文件权限（八进制，默认 0660）
    #[arg(long, default_value = "660")]
    socket_mode: String,
}
```

#### 部署步骤

```bash
# 1. 创建用户组
sudo groupadd gsusb_clients

# 2. 将需要访问守护进程的用户加入组
sudo usermod -aG gsusb_clients alice
sudo usermod -aG gsusb_clients bob

# 3. 用户重新登录或使用 newgrp 激活组权限
newgrp gsusb_clients

# 4. 启动守护进程（需要 root）
sudo ./target/release/gs_usb_daemon \
    --uds /tmp/gs_usb_daemon.sock \
    --socket-group gsusb_clients \
    --socket-mode 660

# 5. 验证权限
ls -l /tmp/gs_usb_daemon.sock
# 应显示: srw-rw---- 1 root gsusb_clients ...
```

---

### 方案 B：使用 systemd Socket 单元管理

**核心思路**：使用 systemd 的 socket 激活功能，由 systemd 创建 socket 并设置权限

#### 实现步骤

1. 创建 `.socket` 单元文件
2. 修改守护进程代码，从 systemd 继承 socket 文件描述符

#### 优点

- ✅ **系统级管理**：权限由 systemd 统一管理
- ✅ **标准化**：符合 Linux 发行版的标准做法
- ✅ **可靠性**：systemd 保证权限设置时机正确

#### 缺点

- ❌ **仅限 systemd 系统**：macOS 不支持 systemd
- ❌ **复杂度增加**：需要配置 systemd 单元
- ❌ **代码修改**：需要支持 socket 激活

#### 实现示例

**systemd socket 单元** (`/etc/systemd/system/gs_usb_daemon.socket`)：

```ini
[Unit]
Description=GS USB Daemon Socket
PartOf=gs_usb_daemon.service

[Socket]
ListenStream=/tmp/gs_usb_daemon.sock
SocketMode=0660
SocketUser=root
SocketGroup=gsusb_clients

[Install]
WantedBy=sockets.target
```

**systemd service 单元** (`/etc/systemd/system/gs_usb_daemon.service`)：

```ini
[Unit]
Description=GS USB Daemon
Requires=gs_usb_daemon.socket

[Service]
Type=notify
ExecStart=/usr/local/bin/gs_usb_daemon
Restart=always

[Install]
WantedBy=multi-user.target
```

**代码修改**：守护进程需要从 `LISTEN_FDS` 环境变量继承 socket

```rust
use std::env;

fn init_sockets(&mut self) -> Result<(), DaemonError> {
    // 检查是否由 systemd socket 激活
    if let Ok(fds) = env::var("LISTEN_FDS") {
        let fd_count: i32 = fds.parse().unwrap_or(0);
        if fd_count > 0 {
            // 从 systemd 继承 socket（fd = 3）
            let fd = std::os::unix::io::FromRawFd::from_raw_fd(3);
            let socket = unsafe { std::os::unix::net::UnixDatagram::from_raw_fd(fd) };
            self.socket_uds = Some(socket);
            return Ok(());
        }
    }

    // 否则，按原逻辑创建 socket
    // ...
}
```

---

### 方案 C：使用 ACL (Access Control Lists)

**核心思路**：为 socket 文件设置 ACL，允许特定用户访问

#### 实现步骤

1. 创建 socket 文件
2. 使用 `setfacl` 设置 ACL

#### 优点

- ✅ **精细控制**：可以为多个用户/组设置不同权限
- ✅ **灵活性**：不需要改变文件属组

#### 缺点

- ❌ **文件系统要求**：需要支持 ACL 的文件系统（如 ext4, xfs）
- ❌ **管理复杂**：ACL 管理不如简单的组权限直观
- ❌ **macOS 限制**：macOS ACL 实现不同

#### 实现示例

```rust
use std::process::Command;

fn init_sockets(&mut self) -> Result<(), DaemonError> {
    // ... 创建 socket ...

    // 设置 ACL：允许 gsusb_clients 组读写
    let output = Command::new("setfacl")
        .arg("-m")
        .arg("g:gsusb_clients:rw")
        .arg(uds_path)
        .output();

    // ...
}
```

---

### 方案 D：启动后脚本修改权限

**核心思路**：守护进程启动后，外部脚本修改 socket 文件权限

#### 实现步骤

```bash
#!/bin/bash
# start_daemon.sh

sudo ./gs_usb_daemon --uds /tmp/gs_usb_daemon.sock &
DAEMON_PID=$!

# 等待 socket 文件创建
while [ ! -S /tmp/gs_usb_daemon.sock ]; do
    sleep 0.1
done

# 修改权限
sudo chown root:gsusb_clients /tmp/gs_usb_daemon.sock
sudo chmod 0660 /tmp/gs_usb_daemon.sock

wait $DAEMON_PID
```

#### 优点

- ✅ **简单**：不需要修改代码
- ✅ **快速实施**：可以立即使用

#### 缺点

- ❌ **竞态条件**：客户端可能在权限设置前尝试连接
- ❌ **不可靠**：依赖外部脚本，容易出错
- ❌ **不推荐**：临时方案，不适合生产环境

---

## 四、方案对比

| 方案 | 实现难度 | 可靠性 | 跨平台 | 安全性 | 推荐度 |
|------|---------|--------|--------|--------|--------|
| **A: 代码设置权限** | ⭐⭐ | ⭐⭐⭐⭐⭐ | ⭐⭐⭐⭐⭐ | ⭐⭐⭐⭐ | ⭐⭐⭐⭐⭐ |
| **B: systemd Socket** | ⭐⭐⭐⭐ | ⭐⭐⭐⭐⭐ | ⭐ (仅 Linux) | ⭐⭐⭐⭐⭐ | ⭐⭐⭐⭐ |
| **C: ACL** | ⭐⭐⭐ | ⭐⭐⭐ | ⭐⭐⭐ | ⭐⭐⭐ | ⭐⭐⭐ |
| **D: 启动脚本** | ⭐ | ⭐⭐ | ⭐⭐⭐⭐⭐ | ⭐⭐⭐ | ⭐⭐ |

---

## 五、推荐方案与实施计划

### 推荐：方案 A（代码设置权限）+ 可选方案 B（systemd）

**理由**：
1. **跨平台支持**：方案 A 在 Linux 和 macOS 上都能工作
2. **自包含**：不需要外部依赖或复杂配置
3. **可靠性高**：权限在 socket 创建后立即设置，无竞态条件
4. **灵活性**：可以通过命令行参数配置

**实施步骤**：

#### 步骤 1：添加依赖

在 `Cargo.toml` 中添加：

```toml
[dependencies]
nix = "0.27"  # 用于 chown/chmod 操作
```

#### 步骤 2：修改代码

1. **添加配置选项**：
   - `--socket-group`：指定用户组（默认 `gsusb_clients`）
   - `--socket-mode`：指定权限（默认 `660`）

2. **修改 `init_sockets()` 方法**：
   - 创建 socket 后立即设置权限和属组
   - 添加错误处理和日志

#### 步骤 3：文档和部署指南

1. **创建用户组**的说明
2. **权限配置**的使用示例
3. **故障排查**指南

#### 步骤 4：测试

- ✅ 测试普通用户能否连接
- ✅ 测试非组用户能否被拒绝
- ✅ 测试守护进程重启后权限是否保持
- ✅ 测试组不存在时的降级行为

---

## 六、安全考虑

### 6.1 最小权限原则

- **推荐权限**：`0660`（`srw-rw----`）
  - 属主（root）：读写
  - 属组（gsusb_clients）：读写
  - 其他用户：无权限

- **不推荐**：`0666`（`srw-rw-rw-`）
  - 任何用户都可以连接，安全风险高

### 6.2 用户组管理

- **创建专用组**：`gsusb_clients` 仅用于此目的
- **审核组成员**：定期检查组内用户
- **最小成员**：只添加需要访问守护进程的用户

### 6.3 目录权限

Socket 文件所在目录也需要适当权限：

```bash
# /tmp 通常权限为 drwxrwxrwt，允许所有用户访问
# 如果使用 /var/run，需要设置目录权限
sudo chmod 755 /var/run
```

---

## 七、故障排查

### 7.1 常见错误

| 错误信息 | 原因 | 解决方案 |
|---------|------|---------|
| `Permission denied` | 用户不在 socket 组中 | `sudo usermod -aG gsusb_clients <user>` |
| `Group not found` | 用户组不存在 | `sudo groupadd gsusb_clients` |
| `No such file or directory` | Socket 文件未创建 | 检查守护进程是否运行 |
| `Connection refused` | Socket 文件权限正确但守护进程未监听 | 检查守护进程日志 |

### 7.2 验证命令

```bash
# 检查 socket 文件权限
ls -l /tmp/gs_usb_daemon.sock
# 应显示: srw-rw---- 1 root gsusb_clients ...

# 检查用户是否在组中
groups
# 应包含: gsusb_clients

# 测试连接
nc -U /tmp/gs_usb_daemon.sock
```

---

## 八、实施优先级

### 阶段 1：基础实施（推荐方案 A）

- [ ] 添加 `nix` 依赖
- [ ] 添加 `--socket-group` 和 `--socket-mode` 参数
- [ ] 实现 `init_sockets()` 中的权限设置
- [ ] 添加错误处理和日志
- [ ] 编写部署文档

### 阶段 2：可选增强（方案 B）

- [ ] 支持 systemd socket 激活（Linux）
- [ ] 创建 systemd 单元文件模板
- [ ] 更新文档

### 阶段 3：测试和验证

- [ ] 单元测试：权限设置功能
- [ ] 集成测试：普通用户连接
- [ ] 安全测试：非授权用户拒绝
- [ ] 跨平台测试：Linux 和 macOS

---

## 九、总结

**核心问题**：守护进程以 root 运行创建的 socket 文件，普通用户无法访问

**推荐解决方案**：
1. **主要方案**：在代码中创建 socket 后立即设置权限和属组（方案 A）
2. **可选方案**：在 Linux 系统上使用 systemd socket 单元（方案 B）

**关键要点**：
- ✅ 使用用户组管理访问权限（最小权限原则）
- ✅ Socket 权限设置为 `0660`（属主和属组可读写）
- ✅ 提供配置选项（组名和权限模式）
- ✅ 跨平台支持（Linux 和 macOS）

**实施难度**：⭐⭐（中等）
**预计时间**：2-4 小时（包括测试和文档）

---

**文档版本**：v1.0
**最后更新**：2024年
**状态**：待实施

