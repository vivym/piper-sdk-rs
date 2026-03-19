//! Mock CAN 适配器（用于测试）
//!
//! 提供无硬件依赖的 CAN 适配器实现，用于 CI 测试和单元测试。

use crate::{CanAdapter, CanError, PiperFrame, RxAdapter, SplittableAdapter, TxAdapter};
use std::collections::VecDeque;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

/// Mock CAN 适配器（无硬件依赖）
///
/// # 用途
///
/// - CI 测试：无需真实硬件即可编译和运行测试
/// - 单元测试：模拟 CAN 通信场景
/// - 开发调试：在没有硬件的情况下开发和调试上层逻辑
///
/// # 行为特性
///
/// - **回环模式**：发送的帧会自动进入接收队列
/// - **零延迟**：所有操作立即完成（无实际硬件延迟）
/// - **线程安全**：内部使用 `Vec`，非线程安全（如需线程安全请使用外部同步）
///
/// # 示例
///
/// ```rust
/// use piper_can::{MockCanAdapter, CanAdapter, CanError, PiperFrame};
///
/// let mut adapter = MockCanAdapter::new();
///
/// // 注入测试帧
/// let frame = PiperFrame::new_standard(0x123, &[1, 2, 3, 4]);
/// adapter.inject(frame.clone());
///
/// // 接收帧
/// let rx_frame = adapter.receive()?;
/// assert_eq!(rx_frame.id, 0x123);
/// # Ok::<(), CanError>(())
/// ```
pub struct MockCanAdapter {
    inner: Arc<Mutex<MockBusInner>>,
}

impl MockCanAdapter {
    /// 创建新的 Mock 适配器
    ///
    /// # 示例
    ///
    /// ```rust
    /// use piper_can::MockCanAdapter;
    ///
    /// let adapter = MockCanAdapter::new();
    /// ```
    pub fn new() -> Self {
        Self {
            inner: Arc::new(Mutex::new(MockBusInner::default())),
        }
    }

    /// 注入测试帧到接收队列
    ///
    /// # 参数
    ///
    /// - `frame`: 要注入的 CAN 帧
    ///
    /// # 示例
    ///
    /// ```rust
    /// use piper_can::{MockCanAdapter, PiperFrame};
    ///
    /// let mut adapter = MockCanAdapter::new();
    /// let frame = PiperFrame::new_standard(0x123, &[1, 2, 3]);
    /// adapter.inject(frame);
    /// ```
    pub fn inject(&mut self, frame: PiperFrame) {
        self.inner.lock().expect("mock bus poisoned").frames.push_back(frame);
    }

    /// 启用超时模式（用于测试超时逻辑）
    ///
    /// 启用后，前 N 次 `receive()` 调用会返回 `CanError::Timeout`。
    ///
    /// # 参数
    ///
    /// - `count`: 超时的次数
    ///
    /// # 示例
    ///
    /// ```rust
    /// use piper_can::{MockCanAdapter, CanAdapter, CanError};
    ///
    /// let mut adapter = MockCanAdapter::new();
    ///
    /// // 设置前 2 次接收超时
    /// adapter.set_timeout_mode(2);
    ///
    /// assert!(matches!(adapter.receive(), Err(CanError::Timeout)));
    /// assert!(matches!(adapter.receive(), Err(CanError::Timeout)));
    ///
    /// // 第 3 次会正常返回（如果没有帧，则超时）
    /// // adapter.inject(frame);
    /// // let _ = adapter.receive();
    /// ```
    pub fn set_timeout_mode(&mut self, count: usize) {
        let mut inner = self.inner.lock().expect("mock bus poisoned");
        inner.timeout_mode = true;
        inner.timeout_count = count;
    }

    /// 禁用超时模式
    pub fn clear_timeout_mode(&mut self) {
        let mut inner = self.inner.lock().expect("mock bus poisoned");
        inner.timeout_mode = false;
        inner.timeout_count = 0;
    }

    /// 获取队列中的帧数量
    ///
    /// # 示例
    ///
    /// ```rust
    /// use piper_can::{MockCanAdapter, PiperFrame};
    ///
    /// let mut adapter = MockCanAdapter::new();
    /// assert_eq!(adapter.len(), 0);
    ///
    /// adapter.inject(PiperFrame::new_standard(0x123, &[1]));
    /// assert_eq!(adapter.len(), 1);
    /// ```
    pub fn len(&self) -> usize {
        self.inner.lock().expect("mock bus poisoned").frames.len()
    }

    /// 检查队列是否为空
    ///
    /// # 示例
    ///
    /// ```rust
    /// use piper_can::MockCanAdapter;
    ///
    /// let adapter = MockCanAdapter::new();
    /// assert!(adapter.is_empty());
    /// ```
    pub fn is_empty(&self) -> bool {
        self.inner.lock().expect("mock bus poisoned").frames.is_empty()
    }

    /// 清空队列
    ///
    /// # 示例
    ///
    /// ```rust
    /// use piper_can::{MockCanAdapter, PiperFrame};
    ///
    /// let mut adapter = MockCanAdapter::new();
    /// adapter.inject(PiperFrame::new_standard(0x123, &[1]));
    /// adapter.clear();
    /// assert!(adapter.is_empty());
    /// ```
    pub fn clear(&mut self) {
        self.inner.lock().expect("mock bus poisoned").frames.clear();
    }
}

impl Default for MockCanAdapter {
    fn default() -> Self {
        Self::new()
    }
}

impl CanAdapter for MockCanAdapter {
    /// 发送帧（回环模式）
    ///
    /// 发送的帧会自动进入接收队列，模拟回环行为。
    ///
    /// # 示例
    ///
    /// ```rust
    /// use piper_can::{MockCanAdapter, CanAdapter, PiperFrame};
    ///
    /// let mut adapter = MockCanAdapter::new();
    /// let frame = PiperFrame::new_standard(0x123, &[1, 2, 3, 4]);
    ///
    /// adapter.send(frame.clone()).unwrap();
    ///
    /// // 接收回环的帧
    /// let rx_frame = adapter.receive().unwrap();
    /// assert_eq!(rx_frame.id, 0x123);
    /// ```
    fn send(&mut self, frame: PiperFrame) -> Result<(), CanError> {
        MockTxAdapter {
            inner: Arc::clone(&self.inner),
        }
        .send_until(frame, Instant::now() + Duration::from_secs(60))
    }

    /// 接收帧
    ///
    /// 从队列中取出一个帧（FIFO）。如果队列为空，返回 `CanError::Timeout`。
    ///
    /// # 示例
    ///
    /// ```rust
    /// use piper_can::{MockCanAdapter, CanAdapter, PiperFrame, CanError};
    ///
    /// let mut adapter = MockCanAdapter::new();
    ///
    /// // 队列为空时超时
    /// assert!(matches!(adapter.receive(), Err(CanError::Timeout)));
    ///
    /// // 注入帧后可以接收
    /// adapter.inject(PiperFrame::new_standard(0x123, &[1, 2, 3]));
    /// let frame = adapter.receive().unwrap();
    /// assert_eq!(frame.id, 0x123);
    /// ```
    fn receive(&mut self) -> Result<PiperFrame, CanError> {
        MockRxAdapter {
            inner: Arc::clone(&self.inner),
        }
        .receive()
    }

    /// 设置接收超时（Mock 实现：无操作）
    ///
    /// Mock 适配器不使用超时，所有操作立即完成。
    fn set_receive_timeout(&mut self, _timeout: Duration) {
        // Mock 实现：无操作
    }
}

impl SplittableAdapter for MockCanAdapter {
    type RxAdapter = MockRxAdapter;
    type TxAdapter = MockTxAdapter;

    fn split(self) -> Result<(Self::RxAdapter, Self::TxAdapter), CanError> {
        Ok((
            MockRxAdapter {
                inner: Arc::clone(&self.inner),
            },
            MockTxAdapter {
                inner: Arc::clone(&self.inner),
            },
        ))
    }
}

#[derive(Default)]
struct MockBusInner {
    frames: VecDeque<PiperFrame>,
    timeout_mode: bool,
    timeout_count: usize,
}

pub struct MockRxAdapter {
    inner: Arc<Mutex<MockBusInner>>,
}

impl RxAdapter for MockRxAdapter {
    fn receive(&mut self) -> Result<PiperFrame, CanError> {
        let mut inner = self.inner.lock().expect("mock bus poisoned");

        if inner.timeout_mode && inner.timeout_count > 0 {
            inner.timeout_count -= 1;
            return Err(CanError::Timeout);
        }

        inner.frames.pop_front().ok_or(CanError::Timeout)
    }
}

pub struct MockTxAdapter {
    inner: Arc<Mutex<MockBusInner>>,
}

impl TxAdapter for MockTxAdapter {
    fn send_until(&mut self, frame: PiperFrame, deadline: Instant) -> Result<(), CanError> {
        if deadline <= Instant::now() {
            return Err(CanError::Timeout);
        }
        self.inner.lock().expect("mock bus poisoned").frames.push_back(frame);
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_mock_adapter_new() {
        let adapter = MockCanAdapter::new();
        assert!(adapter.is_empty());
    }

    #[test]
    fn test_mock_adapter_default() {
        let adapter = MockCanAdapter::default();
        assert!(adapter.is_empty());
    }

    #[test]
    fn test_mock_adapter_inject() {
        let mut adapter = MockCanAdapter::new();
        let frame = PiperFrame::new_standard(0x123, &[1, 2, 3, 4]);

        adapter.inject(frame);

        assert_eq!(adapter.len(), 1);
        assert!(!adapter.is_empty());
    }

    #[test]
    fn test_mock_adapter_send_loopback() {
        let mut adapter = MockCanAdapter::new();
        let frame = PiperFrame::new_standard(0x456, &[5, 6, 7, 8]);

        adapter.send(frame).unwrap();

        assert_eq!(adapter.len(), 1);

        let rx_frame = adapter.receive().unwrap();
        assert_eq!(rx_frame.id, 0x456);
        assert_eq!(rx_frame.data[..4], [5, 6, 7, 8]);
    }

    #[test]
    fn test_mock_adapter_receive_timeout() {
        let mut adapter = MockCanAdapter::new();

        // 队列为空时应超时
        let result = adapter.receive();
        assert!(matches!(result, Err(CanError::Timeout)));
    }

    #[test]
    fn test_mock_adapter_receive_injected() {
        let mut adapter = MockCanAdapter::new();
        let frame = PiperFrame::new_standard(0x789, &[9, 10, 11, 12]);

        adapter.inject(frame);

        let rx_frame = adapter.receive().unwrap();
        assert_eq!(rx_frame.id, 0x789);
    }

    #[test]
    fn test_mock_adapter_fifo() {
        let mut adapter = MockCanAdapter::new();

        // 注入多个帧
        adapter.inject(PiperFrame::new_standard(0x100, &[1]));
        adapter.inject(PiperFrame::new_standard(0x200, &[2]));
        adapter.inject(PiperFrame::new_standard(0x300, &[3]));

        assert_eq!(adapter.len(), 3);

        // FIFO 顺序
        let frame1 = adapter.receive().unwrap();
        assert_eq!(frame1.id, 0x100);

        let frame2 = adapter.receive().unwrap();
        assert_eq!(frame2.id, 0x200);

        let frame3 = adapter.receive().unwrap();
        assert_eq!(frame3.id, 0x300);
    }

    #[test]
    fn test_mock_adapter_clear() {
        let mut adapter = MockCanAdapter::new();

        adapter.inject(PiperFrame::new_standard(0x123, &[1]));
        adapter.inject(PiperFrame::new_standard(0x456, &[2]));

        assert_eq!(adapter.len(), 2);

        adapter.clear();

        assert!(adapter.is_empty());
    }

    #[test]
    fn test_mock_adapter_timeout_mode() {
        let mut adapter = MockCanAdapter::new();

        // 设置前 2 次超时
        adapter.set_timeout_mode(2);

        assert!(matches!(adapter.receive(), Err(CanError::Timeout)));
        assert!(matches!(adapter.receive(), Err(CanError::Timeout)));

        // 注入帧
        adapter.inject(PiperFrame::new_standard(0x123, &[1]));

        // 第 3 次应该成功
        let frame = adapter.receive().unwrap();
        assert_eq!(frame.id, 0x123);
    }

    #[test]
    fn test_mock_adapter_clear_timeout_mode() {
        let mut adapter = MockCanAdapter::new();

        adapter.set_timeout_mode(5);
        adapter.clear_timeout_mode();

        // 注入帧
        adapter.inject(PiperFrame::new_standard(0x123, &[1]));

        // 应该立即成功（不再超时）
        let frame = adapter.receive().unwrap();
        assert_eq!(frame.id, 0x123);
    }

    #[test]
    fn test_mock_adapter_set_receive_timeout() {
        let mut adapter = MockCanAdapter::new();

        // Mock 实现：无操作，但不应 panic
        adapter.set_receive_timeout(Duration::from_secs(5));
    }

    #[test]
    fn test_mock_adapter_extended_frame() {
        let mut adapter = MockCanAdapter::new();
        let frame = PiperFrame::new_extended(0x12345678, &[1, 2, 3, 4]);

        adapter.send(frame).unwrap();

        let rx_frame = adapter.receive().unwrap();
        assert_eq!(rx_frame.id, 0x12345678);
        assert!(rx_frame.is_extended);
    }

    #[test]
    fn test_mock_adapter_split() {
        let adapter = MockCanAdapter::new();
        let (mut rx, mut tx) = adapter.split().unwrap();

        tx.send_until(
            PiperFrame::new_standard(0x123, &[1, 2, 3]),
            Instant::now() + Duration::from_millis(10),
        )
        .unwrap();
        let frame = rx.receive().unwrap();
        assert_eq!(frame.id, 0x123);
    }
}
