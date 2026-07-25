//! 余额子系统(Phase 4):按凭据查询上游剩余额度(getUsageLimits)+ 带 5 分钟 TTL 的
//! 内存缓存(去抖异步落盘 `kiro_balance_cache.json`)。
//!
//! 分层:
//! - [`client`]:上游 `getUsageLimits` 客户端。请求形态(GET、host `q.{region}.amazonaws.com`、
//!   `origin=AI_EDITOR&resourceType=AGENTIC_REQUEST`、可选 `profileArn`、Bearer + 伪装头)为照
//!   观测的 wire 事实;请求编排/响应解析/额度累加口径均为本项目自写。
//! - [`model`]:上游响应的 serde 形状 + "限额/已用/剩余"的累加口径(基础额度 + 激活的 bonus
//!   + 激活的 free trial)。
//! - [`cache`]:`BalanceCache`(按凭据 id 键控,5 分钟 TTL,原子写 + 约 5s 去抖落盘,复用
//!   `stats::persist`)。
//!
//! 注入纪律:所有"当前时刻"由调用方以 `now_unix` 传入,模块内不直接读系统时钟(便于测试)。

pub mod cache;
pub mod client;
pub mod model;

pub use cache::{BalanceCache, BalanceSnapshot};
pub use client::{BalanceError, fetch_usage_limits, fetch_usage_limits_fresh};
pub use model::UsageLimitsResponse;

/// 缓存有效期(秒):5 分钟。
pub const BALANCE_TTL_SECS: u64 = 300;

/// 缓存文件名(落在数据目录,与 stats 同目录)。
pub const BALANCE_CACHE_FILE: &str = "kiro_balance_cache.json";
