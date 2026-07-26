use clap::Parser;

/// kiro2api — 多协议中转,后端 Kiro。
#[derive(Debug, Parser)]
#[command(name = "kiro2api", version, about = "多协议中转,后端 Kiro")]
pub struct Cli {
    /// 配置文件路径
    #[arg(short = 'c', long, default_value = "config.json")]
    pub config: String,
    /// 凭据文件路径(未给出时不参与覆盖,由 `CREDENTIALS_PATH` / config.json / 内置默认决定)
    ///
    /// 刻意不设 `default_value`:有默认值就无法区分"用户显式指定"与"没给",
    /// 会把优先级更高的环境变量与 config.json 一起压掉。
    #[arg(long)]
    pub credentials: Option<String>,
}

pub fn parse() -> Cli {
    Cli::parse()
}

#[cfg(test)]
mod tests {
    use super::Cli;
    use clap::Parser;

    #[test]
    fn credentials_absent_is_none() {
        let c = Cli::parse_from(["kiro2api"]);
        assert_eq!(c.config, "config.json");
        // 未给 --credentials 时必须是 None,调用方才能据此让位给环境变量/config.json。
        assert!(c.credentials.is_none());
    }

    #[test]
    fn credentials_flag_is_captured() {
        let c = Cli::parse_from([
            "kiro2api",
            "-c",
            "/app/data/config.json",
            "--credentials",
            "/app/data/credentials.json",
        ]);
        assert_eq!(c.config, "/app/data/config.json");
        assert_eq!(c.credentials.as_deref(), Some("/app/data/credentials.json"));
    }
}
