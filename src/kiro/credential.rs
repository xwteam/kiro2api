//! Kiro 账号凭据模型与磁盘读写(credentials.json)。
//! 字段与真机 credentials.json 格式对齐(照观测,见 §7 校准清单),代码为本项目自写:
//! - `auth` 的 wire 名是 `authMethod`(非 `auth`)。
//! - `expiresAt` 在磁盘上是 RFC3339 字符串,内部仍存 unix 秒(`expires_at_unix`)。
//! - `id` 在磁盘上是整数,内部统一存 String 以兼容旧数据/其它来源的字符串 id。
use std::sync::atomic::{AtomicU64, Ordering};

use anyhow::Context;
use serde::{Deserialize, Deserializer, Serialize, Serializer};

/// 账号鉴权方式。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum AuthMethod {
    Social,
    Idc,
    /// Kiro API Key(`ksk_…`)。与前两者根本不同:它**本身就是数据面 bearer**,
    /// 不换取令牌、不刷新、不过期。故这类凭据不走 OAuth 刷新链路的任何一步。
    /// 认 `apikey` 与 `api_key` 两种写法(不同工具的落盘习惯不一致)。
    #[serde(alias = "api_key", alias = "API_KEY")]
    ApiKey,
}

/// region 缺省值(契约 §7:credentials.json 无 region 键时回落)。
fn default_region() -> String {
    "us-east-1".to_string()
}

/// 兼容整数或字符串形式的 `id`,内部统一存 String。
fn de_id_flexible<'de, D>(deserializer: D) -> Result<String, D::Error>
where
    D: Deserializer<'de>,
{
    #[derive(Deserialize)]
    #[serde(untagged)]
    enum IdShape {
        Num(i64),
        Text(String),
    }
    match IdShape::deserialize(deserializer)? {
        IdShape::Num(n) => Ok(n.to_string()),
        IdShape::Text(s) => Ok(s),
    }
}

/// `expiresAt`(RFC3339 字符串)→ 内部 unix 秒。
fn de_expires_at_rfc3339<'de, D>(deserializer: D) -> Result<u64, D::Error>
where
    D: Deserializer<'de>,
{
    let s = String::deserialize(deserializer)?;
    let dt = chrono::DateTime::parse_from_rfc3339(&s).map_err(serde::de::Error::custom)?;
    let secs = dt.timestamp();
    Ok(secs.max(0) as u64)
}

/// 内部 unix 秒 → `expiresAt`(RFC3339 字符串,UTC、秒精度、Z 后缀)。
fn se_expires_at_rfc3339<S>(secs: &u64, serializer: S) -> Result<S::Ok, S::Error>
where
    S: Serializer,
{
    let dt = chrono::DateTime::from_timestamp(*secs as i64, 0)
        .ok_or_else(|| serde::ser::Error::custom("expires_at_unix 超出可表示范围"))?;
    serializer.serialize_str(&dt.to_rfc3339_opts(chrono::SecondsFormat::Secs, true))
}

/// 单个账号凭据。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Credential {
    #[serde(deserialize_with = "de_id_flexible")]
    pub id: String,
    pub access_token: String,
    pub refresh_token: String,
    /// 过期时刻(unix 秒;磁盘上是 RFC3339 字符串 `expiresAt`)。
    ///
    /// API Key 凭据没有这个概念,导入时通常也不带该键;缺省落 0,并由
    /// [`is_expired`](Self::is_expired) / [`expires_soon`](Self::expires_soon) 对这类凭据
    /// 恒答"未过期"——否则 0 会被读成"1970 年就过期了",账号一进池就被判死。
    #[serde(
        default,
        rename = "expiresAt",
        deserialize_with = "de_expires_at_rfc3339",
        serialize_with = "se_expires_at_rfc3339"
    )]
    pub expires_at_unix: u64,
    /// 数据面 region;磁盘上多数账号无此键(见契约 §7),缺省回落 `us-east-1`。
    /// (真机 credentials.json 携带的 `authRegion`/`subscriptionTitle` 等额外键由 serde 忽略,
    ///  不做别名映射。)
    #[serde(default = "default_region")]
    pub region: String,
    #[serde(rename = "authMethod")]
    pub auth: AuthMethod,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub client_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub client_secret: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub profile_arn: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub machine_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub email: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub nickname: Option<String>,
    #[serde(default)]
    pub weight: u32,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub label: Option<String>,
    #[serde(default)]
    pub disabled: bool,
    /// Kiro API Key(`ksk_…`)。存在即视为 API Key 凭据。
    ///
    /// 与 `access_token` 分开存:后者是刷新换来的、会被覆写,而 ksk 是用户给的长期密钥,
    /// 一旦被刷新逻辑当成 access_token 覆写掉就再也拿不回来。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub kiro_api_key: Option<String>,
    /// 最近一次失败的结论,落盘那份(取值见 `pool::StatusReason::as_str`)。
    ///
    /// 只有**结论**落盘,strike 计数与冷却截止时刻不落——后两者是计时器,重启后从零开始
    /// 无非是让账号早点重试一次,无害。结论不同:`banned` 会把账号挡在池外,若只活在内存里,
    /// 每次重启/发版都会把它抹掉,账号悄悄回到可用池,直到再失败一次才重新被挡——于是
    /// 「253 个账号有 1 个封禁,可用数却是 253」这件事会在每次重启后重现。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub status_reason: Option<String>,
}

#[cfg(test)]
pub(crate) fn tests_support_cred() -> Credential {
    Credential {
        id: "1".into(),
        access_token: "AT".into(),
        refresh_token: "RT".into(),
        kiro_api_key: None,
        expires_at_unix: 1000,
        region: "us-east-1".into(),
        auth: AuthMethod::Social,
        client_id: None,
        client_secret: None,
        profile_arn: None,
        machine_id: None,
        email: None,
        nickname: None,
        weight: 1,
        label: None,
        disabled: false,
        status_reason: None,
    }
}

impl Credential {
    /// 是否为 API Key 凭据:带 `kiroApiKey`,或 `authMethod` 显式声明。
    ///
    /// 两个条件取并集是刻意的:导入时只填 key 不填 authMethod 是最自然的用法,
    /// 而只声明 authMethod 却没有 key 是配置错误——后者由取 bearer 的那一步报出来,
    /// 不在这里静默回落成 OAuth(回落会让它拿着空 token 去打上游,错得更远)。
    pub fn is_api_key(&self) -> bool {
        self.kiro_api_key.is_some() || self.auth == AuthMethod::ApiKey
    }
    /// 配置是否自相矛盾:声明了 `authMethod=api_key` 却没给 `kiroApiKey`。
    ///
    /// 这类凭据取不到 bearer,又因为 [`is_api_key`](Self::is_api_key) 取并集而通过了
    /// "是 API Key 凭据"的判定,于是每次被选中都在同一处失败 —— 不刷新(API Key 本就不刷)、
    /// 也没有可用的 token,只能在跨账号重试里空转。故必须在**入池那一刻**就判出来并禁用,
    /// 而不是留给运行时反复触发。(照观测补齐。)
    pub fn is_invalid_api_key_config(&self) -> bool {
        self.auth == AuthMethod::ApiKey && self.kiro_api_key.is_none()
    }
    /// 数据面 bearer:API Key 凭据用 ksk 本身,其余用刷新换来的 access_token。
    pub fn bearer(&self) -> &str {
        match &self.kiro_api_key {
            Some(k) if self.is_api_key() => k,
            _ => &self.access_token,
        }
    }
    /// 是否已过期(到点即算)。API Key 无过期概念,恒答否。
    pub fn is_expired(&self, now_unix: u64) -> bool {
        !self.is_api_key() && self.expires_at_unix <= now_unix
    }
    /// 是否即将在 margin_secs 内过期。API Key 无过期概念,恒答否。
    pub fn expires_soon(&self, now_unix: u64, margin_secs: u64) -> bool {
        !self.is_api_key() && self.expires_at_unix <= now_unix.saturating_add(margin_secs)
    }
    /// 负载均衡权重下限 1。
    pub fn effective_weight(&self) -> u32 {
        self.weight.max(1)
    }
}

/// 读取 credentials.json(数组);文件不存在返回空。
pub fn load(path: &str) -> anyhow::Result<Vec<Credential>> {
    match std::fs::read_to_string(path) {
        Ok(s) => Ok(serde_json::from_str(&s).with_context(|| "解析 credentials.json")?),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(Vec::new()),
        Err(e) => Err(e.into()),
    }
}

/// 每次写盘的临时文件后缀计数器,保证并发写各用独立 tmp 文件(不共享、不互相截断)。
static TMP_COUNTER: AtomicU64 = AtomicU64::new(0);

/// 生成本次写盘专用的唯一临时路径:`{path}.tmp.{pid}.{counter}`。
/// pid 隔离多进程、进程内单调计数器隔离并发写,故两个写者绝不会共享同一 tmp 文件
/// (避免 fixed `{path}.tmp` 的交错写/rename 竞态损坏 credentials.json)。
fn unique_tmp_path(path: &str) -> String {
    let n = TMP_COUNTER.fetch_add(1, Ordering::Relaxed);
    format!("{path}.tmp.{}.{n}", std::process::id())
}

/// 陈旧 tmp 判定阈值:比这更久没被动过的 `{path}.tmp.*` 视为进程崩溃遗留的孤儿,可清。
/// 取 5 分钟,远大于一次正常 save(写+fsync+rename,亚秒级),故绝不会误删并发写者正在写的 tmp。
const STALE_TMP_AGE_SECS: u64 = 300;

/// #11:清理同目录下遗留的孤儿 `{path}.tmp.*` 文件(进程在 tmp 写完与 rename 之间崩溃留下的)。
///
/// best-effort:任何 IO 错误都忽略(不使 save 失败)。只删**足够旧**(mtime 早于 `STALE_TMP_AGE_SECS`)
/// 的 tmp,从而不会碰到另一并发写者此刻正在写的新 tmp(其 mtime 是当下)。前缀严格匹配
/// `{basename}.tmp.` 以免误伤同目录其它文件。
fn sweep_stale_tmp(path: &str) {
    let p = std::path::Path::new(path);
    let (dir, file_name) = match (p.parent(), p.file_name()) {
        (Some(d), Some(f)) => (d, f.to_string_lossy().into_owned()),
        _ => return,
    };
    let dir = if dir.as_os_str().is_empty() {
        std::path::Path::new(".")
    } else {
        dir
    };
    let prefix = format!("{file_name}.tmp.");
    let now = std::time::SystemTime::now();
    let rd = match std::fs::read_dir(dir) {
        Ok(rd) => rd,
        Err(_) => return,
    };
    for entry in rd.flatten() {
        let name = entry.file_name();
        let name = name.to_string_lossy();
        if !name.starts_with(&prefix) {
            continue;
        }
        // 仅清足够旧的:mtime 距今 >= 阈值。取不到 mtime 则保守跳过(不删)。
        let stale = entry
            .metadata()
            .ok()
            .and_then(|m| m.modified().ok())
            .and_then(|mtime| now.duration_since(mtime).ok())
            .map(|age| age.as_secs() >= STALE_TMP_AGE_SECS)
            .unwrap_or(false);
        if stale {
            let _ = std::fs::remove_file(entry.path());
        }
    }
}

/// 原子写 credentials.json:写唯一临时文件 → fsync(落数据到盘)→ rename 覆盖。
///
/// 持久性:rename 前对 tmp 文件 `sync_all`,确保进程/系统崩溃后要么是完整旧文件、
/// 要么是完整新文件,绝不会半写或丢失已提交内容。tmp 路径按 pid+计数器唯一化,
/// 并发写彼此隔离,不会交错污染。
///
/// #11:每次 save 顺手清理同目录下遗留的**孤儿** `{path}.tmp.*`(此前进程在 tmp 写完与 rename
/// 之间崩溃留下的),仅清足够旧的,不碰并发写者正在写的新 tmp。
pub fn save(path: &str, creds: &[Credential]) -> anyhow::Result<()> {
    let data = serde_json::to_vec_pretty(creds)?;
    atomic_write(path, &data).map_err(Into::into)
}

/// 原子写一个文件:唯一 tmp(0600)→ 写 → fsync → rename 顶替,失败清 tmp,顺手清孤儿 tmp。
///
/// 抽成独立函数是为了让**所有**落到凭据目录的持久化写共用同一套规矩:除 credentials.json 本体外,
/// 旁挂的 `{path}.next-id`(账号 id 高水位)也必须走这条路径 —— 裸 `fs::write` 是「先截断到 0
/// 再写」,崩溃/掉电正好落在这个窗口就留下 0 字节或半截文件,读回来解析不出 → 高水位丢失 →
/// 已删除账号的编号被复用(见 [`save_next_id`])。
fn atomic_write(path: &str, data: &[u8]) -> std::io::Result<()> {
    let tmp = unique_tmp_path(path);
    // 写失败/中途 panic 时清理残留 tmp,避免堆积;成功 rename 后 tmp 已消失。
    let res = (|| -> std::io::Result<()> {
        // 目标文件权限继承自 tmp(rename 顶替)。unix 下按 0600 建 tmp:文件里是全部账号的
        // 明文 access/refresh token,若用 `File::create`(0666 & ~umask,通常落 0644),
        // 用户手动 chmod 600 过的凭据文件每写一次就被改回全局可读。
        #[cfg(unix)]
        let f = {
            use std::os::unix::fs::OpenOptionsExt;
            std::fs::OpenOptions::new()
                .write(true)
                .create(true)
                .truncate(true)
                .mode(0o600)
                .open(&tmp)?
        };
        #[cfg(not(unix))]
        let f = std::fs::File::create(&tmp)?;
        {
            use std::io::Write;
            let mut w = std::io::BufWriter::new(&f);
            w.write_all(data)?;
            w.flush()?;
        }
        f.sync_all()?; // fsync:确保数据真正落盘,再做 rename
        std::fs::rename(&tmp, path)?;
        Ok(())
    })();
    if res.is_err() {
        let _ = std::fs::remove_file(&tmp);
    }
    // best-effort 清理孤儿 tmp(不影响本次写结果)。
    sweep_stale_tmp(path);
    res
}

/// 账号 id 高水位的旁挂文件路径:`{path}.next-id`。
///
/// 刻意**不**改动 credentials.json 自身的形状——它是与 Kiro 原生凭据文件 drop-in 互通的裸 JSON
/// 数组(见部署文档),换成对象信封会破坏互通。故把池的 id 高水位写在同目录的旁挂小文件里,
/// 与凭据在同一次落盘临界区内一起写。
fn next_id_path(path: &str) -> String {
    format!("{path}.next-id")
}

/// 读取持久化的账号 id 高水位;文件缺失或内容非法 → `None`。
/// 旧安装没有该文件,调用方据此退化为"`max(现有 id)+1`"的原语义(见 [`crate::kiro::pool::Pool::with_next_id`])。
pub fn load_next_id(path: &str) -> Option<u64> {
    std::fs::read_to_string(next_id_path(path))
        .ok()
        .and_then(|s| s.trim().parse::<u64>().ok())
}

/// 落盘账号 id 高水位。best-effort:写失败只记 warn、不使凭据落盘失败——退化后果仅是
/// 重启后可能复用已删除账号的编号,而凭据本身必须先写成功。
///
/// **必须与 credentials.json 本体同规格走 [`atomic_write`]**(tmp + fsync + rename),不能用裸
/// `std::fs::write`:后者先把目标文件截断到 0 再写,而本函数在每次令牌刷新时都会被调用
/// (`ensure_fresh` 每刷一次就落一次盘,生产上约千个账号即高频重复),崩溃/掉电落在那个窗口
/// 就留下 0 字节或半截文件。读回时 [`load_next_id`] 解析不出 → 回落 `None` → 池退化成
/// `max(现有 id)+1`:若被删掉的恰是编号最大的账号,下一个新账号就拿回它的编号,继承其
/// 用量记录、并可能被旧令牌写回覆盖。fsync 后 rename 则保证任何时刻读到的要么是旧值、
/// 要么是新值,绝不会是半截。
pub fn save_next_id(path: &str, next_id: u64) {
    if let Err(e) = atomic_write(&next_id_path(path), next_id.to_string().as_bytes()) {
        tracing::warn!("持久化账号 id 高水位失败(重启后可能复用已删除的账号编号): {e}");
    }
}

/// 序列化持久化活池凭据:在 `persist_lock` 临界区内取池快照 + 原子落盘,单一序列化点。
///
/// #13(lost update)修复:所有凭据落盘(admin 增删改 + 刷新路径写回)都经此函数、共用同一把
/// `persist_lock`,把「取快照 → 序列化 → 原子写」收敛成唯一临界区。因此:
/// - 两个并发落盘不会交错(save 已是唯一 tmp + fsync + rename 原子);
/// - 快照在持锁时才取,晚到的写不会用过期快照覆盖新状态(先改池、后在锁内重新快照 → 落最新)。
///
/// 并发纪律:`persist_lock` 只覆盖「锁池取快照 + 落盘」这段极短同步区,**不跨网络 await**。
/// 调用方须先在各自的 pool 锁临界区内完成池变更(add/update/remove),再调用本函数落盘。
///
/// 与刷新路径的协调:刷新路径(`ensure_fresh::persist_pool_credentials`)持有同一把
/// `RefreshCtx.persist_lock`;admin 侧传入 `state.refresh_ctx.persist_lock` 即与其互斥,
/// 二者不会并发写 credentials.json。
pub async fn persist_pool_credentials_serialized(
    pool: &std::sync::Arc<tokio::sync::Mutex<crate::kiro::pool::Pool>>,
    persist_lock: &std::sync::Arc<tokio::sync::Mutex<()>>,
    path: &str,
) -> anyhow::Result<()> {
    let _guard = persist_lock.lock().await;
    let (snapshot, next_id) = {
        let pool_lock = pool.lock().await;
        (pool_lock.snapshot_credentials(), pool_lock.next_id_hint())
    };
    let owned_path = path.to_string();
    // 写盘 + fsync 是阻塞 IO,不能占着 async 运行时的工作线程做(fsync 可长达数十毫秒,
    // 期间该线程上的所有任务都被卡住)。挪到 blocking 线程池执行。
    tokio::task::spawn_blocking(move || {
        save(&owned_path, &snapshot)?;
        save_next_id(&owned_path, next_id);
        Ok::<(), anyhow::Error>(())
    })
    .await
    .map_err(|e| anyhow::anyhow!("凭据落盘任务执行失败: {e}"))?
}

#[cfg(test)]
mod tests {

    /// ksk 凭据没有 `expiresAt`,导入时通常也不带该键 → 缺省落 0。若过期判定照常按 0 比,
    /// 账号一进池就被读成「1970 年就过期了」,立刻被判死;必须对这类凭据恒答"未过期"。
    #[test]
    fn api_key_credential_never_looks_expired() {
        let mut c = super::tests_support_cred();
        c.kiro_api_key = Some("ksk_abc".into());
        c.expires_at_unix = 0;
        assert!(!c.is_expired(9_999_999_999));
        assert!(!c.expires_soon(9_999_999_999, 300));
        assert!(c.is_api_key());
        assert_eq!(c.bearer(), "ksk_abc", "数据面 bearer 必须是 ksk 本身");
    }

    /// 没有 ksk 的凭据一律走原路:bearer 仍是刷新换来的 access_token,过期判定照旧。
    #[test]
    fn oauth_credential_is_unaffected() {
        let c = super::tests_support_cred();
        assert!(!c.is_api_key());
        assert_eq!(c.bearer(), c.access_token);
        assert!(c.is_expired(9_999_999_999));
    }
    use super::*;

    fn sample() -> Credential {
        Credential {
            id: "a1".into(),
            access_token: "at".into(),
            refresh_token: "rt".into(),
            kiro_api_key: None,
            expires_at_unix: 1000,
            region: "us-east-1".into(),
            auth: AuthMethod::Social,
            client_id: None,
            client_secret: None,
            profile_arn: None,
            machine_id: None,
            email: None,
            nickname: None,
            weight: 0,
            label: Some("acct".into()),
            disabled: false,
            status_reason: None,
        }
    }

    #[test]
    fn expiry_and_weight() {
        let c = sample();
        assert!(c.is_expired(1000)); // <= 视为过期
        assert!(!c.is_expired(999));
        assert!(c.expires_soon(940, 60)); // 940+60=1000 >= 1000
        assert_eq!(c.effective_weight(), 1); // weight 0 -> 1
    }

    #[test]
    fn serde_roundtrip_camel_and_authmethod() {
        let json = serde_json::to_string(&sample()).unwrap();
        assert!(json.contains("\"accessToken\""));
        assert!(json.contains("\"authMethod\":\"social\""));
        let back: Credential = serde_json::from_str(&json).unwrap();
        assert_eq!(back, sample());
        // idc 也能解析
        let idc: Credential = serde_json::from_str(&json.replace("\"social\"", "\"idc\"")).unwrap();
        assert_eq!(idc.auth, AuthMethod::Idc);
    }

    #[test]
    fn load_missing_is_empty_then_save_roundtrip() {
        let dir = std::env::temp_dir();
        let path = dir.join(format!("kiro2api_cred_test_{}.json", std::process::id()));
        let p = path.to_str().unwrap();
        let _ = std::fs::remove_file(p);
        assert!(load(p).unwrap().is_empty());
        save(p, &[sample()]).unwrap();
        let got = load(p).unwrap();
        assert_eq!(got.len(), 1);
        assert_eq!(got[0], sample());
        let _ = std::fs::remove_file(p);
    }

    /// 真实 credentials.json 的最小样例(照 §7 观测):
    /// `id` 为整数、`authMethod`(非 `auth`)、`expiresAt` 为 RFC3339 字符串、`machineId` 可带。
    const REAL_SAMPLE: &str = r#"{
        "id": 12345,
        "accessToken": "real-at",
        "refreshToken": "real-rt",
        "expiresAt": "2026-07-19T12:00:00Z",
        "region": "us-east-1",
        "authMethod": "social",
        "profileArn": "arn:aws:codewhisperer:us-east-1:111111111111:profile/ABCDEF",
        "machineId": "deadbeef",
        "email": "acct@example.com",
        "nickname": "acct-nick",
        "disabled": false
    }"#;

    #[test]
    fn parses_real_credentials_json_shape() {
        let c: Credential = serde_json::from_str(REAL_SAMPLE).unwrap();
        assert_eq!(c.id, "12345"); // 整数 id 兼容存成 String
        assert_eq!(c.access_token, "real-at");
        assert_eq!(c.auth, AuthMethod::Social);
        assert_eq!(c.machine_id.as_deref(), Some("deadbeef"));
        assert_eq!(c.email.as_deref(), Some("acct@example.com"));
        assert_eq!(c.nickname.as_deref(), Some("acct-nick"));
        // 2026-07-19T12:00:00Z 的 unix 秒
        assert_eq!(c.expires_at_unix, 1_784_462_400);
    }

    #[test]
    fn real_sample_id_as_string_also_parses() {
        let as_string = REAL_SAMPLE.replacen("\"id\": 12345", "\"id\": \"12345\"", 1);
        let c: Credential = serde_json::from_str(&as_string).unwrap();
        assert_eq!(c.id, "12345");
    }

    #[test]
    fn expires_at_rfc3339_roundtrips_through_serialize() {
        let c: Credential = serde_json::from_str(REAL_SAMPLE).unwrap();
        let out = serde_json::to_string(&c).unwrap();
        assert!(out.contains("\"expiresAt\":\"2026-07-19T12:00:00Z\""));
        assert!(out.contains("\"authMethod\":\"social\""));
    }

    #[test]
    fn real_sample_is_expired_relative_to_known_instant() {
        let c: Credential = serde_json::from_str(REAL_SAMPLE).unwrap();
        assert!(c.is_expired(1_784_462_400)); // 到点算过期
        assert!(!c.is_expired(1_784_462_399)); // 差一秒未到期
    }

    /// 唯一 tmp:同一 path 连续两次 save 各自用不同的 tmp 文件名(pid+counter),
    /// 且成功后不残留任何 `{path}.tmp.*`(rename 掉了),最终文件内容为最后一次写入。
    #[test]
    fn save_uses_unique_tmp_and_leaves_no_residue() {
        let dir = std::env::temp_dir();
        let path = dir.join(format!(
            "kiro2api_cred_unique_tmp_{}_{}.json",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        let p = path.to_str().unwrap();

        // 两次生成的 tmp 路径必须不同(计数器单调)。
        let t1 = unique_tmp_path(p);
        let t2 = unique_tmp_path(p);
        assert_ne!(t1, t2);

        let mut a = sample();
        a.id = "1".into();
        let mut b = sample();
        b.id = "2".into();
        save(p, &[a]).unwrap();
        save(p, std::slice::from_ref(&b)).unwrap();

        // 最终内容为最后一次写入。
        let got = load(p).unwrap();
        assert_eq!(got.len(), 1);
        assert_eq!(got[0].id, "2");

        // 无 tmp 残留。
        let mut residue = false;
        if let Ok(rd) = std::fs::read_dir(&dir) {
            let prefix = format!("{}.tmp.", path.file_name().unwrap().to_string_lossy());
            for e in rd.flatten() {
                if e.file_name().to_string_lossy().starts_with(&prefix) {
                    residue = true;
                    break;
                }
            }
        }
        assert!(
            !residue,
            "no {{path}}.tmp.* residue expected after successful save"
        );
        let _ = std::fs::remove_file(p);
    }

    /// #11:save 时清理**陈旧**孤儿 `{path}.tmp.*`(崩溃遗留),但保留**新近**的 tmp
    /// (并发写者正在写的,mtime 是当下,不能误删)。
    #[test]
    fn save_sweeps_stale_orphan_tmp_but_keeps_fresh() {
        use std::time::SystemTime;
        let dir = std::env::temp_dir();
        let base = format!(
            "kiro2api_cred_sweep_{}_{}.json",
            std::process::id(),
            SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        );
        let path = dir.join(&base);
        let p = path.to_str().unwrap().to_string();
        let _ = std::fs::remove_file(&p);

        // 造一个"陈旧"孤儿 tmp:用 `touch -d` 把 mtime 回拨到远超阈值(无第三方 crate 依赖)。
        let stale_tmp = dir.join(format!("{base}.tmp.99999.0"));
        std::fs::write(&stale_tmp, b"orphan").unwrap();
        let backdated = std::process::Command::new("touch")
            .arg("-d")
            .arg("2000-01-01 00:00:00")
            .arg(&stale_tmp)
            .status()
            .map(|s| s.success())
            .unwrap_or(false);

        // 造一个"新近"孤儿 tmp:mtime 为当下,必须被保留。
        let fresh_tmp = dir.join(format!("{base}.tmp.99999.1"));
        std::fs::write(&fresh_tmp, b"in-flight").unwrap();

        // 一个无关的同目录文件(前缀不匹配)必须不受影响。
        let unrelated = dir.join(format!("{base}.other"));
        std::fs::write(&unrelated, b"keep").unwrap();

        save(&p, &[sample()]).unwrap();

        // 陈旧 tmp 被清(仅当 mtime 成功回拨,即 touch 可用时才断言,避免无 touch 环境误报)。
        if backdated {
            assert!(!stale_tmp.exists(), "陈旧孤儿 tmp 应被 save 清理");
        }
        assert!(fresh_tmp.exists(), "新近(并发写者的)tmp 必须保留");
        assert!(unrelated.exists(), "前缀不匹配的无关文件必须保留");

        let _ = std::fs::remove_file(&p);
        let _ = std::fs::remove_file(&stale_tmp);
        let _ = std::fs::remove_file(&fresh_tmp);
        let _ = std::fs::remove_file(&unrelated);
    }

    /// #13:序列化持久化助手在持锁下取活池快照 + 原子落盘,落到磁盘的是池当前状态。
    #[tokio::test]
    async fn serialized_persist_writes_current_pool_snapshot() {
        use crate::kiro::pool::{LbMode, Pool};
        let dir = std::env::temp_dir();
        let path = dir.join(format!(
            "kiro2api_cred_serialized_persist_{}_{}.json",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        let p = path.to_str().unwrap().to_string();

        let mut seed = sample();
        seed.id = "1".into();
        let pool = std::sync::Arc::new(tokio::sync::Mutex::new(Pool::new(
            vec![seed],
            LbMode::Priority,
        )));
        let lock = std::sync::Arc::new(tokio::sync::Mutex::new(()));

        persist_pool_credentials_serialized(&pool, &lock, &p)
            .await
            .unwrap();
        let got = load(&p).unwrap();
        assert_eq!(got.len(), 1);
        // 凭据与 id 高水位在同一次落盘里一起写出,重启后不会复用已删除账号的编号。
        assert_eq!(load_next_id(&p), Some(2));
        let _ = std::fs::remove_file(&p);
        let _ = std::fs::remove_file(next_id_path(&p));
    }

    /// 原子写不得放宽凭据文件权限:tmp 按 0600 创建,rename 后目标文件也必须是 0600
    /// (文件里是全部账号的明文 access/refresh token)。即便目标文件此前是 0644 也要收紧。
    #[cfg(unix)]
    #[test]
    fn save_keeps_credentials_file_owner_only() {
        use std::os::unix::fs::PermissionsExt;
        let dir = std::env::temp_dir();
        let path = dir.join(format!(
            "kiro2api_cred_mode_{}_{}.json",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        let p = path.to_str().unwrap().to_string();
        let _ = std::fs::remove_file(&p);

        save(&p, &[sample()]).unwrap();
        let mode = std::fs::metadata(&p).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode, 0o600, "凭据文件必须仅属主可读写,实际 {mode:o}");

        // 覆盖写(rename 顶替)后依然是 0600,不会被 tmp 的默认权限放宽回 0644。
        save(&p, &[sample()]).unwrap();
        let mode = std::fs::metadata(&p).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode, 0o600, "覆盖写后权限不得被放宽,实际 {mode:o}");

        let _ = std::fs::remove_file(&p);
        let _ = std::fs::remove_file(next_id_path(&p));
    }

    /// 账号 id 高水位旁挂落盘:写入后可读回;文件缺失时返回 None(旧安装平滑降级)。
    /// credentials.json 本体仍是裸数组,drop-in 互通不受影响。
    #[test]
    fn next_id_sidecar_roundtrips_and_degrades_when_absent() {
        let dir = std::env::temp_dir();
        let path = dir.join(format!(
            "kiro2api_cred_nextid_{}_{}.json",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        let p = path.to_str().unwrap().to_string();
        let _ = std::fs::remove_file(&p);
        let _ = std::fs::remove_file(next_id_path(&p));

        assert_eq!(load_next_id(&p), None); // 无旁挂文件 → 降级
        save_next_id(&p, 42);
        assert_eq!(load_next_id(&p), Some(42));
        save_next_id(&p, 43);
        assert_eq!(load_next_id(&p), Some(43));

        // 内容非法 → 视作无记录(不 panic)。
        std::fs::write(next_id_path(&p), "not-a-number").unwrap();
        assert_eq!(load_next_id(&p), None);

        // 凭据本体仍是裸 JSON 数组。
        save(&p, &[sample()]).unwrap();
        let raw = std::fs::read_to_string(&p).unwrap();
        assert!(
            raw.trim_start().starts_with('['),
            "credentials.json 必须保持裸数组"
        );

        let _ = std::fs::remove_file(&p);
        let _ = std::fs::remove_file(next_id_path(&p));
    }

    /// 关键回归:账号 id 高水位必须**原子**落盘(tmp + fsync + rename),不能用裸 `fs::write`。
    ///
    /// 判据取 inode:rename 顶替会换一个 inode,就地截断写(`fs::write`/`File::create`)则 inode 不变。
    /// 就地写意味着"先截断到 0 再写"——本函数每次令牌刷新都跑,崩溃/掉电落在那个窗口就留下 0 字节
    /// 文件,重启后高水位丢失、已删除账号的编号被复用(新账号继承旧账号的用量,并可能被旧令牌覆盖)。
    #[cfg(unix)]
    #[test]
    fn save_next_id_replaces_atomically_instead_of_truncating_in_place() {
        use std::os::unix::fs::MetadataExt;
        let dir = std::env::temp_dir();
        let path = dir.join(format!(
            "kiro2api_cred_nextid_atomic_{}_{}.json",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        let p = path.to_str().unwrap().to_string();
        let sidecar = next_id_path(&p);
        let _ = std::fs::remove_file(&sidecar);

        save_next_id(&p, 41);
        let ino_first = std::fs::metadata(&sidecar).unwrap().ino();
        save_next_id(&p, 42);
        let ino_second = std::fs::metadata(&sidecar).unwrap().ino();
        assert_ne!(
            ino_first, ino_second,
            "覆盖写必须经 tmp+rename 顶替;inode 不变 = 就地截断写,崩溃窗口内会留下 0 字节高水位文件"
        );
        assert_eq!(load_next_id(&p), Some(42), "内容仍须是最新高水位");

        // 成功路径不得留下 tmp 残渣(否则数据盘上会随每次令牌刷新堆积)。
        let prefix = format!(
            "{}.tmp.",
            std::path::Path::new(&sidecar)
                .file_name()
                .unwrap()
                .to_string_lossy()
        );
        let residue = std::fs::read_dir(&dir)
            .unwrap()
            .flatten()
            .any(|e| e.file_name().to_string_lossy().starts_with(prefix.as_str()));
        assert!(!residue, "落盘成功后不应残留 {prefix}*");

        let _ = std::fs::remove_file(&sidecar);
    }

    /// 契约 §7:真机 credentials.json 里绝大多数账号没有 `region` 键(只带
    /// `authRegion`/`subscriptionTitle` 等),缺省须回落到 us-east-1;额外键被忽略。
    #[test]
    fn absent_region_defaults_to_us_east_1() {
        let no_region = r#"{
            "id": 7,
            "accessToken": "at",
            "refreshToken": "rt",
            "expiresAt": "2026-07-19T12:00:00Z",
            "authMethod": "idc",
            "authRegion": "eu-west-1",
            "subscriptionTitle": "ignored",
            "clientId": "cid",
            "clientSecret": "csec",
            "disabled": false
        }"#;
        let c: Credential = serde_json::from_str(no_region).unwrap();
        assert_eq!(c.region, "us-east-1"); // 缺 region → 默认;不别名 authRegion
    }
}
