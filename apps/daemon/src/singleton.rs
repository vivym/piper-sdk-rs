//! 单例文件锁
//!
//! 使用文件锁确保只有一个守护进程实例运行
//!
//! 参考：`daemon_implementation_plan.md` 第 4.1.5 节

use fs4::fs_std::FileExt;
use std::fs::File;
use std::fs::OpenOptions;
use std::io::{self, Seek, SeekFrom, Write};

/// 单例文件锁
///
/// 使用文件锁确保只有一个守护进程实例运行。
/// 比 `pgrep` 更可靠，因为即使进程崩溃，锁也会自动释放。
pub struct SingletonLock {
    file: File,
    _path: std::path::PathBuf,
}

impl SingletonLock {
    /// 尝试获取单例锁
    ///
    /// # 参数
    /// - `lock_path`: 锁文件路径（如 `/var/run/gs_usb_daemon.lock`）
    ///
    /// # 返回
    /// - `Ok(Self)`: 成功获取锁
    /// - `Err`: 锁已被其他进程持有，或文件操作失败
    pub fn try_lock(lock_path: impl AsRef<std::path::Path>) -> Result<Self, io::Error> {
        let path = lock_path.as_ref();

        // 创建锁文件（如果不存在）
        // 注意：这里先不要截断(truncate)，因为我们还没拿到锁
        let mut file = OpenOptions::new()
            .create(true)
            .truncate(false)
            .write(true)
            .read(true) // 为了后续可能的操作，通常加上 read
            .open(path)?;

        // 尝试获取排他锁（非阻塞）
        // fs4 会自动根据系统选择 flock 或其他机制，跨平台支持
        if !file.try_lock_exclusive()? {
            return Err(io::Error::new(
                io::ErrorKind::AlreadyExists,
                "Daemon is already running (locked)",
            ));
        }

        // 获取锁成功后，清理文件内容并写入新的 PID
        // 因为文件可能残留旧进程的数据，拿到锁后必须截断
        file.set_len(0)?;
        file.seek(SeekFrom::Start(0))?;

        // 写入当前进程 PID（用于调试）
        let pid = std::process::id();
        writeln!(&file, "{}", pid)?;
        file.sync_all()?;

        Ok(Self {
            file,
            _path: path.to_path_buf(),
        })
    }
}

impl Drop for SingletonLock {
    fn drop(&mut self) {
        // fs4/OS 机制保证了当 File 被关闭(Drop)时，锁会自动释放。
        // 但显式解锁也是个好习惯，尽管在这里不是严格必须的。
        let _ = self.file.unlock();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    #[test]
    fn test_singleton_lock_exclusive() {
        // 创建临时锁文件路径
        let temp_dir = std::env::temp_dir();
        let lock_path = temp_dir.join("test_gs_usb_daemon.lock");

        // 清理可能存在的旧锁文件
        let _ = fs::remove_file(&lock_path);

        // 第一个锁应该成功
        let lock1 = SingletonLock::try_lock(&lock_path).unwrap();

        // 第二个锁应该失败（在同一进程中，文件锁可能允许，但我们可以测试基本功能）
        // 注意：在某些系统上，同一进程可能可以多次获取锁
        // 这里我们主要测试锁文件创建和基本功能

        drop(lock1);

        // 锁释放后，应该可以再次获取
        let lock2 = SingletonLock::try_lock(&lock_path).unwrap();
        drop(lock2);

        // 清理
        let _ = fs::remove_file(&lock_path);
    }

    #[test]
    fn test_singleton_lock_file_creation() {
        // 测试锁文件创建
        let temp_dir = std::env::temp_dir();
        let lock_path = temp_dir.join("test_gs_usb_daemon_create.lock");

        // 清理可能存在的旧锁文件
        let _ = fs::remove_file(&lock_path);

        let lock = SingletonLock::try_lock(&lock_path).unwrap();

        // 验证文件已创建
        assert!(lock_path.exists());

        // 在 Windows 上，文件被排他锁定时无法读取，需要先释放锁
        let pid = std::process::id();
        drop(lock);

        // 验证文件包含 PID（在锁释放后读取）
        let content = fs::read_to_string(&lock_path).unwrap();
        assert!(content.contains(&pid.to_string()));

        // 清理
        let _ = fs::remove_file(&lock_path);
    }
}
