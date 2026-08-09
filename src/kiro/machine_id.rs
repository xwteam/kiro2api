//! 机器 ID 派生。用于 Kiro 伪装 UA。
//! 盐 "KotlinNativeAPI/{refresh_token}" 为功能必需的 wire 事实(照观测,无公开文档);
//! 派生/归一代码为本项目自写。
use sha2::{Digest, Sha256};

/// 由 API Key 派生机器 ID:hex(SHA256("KiroAPIKey/" + ksk))。
///
/// 盐与 OAuth 那条**不同且不可互换**(照观测):同一账号用 ksk 和用 refreshToken
/// 派生出的机器 ID 本就该不同,混用会让上游看到对不上的设备标识。
pub fn derive_from_api_key(api_key: &str) -> String {
    let salted = format!("KiroAPIKey/{api_key}");
    hex::encode(Sha256::digest(salted.as_bytes()))
}

/// 由 refresh_token 派生机器 ID:hex(SHA256("KotlinNativeAPI/" + rt))。
pub fn derive(refresh_token: &str) -> String {
    let salted = format!("KotlinNativeAPI/{refresh_token}");
    hex::encode(Sha256::digest(salted.as_bytes()))
}

/// 归一化一个外部提供的机器 ID:64 位十六进制原样(转小写);32 位十六进制复制成 64;否则 None。
pub fn normalize(raw: &str) -> Option<String> {
    let is_hex = |s: &str| s.chars().all(|c| c.is_ascii_hexdigit());
    match raw.len() {
        64 if is_hex(raw) => Some(raw.to_ascii_lowercase()),
        32 if is_hex(raw) => {
            let low = raw.to_ascii_lowercase();
            Some(format!("{low}{low}"))
        }
        _ => None,
    }
}

/// explicit 能归一则用它,否则由 refresh_token 派生。
pub fn resolve(explicit: Option<&str>, refresh_token: &str) -> String {
    resolve_with_config(explicit, None, refresh_token)
}

/// 三级优先:凭据自带 > 配置全局 > 由 refresh_token 派生。
///
/// 配置级是此前缺的一层。它的用途是让整池共用一个 machineId ——
/// 部署在单机、希望对上游呈现"一台机器"时用得上;不设则维持每账号各自派生(默认)。
pub fn resolve_with_config(
    explicit: Option<&str>,
    config_level: Option<&str>,
    refresh_token: &str,
) -> String {
    explicit
        .and_then(normalize)
        .or_else(|| config_level.and_then(normalize))
        .unwrap_or_else(|| derive(refresh_token))
}

#[cfg(test)]
mod tests {
    use super::*;
    use sha2::{Digest, Sha256};

    #[test]
    fn derive_is_sha256_of_salted_token_hex() {
        // 独立用同样原语算期望值,钉住"盐前缀 + sha256 + hex"这套组合
        let rt = "sample-refresh-token";
        let expect = hex::encode(Sha256::digest(format!("KotlinNativeAPI/{rt}").as_bytes()));
        assert_eq!(derive(rt), expect);
        assert_eq!(derive(rt).len(), 64);
        assert!(derive(rt).chars().all(|c| c.is_ascii_hexdigit()));
        assert_ne!(derive(rt), derive("other")); // 不同输入不同
    }

    #[test]
    fn normalize_cases() {
        let full = "a".repeat(64);
        assert_eq!(normalize(&full), Some(full.clone()));
        let uuid32 = "0123456789abcdef0123456789abcdef"; // 32 hex
        assert_eq!(normalize(uuid32), Some(format!("{uuid32}{uuid32}")));
        assert_eq!(normalize("xyz"), None); // 非法
        assert_eq!(normalize(&"a".repeat(40)), None); // 长度不符
    }

    #[test]
    fn resolve_prefers_explicit_then_derives() {
        let full = "b".repeat(64);
        assert_eq!(resolve(Some(&full), "rt"), full); // explicit 合法
        assert_eq!(resolve(Some("bad"), "rt"), derive("rt")); // explicit 非法→derive
        assert_eq!(resolve(None, "rt"), derive("rt")); // 无 explicit→derive
    }

    /// 三级优先:凭据自带 > 配置全局 > 由 refresh_token 派生。
    /// 配置级是此前缺的一层。
    #[test]
    fn machine_id_resolution_is_three_tiered() {
        // machineId 必须是 32/64 位十六进制(见 `normalize`),故夹具用合法值。
        let cred_mid = "a".repeat(64);
        let cfg_mid = "b".repeat(64);
        // 凭据级最优先
        assert_eq!(
            resolve_with_config(Some(&cred_mid), Some(&cfg_mid), "rt"),
            cred_mid
        );
        // 凭据缺 → 用配置级
        assert_eq!(resolve_with_config(None, Some(&cfg_mid), "rt"), cfg_mid);
        // 配置级是非法值 → 不采用,回落派生(别把垃圾值当身份发出去)
        assert_eq!(
            resolve_with_config(None, Some("not-hex"), "rt"),
            derive("rt")
        );
        // 都缺 → 派生,且与 refresh_token 绑定
        let derived = resolve_with_config(None, None, "rt");
        assert_eq!(derived, derive("rt"));
        assert_ne!(
            derived,
            derive("other-rt"),
            "不同 rt 必须派生出不同 machineId"
        );
    }
}
