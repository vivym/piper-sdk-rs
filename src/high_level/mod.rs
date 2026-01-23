//! Piper Robot 高层 API
//!
//! 提供类型安全、易于使用的机器人控制接口。
//!
//! # 架构设计
//!
//! 本模块实现了工业级的高层 API，具有以下特性：
//!
//! - **Type State Pattern**: 编译期状态安全
//! - **读写分离**: Commander/Observer 模式
//! - **无锁热路径**: 高频控制优化（> 1kHz）
//! - **强类型单位**: 防止单位混淆
//! - **安全重置**: 智能时间跳变处理
//!
//! # 模块组织
//!
//! - `types`: 基础类型系统（单位、关节、错误）
//! - `client`: 客户端接口（Commander、Observer）
//! - `state`: Type State 状态机
//! - `control`: 控制器和轨迹规划
//!
//! # 设计文档
//!
//! 详见 `docs/v0/high-level-api/rust_high_level_api_design_v3.2_final.md`

pub mod client;
pub mod control;
pub mod state;
pub mod types;
// pub mod state;
// pub mod control;

// 重新导出常用类型
pub use client::*;
pub use types::*;
