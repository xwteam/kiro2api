//! 动态模型清单子系统(FIX 1):按凭据向上游 `ListAvailableModels` 拉取该账号可用模型,
//! 带 TTL 的内存缓存(按凭据 id 键控),并把各账号缓存做并集/去重/排序供 admin 前端展示。
//!
//! 分层:
//! - [`model`]:上游 `ListAvailableModels` 响应的 serde 形状 + 归约成扁平的 [`model::ModelInfo`]。
//! - [`client`]:上游 `ListAvailableModels` 客户端。请求形态(POST、amz-json-1.0、
//!   `X-Amz-Target: AmazonCodeWhispererService.ListAvailableModels`、`origin=AI_EDITOR`、
//!   可选 `profileArn`、Bearer + 与数据面一致的伪装头)为照观测的 wire 事实;URL 组装/
//!   header 构造/响应解析/错误分类均为本项目自写。
//! - [`cache`]:[`cache::ModelsCache`](按凭据 id 键控,TTL 判定用注入的 `now_unix`),
//!   `get_union` 汇总所有账号缓存的并集(去重+排序),空时由调用方回落静态目录。
//!
//! 注入纪律:所有"当前时刻"由调用方以 `now_unix` 传入,模块内不直接读系统时钟(便于测试)。

pub mod cache;
pub mod client;
pub mod model;

pub use cache::ModelsCache;
pub use client::{ModelsError, fetch_available_models, fetch_available_models_fresh};
pub use model::{AvailableModelsResponse, ModelInfo};

/// 模型清单缓存有效期(秒):30 分钟。
pub const MODELS_TTL_SECS: u64 = 1_800;
