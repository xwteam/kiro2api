use std::sync::Arc;

use kiro2api::{cli, config, logcap, server};

/// 定下最终的凭据路径:CLI > 环境变量 > config.json > 内置默认
/// (后三层已由 `Config::load` 合并完毕,此处只补最高的一层)。
///
/// 这条路径决定的不只是 credentials.json:用量统计、api_keys.json、余额缓存
/// 都按其父目录推断,容器里必须落在挂载卷内,否则重建即丢。
fn resolve_credentials_path(cfg: &mut config::Config, args: &cli::Cli) {
    let cli_credentials = args.credentials.as_deref().map(str::trim);
    if let Some(path) = cli_credentials.filter(|s| !s.is_empty()) {
        cfg.credentials_path = path.to_string();
    }
    // 空值(如 config.json 里写了 `"credentialsPath": ""`)会让数据目录退化成当前工作目录。
    // 兜底必须走**与默认层同一套**的就近解析:直接取 `Config::default()` 的裸相对名会绕过它,
    // 容器里就落到 /app/credentials.json(卷外),重建即丢——正是本次要堵的坑。
    if cfg.credentials_path.trim().is_empty() {
        cfg.credentials_path = config::default_credentials_beside_config(&args.config);
    }
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    // 先解析 CLI + 载入配置,再据 log_capacity 决定是否建日志捕获器——
    // 捕获器必须在 init_tracing 之前建好并作为可选层挂进 registry,才能拦截全部事件。
    let args = cli::parse();
    let mut cfg = config::Config::load(&args.config)?;
    resolve_credentials_path(&mut cfg, &args);

    // log_capacity > 0 时启用实时日志捕获(历史环形缓冲 + 广播),否则仅 stdout。
    let log_capture = if cfg.log_capacity > 0 {
        Some(Arc::new(logcap::LogCapture::new(cfg.log_capacity)))
    } else {
        None
    };
    logcap::init_tracing(log_capture.clone());

    server::serve(cfg, log_capture).await
}

#[cfg(test)]
mod tests {
    use super::resolve_credentials_path;
    use clap::Parser;
    use kiro2api::cli::Cli;
    use kiro2api::config::Config;

    #[test]
    fn cli_credentials_overrides_config_value() {
        let mut cfg = Config::default();
        let args = Cli::parse_from(["kiro2api", "--credentials", "/app/data/credentials.json"]);
        resolve_credentials_path(&mut cfg, &args);
        assert_eq!(cfg.credentials_path, "/app/data/credentials.json");
    }

    #[test]
    fn absent_cli_credentials_keeps_lower_layers() {
        let mut cfg = Config {
            credentials_path: "/from/config/creds.json".into(),
            ..Config::default()
        };
        let args = Cli::parse_from(["kiro2api"]);
        resolve_credentials_path(&mut cfg, &args);
        assert_eq!(cfg.credentials_path, "/from/config/creds.json");
    }

    /// 空白值不得把下层路径清空(否则数据目录会退化成当前工作目录)。
    #[test]
    fn blank_cli_credentials_is_ignored() {
        let mut cfg = Config {
            credentials_path: "/from/config/creds.json".into(),
            ..Config::default()
        };
        let args = Cli::parse_from(["kiro2api", "--credentials", "   "]);
        resolve_credentials_path(&mut cfg, &args);
        assert_eq!(cfg.credentials_path, "/from/config/creds.json");
    }

    /// config.json 里写了 credentialsPath 时 CLI 仍应压过它,否则裸机部署按文档传的
    /// `--credentials` 形同虚设。
    #[test]
    fn cli_beats_config_file_value() {
        let dir = std::env::temp_dir();
        let path = dir.join(format!("kiro2api_cli_override_{}.json", std::process::id()));
        std::fs::write(&path, r#"{"credentialsPath":"/from/config/creds.json"}"#).unwrap();
        let mut cfg = Config::load(path.to_str().unwrap()).unwrap();
        let args = Cli::parse_from(["kiro2api", "--credentials", "/app/data/credentials.json"]);
        resolve_credentials_path(&mut cfg, &args);
        assert_eq!(cfg.credentials_path, "/app/data/credentials.json");
        let _ = std::fs::remove_file(&path);
    }

    /// 空的 credentialsPath(如 `CREDENTIALS_PATH=`)不能让数据目录退化到当前工作目录。
    #[test]
    fn empty_credentials_path_falls_back_to_default() {
        let mut cfg = Config {
            credentials_path: String::new(),
            ..Config::default()
        };
        let args = Cli::parse_from(["kiro2api"]);
        resolve_credentials_path(&mut cfg, &args);
        assert_eq!(cfg.credentials_path, Config::default().credentials_path);
        assert!(!cfg.credentials_path.is_empty());
    }
}
