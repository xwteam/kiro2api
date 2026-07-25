use std::sync::Arc;

use kiro2api::{cli, config, logcap, server};

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    // 先解析 CLI + 载入配置,再据 log_capacity 决定是否建日志捕获器——
    // 捕获器必须在 init_tracing 之前建好并作为可选层挂进 registry,才能拦截全部事件。
    let args = cli::parse();
    let cfg = config::Config::load(&args.config)?;

    // log_capacity > 0 时启用实时日志捕获(历史环形缓冲 + 广播),否则仅 stdout。
    let log_capture = if cfg.log_capacity > 0 {
        Some(Arc::new(logcap::LogCapture::new(cfg.log_capacity)))
    } else {
        None
    };
    logcap::init_tracing(log_capture.clone());

    server::serve(cfg, log_capture).await
}
