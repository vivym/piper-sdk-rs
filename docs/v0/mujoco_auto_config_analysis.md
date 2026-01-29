# MuJoCo 全平台自动配置方案分析报告

**日期**: 2025-01-29
**版本**: v1.0
**目标**: 实现Mac、Linux、Windows三个平台的MuJoCo全自动配置，无需用户手动设置环境变量

---

## 📋 执行摘要

### 当前问题

| 平台 | 自动下载 | 自动配置 | 用户负担 |
|------|----------|----------|----------|
| **Linux** | ✅ 支持 | ⚠️ 需设置环境变量 | 中 |
| **Windows** | ✅ 支持 | ⚠️ 需设置环境变量 | 中 |
| **macOS** | ❌ 不支持 | ❌ 完全手动 | 高 |

### 目标

| 平台 | 自动下载 | 自动配置 | 默认目录 | 环境变量 |
|------|----------|----------|----------|----------|
| **Linux** | ✅ | ✅ | `~/.local/lib/mujoco/` | 自动 |
| **Windows** | ✅ | ✅ | `%LOCALAPPDATA%\mujoco\` | 自动 |
| **macOS** | ✅ | ✅ | `~/Library/Frameworks/mujoco.framework` | 自动 |

### 推荐方案

**方案A**: 扩展 `piper-physics` crate 的 `build.rs` 实现自动配置

**优势**:
- ✅ 用户零配置（`cargo add piper-physics` 即可）
- ✅ 跨平台统一体验
- ✅ 与mujoco-rs解耦（不依赖其feature）
- ✅ 可控性强（完全自主实现）

**劣势**:
- ⚠️ 需要维护build.rs代码
- ⚠️ 首次编译会下载（增加时间）

**预计工作量**: 3-5天

---

## 1. 当前状况分析

### 1.1 mujoco-rs 的配置机制

根据 `installation.rst` 文档，mujoco-rs 支持三种配置方式：

#### 方式1: 自动下载（Linux/Windows）

```toml
[dependencies]
mujoco-rs = { version = "2.3", features = ["auto-download-mujoco"] }
```

**环境变量**:
```bash
export MUJOCO_DOWNLOAD_DIR=/path/to/download/dir/
```

**行为**:
- build.rs 检测 `MUJOCO_DOWNLOAD_DIR`
- 自动下载 MuJoCo 到指定目录
- 创建 `mujoco-x.y.z/` 子目录
- 解压共享库文件

#### 方式2: 手动配置（全平台）

**环境变量**:
```bash
export MUJOCO_DYNAMIC_LINK_DIR=/path/to/mujoco/lib/
export LD_LIBRARY_PATH=$LD_LIBRARY_PATH:$MUJOCO_DYNAMIC_LINK_DIR  # Linux
export DYLD_LIBRARY_PATH=$DYLD_LIBRARY_PATH:$MUJOCO_DYNAMIC_LINK_DIR  # macOS
```

#### 方式3: pkg-config（Linux/macOS）

如果MuJoCo已注册到pkg-config，无需设置环境变量。

### 1.2 macOS 的特殊挑战

**当前 macOS 手动配置步骤**（来自文档第118-133行）:

```bash
# 1. 下载 DMG
# 2. 打开 DMG 文件
# 3. 复制 mujoco.framework 到当前目录
cp -R /Volumes/mujoco/mujoco.framework .
# 4. 创建符号链接
ln -s mujoco.framework/Versions/Current/libmujoco.x.x.x.dylib libmujoco.dylib
# 5. 设置环境变量
export MUJOCO_DYNAMIC_LINK_DIR=$(realpath .)
# 6. 设置运行时路径
export DYLD_LIBRARY_PATH=$DYLD_LIBRARY_PATH:$(realpath .)
```

**问题**:
1. ❌ 没有自动下载
2. ❌ 需要手动挂载DMG
3. ❌ 需要手动复制framework
4. ❌ 需要手动创建符号链接
5. ❌ 需要手动设置环境变量
6. ❌ **quarantine限制**：从网络下载的DMG会被macOS标记为隔离状态

### 1.3 Quarantine 限制详解

**问题来源**:
- macOS下载的文件会有 `com.apple.quarantine` 扩展属性
- 这个属性会导致运行时安全警告
- Rust编译或运行可能受影响

**用户提供的解决方案**:
```bash
xattr -r -d com.apple.quarantine /path/to/mujoco.framework
```

**命令解析**:
- `xattr`: 修改扩展属性
- `-r`: 递归处理（包括子文件）
- `-d`: 删除属性
- `com.apple.quarantine`: quarantine属性名

---

## 2. 技术方案设计

### 2.1 整体架构

```
┌─────────────────────────────────────────────────────────────┐
│                  piper-physics build.rs                     │
│                  (自动配置脚本)                             │
└───────────────────┬─────────────────────────────────────────┘
                    │
        ┌───────────┴───────────┬───────────────┐
        │                       │               │
        ▼                       ▼               ▼
   ┌─────────┐           ┌───────────┐   ┌───────────┐
   │ Linux   │           │  Windows  │   │   macOS   │
   └────┬────┘           └─────┬─────┘   └─────┬─────┘
        │                      │              │
        │ 1. 检查已安装          │              │ 1. 检查已安装
        │ 2. 下载tar.gz         │              │ 2. 下载DMG
        │ 3. 解压到~/.local/lib │              │ 3. hdiutil挂载
        │ 4. 设置环境变量        │              │ 4. 复制framework
        │                      │              │ 5. 去quarantine
        │                      │              │ 6. 创建符号链接
        │                      │              │ 7. 设置环境变量
        ▼                      ▼              ▼
   ┌─────────────────────────────────────────────────┐
   │           cargo:rerun-if-env-changed=...        │
   │    (cargo build 触发重新检查)                   │
   └─────────────────────────────────────────────────┘
```

### 2.2 目录结构设计

#### Linux

```bash
~/.local/lib/mujoco/
├── mujoco-3.3.7/
│   ├── include/
│   │   └── mujoco/
│   │       ├── mjdata.h
│   │       ├── mjmodel.h
│   │       └── ...
│   └── lib/
│       ├── libmujoco.so.3.3.7
│       └── libmujoco.so -> libmujoco.so.3.3.7
└── current -> mujoco-3.3.7/
```

**环境变量**:
```bash
export MUJOCO_DYNAMIC_LINK_DIR=$HOME/.local/lib/mujoco/current/lib
export LD_LIBRARY_PATH=$LD_LIBRARY_PATH:$MUJOCO_DYNAMIC_LINK_DIR
```

#### Windows

```
%LOCALAPPDATA%\mujoco\
├── mujoco-3.3.7\
│   ├── include\
│   │   └── mujoco\
│   └── lib\
│       └── mujoco.lib
└── current\ -> mujoco-3.3.7\
```

**环境变量**:
```powershell
$env:MUJOCO_DYNAMIC_LINK_DIR="$env:LOCALAPPDATA\mujoco\current\lib"
```

**运行时**:
- 复制 `mujoco.dll` 到 target/debug 或 target/release
- 或添加到 PATH

#### macOS

```bash
~/Library/Frameworks/mujoco.framework/
├── Versions/
│   ├── A/
│   │   ├── Libraries/           (共享库)
│   │   │   ├── libmujoco.3.3.7.dylib
│   │   │   └── libmujoco.dylib -> libmujoco.3.3.7.dylib
│   │   └── include/
│   │       └── mujoco/
│   │           ├── mjdata.h
│   │           └── ...
│   └── Current -> A/            (符号链接)
└── mujoco.framework -> Versions/Current/ (符号链接)
```

**环境变量**:
```bash
export MUJOCO_DYNAMIC_LINK_DIR=$HOME/Library/Frameworks/mujoco.framework/Libraries
export DYLD_LIBRARY_PATH=$DYLD_LIBRARY_PATH:$MUJOCO_DYNAMIC_LINK_DIR
```

### 2.3 实现流程详解

#### 平台检测

```rust
// build.rs
fn main() {
    println!("cargo:rerun-if-env-changed=MUJOCO_AUTO_CONFIG");

    // 如果用户已手动设置 MUJOCO_DYNAMIC_LINK_DIR，跳过自动配置
    if env::var("MUJOCO_DYNAMIC_LINK_DIR").is_ok() {
        println!("cargo:warning=Using manually configured MUJOCO_DYNAMIC_LINK_DIR");
        return;
    }

    // 执行自动配置
    #[cfg(target_os = "linux")]
    configure_linux();

    #[cfg(target_os = "windows")]
    configure_windows();

    #[cfg(target_os = "macos")]
    configure_macos();
}
```

#### Linux 配置流程

```rust
fn configure_linux() {
    use std::fs;
    use std::path::PathBuf;
    use std::process::Command;

    // 1. 确定安装目录
    let base_dir = dirs::home_dir()
        .expect("无法确定用户主目录")
        .join(".local/lib/mujoco");

    let version_dir = base_dir.join("mujoco-3.3.7");
    let current_dir = base_dir.join("current");

    // 2. 检查是否已安装
    if version_dir.exists() {
        println!("cargo:warning=MuJoCo 3.3.7 already installed at {:?}", version_dir);
        // 更新 current 符号链接
        let _ = fs::remove_file(&current_dir);
        let _ = std::os::unix::fs::symlink(&version_dir, &current_dir);
    } else {
        // 3. 创建目录
        fs::create_dir_all(&base_dir).expect("无法创建MuJoCo安装目录");

        // 4. 下载
        let download_url = "https://github.com/google-deepmind/mujoco/releases/download/3.3.7/mujoco-3.3.7-linux-x86_64.tar.gz";
        let tar_path = base_dir.join("mujoco.tar.gz");

        println!("cargo:warning=Downloading MuJoCo from {}...", download_url);
        download_file(download_url, &tar_path);

        // 5. 解压
        println!("cargo:warning=Extracting MuJoCo...");
        extract_tarball(&tar_path, &base_dir);

        // 6. 创建 current 符号链接
        std::os::unix::fs::symlink(&version_dir, &current_dir)
            .expect("无法创建符号链接");
    }

    // 7. 设置环境变量（通过cargo:指令传递给编译器）
    let lib_dir = current_dir.join("lib");
    let lib_path = lib_dir.to_string_lossy();

    println!("cargo:rustc-link-search={}", lib_path);
    println!("cargo:rustc-link-lib=mujoco");
    println!("cargo:warning=MUJOCO_DYNAMIC_LINK_DIR={}", lib_path);
    println!("cargo:warning=请设置 LD_LIBRARY_PATH=$LD_LIBRARY_PATH:{}", lib_path);

    // 8. 生成环境变量设置脚本
    generate_env_script("linux", &lib_path);
}
```

#### Windows 配置流程

```rust
fn configure_windows() {
    use std::fs;
    use std::path::PathBuf;

    // 1. 确定安装目录
    let local_app_data = env::var("LOCALAPPDATA")
        .unwrap_or_else(|_| {
            dirs::home_dir()
                .expect("无法确定用户目录")
                .join("AppData")
                .join("Local")
                .to_string_lossy()
                .to_string()
        });

    let base_dir = PathBuf::from(local_app_data).join("mujoco");
    let version_dir = base_dir.join("mujoco-3.3.7");
    let current_dir = base_dir.join("current");

    // 2-6. 类似Linux的流程...
    // (下载、解压、创建junction等)

    // 7. 复制DLL到target目录
    let target_debug = PathBuf::from("target/debug");
    let target_release = PathBuf::from("target/release");

    if let Ok(src_dll) = version_dir.join("bin").join("mujoco.dll").canonicalize() {
        if target_debug.exists() {
            fs::copy(&src_dll, target_debug.join("mujoco.dll")).ok();
        }
        if target_release.exists() {
            fs::copy(&src_dll, target_release.join("mujoco.dll")).ok();
        }
    }

    // 8. 生成环境变量设置脚本
    generate_env_script("windows", &lib_path);
}
```

#### macOS 配置流程

```rust
fn configure_macos() {
    use std::fs;
    use std::path::PathBuf;
    use std::process::Command;

    // 1. 确定安装目录
    let home = dirs::home_dir().expect("无法确定用户主目录");
    let frameworks_dir = home.join("Library/Frameworks");
    let framework_path = frameworks_dir.join("mujoco.framework");

    // 2. 检查是否已安装
    if framework_path.exists() {
        println!("cargo:warning=MuJoCo framework already installed at {:?}", framework_path);
        setup_macos_env(&framework_path);
        return;
    }

    // 3. 下载DMG
    let download_url = "https://github.com/google-deepmind/mujoco/releases/download/3.3.7/mujoco-3.3.7-macos-universal.dmg";
    let dmg_path = frameworks_dir.join("mujoco.dmg");

    fs::create_dir_all(&frameworks_dir).expect("无法创建Frameworks目录");

    println!("cargo:warning=Downloading MuJoCo DMG from {}...", download_url);
    download_file(download_url, &dmg_path);

    // 4. 去除quarantine属性（挂载前）
    println!("cargo:warning=Removing quarantine attribute...");
    let status = Command::new("xattr")
        .args(["-d", "com.apple.quarantine"])
        .arg(&dmg_path)
        .status();

    if let Ok(status) = status {
        if !status.success() {
            println!("cargo:warning=Failed to remove quarantine (non-critical)");
        }
    }

    // 5. 挂载DMG
    println!("cargo:warning=Mounting DMG...");
    let mount_output = Command::new("hdiutil")
        .args(["attach", "-nobrowse", "-readonly", "-mountpoint"])
        .arg("/tmp/mujoco_mount")
        .arg(&dmg_path)
        .output()
        .expect("无法挂载DMG");

    if !mount_output.status.success() {
        panic!("挂载DMG失败: {}", String::from_utf8_lossy(&mount_output.stderr));
    }

    // 6. 复制framework
    println!("cargo:warning=Copying MuJoCo framework...");
    let src_framework = PathBuf::from("/tmp/mujoco_mount/mujoco.framework");

    let status = Command::new("cp")
        .arg("-R")
        .arg(&src_framework)
        .arg(&frameworks_dir)
        .status()
        .expect("无法复制framework");

    if !status.success() {
        // 卸载DMG
        let _ = Command::new("hdiutil")
            .args(["detach", "/tmp/mujoco_mount"])
            .status();

        panic!("复制framework失败");
    }

    // 7. 卸载DMG
    println!("cargo:warning=Unmounting DMG...");
    let _ = Command::new("hdiutil")
        .args(["detach", "/tmp/mujoco_mount"])
        .status();

    // 8. 删除DMG文件
    let _ = fs::remove_file(&dmg_path);

    // 9. 去除quarantine（递归处理framework内所有文件）
    println!("cargo:warning=Removing quarantine from framework...");
    let status = Command::new("xattr")
        .args(["-r", "-d", "com.apple.quarantine"])
        .arg(&framework_path)
        .status();

    if let Ok(status) = status {
        if !status.success() {
            println!("cargo:warning=Failed to remove quarantine (may cause runtime warnings)");
        }
    }

    // 10. 设置环境变量
    setup_macos_env(&framework_path);
}

fn setup_macos_env(framework_path: &PathBuf) {
    let libraries_dir = framework_path.join("Versions/A/Libraries");
    let lib_path = libraries_dir.to_string_lossy();

    println!("cargo:rustc-link-search={}", lib_path);
    println!("cargo:rustc-link-lib=mujoco");
    println!("cargo:warning=MUJOCO_DYNAMIC_LINK_DIR={}", lib_path);
    println!("cargo:warning=DYLD_LIBRARY_PATH=$DYLD_LIBRARY_PATH:{}", lib_path);

    // 生成环境变量脚本
    generate_env_script("macos", &lib_path);
}
```

### 2.4 辅助函数实现

#### 文件下载

```rust
fn download_file(url: &str, path: &PathBuf) {
    use std::io::Write;
    use std::time::Duration;

    let client = ureq::AgentBuilder::new()
        .timeout(Duration::from_secs(300))
        .build();

    let response = client.get(url)
        .call()
        .expect("下载MuJoCo失败");

    let mut file = fs::File::create(path)
        .expect("无法创建下载文件");

    let mut reader = response.into_reader();

    std::io::copy(&mut reader, &mut file)
        .expect("保存下载文件失败");
}
```

#### 解压（Linux）

```rust
fn extract_tarball(tar_path: &PathBuf, dest_dir: &PathBuf) {
    use flate2::read::GzDecoder;
    use tar::Archive;

    let file = fs::File::open(tar_path)
        .expect("无法打开tar.gz文件");

    let decoder = GzDecoder::new(file);
    let mut archive = Archive::new(decoder);

    archive.unpack(dest_dir)
        .expect("解压失败");
}
```

#### 解压（Windows）

```rust
fn extract_zip(zip_path: &PathBuf, dest_dir: &PathBuf) {
    use zip::ZipArchive;

    let file = fs::File::open(zip_path)
        .expect("无法打开zip文件");

    let mut archive = ZipArchive::new(file)
        .expect("读取zip失败");

    archive.extract(dest_dir)
        .expect("解压失败");
}
```

#### 环境变量脚本生成

```rust
fn generate_env_script(platform: &str, lib_path: &str) {
    let script_path = match platform {
        "linux" => PathBuf::from("setup_mujoco.sh"),
        "macos" => PathBuf::from("setup_mujoco.sh"),
        "windows" => PathBuf::from("setup_mujoco.ps1"),
        _ => return,
    };

    let content = match platform {
        "linux" => format!(
            r#"#!/bin/bash
# MuJoCo环境变量设置脚本
# 运行: source setup_mujoco.sh

export MUJOCO_DYNAMIC_LINK_DIR={}
export LD_LIBRARY_PATH=$LD_LIBRARY_PATH:$MUJOCO_DYNAMIC_LINK_DIR

echo "✓ MuJoCo环境变量已设置"
echo "  MUJOCO_DYNAMIC_LINK_DIR=$MUJOCO_DYNAMIC_LINK_DIR"
"#,
            lib_path
        ),
        "macos" => format!(
            r#"#!/bin/bash
# MuJoCo环境变量设置脚本
# 运行: source setup_mujoco.sh

export MUJOCO_DYNAMIC_LINK_DIR={}
export DYLD_LIBRARY_PATH=$DYLD_LIBRARY_PATH:$MUJOCO_DYNAMIC_LINK_DIR

echo "✓ MuJoCo环境变量已设置"
echo "  MUJOCO_DYNAMIC_LINK_DIR=$MUJOCO_DYNAMIC_LINK_DIR"
"#,
            lib_path
        ),
        "windows" => format!(
            r#"# MuJoCo环境变量设置脚本
# 运行: .\setup_mujoco.ps1

$env:MUJOCO_DYNAMIC_LINK_DIR="{}"

Write-Host "✓ MuJoCo环境变量已设置"
Write-Host "  MUJOCO_DYNAMIC_LINK_DIR=$env:MUJOCO_DYNAMIC_LINK_DIR"
"#,
            lib_path
        ),
        _ => return,
    };

    fs::write(&script_path, content)
        .expect("无法写入环境变量脚本");

    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mut perms = fs::metadata(&script_path)
            .unwrap()
            .permissions();
        perms.set_mode(0o755);
        fs::set_permissions(&script_path, perms)
            .unwrap();
    }

    println!("cargo:warning=已生成环境变量脚本: {:?}", script_path);
}
```

### 2.5 依赖项

```toml
[build-dependencies]
# HTTP下载
ureq = "2.9"

# 文件压缩
flate2 = "1.0"
tar = "0.4"
zip = "0.6"

# 目录处理
dirs = "5.0"

# 错误处理
thiserror = "1.0"
```

---

## 3. 实现细节

### 3.1 macOS DMG 处理详解

#### DMG结构

```
mujoco-3.3.7-macos-universal.dmg
├── mujoco.framework/
│   ├── Versions/
│   │   ├── A/
│   │   │   ├── Libraries/          (关键：共享库在这里)
│   │   │   │   ├── libmujoco.3.3.7.dylib
│   │   │   │   └── libmujoco.dylib -> libmujoco.3.3.7.dylib
│   │   │   ├── include/            (头文件)
│   │   │   └── mujoco              (可执行文件)
│   │   └── Current -> A/
│   └── mujoco.framework -> Versions/Current/
```

#### 关键点

1. **Libraries 目录**：
   - 不是 `lib/`，而是 `Libraries/`
   - 这是macOS framework的标准结构

2. **符号链接**：
   - `Versions/Current -> A/`
   - `mujoco.framework -> Versions/Current/`
   - `libmujoco.dylib -> libmujoco.3.3.7.dylib`

3. **动态库路径**：
   - 编译时需要：`-framework mujoco -F ~/Library/Frameworks`
   - 运行时需要：`DYLD_LIBRARY_PATH=~/Library/Frameworks/mujoco.framework/Libraries`

#### 完整实现代码

```rust
#[cfg(target_os = "macos")]
fn configure_macos() {
    use std::fs;
    use std::path::PathBuf;
    use std::process::Command;

    // 1. 确定安装目录
    let home = dirs::home_dir().expect("无法确定用户主目录");
    let frameworks_dir = home.join("Library/Frameworks");
    let framework_path = frameworks_dir.join("mujoco.framework");
    let mount_point = PathBuf::from("/tmp/mujoco_mount");
    let dmg_path = frameworks_dir.join("mujoco-3.3.7-macos-universal.dmg");

    // 2. 检查是否已安装
    if framework_path.join("Versions/A").exists() {
        println!("cargo:warning=MuJoCo framework already installed");
        setup_macos_linking(&framework_path);
        return;
    }

    // 3. 创建目录
    fs::create_dir_all(&frameworks_dir)
        .expect("无法创建Frameworks目录");

    // 4. 下载DMG
    let download_url = "https://github.com/google-deepmind/mujoco/releases/download/3.3.7/mujoco-3.3.7-macos-universal.dmg";
    println!("cargo:warning=Downloading MuJoCo from {}...", download_url);

    download_file_with_progress(download_url, &dmg_path);

    // 5. 去除DMG的quarantine
    println!("cargo:warning=Removing quarantine from DMG...");
    let _ = Command::new("xattr")
        .args(["-d", "com.apple.quarantine"])
        .arg(&dmg_path)
        .status();

    // 6. 挂载DMG
    println!("cargo:warning=Mounting DMG...");
    let attach_output = Command::new("hdiutil")
        .args([
            "attach",
            &dmg_path.to_string_lossy(),
            "-nobrowse",
            "-readonly",
            "-mountpoint",
            &mount_point.to_string_lossy()
        ])
        .output();

    match attach_output {
        Ok(output) if output.status.success() => {
            println!("cargo:warning=DMG mounted successfully");
        },
        Ok(output) => {
            let stderr = String::from_utf8_lossy(&output.stderr);
            panic!("挂载DMG失败: {}", stderr);
        },
        Err(e) => {
            panic!("无法执行hdiutil: {}", e);
        }
    }

    // 7. 复制framework
    println!("cargo:warning=Copying MuJoCo framework...");
    let src_framework = mount_point.join("mujoco.framework");

    let copy_output = Command::new("cp")
        .arg("-R")  // 递归复制
        .arg(&src_framework)
        .arg(&frameworks_dir)
        .output();

    match copy_output {
        Ok(output) if output.status.success() => {
            println!("cargo:warning=Framework copied successfully");
        },
        Ok(output) => {
            let stderr = String::from_utf8_lossy(&output.stderr);
            eprintln!("cargo:warning=复制framework失败: {}", stderr);
        },
        Err(e) => {
            eprintln!("cargo:warning=无法执行cp命令: {}", e);
        }
    }

    // 8. 卸载DMG
    println!("cargo:warning=Unmounting DMG...");
    let detach_output = Command::new("hdiutil")
        .args(["detach", &mount_point.to_string_lossy()])
        .output();

    match detach_output {
        Ok(output) if output.status.success() => {
            println!("cargo:warning=DMG unmounted successfully");
        },
        _ => {
            eprintln!("cargo:warning=卸载DMG失败（可能需要手动卸载）");
        }
    }

    // 9. 删除DMG文件
    let _ = fs::remove_file(&dmg_path);

    // 10. 去除framework的quarantine（递归）
    println!("cargo:warning=Removing quarantine from framework...");
    let xattr_output = Command::new("xattr")
        .args(["-r", "-d", "com.apple.quarantine"])
        .arg(&framework_path)
        .output();

    match xattr_output {
        Ok(output) if output.status.success() => {
            println!("cargo:warning=Quarantine removed successfully");
        },
        _ => {
            println!("cargo:warning=Failed to remove quarantine (may cause warnings)");
        }
    }

    // 11. 设置链接
    setup_macos_linking(&framework_path);
}

#[cfg(target_os = "macos")]
fn setup_macos_linking(framework_path: &PathBuf) {
    let libraries_dir = framework_path.join("Versions/A/Libraries");
    let include_dir = framework_path.join("Versions/A/include");
    let lib_path = libraries_dir.to_string_lossy();
    let framework_dir = framework_path.parent().unwrap().to_string_lossy();

    // 编译时链接
    println!("cargo:rustc-link-search=framework={}", framework_dir);
    println!("cargo:rustc-link-lib=framework=mujoco");

    // 环境变量
    println!("cargo:warning=MUJOCO_DYNAMIC_LINK_DIR={}", lib_path);
    println!("cargo:warning=DYLD_LIBRARY_PATH=$DYLD_LIBRARY_PATH:{}", lib_path);

    // 生成脚本
    generate_env_script("macos", &lib_path);

    println!("cargo:warning=✓ MuJoCo for macOS configured successfully!");
    println!("cargo:warning=  Framework: {}", framework_path);
    println!("cargo:warning=  请运行: source setup_mujoco.sh");
}
```

### 3.2 错误处理和恢复

#### 下载失败

```rust
fn download_file_with_progress(url: &str, path: &PathBuf) {
    use ureq::Agent;
    use std::time::Duration;

    let agent = Agent::builder()
        .timeout(Duration::from_secs(300))
        .user_agent(&format!("piper-physics/{}", env!("CARGO_PKG_VERSION")))
        .build();

    match agent.get(url).call() {
        Ok(response) => {
            let total_size = response.header("Content-Length")
                .and_then(|v| v.parse::<u64>().ok());

            let reader = response.into_reader();
            let mut file = fs::File::create(path)?;

            if let Some(size) = total_size {
                println!("cargo:warning=Downloading {} bytes...", size);
            }

            std::io::copy(&mut reader.take(100_000_000), &mut file)?;
        },
        Err(ureq::Error::Status(code, _)) => {
            panic!("下载失败，HTTP状态码: {}", code);
        },
        Err(e) => {
            panic!("下载失败: {}", e);
        }
    }
}
```

#### 清理临时文件

```rust
struct CleanupGuard {
    mount_point: PathBuf,
    dmg_path: PathBuf,
}

impl Drop for CleanupGuard {
    fn drop(&mut self) {
        // 卸载DMG
        let _ = Command::new("hdiutil")
            .args(["detach", &self.mount_point.to_string_lossy()])
            .status();

        // 删除DMG
        let _ = fs::remove_file(&self.dmg_path);
    }
}
```

### 3.3 版本管理

```rust
const MUJOCO_VERSION: &str = "3.3.7";
const MUJOCO_BASE_URL: &str = "https://github.com/google-deepmind/mujoco/releases/download";

fn get_download_url(platform: &str) -> String {
    match platform {
        "linux" => format!("{}/mujoco-{}-linux-x86_64.tar.gz",
            MUJOCO_BASE_URL, MUJOCO_VERSION),
        "macos" => format!("{}/mujoco-{}-macos-universal.dmg",
            MUJOCO_BASE_URL, MUJOCO_VERSION),
        "windows" => format!("{}/mujoco-{}-windows-x64.zip",
            MUJOCO_BASE_URL, MUJOCO_VERSION),
        _ => panic!("不支持的平台: {}", platform),
    }
}
```

---

## 4. 用户使用流程

### 4.1 零配置体验

```bash
# 用户只需添加依赖
$ cargo add piper-physics

# 首次编译时自动配置
$ cargo build
   Compiling piper-physics v0.0.3
   warning: Downloading MuJoCo from https://github.com/...
   warning: Extracting MuJoCo...
   warning: MuJoCo installed to ~/.local/lib/mujoco/
   warning: ✓ MuJoCo configured successfully!
   warning: 请运行: source setup_mujoco.sh
   Finished dev profile [unoptimized + debuginfo] target(s)
```

### 4.2 生成脚本使用

```bash
# Linux/macOS
$ source setup_mujoco.sh
✓ MuJoCo环境变量已设置
  MUJOCO_DYNAMIC_LINK_DIR=/home/user/.local/lib/mujoco/current/lib

# Windows PowerShell
PS> .\setup_mujoco.ps1
✓ MuJoCo环境变量已设置
  MUJOCO_DYNAMIC_LINK_DIR=C:\Users\User\AppData\Local\mujoco\current\lib
```

### 4.3 永久设置

#### Linux/macOS

添加到 `~/.bashrc` 或 `~/.zshrc`:

```bash
# MuJoCo Environment
[ -f ~/path/to/project/setup_mujoco.sh ] && source ~/path/to/project/setup_mujoco.sh
```

#### Windows

添加到 PowerShell Profile (`$PROFILE`):

```powershell
# MuJoCo Environment
if (Test-Path "path\to\setup_mujoco.ps1") {
    . path\to\setup_mujoco.ps1
}
```

---

## 5. 替代方案

### 方案B: 独立安装工具

创建一个独立的二进制工具 `piper-mujoco-installer`：

```bash
# 安装
$ cargo install piper-mujoco-installer

# 运行
$ piper-mujoco-installer install
✓ MuJoCo已安装到 ~/.local/lib/mujoco/
✓ 环境变量已写入 ~/.bashrc
```

**优势**:
- ✅ 与build.rs解耦
- ✅ 可以单独版本管理
- ✅ 更好的错误提示

**劣势**:
- ❌ 用户需要额外安装工具
- ❌ 多了一个依赖

### 方案C: 扩展mujoco-rs

向mujoco-rs提交PR，添加macOS自动下载支持。

**优势**:
- ✅ 社区受益
- ✅ 官方支持

**劣势**:
- ❌ 审核周期长
- ❌ 可能被拒绝
- ❌ 不受控

### 方案D: Docker容器

```bash
# 使用预配置的Docker镜像
$ docker run -it piper-sdk/mujoco:latest
```

**优势**:
- ✅ 完全隔离
- ✅ 跨平台一致

**劣势**:
- ❌ Docker学习曲线
- ❌ 性能开销
- ❌ 不适合开发

---

## 6. 工作量估算

| 任务 | 工作量 | 风险 |
|------|--------|------|
| **build.rs 框架搭建** | 0.5天 | 低 |
| **Linux 自动配置** | 0.5天 | 低 |
| **Windows 自动配置** | 1天 | 中 |
| **macOS DMG 处理** | 1-1.5天 | 高 |
| **环境变量脚本生成** | 0.5天 | 低 |
| **错误处理和恢复** | 1天 | 中 |
| **测试和验证** | 1天 | 中 |
| **文档编写** | 0.5天 | 低 |
| **总计** | **6-7天** | - |

### 风险项

1. **macOS DMG 挂载**
   - 风险：hdiutil权限问题
   - 缓解：提供手动配置的fallback

2. **quarantine 去除**
   - 风险：可能被Apple安全机制阻止
   - 缓解：提供清晰的错误提示

3. **网络下载**
   - 风险：GitHub下载失败或速度慢
   - 缓解：支持镜像源、断点续传

4. **权限问题**
   - 风险：写入~/Library需要权限
   - 缓解：提前检查权限

---

## 7. 推荐实施方案

### 阶段1: MVP (最小可行产品) - 3天

**目标**: 支持Linux和macOS基本功能

```rust
// build.rs (简化版)
fn main() {
    if env::var("MUJOCO_DYNAMIC_LINK_DIR").is_ok() {
        return; // 用户已手动配置
    }

    #[cfg(target_os = "linux")]
    auto_configure_linux();

    #[cfg(target_os = "macos")]
    auto_configure_macos();
}
```

**功能**:
- ✅ 自动下载MuJoCo
- ✅ 自动安装到默认目录
- ✅ 生成环境变量脚本
- ⚠️ 简单错误处理

### 阶段2: 完善 - 2-3天

**功能**:
- ✅ Windows支持
- ✅ 完善错误处理
- ✅ 进度显示
- ✅ 版本检查
- ✅ 缓存机制（避免重复下载）

### 阶段3: 优化 - 1-2天

**功能**:
- ✅ 离线安装支持
- ✅ 自定义安装路径
- ✅ 镜像源支持
- ✅ 更新检查

---

## 8. 测试计划

### 8.1 单元测试

```rust
#[cfg(test)]
mod tests {
    #[test]
    fn test_download_url_generation() {
        assert_eq!(get_download_url("linux"), "...");
        assert_eq!(get_download_url("macos"), "...");
    }

    #[test]
    fn test_default_paths() {
        let home = dirs::home_dir().unwrap();
        #[cfg(target_os = "macos")]
        assert_eq!(get_default_install_dir(), home.join("Library/Frameworks"));
    }
}
```

### 8.2 集成测试

```bash
# 测试真实下载和安装
$ cargo clean
$ MUJOCO_DYNAMIC_LINK_DIR="" cargo build
$ ls ~/.local/lib/mujoco/current/lib
$ ls ~/Library/Frameworks/mujoco.framework/Versions/A
```

### 8.3 跨平台测试

| 平台 | 测试项 | 状态 |
|------|--------|------|
| **Ubuntu 22.04** | 下载、解压、编译 | ⏳ |
| **macOS 13+ (Intel)** | DMG下载、挂载、quarantine去除 | ⏳ |
| **macOS 13+ (ARM)** | Universal binary运行 | ⏳ |
| **Windows 11** | 下载、解压、DLL复制 | ⏳ |

---

## 9. 文档计划

### 9.1 README 更新

```markdown
## Quick Start

piper-physics will automatically download and configure MuJoCo on first build.

### Prerequisites

- Linux: None (automatic)
- macOS: None (automatic)
- Windows: None (automatic)

### Installation

```bash
cargo add piper-physics
cargo build  # MuJoCo will be downloaded automatically
source setup_mujoco.sh  # Set environment variables
```

### Advanced Configuration

If you want to use your own MuJoCo installation:

```bash
export MUJOCO_DYNAMIC_LINK_DIR=/path/to/mujoco/lib
cargo build
```
```

### 9.2 环境变量说明文档

**docs/MUJOCO_SETUP.md**:

- 自动配置的工作原理
- 手动配置的方法
- 环境变量说明
- 故障排除

---

## 10. 最终推荐

### 推荐方案：方案A（build.rs）

**理由**:
1. ✅ **用户体验最佳** - 完全零配置
2. ✅ **跨平台统一** - 三个平台一致的体验
3. ✅ **维护可控** - 代码在自己的crate中
4. ✅ **灵活性强** - 可以根据需要定制

**实施建议**:
1. 分阶段实施（MVP → 完善 → 优化）
2. 保留手动配置的fallback路径
3. 充分的错误处理和用户提示
4. 详细的文档和测试

**预期效果**:
```bash
# 用户体验
$ cargo new my_robot --bin
$ cd my_robot
$ cargo add piper-physics
$ cargo build
# ✅ 自动下载和配置MuJoCo
$ ./target/debug/my_robot
# ✅ 直接运行，无需手动配置
```

---

## 11. 参考资料

### 官方文档

- [MuJoCo-rs Installation Guide](https://mujoco-rs.readthedocs.io/en/latest/installation.html)
- [MuJoCo Releases](https://github.com/google-deepmind/mujoco/releases)
- [MuJoCo BUILD.md](https://github.com/google-deepmind/mujoco/blob/main/BUILD.md)

### 相关工具

- [hdiutil man page](https://ss64.com/osx/hdiutil.html) - macOS DMG挂载工具
- [xattr man page](https://ss64.com/osx/xattr.html) - macOS扩展属性工具
- [Cargo Build Script](https://doc.rust-lang.org/cargo/reference/build-scripts.html)

### 示例项目

- [mujoco-rs build.rs](https://github.com/davidhozic/mujoco-rs/blob/main/build.rs)
- [PyMuJoCo macOS setup](https://github.com/openai/mujoco-py)

---

**报告版本**: v1.0
**最后更新**: 2025-01-29
**作者**: Claude (Anthropic)
**项目**: Piper SDK - piper-physics crate
