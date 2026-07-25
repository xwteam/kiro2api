use clap::Parser;

/// kiro2api — 多协议中转,后端 Kiro。
#[derive(Debug, Parser)]
#[command(name = "kiro2api", version, about = "多协议中转,后端 Kiro")]
pub struct Cli {
    /// 配置文件路径
    #[arg(short = 'c', long, default_value = "config.json")]
    pub config: String,
    /// 凭据文件路径
    #[arg(long, default_value = "credentials.json")]
    pub credentials: String,
}

pub fn parse() -> Cli {
    Cli::parse()
}
