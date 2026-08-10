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

/// 数据面端点。**只有一个** —— 真实客户端只打这一个。
///
/// 曾经这里有三个:另外两个是 `codewhisperer.{region}` 这台主机,以及把
/// `AmazonQDeveloperStreamingService.SendMessage` 这个 `x-amz-target` 打在
/// `/generateAssistantResponse` 这条 REST 路径上。两者真实客户端都不产生,而且后者本身
/// 不自洽:`x-amz-target` 是 awsJson 协议的头,真实客户端配它时路径是 `/`。
///
/// 更糟的是它们何时被用:上游一限流(429),我们就**立刻拿同一个账号的令牌**去打这两个
/// 端点。于是"被限流的账号紧接着去探另外两个服务入口"——这是探测行为的形状,不是客户端
/// 行为的形状。收敛成一个之后,限流就只是限流,退避重试即可。
pub fn all(region: &str) -> Vec<Endpoint> {
    vec![Endpoint {
        url: format!("https://q.{region}.amazonaws.com/generateAssistantResponse"),
        origin: "AI_EDITOR",
        target: None,
    }]
}

/// 命名 → 下标。只剩 `kiro` 一个真实端点;`codewhisperer`/`amazonq` 两个历史名字仍被
/// 接受(旧配置不会因此报错),但一律解析到同一个真实端点,不再另发请求。
fn named_index(name: &str) -> Option<usize> {
    match name {
        "kiro" | "codewhisperer" | "amazonq" => Some(0),
        _ => None,
    }
}

/// 选择端点尝试顺序。
///
/// 端点收敛成一个之后,本函数**恒返回那一个**;`preferred` / `fallback` 两个参数保留是为了
/// 不破坏调用方与配置的既有形状(旧配置写着 `codewhisperer` 也不会报错)。
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

/// 该账号实际使用的 region:**优先取 profileArn 里编码的真实 region**,回落 `cred.region`,
/// 再回落 `us-east-1`。
///
/// 全项目**唯一入口**。此前数据面用这套、余额与模型清单直接用裸 `cred.region` —— 于是一个
/// ARN 写 eu-central-1、导入时没给 region 的账号,数据面打 `q.eu-central-1`、余额却打
/// `q.us-east-1`:功能上余额恒查不出(面板一直显示未知额度),wire 上则是同一枚 Bearer 在
/// 同一分钟命中两个 region 的端点 —— 真实客户端不会这么做。
pub fn effective_region(cred: &crate::kiro::credential::Credential) -> String {
    cred.profile_arn
        .as_deref()
        .and_then(region_from_profile_arn)
        .or_else(|| {
            let r = cred.region.trim();
            (!r.is_empty()).then(|| r.to_string())
        })
        .unwrap_or_else(|| "us-east-1".to_string())
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
    fn only_the_real_endpoint_exists_and_is_regionalized() {
        let eps = all("eu-west-1");
        // **只有一个**。曾经这里有三个,另外两个(codewhisperer 主机、带 x-amz-target 的
        // AmazonQ 变体)真实客户端从不产生,却会在被限流后拿同一个账号去打 —— 那是探测
        // 行为的形状。收敛回一个,并把"只有一个"钉死。
        assert_eq!(eps.len(), 1, "数据面端点只能有一个(真实客户端只打这一个)");
        assert!(eps[0].url.contains("q.eu-west-1.amazonaws.com"));
        assert!(eps[0].url.ends_with("/generateAssistantResponse"));
        assert_eq!(eps[0].target, None, "真实客户端不发 x-amz-target");
        assert!(
            !eps.iter().any(|e| e.url.contains("codewhisperer")),
            "codewhisperer 主机真实客户端从不访问"
        );
    }

    #[test]
    fn sorted_auto_returns_the_single_endpoint() {
        let s = sorted("us-east-1", "auto", false);
        assert_eq!(s.len(), 1);
        assert_eq!(s[0].origin, all("us-east-1")[0].origin);
    }

    #[test]
    fn legacy_endpoint_names_still_parse_but_map_to_the_real_one() {
        // 旧配置里可能写着 codewhisperer/amazonq。它们仍被接受(不报错、不回落成"未知"),
        // 但一律解析到唯一的真实端点,不再另发请求到那两台主机。
        for name in ["kiro", "codewhisperer", "amazonq"] {
            let s = sorted("us-east-1", name, false);
            assert_eq!(s.len(), 1, "{name}:只应得到一个端点");
            assert!(s[0].url.contains("q.us-east-1.amazonaws.com"), "{name}");
            assert!(!s[0].url.contains("codewhisperer"), "{name}");
            assert_eq!(s[0].target, None, "{name}");
            assert!(s[0].url.ends_with("/generateAssistantResponse"), "{name}");
        }
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
    fn fallback_flag_no_longer_adds_extra_endpoints() {
        // 开着 fallback 也只有一个端点可打。此前 fallback=true 会把另外两个端点补在后面,
        // 于是一次 429 会被放大成三次上游请求(还都打在真实客户端不用的入口上)。
        let s = sorted("us-east-1", "amazonq", true);
        assert_eq!(s.len(), 1);
        assert!(s[0].url.contains("q.us-east-1.amazonaws.com"));
    }

    /// region 解析必须**全项目一个口径**。
    ///
    /// 回归:此前只有数据面按 profileArn 解析,余额与模型清单直接用裸 `cred.region` —— 一个
    /// ARN 写 eu-central-1、导入时没给 region 的账号,数据面打 q.eu-central-1、余额却打
    /// q.us-east-1:余额恒查不出,而且同一枚 Bearer 在同一分钟命中两个 region 的端点。
    #[test]
    fn effective_region_prefers_the_arn_then_falls_back() {
        let mut c = crate::kiro::credential::tests_support_cred();
        c.region = "us-east-1".into();

        // ARN 里的 region 优先(它才是账号真实所在)
        c.profile_arn = Some("arn:aws:codewhisperer:eu-central-1:1:profile/A".into());
        assert_eq!(effective_region(&c), "eu-central-1");

        // 没有 ARN → 用 cred.region
        c.profile_arn = None;
        assert_eq!(effective_region(&c), "us-east-1");

        // ARN 畸形 → 不把垃圾段当 region,回落 cred.region
        c.profile_arn = Some("arn:aws:codewhisperer:NOT_A_REGION:1:profile/A".into());
        assert_eq!(effective_region(&c), "us-east-1");

        // 两者都缺 → us-east-1,绝不产出空 region(那会拼出 `q..amazonaws.com`,DNS 直接失败)
        c.profile_arn = None;
        c.region = "  ".into();
        assert_eq!(effective_region(&c), "us-east-1");
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
