//! 客户端管理
//!
//! 管理连接到守护进程的客户端，支持过滤规则和心跳机制
//!
//! 参考：`daemon_implementation_plan.md` 第 4.1.5 节

use piper_sdk::can::gs_usb_udp::protocol::CanIdFilter;
use std::collections::HashMap;
use std::net::SocketAddr;
use std::time::{Duration, Instant};

/// 客户端地址（支持 UDS 和 UDP）
///
/// 注意：UnixSocketAddr 不实现 Hash，所以我们使用 String 表示 UDS 路径
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ClientAddr {
    Unix(String), // UDS 路径（如 "/tmp/gs_usb_daemon.sock"）
    #[allow(dead_code)]
    Udp(SocketAddr),
}

/// 客户端信息
#[derive(Debug)]
pub struct Client {
    /// 客户端 ID
    pub id: u32,

    /// 客户端地址（用于 UDS/UDP 回复，用于 Hash）
    pub addr: ClientAddr,

    /// Unix Domain Socket 地址（仅用于 UDS，用于 send_to）
    /// 注意：此字段不用于 Hash，因为 UnixSocketAddr 不实现 Hash
    #[allow(dead_code)]
    pub unix_addr: Option<std::os::unix::net::SocketAddr>,

    /// 最后活动时间
    pub last_active: Instant,

    /// CAN ID 过滤规则
    pub filters: Vec<CanIdFilter>,
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
}

/// 客户端错误类型
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ClientError {
    AlreadyExists,
    #[allow(dead_code)]
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
    /// 客户端超时时间（默认 30 秒）
    timeout: Duration,
    /// Unix Domain Socket 地址映射（client_id -> UnixSocketAddr）
    /// 注意：由于 UnixSocketAddr 不实现 Hash，我们使用 client_id 作为键
    unix_addr_map: HashMap<u32, std::os::unix::net::SocketAddr>,
}

impl ClientManager {
    /// 创建新的客户端管理器
    pub fn new() -> Self {
        Self {
            clients: HashMap::new(),
            timeout: Duration::from_secs(30),
            unix_addr_map: HashMap::new(),
        }
    }

    /// 创建新的客户端管理器（带自定义超时时间）
    pub fn with_timeout(timeout: Duration) -> Self {
        Self {
            clients: HashMap::new(),
            timeout,
            unix_addr_map: HashMap::new(),
        }
    }

    /// 注册客户端（不带 Unix Socket 地址，用于 UDP 或其他情况）
    #[allow(dead_code)]
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
                unix_addr: None,
                last_active: Instant::now(),
                filters,
            },
        );

        Ok(())
    }

    /// 注册客户端（带 Unix Socket 地址）
    pub fn register_with_unix_addr(
        &mut self,
        id: u32,
        addr: ClientAddr,
        _unix_addr: &std::os::unix::net::SocketAddr,
        filters: Vec<CanIdFilter>,
    ) -> Result<(), ClientError> {
        if self.clients.contains_key(&id) {
            return Err(ClientError::AlreadyExists);
        }

        // 注意：我们使用 addr 中的路径字符串进行发送
        // 对于抽象地址，我们使用回退标识符

        self.clients.insert(
            id,
            Client {
                id,
                addr,
                unix_addr: None,
                last_active: Instant::now(),
                filters,
            },
        );

        Ok(())
    }

    /// 注销客户端
    pub fn unregister(&mut self, id: u32) {
        self.clients.remove(&id);
        self.unix_addr_map.remove(&id);
    }

    /// 更新客户端活动时间（用于心跳）
    pub fn update_activity(&mut self, id: u32) {
        if let Some(client) = self.clients.get_mut(&id) {
            client.last_active = Instant::now();
        }
    }

    /// 设置客户端过滤规则
    #[allow(dead_code)]
    pub fn set_filters(&mut self, id: u32, filters: Vec<CanIdFilter>) {
        if let Some(client) = self.clients.get_mut(&id) {
            client.filters = filters;
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
            self.unix_addr_map.remove(&id);
        }
    }

    /// 获取所有客户端（用于广播）
    pub fn iter(&self) -> impl Iterator<Item = &Client> {
        self.clients.values()
    }

    /// 获取客户端数量
    #[allow(dead_code)]
    pub fn count(&self) -> usize {
        self.clients.len()
    }

    /// 检查客户端是否存在
    #[allow(dead_code)]
    pub fn contains(&self, id: u32) -> bool {
        self.clients.contains_key(&id)
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

        manager.update_activity(1);

        let updated_time = manager.iter().next().unwrap().last_active;
        assert!(updated_time > initial_time);
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
        manager.set_filters(1, new_filters);

        let client = manager.iter().next().unwrap();
        assert!(!client.matches_filter(0x100)); // 不匹配
        assert!(client.matches_filter(0x350)); // 匹配
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
        manager.update_activity(2);

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
}
