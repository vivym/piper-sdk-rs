//! 客户端管理
//!
//! 管理连接到守护进程的客户端，支持过滤规则和心跳机制
//!
//! 参考：`daemon_implementation_plan.md` 第 4.1.5 节

use piper_sdk::can::gs_usb_udp::protocol::CanIdFilter;
use std::collections::HashMap;
use std::net::SocketAddr;
use std::sync::atomic::{AtomicU32, Ordering};
use std::time::{Duration, Instant};

/// 客户端地址（支持 UDS 和 UDP）
///
/// 注意：UnixSocketAddr 不实现 Hash，所以我们使用 String 表示 UDS 路径
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ClientAddr {
    #[cfg(unix)]
    Unix(String), // UDS 路径（如 "/tmp/gs_usb_daemon.sock"）
    Udp(SocketAddr), // UDP 地址（如 "127.0.0.1:8888"）
}

/// 客户端信息
#[derive(Debug)]
pub struct Client {
    /// 客户端 ID
    pub id: u32,

    /// 客户端地址（用于 UDS/UDP 回复，用于 Hash）
    pub addr: ClientAddr,

    /// 最后活动时间
    pub last_active: Instant,

    /// CAN ID 过滤规则
    pub filters: Vec<CanIdFilter>,

    /// 连续发送错误计数（用于死客户端检测）
    /// 当连续丢包 1000 次（1 秒，1kHz）时，视为客户端已死，主动断开
    pub consecutive_errors: AtomicU32,

    /// 客户端发送频率降级级别（0=正常, 1=100Hz, 2=10Hz）
    /// 用于自适应降级机制
    /// 注意：此字段仅在 Unix 平台上使用（UDS 客户端）
    #[cfg_attr(not(unix), allow(dead_code))]
    pub send_frequency_level: AtomicU32,

    /// 客户端创建时间（便于调试和追踪）
    /// 可通过 client_age() 方法访问，用于监控和调试
    #[allow(dead_code)]
    pub created_at: Instant,
}

impl Client {
    /// 检查帧是否匹配客户端的过滤规则
    pub fn matches_filter(&self, can_id: u32) -> bool {
        // 如果没有过滤规则，接收所有帧
        if self.filters.is_empty() {
            return true;
        }

        // 检查是否匹配任一过滤规则
        self.filters.iter().any(|filter: &CanIdFilter| filter.matches(can_id))
    }

    /// 获取客户端存活时长（从创建到现在的时长）
    ///
    /// # 返回
    /// 客户端的存活时长，用于调试和追踪
    #[allow(dead_code)]
    pub fn client_age(&self) -> Duration {
        self.created_at.elapsed()
    }
}

/// 客户端错误类型
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ClientError {
    AlreadyExists,
    NotFound,
}

impl std::fmt::Display for ClientError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ClientError::AlreadyExists => write!(f, "Client already exists"),
            ClientError::NotFound => write!(f, "Client not found"),
        }
    }
}

impl std::error::Error for ClientError {}

/// 客户端管理器
pub struct ClientManager {
    clients: HashMap<u32, Client>,
    /// 客户端 ID 生成器（线程安全，单调递增）
    /// 从 1 开始（0 保留为无效 ID），溢出后从 1 重新开始
    next_id: AtomicU32,
    /// 客户端超时时间（默认 30 秒）
    timeout: Duration,
    /// Unix Domain Socket 地址映射（client_id -> UnixSocketAddr）
    /// 注意：由于 UnixSocketAddr 不实现 Hash，我们使用 client_id 作为键
    #[cfg(unix)]
    unix_addr_map: HashMap<u32, std::os::unix::net::SocketAddr>,
}

impl ClientManager {
    /// 创建新的客户端管理器
    pub fn new() -> Self {
        Self {
            clients: HashMap::new(),
            next_id: AtomicU32::new(1), // 从 1 开始（0 保留为无效 ID）
            timeout: Duration::from_secs(30),
            #[cfg(unix)]
            unix_addr_map: HashMap::new(),
        }
    }

    /// 创建新的客户端管理器（带自定义超时时间）
    pub fn with_timeout(timeout: Duration) -> Self {
        Self {
            clients: HashMap::new(),
            next_id: AtomicU32::new(1), // 从 1 开始（0 保留为无效 ID）
            timeout,
            #[cfg(unix)]
            unix_addr_map: HashMap::new(),
        }
    }

    /// 生成唯一 Client ID
    ///
    /// 策略：单调递增，溢出后从 1 重新开始（跳过 0）
    /// 冲突检测：如果 ID 已存在，继续递增直到找到空闲 ID
    fn generate_client_id(&self) -> u32 {
        loop {
            let id = self.next_id.fetch_add(1, Ordering::Relaxed);

            // 处理溢出：从 1 重新开始（0 保留为无效 ID）
            let id = if id == 0 { 1 } else { id };

            // 冲突检测：确保 ID 未被占用
            if !self.clients.contains_key(&id) {
                return id;
            }

            // 如果 ID 被占用（极罕见），继续尝试下一个
            // 注意：如果所有 ID 都被占用（42 亿客户端），会死循环
            // 实际场景中不可能，但可以添加超时保护
        }
    }

    /// 注册客户端（自动生成 ID）
    ///
    /// 返回生成的客户端 ID
    pub fn register_auto(
        &mut self,
        addr: ClientAddr,
        filters: Vec<CanIdFilter>,
    ) -> Result<u32, ClientError> {
        let id = self.generate_client_id();

        self.clients.insert(
            id,
            Client {
                id,
                addr,
                last_active: Instant::now(),
                filters,
                consecutive_errors: AtomicU32::new(0),
                send_frequency_level: AtomicU32::new(0), // 初始为正常频率
                created_at: Instant::now(),
            },
        );

        Ok(id)
    }

    /// 注册客户端（不带 Unix Socket 地址，用于 UDP 或其他情况）
    pub fn register(
        &mut self,
        id: u32,
        addr: ClientAddr,
        filters: Vec<CanIdFilter>,
    ) -> Result<(), ClientError> {
        if self.clients.contains_key(&id) {
            return Err(ClientError::AlreadyExists);
        }

        self.clients.insert(
            id,
            Client {
                id,
                addr,
                last_active: Instant::now(),
                filters,
                consecutive_errors: AtomicU32::new(0),
                send_frequency_level: AtomicU32::new(0), // 初始为正常频率
                created_at: Instant::now(),
            },
        );

        Ok(())
    }

    /// 注册客户端（带 Unix Socket 地址）
    ///
    /// # 参数
    /// - `unix_addr`: Unix Socket 地址（接收所有权，因为 SocketAddr 不实现 Copy/Clone）
    #[cfg(unix)]
    pub fn register_with_unix_addr(
        &mut self,
        id: u32,
        addr: ClientAddr,
        unix_addr: std::os::unix::net::SocketAddr,
        filters: Vec<CanIdFilter>,
    ) -> Result<(), ClientError> {
        if self.clients.contains_key(&id) {
            return Err(ClientError::AlreadyExists);
        }

        // 存储 Unix Socket 地址（用于 UDS send_to 操作）
        // 注意：我们同时使用 addr 中的路径字符串进行发送（作为备用）
        // 对于抽象地址，我们使用回退标识符

        // 由于 UnixSocketAddr 不实现 Copy/Clone，我们将其存储到 unix_addr_map 中
        // Client.unix_addr 字段保持为 None，实际地址通过 unix_addr_map 访问
        // 这避免了复制问题，同时保持了功能完整性
        self.unix_addr_map.insert(id, unix_addr);

        self.clients.insert(
            id,
            Client {
                id,
                addr,
                last_active: Instant::now(),
                filters,
                consecutive_errors: AtomicU32::new(0),
                send_frequency_level: AtomicU32::new(0), // 初始为正常频率
                created_at: Instant::now(),
            },
        );

        Ok(())
    }

    /// 注销客户端
    pub fn unregister(&mut self, id: u32) {
        self.clients.remove(&id);
        #[cfg(unix)]
        {
            self.unix_addr_map.remove(&id);
        }
    }

    /// 更新客户端活动时间（用于心跳）
    ///
    /// # 返回
    /// - `Ok(())` - 成功更新
    /// - `Err(ClientError::NotFound)` - 客户端不存在
    pub fn update_activity(&mut self, id: u32) -> Result<(), ClientError> {
        if let Some(client) = self.clients.get_mut(&id) {
            client.last_active = Instant::now();
            Ok(())
        } else {
            Err(ClientError::NotFound)
        }
    }

    /// 设置客户端过滤规则
    ///
    /// # 返回
    /// - `Ok(())` - 成功设置
    /// - `Err(ClientError::NotFound)` - 客户端不存在
    pub fn set_filters(&mut self, id: u32, filters: Vec<CanIdFilter>) -> Result<(), ClientError> {
        if let Some(client) = self.clients.get_mut(&id) {
            client.filters = filters;
            Ok(())
        } else {
            Err(ClientError::NotFound)
        }
    }

    /// 清理超时客户端
    pub fn cleanup_timeout(&mut self) {
        let now = Instant::now();
        let mut timeout_ids = Vec::new();

        // 找出超时的客户端 ID
        for (id, client) in &self.clients {
            if now.duration_since(client.last_active) >= self.timeout {
                timeout_ids.push(*id);
            }
        }

        // 移除超时的客户端
        for id in timeout_ids {
            self.clients.remove(&id);
            #[cfg(unix)]
            {
                self.unix_addr_map.remove(&id);
            }
        }
    }

    /// 获取所有客户端（用于广播）
    pub fn iter(&self) -> impl Iterator<Item = &Client> {
        self.clients.values()
    }

    /// 获取客户端数量
    pub fn count(&self) -> usize {
        self.clients.len()
    }

    /// 检查客户端是否存在
    #[cfg(test)]
    pub fn contains(&self, id: u32) -> bool {
        self.clients.contains_key(&id)
    }

    /// 为已注册的客户端设置 Unix Socket 地址（用于自动分配的 UDS 客户端）
    ///
    /// 此方法用于在自动分配 ID 后，为 UDS 客户端存储 Unix Socket 地址
    #[cfg(unix)]
    pub fn set_unix_addr(&mut self, id: u32, unix_addr: std::os::unix::net::SocketAddr) {
        self.unix_addr_map.insert(id, unix_addr);
    }
}

impl Default for ClientManager {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use piper_sdk::can::gs_usb_udp::protocol::CanIdFilter;

    #[test]
    fn test_client_register() {
        let mut manager = ClientManager::new();
        let addr = ClientAddr::Udp("127.0.0.1:8888".parse().unwrap());
        manager.register(1, addr, vec![]).unwrap();
        assert_eq!(manager.count(), 1);
        assert!(manager.contains(1));
    }

    #[test]
    fn test_client_duplicate_id() {
        let mut manager = ClientManager::new();
        let addr1 = ClientAddr::Udp("127.0.0.1:8888".parse().unwrap());
        let addr2 = ClientAddr::Udp("127.0.0.1:8889".parse().unwrap());
        manager.register(1, addr1, vec![]).unwrap();
        // 重复 ID 应该失败
        assert_eq!(
            manager.register(1, addr2, vec![]),
            Err(ClientError::AlreadyExists)
        );
    }

    #[test]
    fn test_client_unregister() {
        let mut manager = ClientManager::new();
        let addr = ClientAddr::Udp("127.0.0.1:8888".parse().unwrap());
        manager.register(1, addr, vec![]).unwrap();
        assert_eq!(manager.count(), 1);

        manager.unregister(1);
        assert_eq!(manager.count(), 0);
        assert!(!manager.contains(1));
    }

    #[test]
    fn test_client_filter_matching() {
        let mut manager = ClientManager::new();
        let addr = ClientAddr::Udp("127.0.0.1:8888".parse().unwrap());
        let filters = vec![CanIdFilter::new(0x100, 0x200)];
        manager.register(1, addr, filters).unwrap();

        let client = manager.iter().next().unwrap();
        assert!(client.matches_filter(0x150)); // 匹配
        assert!(!client.matches_filter(0x250)); // 不匹配
        assert!(client.matches_filter(0x100)); // 边界：匹配
        assert!(client.matches_filter(0x200)); // 边界：匹配
    }

    #[test]
    fn test_client_filter_empty() {
        let mut manager = ClientManager::new();
        let addr = ClientAddr::Udp("127.0.0.1:8888".parse().unwrap());
        manager.register(1, addr, vec![]).unwrap();

        let client = manager.iter().next().unwrap();
        // 没有过滤规则，应该接收所有帧
        assert!(client.matches_filter(0x100));
        assert!(client.matches_filter(0x200));
        assert!(client.matches_filter(0x300));
    }

    #[test]
    fn test_client_update_activity() {
        let mut manager = ClientManager::new();
        let addr = ClientAddr::Udp("127.0.0.1:8888".parse().unwrap());
        manager.register(1, addr, vec![]).unwrap();

        let initial_time = manager.iter().next().unwrap().last_active;

        // 等待一小段时间
        std::thread::sleep(Duration::from_millis(10));

        assert!(manager.update_activity(1).is_ok());

        let updated_time = manager.iter().next().unwrap().last_active;
        assert!(updated_time > initial_time);
    }

    #[test]
    fn test_client_update_activity_not_found() {
        let mut manager = ClientManager::new();
        // 尝试更新不存在的客户端
        assert_eq!(manager.update_activity(999), Err(ClientError::NotFound));
    }

    #[test]
    fn test_client_set_filters() {
        let mut manager = ClientManager::new();
        let addr = ClientAddr::Udp("127.0.0.1:8888".parse().unwrap());
        manager.register(1, addr, vec![]).unwrap();

        // 初始没有过滤规则
        let client = manager.iter().next().unwrap();
        assert!(client.matches_filter(0x100));

        // 设置过滤规则
        let new_filters = vec![CanIdFilter::new(0x300, 0x400)];
        assert!(manager.set_filters(1, new_filters).is_ok());

        let client = manager.iter().next().unwrap();
        assert!(!client.matches_filter(0x100)); // 不匹配
        assert!(client.matches_filter(0x350)); // 匹配
    }

    #[test]
    fn test_client_set_filters_not_found() {
        let mut manager = ClientManager::new();
        // 尝试为不存在的客户端设置过滤规则
        let filters = vec![CanIdFilter::new(0x100, 0x200)];
        assert_eq!(
            manager.set_filters(999, filters),
            Err(ClientError::NotFound)
        );
    }

    #[test]
    fn test_client_age() {
        let mut manager = ClientManager::new();
        let addr = ClientAddr::Udp("127.0.0.1:8888".parse().unwrap());
        manager.register(1, addr, vec![]).unwrap();

        let client = manager.iter().next().unwrap();
        let age = client.client_age();

        // 客户端应该是刚刚创建的，年龄应该很小（小于1秒）
        assert!(age < Duration::from_secs(1));
        assert!(age >= Duration::from_secs(0));
    }

    #[test]
    fn test_client_cleanup_timeout() {
        let mut manager = ClientManager::new();
        let addr1 = ClientAddr::Udp("127.0.0.1:8888".parse().unwrap());
        let addr2 = ClientAddr::Udp("127.0.0.1:8889".parse().unwrap());

        manager.register(1, addr1, vec![]).unwrap();
        manager.register(2, addr2, vec![]).unwrap();

        // 模拟 client 1 超时（手动设置一个很早的时间）
        // 注意：由于 last_active 是 Instant，我们无法直接修改
        // 这里我们测试 cleanup_timeout 的基本逻辑
        // 在实际使用中，超时检查会在后台线程中进行

        // 更新 client 2 的活动时间
        manager.update_activity(2).unwrap();

        // 由于我们无法直接修改 last_active，这里主要测试 cleanup 不会 panic
        manager.cleanup_timeout();

        // 至少 client 2 应该还在（因为刚刚更新了活动时间）
        assert!(manager.contains(2));
    }

    #[test]
    fn test_client_manager_iter() {
        let mut manager = ClientManager::new();
        let addr1 = ClientAddr::Udp("127.0.0.1:8888".parse().unwrap());
        let addr2 = ClientAddr::Udp("127.0.0.1:8889".parse().unwrap());

        manager.register(1, addr1, vec![]).unwrap();
        manager.register(2, addr2, vec![]).unwrap();

        let count: usize = manager.iter().count();
        assert_eq!(count, 2);
    }

    #[test]
    fn test_register_auto() {
        let mut manager = ClientManager::new();
        let addr = ClientAddr::Udp("127.0.0.1:8888".parse().unwrap());

        // 测试自动分配
        let id1 = manager.register_auto(addr.clone(), vec![]).unwrap();
        assert!(id1 > 0, "Auto-assigned ID should be > 0");

        // 测试多个客户端自动分配不同 ID
        let addr2 = ClientAddr::Udp("127.0.0.1:8889".parse().unwrap());
        let id2 = manager.register_auto(addr2, vec![]).unwrap();
        assert_ne!(id1, id2, "Auto-assigned IDs should be different");

        // 验证客户端存在
        assert!(manager.contains(id1));
        assert!(manager.contains(id2));
    }

    #[test]
    fn test_register_auto_with_filters() {
        let mut manager = ClientManager::new();
        let addr = ClientAddr::Udp("127.0.0.1:8888".parse().unwrap());
        let filters = vec![CanIdFilter::new(0x100, 0x200)];

        let id = manager.register_auto(addr, filters.clone()).unwrap();

        let client = manager.iter().find(|c| c.id == id).unwrap();
        assert_eq!(client.filters.len(), 1);
        assert_eq!(client.filters[0].min_id, 0x100);
        assert_eq!(client.filters[0].max_id, 0x200);
    }

    #[test]
    fn test_auto_and_manual_id_coexistence() {
        let mut manager = ClientManager::new();
        let addr1 = ClientAddr::Udp("127.0.0.1:8888".parse().unwrap());
        let addr2 = ClientAddr::Udp("127.0.0.1:8889".parse().unwrap());
        let addr3 = ClientAddr::Udp("127.0.0.1:8890".parse().unwrap());

        // 自动分配
        let auto_id = manager.register_auto(addr1, vec![]).unwrap();

        // 手动指定（使用自动分配的 ID，应该冲突）
        assert_eq!(
            manager.register(auto_id, addr2, vec![]),
            Err(ClientError::AlreadyExists)
        );

        // 手动指定（使用不同的 ID，应该成功）
        manager.register(9999, addr3, vec![]).unwrap();

        assert_eq!(manager.count(), 2);
    }

    #[test]
    fn test_generate_client_id_uniqueness() {
        // 测试自动分配 ID 的唯一性
        let mut manager = ClientManager::new();
        let mut ids = std::collections::HashSet::new();

        // 生成多个 ID，验证唯一性
        for i in 0..100 {
            let addr = ClientAddr::Udp(format!("127.0.0.1:{}", 8000 + i).parse().unwrap());
            let id = manager.register_auto(addr, vec![]).unwrap();

            assert!(ids.insert(id), "Generated ID {} should be unique", id);
        }

        assert_eq!(ids.len(), 100);
    }
}
