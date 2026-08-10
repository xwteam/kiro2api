use std::path::Path;
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
    /// 全局 machineId 覆盖(可选)。优先级:凭据自带 > 本配置 > 由 refresh_token 派生。
    ///
    /// 有此项时整池共用同一个 machineId —— 适合"一台机器、一个身份"的部署形态;
    /// 不设则每个账号派生各自的 machineId(默认,与真实客户端"一账号一机器"一致)。
    /// 此前只有凭据级覆盖,缺配置级兜底。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub machine_id: Option<String>,
    /// 全局出站代理。凭据级 `proxyUrl` 未设时用它;凭据级填 `"direct"` 可单独退回直连。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub proxy_url: Option<String>,
    /// TLS 后端:`"native-tls"`(默认)或 `"rustls"`。**运行时可切,改配置重启即生效。**
    ///
    /// 为什么要能切:两者的 CA 处理不同 —— native-tls 用系统证书库,rustls 用内置根证书。
    /// 一旦走自签 CA 的代理(企业出口、自建 MITM 代理),往往只有其中一个能握上手,
    /// 而现象是"刷不出令牌"或"直接连不上",与 TLS 毫无字面关系。此前是**编译期**二选一,
    /// 换后端得重新出镜像 —— 排查时最不该卡在这种地方。
    ///
    /// 取值非法时按默认处理并记 warn,不让服务因为一个拼错的配置起不来。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tls_backend: Option<String>,
    /// 每凭据每分钟最大请求数;0 = 无限(默认,兼容既有行为)。
    pub max_rpm_per_credential: u32,
    /// 负载均衡模式:"priority"(默认,等权轮询)或 "balanced"(按权重轮询)。
    pub load_balancing_mode: String,
    /// 本服务与调用方之间**可信反代的跳数**,决定从 `X-Forwarded-For` 的哪一项取客户端 IP。
    ///
    /// 转发头是普通请求头,谁都能自己写一个;XFF 的**最左**项恰恰是客户端可控的那一项。
    /// 而每一跳反代会把它**自己看到的对端**追加到最右,所以从右往左数第 `n` 项 = 第 `n` 跳
    /// 反代亲眼观测到的地址,伪造不了。
    ///
    /// - `1`(默认):服务前面只有一层自己的反代(Caddy / nginx / 直连回源的 CDN)。
    /// - `2`:CDN → 自己的反代 → 本服务,依此类推。
    /// - `0`:裸跑、无反代,一律不采信任何转发头,直接用 socket 对端。
    ///
    /// 设大了会取到客户端可控的位置(可伪造),设小了只会记成上一跳反代的地址(不准但安全)。
    pub trusted_proxy_hops: u8,
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
            machine_id: None,
            proxy_url: None,
            tls_backend: None,
            max_rpm_per_credential: 0,
            load_balancing_mode: "priority".into(),
            trusted_proxy_hops: 1,
            log_capacity: 5000,
            config_path: "config.json".into(),
        }
    }
}

/// **内置默认层**的凭据路径:相对文件名 `credentials.json` 就近解析到配置文件所在目录。
///
/// 凭据路径的父目录同时承载用量统计 / api_keys.json / 余额缓存,容器里必须落在挂载卷内,
/// 否则重建即丢。容器以 `-c /app/data/config.json` 启动,故就近解析后默认落在 `/app/data/`
/// (卷内)——无需在镜像里烘焙 `ENV CREDENTIALS_PATH`,那会让环境变量层**凌驾于 config.json
/// 之上**,把用户在 config.json 里设的自定义路径静默改道。
///
/// 只作用于"默认层":config.json 写了 credentialsPath、或设了 `CREDENTIALS_PATH` / `--credentials`
/// 时都不会走到这里。配置文件本身是无目录的相对名(裸机默认 `config.json`)时原样返回,裸机行为不变。
pub fn default_credentials_beside_config(config_path: &str) -> String {
    let builtin = Config::default().credentials_path;
    match Path::new(config_path).parent() {
        Some(dir) if !dir.as_os_str().is_empty() => {
            dir.join(&builtin).to_string_lossy().into_owned()
        }
        _ => builtin,
    }
}

impl Config {
    pub fn load(path: &str) -> anyhow::Result<Config> {
        // 同时留一份原始 JSON:判"credentialsPath 这个键在文件里到底有没有出现"。
        // 不能拿"值 == 内置默认"来判——那会误伤显式写了 `"credentialsPath": "credentials.json"`
        // 的用户,把他们的数据目录静默搬到配置文件旁边。
        let (mut cfg, explicit_credentials): (Config, bool) = match std::fs::read_to_string(path) {
            Ok(s) => {
                let cfg: Config = serde_json::from_str(&s)?;
                let explicit = serde_json::from_str::<serde_json::Value>(&s)
                    .ok()
                    .and_then(|v| v.get("credentialsPath").cloned())
                    .is_some();
                (cfg, explicit)
            }
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => (Config::default(), false),
            Err(e) => return Err(e.into()),
        };
        // 文件里没写 credentialsPath 时,把内置默认就近解析到配置文件所在目录
        // (必须在 apply_env_overrides 之前:它只是"默认层",不能盖过 env/config.json)。
        if !explicit_credentials {
            cfg.credentials_path = default_credentials_beside_config(path);
        }
        cfg.apply_env_overrides();
        // 记录本配置文件路径,供运行期改写(auth-keys / load-balancing)原子落盘定位。
        cfg.config_path = path.to_string();
        Ok(cfg)
    }

    fn apply_env_overrides(&mut self) {
        if let Some(v) = env_non_empty("HOST") {
            self.host = v;
        }
        if let Ok(v) = std::env::var("PORT")
            && let Ok(p) = v.parse()
        {
            self.port = p;
        }
        if let Some(v) = env_non_empty("REGION") {
            self.region = v;
        }
        if let Some(v) = env_non_empty("API_KEY") {
            self.api_key = Some(v);
        }
        if let Some(v) = env_non_empty("ADMIN_API_KEY") {
            self.admin_api_key = Some(v);
        }
        if let Some(v) = env_non_empty("CREDENTIALS_PATH") {
            self.credentials_path = v;
        }
        if let Ok(v) = std::env::var("MAX_RPM_PER_CREDENTIAL")
            && let Ok(n) = v.parse()
        {
            self.max_rpm_per_credential = n;
        }
        if let Some(v) = env_non_empty("LOAD_BALANCING_MODE") {
            self.load_balancing_mode = v;
        }
        if let Ok(v) = std::env::var("TRUSTED_PROXY_HOPS")
            && let Ok(n) = v.parse()
        {
            self.trusted_proxy_hops = n;
        }
    }
}

/// 读取字符串类环境变量:未设置、或 trim 后为空一律视为**未设置**(返回 `None`),
/// 由调用方保留 `config.json` / 面板里已有的值。
///
/// 约束来自部署形态:`.env.example` 自带 `API_KEY=` 这类空赋值,compose 的 `env_file`
/// 会把空串原样注入容器;若不过滤,空串会覆盖掉已配好的密钥(等于静默关掉鉴权)。
/// 返回值取 trim 后的内容:`.env` 行尾空白/CR 不会混进 key 与路径
/// (鉴权闸提取到的调用方 key 本就是 trim 过的,期望值带空白永远匹配不上)。
fn env_non_empty(name: &str) -> Option<String> {
    std::env::var(name)
        .ok()
        .map(|v| v.trim().to_string())
        .filter(|v| !v.is_empty())
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
        // 复用共享原子写:唯一 tmp 名(不再是每进程固定名——两个并发管理请求会互相踩)、
        // 建 tmp 即 0600(config.json 装着主 apiKey 与 adminApiKey 明文,不能全局可读)、
        // rename 前 fsync 数据、rename 后 fsync 父目录(崩溃后不会整体回退)、顺手清孤儿 tmp。
        crate::stats::persist::write_bytes_atomic(path, output.as_bytes())?;
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
    /// 回归:config.json **显式**写了 credentialsPath(哪怕值恰好等于内置默认名)时,
    /// 绝不能被"就近解析"搬到配置文件旁边——那会让老部署的数据目录静默换位置。
    #[test]
    fn explicit_credentials_path_is_never_relocated() {
        let dir = std::env::temp_dir().join(format!("k2a_cfg_explicit_{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let cfg_path = dir.join("config.json");
        std::fs::write(&cfg_path, r#"{"credentialsPath":"credentials.json"}"#).unwrap();
        let c = super::Config::load(cfg_path.to_str().unwrap()).unwrap();
        assert_eq!(
            c.credentials_path, "credentials.json",
            "显式配置必须原样保留(相对当前工作目录),不得被搬到 config.json 旁边"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// 回归:文件里**没写** credentialsPath 时,默认就近落到配置文件所在目录
    /// (容器 -c /app/data/config.json → /app/data/credentials.json,在挂载卷内)。
    #[test]
    fn absent_credentials_path_defaults_beside_config() {
        let dir = std::env::temp_dir().join(format!("k2a_cfg_absent_{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let cfg_path = dir.join("config.json");
        std::fs::write(&cfg_path, r#"{"port":8990}"#).unwrap();
        let c = super::Config::load(cfg_path.to_str().unwrap()).unwrap();
        assert_eq!(
            c.credentials_path,
            dir.join("credentials.json").to_string_lossy(),
            "缺省时必须就近解析到配置文件目录,否则容器里会落到卷外"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// 无目录部分的配置名(裸机默认 `config.json`)保持原相对名,裸机行为不变。
    #[test]
    fn bare_config_name_keeps_relative_default() {
        assert_eq!(
            super::default_credentials_beside_config("config.json"),
            "credentials.json"
        );
    }

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

    /// 空串环境变量(compose 的 env_file 注入 `.env.example` 里的 `API_KEY=`)
    /// 不得覆盖 config.json / 面板已配好的值——否则鉴权被静默关闭。
    /// 只用空白的值同样视为未设置。
    #[test]
    fn blank_env_vars_do_not_override_configured_values() {
        let mut c = Config {
            api_key: Some("sk-from-config".into()),
            admin_api_key: Some("adm-from-config".into()),
            credentials_path: "/data/credentials.json".into(),
            region: "eu-west-1".into(),
            load_balancing_mode: "balanced".into(),
            ..Config::default()
        };
        unsafe {
            std::env::set_var("API_KEY", "");
            std::env::set_var("ADMIN_API_KEY", "   ");
            std::env::set_var("CREDENTIALS_PATH", "");
            std::env::set_var("REGION", "");
            std::env::set_var("LOAD_BALANCING_MODE", "");
        }
        c.apply_env_overrides();
        assert_eq!(c.api_key.as_deref(), Some("sk-from-config"));
        assert_eq!(c.admin_api_key.as_deref(), Some("adm-from-config"));
        assert_eq!(c.credentials_path, "/data/credentials.json");
        assert_eq!(c.region, "eu-west-1");
        assert_eq!(c.load_balancing_mode, "balanced");

        // 非空环境变量仍照常覆盖,且前后空白被剔除。
        unsafe {
            std::env::set_var("API_KEY", "  sk-from-env  ");
        }
        c.apply_env_overrides();
        assert_eq!(c.api_key.as_deref(), Some("sk-from-env"));

        unsafe {
            std::env::remove_var("API_KEY");
            std::env::remove_var("ADMIN_API_KEY");
            std::env::remove_var("CREDENTIALS_PATH");
            std::env::remove_var("REGION");
            std::env::remove_var("LOAD_BALANCING_MODE");
        }
    }
}
