//! Kiro 数据面端点表与选择/回退。
//! 端点 URL/origin/X-Amz-Target 为照观测的 wire 事实(真机接线时据实核对);
//! 端点表组织与选择顺序为本项目自写。

/// 一个数据面端点。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Endpoint {
    pub url: String,
    pub origin: &'static str,
    pub target: Option<&'static str>,
}

/// 三个端点(region 化,固定顺序:Kiro IDE、CodeWhisperer、AmazonQ)。
pub fn all(region: &str) -> Vec<Endpoint> {
    vec![
        Endpoint {
            url: format!("https://q.{region}.amazonaws.com/generateAssistantResponse"),
            origin: "AI_EDITOR",
            target: None,
        },
        Endpoint {
            url: format!("https://codewhisperer.{region}.amazonaws.com/generateAssistantResponse"),
            origin: "AI_EDITOR",
            target: Some("AmazonCodeWhispererStreamingService.GenerateAssistantResponse"),
        },
        Endpoint {
            url: format!("https://q.{region}.amazonaws.com/generateAssistantResponse"),
            origin: "AI_EDITOR",
            target: Some("AmazonQDeveloperStreamingService.SendMessage"),
        },
    ]
}

/// 命名 → 在 all() 里的下标。
fn named_index(name: &str) -> Option<usize> {
    match name {
        "kiro" => Some(0),
        "codewhisperer" => Some(1),
        "amazonq" => Some(2),
        _ => None,
    }
}

/// 选择端点尝试顺序:
/// - `preferred` 为 `""`/`auto`/未知 → 全部三个原序(忽略 fallback);
/// - 命名且 `fallback=false` → 只该一个;
/// - 命名且 `fallback=true` → 该一个在前、其余按原序补后。
pub fn sorted(region: &str, preferred: &str, fallback: bool) -> Vec<Endpoint> {
    let eps = all(region);
    match named_index(preferred) {
        Some(i) if !fallback => vec![eps[i].clone()],
        Some(i) => {
            let mut out = vec![eps[i].clone()];
            for (j, e) in eps.iter().enumerate() {
                if j != i {
                    out.push(e.clone());
                }
            }
            out
        }
        None => eps,
    }
}

/// 校验字符串是否长得像 AWS region:`^[a-z]{2}-[a-z]+-\d+$`
/// (两位小写字母国别 - 一段小写字母方位 - 一位以上数字,如 us-east-1 / ap-northeast-1)。
/// 无正则依赖,手写扫描等价校验。
fn looks_like_region(s: &str) -> bool {
    let mut segs = s.split('-');
    // 第 1 段:恰好两位小写字母。
    let country = match segs.next() {
        Some(c) if c.len() == 2 && c.bytes().all(|b| b.is_ascii_lowercase()) => c,
        _ => return false,
    };
    let _ = country;
    // 中间段:一个或多个,每段一位以上小写字母。至少要有一个方位段。
    let mut dir_segs = 0usize;
    // 最后一段必须是一位以上数字;倒数第二起才是方位段。先收集剩余段。
    let rest: Vec<&str> = segs.collect();
    if rest.len() < 2 {
        return false; // 需要至少 [方位, 数字]
    }
    let (num, dirs) = rest.split_last().unwrap();
    if num.is_empty() || !num.bytes().all(|b| b.is_ascii_digit()) {
        return false;
    }
    for d in dirs {
        if d.is_empty() || !d.bytes().all(|b| b.is_ascii_lowercase()) {
            return false;
        }
        dir_segs += 1;
    }
    dir_segs >= 1
}

/// 从 profileArn 取 region(arn:aws:codewhisperer:{region}:{acct}:profile/{id})。
/// #13:仅当第 4 段形如合法 region(`^[a-z]{2}-[a-z]+-\d+$`)时才采纳,否则返回 None
/// (交给调用方回落 cred.region / us-east-1),避免把畸形 ARN 的垃圾段当 region 拼进端点。
pub fn region_from_profile_arn(arn: &str) -> Option<String> {
    let parts: Vec<&str> = arn.splitn(6, ':').collect();
    if parts.len() >= 4 && parts[0] == "arn" && looks_like_region(parts[3]) {
        Some(parts[3].to_string())
    } else {
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn all_three_regionalized() {
        let eps = all("eu-west-1");
        assert_eq!(eps.len(), 3);
        assert!(eps[0].url.contains("q.eu-west-1.amazonaws.com"));
        assert_eq!(eps[0].target, None); // Kiro IDE 无 target
        assert!(eps[1].url.contains("codewhisperer.eu-west-1"));
        assert!(eps[1].target.is_some());
    }

    #[test]
    fn sorted_auto_returns_all_in_order() {
        let s = sorted("us-east-1", "auto", false);
        assert_eq!(s.len(), 3);
        assert_eq!(s[0].origin, all("us-east-1")[0].origin);
    }

    #[test]
    fn sorted_named_no_fallback_is_single() {
        let s = sorted("us-east-1", "codewhisperer", false);
        assert_eq!(s.len(), 1);
        assert!(s[0].url.contains("codewhisperer"));
        // 真机契约:三个端点 path 都是 /generateAssistantResponse(非裸 /)。
        assert!(s[0].url.ends_with("/generateAssistantResponse"));
    }

    #[test]
    fn all_endpoints_use_generate_assistant_response_path() {
        let eps = all("us-east-1");
        for e in &eps {
            assert!(
                e.url.ends_with("/generateAssistantResponse"),
                "endpoint url must end with /generateAssistantResponse: {}",
                e.url
            );
        }
    }

    #[test]
    fn sorted_named_with_fallback_puts_it_first() {
        let s = sorted("us-east-1", "amazonq", true);
        assert_eq!(s.len(), 3);
        assert!(s[0].target.unwrap().contains("AmazonQ"));
    }

    #[test]
    fn region_from_arn() {
        assert_eq!(
            region_from_profile_arn("arn:aws:codewhisperer:ap-northeast-1:123:profile/x"),
            Some("ap-northeast-1".into())
        );
        assert_eq!(region_from_profile_arn("not-an-arn"), None);
    }

    /// #13:第 4 段不像 region 时返回 None(不把垃圾段当 region)。
    #[test]
    fn region_from_arn_rejects_non_region_segment() {
        // 第 4 段是空/非 region 形状 → None
        assert_eq!(
            region_from_profile_arn("arn:aws:codewhisperer::123:profile/x"),
            None
        );
        assert_eq!(
            region_from_profile_arn("arn:aws:codewhisperer:profile:123:profile/x"),
            None
        );
        assert_eq!(
            region_from_profile_arn("arn:aws:codewhisperer:US-EAST-1:123:profile/x"),
            None
        ); // 大写
        assert_eq!(
            region_from_profile_arn("arn:aws:codewhisperer:us-east:123:profile/x"),
            None
        ); // 缺数字段
        assert_eq!(
            region_from_profile_arn("arn:aws:codewhisperer:useast1:123:profile/x"),
            None
        ); // 无分隔
        assert_eq!(
            region_from_profile_arn("arn:aws:codewhisperer:u-east-1:123:profile/x"),
            None
        ); // 国别非两位
    }

    /// #13:各类合法 region 形状都被接受(含多方位段如 ap-southeast-1)。
    #[test]
    fn region_from_arn_accepts_valid_region_shapes() {
        for r in [
            "us-east-1",
            "eu-west-2",
            "ap-northeast-1",
            "ap-southeast-3",
            "me-central-1",
        ] {
            let arn = format!("arn:aws:codewhisperer:{r}:123456789012:profile/ABC");
            assert_eq!(
                region_from_profile_arn(&arn),
                Some(r.to_string()),
                "region {r} 应被接受"
            );
        }
    }

    #[test]
    fn looks_like_region_unit() {
        assert!(looks_like_region("us-east-1"));
        assert!(looks_like_region("ap-northeast-1"));
        assert!(!looks_like_region(""));
        assert!(!looks_like_region("us-east"));
        assert!(!looks_like_region("us-east-"));
        assert!(!looks_like_region("us--1"));
        assert!(!looks_like_region("profile"));
    }
}
