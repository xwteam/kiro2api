use std::sync::Arc;

use parking_lot::RwLock;
use serde::Deserialize;

/// 运行期配置。字段命名与含义为本项目自有(不沿用任何上游命名)。
#[derive(Debug, Clone, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct Config {
    pub host: String,
    pub port: u16,
    pub region: String,
    /// 主 API key(为空则启动时校验失败,由上层处理)。
    pub api_key: Option<String>,
    /// 管理端 key;非空时启用 admin/user API+UI(P0 仅占位)。
    pub admin_api_key: Option<String>,
    /// Kiro 账号凭据文件路径(默认相对路径,部署时按需覆盖)。
    pub credentials_path: String,
    /// 伪装 UA 里的 Kiro 版本号(契约 §3)。
    pub kiro_version: String,
    /// 伪装 UA 里的 `os/{system_version}`(契约 §3)。
    pub system_version: String,
    /// 伪装 UA 里的 `md/nodejs#{node_version}`(契约 §3)。
    pub node_version: String,
    /// 每凭据每分钟最大请求数;0 = 无限(默认,兼容既有行为)。
    pub max_rpm_per_credential: u32,
    /// 负载均衡模式:"priority"(默认,等权轮询)或 "balanced"(按权重轮询)。
    pub load_balancing_mode: String,
    /// 实时日志历史环形缓冲容量(条数)。>0 时启用日志捕获(admin 日志端点回放/流式);
    /// 0 = 关闭捕获(不建缓冲、不挂捕获层,admin 日志端点返回 503)。默认 5000
    /// (对齐 gemini2api 的"全量连续日志"体验:每请求一条 INFO + 关键生命周期事件,容量要够大
    /// 才能在实时日志页看到一段连续的活动记录,而非只剩最近十几条)。
    pub log_capacity: usize,
    /// 本配置文件的磁盘路径(不从 JSON 读入,由 `load()` 注入),供运行期改写落盘定位。
    #[serde(skip)]
    pub config_path: String,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            host: "127.0.0.1".into(),
            port: 8080,
            region: "us-east-1".into(),
            api_key: None,
            admin_api_key: None,
            credentials_path: "credentials.json".into(),
            kiro_version: "0.11.107".into(),
            system_version: "win32#10.0.22631".into(),
            node_version: "22.22.0".into(),
            max_rpm_per_credential: 0,
            load_balancing_mode: "priority".into(),
            log_capacity: 5000,
            config_path: "config.json".into(),
        }
    }
}

impl Config {
    pub fn load(path: &str) -> anyhow::Result<Config> {
        let mut cfg: Config = match std::fs::read_to_string(path) {
            Ok(s) => serde_json::from_str(&s)?,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Config::default(),
            Err(e) => return Err(e.into()),
        };
        cfg.apply_env_overrides();
        // 记录本配置文件路径,供运行期改写(auth-keys / load-balancing)原子落盘定位。
        cfg.config_path = path.to_string();
        Ok(cfg)
    }

    fn apply_env_overrides(&mut self) {
        if let Ok(v) = std::env::var("HOST") {
            self.host = v;
        }
        if let Ok(v) = std::env::var("PORT")
            && let Ok(p) = v.parse()
        {
            self.port = p;
        }
        if let Ok(v) = std::env::var("REGION") {
            self.region = v;
        }
        if let Ok(v) = std::env::var("API_KEY") {
            self.api_key = Some(v);
        }
        if let Ok(v) = std::env::var("ADMIN_API_KEY") {
            self.admin_api_key = Some(v);
        }
        if let Ok(v) = std::env::var("CREDENTIALS_PATH") {
            self.credentials_path = v;
        }
        if let Ok(v) = std::env::var("MAX_RPM_PER_CREDENTIAL")
            && let Ok(n) = v.parse()
        {
            self.max_rpm_per_credential = n;
        }
        if let Ok(v) = std::env::var("LOAD_BALANCING_MODE") {
            self.load_balancing_mode = v;
        }
    }
}

/// 运行期可变配置存储:仅承载**可在运行时改写**的字段(auth key / RPM / 负载均衡模式),
/// 与不可变的 `Arc<Config>`(host/port/region/版本号等)分离,改动面最小。
///
/// 读多写少 → `Arc<RwLock<_>>`。auth 闸每请求读当前 key(轮换即时生效);
/// admin 设置端点写入后立即持久化回 `config.json`(tmp+rename 原子替换)。
#[derive(Debug, Clone)]
pub struct RuntimeConfig {
    /// 主 API key(可轮换;`None`/空 = 该来源开放模式)。
    pub api_key: Option<String>,
    /// 管理端 key / 密码(可轮换)。
    pub admin_api_key: Option<String>,
    /// 每凭据每分钟最大请求数;0 = 无限。
    pub max_rpm_per_credential: u32,
    /// 负载均衡模式:"priority" 或 "balanced"。
    pub load_balancing_mode: String,
    /// `config.json` 磁盘路径,持久化写回时定位。
    pub config_path: String,
}

impl RuntimeConfig {
    /// 从启动期 `Config` 抽取可变字段建立运行期视图。
    pub fn from_config(cfg: &Config) -> Self {
        Self {
            api_key: cfg.api_key.clone(),
            admin_api_key: cfg.admin_api_key.clone(),
            max_rpm_per_credential: cfg.max_rpm_per_credential,
            load_balancing_mode: cfg.load_balancing_mode.clone(),
            config_path: cfg.config_path.clone(),
        }
    }

    /// 把当前可变字段原子写回 `config.json`:读原文件为 JSON(不存在则空对象)→
    /// 仅更新受管字段(保留其它字段/格式尽量不动)→ 写临时文件 → `rename` 原子替换。
    ///
    /// 原子性:先写同目录下 `*.tmp.<pid>`,再 `fs::rename`(同分区 rename 原子),
    /// 避免半写导致的配置损坏;调用方失败时应记录但不必回滚(运行期已生效)。
    pub fn persist(&self) -> anyhow::Result<()> {
        use std::path::Path;
        let path = Path::new(&self.config_path);
        let mut json: serde_json::Value = match std::fs::read_to_string(path) {
            Ok(s) if !s.trim().is_empty() => serde_json::from_str(&s)?,
            _ => serde_json::Value::Object(serde_json::Map::new()),
        };
        if !json.is_object() {
            json = serde_json::Value::Object(serde_json::Map::new());
        }
        let obj = json.as_object_mut().expect("json is object");
        // camelCase 键名与 Config 的 serde(rename_all = "camelCase") 反序列化保持一致。
        match &self.api_key {
            Some(k) => obj.insert("apiKey".into(), serde_json::Value::String(k.clone())),
            None => obj.remove("apiKey"),
        };
        match &self.admin_api_key {
            Some(k) => obj.insert("adminApiKey".into(), serde_json::Value::String(k.clone())),
            None => obj.remove("adminApiKey"),
        };
        obj.insert(
            "maxRpmPerCredential".into(),
            serde_json::Value::Number(self.max_rpm_per_credential.into()),
        );
        obj.insert(
            "loadBalancingMode".into(),
            serde_json::Value::String(self.load_balancing_mode.clone()),
        );

        let output = serde_json::to_string_pretty(&json)?;
        let parent = path.parent().filter(|p| !p.as_os_str().is_empty());
        let tmp = match parent {
            Some(dir) => dir.join(format!(
                ".{}.tmp.{}",
                path.file_name()
                    .and_then(|n| n.to_str())
                    .unwrap_or("config.json"),
                std::process::id()
            )),
            None => std::path::PathBuf::from(format!("config.json.tmp.{}", std::process::id())),
        };
        std::fs::write(&tmp, output)?;
        std::fs::rename(&tmp, path)?;
        Ok(())
    }
}

/// 运行期可变配置的共享句柄(clone 廉价,跨 state 共享同一份)。
pub type SharedRuntimeConfig = Arc<RwLock<RuntimeConfig>>;

/// 便捷构造:从 `Config` 建立共享运行期配置。
pub fn shared_runtime_config(cfg: &Config) -> SharedRuntimeConfig {
    Arc::new(RwLock::new(RuntimeConfig::from_config(cfg)))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn defaults_are_sane() {
        let c = Config::default();
        assert_eq!(c.host, "127.0.0.1");
        assert_eq!(c.port, 8080);
        assert_eq!(c.region, "us-east-1");
    }

    #[test]
    fn impersonation_and_creds_path_defaults() {
        let c = Config::default();
        // 凭据文件路径:相对、无害的默认(不硬编码任何绝对生产路径)。
        assert_eq!(c.credentials_path, "credentials.json");
        assert!(!c.credentials_path.starts_with('/'));
        // 伪装版本号默认(契约 §3)。
        assert_eq!(c.kiro_version, "0.11.107");
        assert_eq!(c.system_version, "win32#10.0.22631");
        assert_eq!(c.node_version, "22.22.0");
    }

    #[test]
    fn camel_case_config_parses_new_fields() {
        let raw = r#"{"credentialsPath":"/x/creds.json","kiroVersion":"1.2.3"}"#;
        let c: Config = serde_json::from_str(raw).unwrap();
        assert_eq!(c.credentials_path, "/x/creds.json");
        assert_eq!(c.kiro_version, "1.2.3");
        // 未给的字段仍取默认
        assert_eq!(c.system_version, "win32#10.0.22631");
    }

    #[test]
    fn rpm_and_lb_mode_defaults() {
        let c = Config::default();
        assert_eq!(c.max_rpm_per_credential, 0);
        assert_eq!(c.load_balancing_mode, "priority");
    }

    #[test]
    fn camel_case_config_parses_rpm_and_lb_mode() {
        let raw = r#"{"maxRpmPerCredential":10,"loadBalancingMode":"balanced"}"#;
        let c: Config = serde_json::from_str(raw).unwrap();
        assert_eq!(c.max_rpm_per_credential, 10);
        assert_eq!(c.load_balancing_mode, "balanced");
    }

    #[test]
    fn runtime_config_persist_round_trips_camel_case() {
        let dir = std::env::temp_dir();
        let path = dir.join(format!("kiro2api_rc_persist_{}.json", std::process::id()));
        let _ = std::fs::remove_file(&path);
        // 预置一个含无关字段的配置文件,验证 persist 保留它。
        std::fs::write(&path, r#"{"host":"0.0.0.0","port":9090}"#).unwrap();

        let rc = RuntimeConfig {
            api_key: Some("sk-new".into()),
            admin_api_key: Some("adm-new".into()),
            max_rpm_per_credential: 42,
            load_balancing_mode: "balanced".into(),
            config_path: path.to_string_lossy().into_owned(),
        };
        rc.persist().unwrap();

        // 读回并用 Config 的 camelCase 反序列化解析,确认受管字段与无关字段都在。
        let reloaded: Config =
            serde_json::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
        assert_eq!(reloaded.api_key.as_deref(), Some("sk-new"));
        assert_eq!(reloaded.admin_api_key.as_deref(), Some("adm-new"));
        assert_eq!(reloaded.max_rpm_per_credential, 42);
        assert_eq!(reloaded.load_balancing_mode, "balanced");
        assert_eq!(reloaded.host, "0.0.0.0"); // 无关字段保留
        assert_eq!(reloaded.port, 9090);
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn runtime_config_persist_removes_key_when_none() {
        let dir = std::env::temp_dir();
        let path = dir.join(format!("kiro2api_rc_none_{}.json", std::process::id()));
        let _ = std::fs::remove_file(&path);
        std::fs::write(&path, r#"{"apiKey":"sk-old"}"#).unwrap();
        let rc = RuntimeConfig {
            api_key: None,
            admin_api_key: None,
            max_rpm_per_credential: 0,
            load_balancing_mode: "priority".into(),
            config_path: path.to_string_lossy().into_owned(),
        };
        rc.persist().unwrap();
        let v: serde_json::Value =
            serde_json::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
        assert!(v.get("apiKey").is_none());
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn env_overrides_win() {
        let mut c = Config::default();
        unsafe {
            std::env::set_var("PORT", "9099");
            std::env::set_var("HOST", "0.0.0.0");
        }
        c.apply_env_overrides();
        assert_eq!(c.port, 9099);
        assert_eq!(c.host, "0.0.0.0");
        unsafe {
            std::env::remove_var("PORT");
            std::env::remove_var("HOST");
        }
    }
}
