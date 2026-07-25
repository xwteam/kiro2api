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
    #[serde(
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
}

impl Credential {
    /// 是否已过期(到点即算)。
    pub fn is_expired(&self, now_unix: u64) -> bool {
        self.expires_at_unix <= now_unix
    }
    /// 是否即将在 margin_secs 内过期。
    pub fn expires_soon(&self, now_unix: u64, margin_secs: u64) -> bool {
        self.expires_at_unix <= now_unix.saturating_add(margin_secs)
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
    let tmp = unique_tmp_path(path);
    let data = serde_json::to_vec_pretty(creds)?;
    // 写失败/中途 panic 时清理残留 tmp,避免堆积;成功 rename 后 tmp 已消失。
    let res = (|| -> std::io::Result<()> {
        let f = std::fs::File::create(&tmp)?;
        {
            use std::io::Write;
            let mut w = std::io::BufWriter::new(&f);
            w.write_all(&data)?;
            w.flush()?;
        }
        f.sync_all()?; // fsync:确保数据真正落盘,再做 rename
        std::fs::rename(&tmp, path)?;
        Ok(())
    })();
    if res.is_err() {
        let _ = std::fs::remove_file(&tmp);
    }
    // best-effort 清理孤儿 tmp(不影响本次 save 结果)。
    sweep_stale_tmp(path);
    res.map_err(Into::into)
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
    let snapshot = {
        let pool_lock = pool.lock().await;
        pool_lock.snapshot_credentials()
    };
    save(path, &snapshot)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample() -> Credential {
        Credential {
            id: "a1".into(),
            access_token: "at".into(),
            refresh_token: "rt".into(),
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
        let _ = std::fs::remove_file(&p);
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
