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

/// 为一条凭据定出机器 ID:**凭据自带 > 配置全局 > 按凭据类型派生**。
///
/// 这是全项目唯一的入口 —— 数据面、令牌刷新、余额查询、模型清单必须**全部**走它。
/// 此前每个模块各写一份,于是同一个账号在数据面报一个 machineId、在余额查询报另一个,
/// 上游在同一时刻看到同一个 Bearer 声称来自两台机器。
///
/// 派生按凭据类型**互斥**分流:
/// - API Key(ksk)凭据 → `sha256("KiroAPIKey/" + ksk)`
/// - OAuth 凭据 → `sha256("KotlinNativeAPI/" + refreshToken)`
///
/// 两条路**不互相回落**。此前 ksk 凭据也走 refreshToken 那条,而 ksk 账号根本没有
/// refreshToken(空串),于是全部落到 `sha256("KotlinNativeAPI/")` 这个**固定常量**——
/// 所有 ksk 账号对上游自称同一台机器,而且这个常量在任何人的部署里都一样。
///
/// 两样材料都没有时返回 `None`:调用方须用一个稳定的兜底(见 [`fallback_for`]),
/// 绝不能把常量指纹发出去。
pub fn for_credential(
    explicit: Option<&str>,
    config_level: Option<&str>,
    is_api_key: bool,
    api_key: Option<&str>,
    refresh_token: &str,
) -> Option<String> {
    if let Some(v) = explicit.and_then(normalize) {
        return Some(v);
    }
    if let Some(v) = config_level.and_then(normalize) {
        return Some(v);
    }
    if is_api_key {
        return api_key.filter(|k| !k.is_empty()).map(derive_from_api_key);
    }
    if refresh_token.is_empty() {
        return None;
    }
    Some(derive(refresh_token))
}

/// 缺派生材料时的兜底机器 ID:`sha256("KiroFallback/" + <凭据 id>)`。
///
/// 按凭据 id 派生而非随机,是为了让同一个账号在**进程重启后仍是同一台机器**——
/// 随机兜底会让每次重启都换一批设备身份,那和"每次刷新换一台机器"是同一个病。
pub fn fallback_for(credential_id: &str) -> String {
    hex::encode(Sha256::digest(
        format!("KiroFallback/{credential_id}").as_bytes(),
    ))
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

    /// ksk 凭据必须走 KiroAPIKey 那条盐,**绝不**回落到 refreshToken 那条。
    ///
    /// 回归:`derive_from_api_key` 曾是死代码(全仓零调用),而 ksk 账号没有 refreshToken
    /// (空串),于是全部落到 `sha256("KotlinNativeAPI/")` 这个固定常量 —— 所有 ksk 账号
    /// 对上游自称同一台机器,且该常量在任何人的部署里都一样。
    #[test]
    fn api_key_credentials_derive_from_their_own_key_never_a_constant() {
        let a = for_credential(None, None, true, Some("ksk_AAA"), "").expect("应派生出来");
        let b = for_credential(None, None, true, Some("ksk_BBB"), "").expect("应派生出来");
        assert_eq!(a, derive_from_api_key("ksk_AAA"));
        assert_ne!(a, b, "两个不同 ksk 必须派生出不同 machineId");
        // 决不能等于"空 refreshToken 派生"那个常量
        assert_ne!(a, derive(""));
        assert_ne!(b, derive(""));
        // 材料都缺 → None,由调用方走按 id 的稳定兜底,而不是把常量发出去
        assert_eq!(for_credential(None, None, true, None, ""), None);
        assert_eq!(for_credential(None, None, false, None, ""), None);
    }

    /// 兜底值按凭据 id 稳定:同一个账号重启前后必须是同一台机器。
    #[test]
    fn fallback_is_stable_per_credential() {
        assert_eq!(fallback_for("7"), fallback_for("7"));
        assert_ne!(fallback_for("7"), fallback_for("8"));
        assert_eq!(fallback_for("7").len(), 64);
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
