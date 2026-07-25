// 抑制若干风格/取舍类 clippy lint(较新版 clippy 更严格,与本项目取舍不符,且不影响正确性):
// - result_large_err:axum handler 的错误枚举天然较大,统一装箱会污染大量签名,得不偿失;
// - result_unit_err:少数内部函数用 `Result<_, ()>` 表达"成功/跳过"语义,已足够清晰;
// - unnecessary_sort_by / doc_lazy_continuation / doc_nested_refdefs:纯风格取舍。
#![allow(clippy::result_large_err)]
#![allow(clippy::result_unit_err)]
#![allow(clippy::unnecessary_sort_by)]
#![allow(clippy::doc_lazy_continuation)]
#![allow(clippy::doc_nested_refdefs)]

pub mod admin;
pub mod apikey;
pub mod balance;
pub mod cli;
pub mod config;
pub mod http;
pub mod kiro;
pub mod logcap;
pub mod models_cache;
pub mod protocol;
pub mod server;
pub mod stats;
pub mod user;
pub mod webui;
