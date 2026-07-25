//! 上游 `getUsageLimits` 响应的 serde 形状 + 额度累加口径。
//!
//! 字段名为照观测的 wire 事实(camelCase),`#[serde(default)]` 使缺字段/空对象也能解析。
//! "总限额/总已用/剩余"的口径:基础额度 + 处于 `ACTIVE` 的 bonus + 处于 `ACTIVE` 的 free
//! trial,均取 `*WithPrecision` 精确值;这套累加规则为本项目自写。

use serde::Deserialize;

/// `getUsageLimits` 顶层响应。
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct UsageLimitsResponse {
    /// 下次重置时刻(Unix 秒,可能带小数)。
    #[serde(default)]
    pub next_date_reset: Option<f64>,
    /// 订阅信息(标题等)。
    #[serde(default)]
    pub subscription_info: Option<SubscriptionInfo>,
    /// 用量明细(通常取首个)。
    #[serde(default)]
    pub usage_breakdown_list: Vec<UsageBreakdown>,
}

/// 订阅信息。
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SubscriptionInfo {
    #[serde(default)]
    pub subscription_title: Option<String>,
}

/// 单条用量明细。
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct UsageBreakdown {
    #[serde(default)]
    pub current_usage_with_precision: f64,
    #[serde(default)]
    pub usage_limit_with_precision: f64,
    /// 奖励额度(可叠加,仅计激活的)。
    #[serde(default)]
    pub bonuses: Vec<Bonus>,
    /// 免费试用(可叠加,仅计激活的)。
    #[serde(default)]
    pub free_trial_info: Option<FreeTrialInfo>,
    /// 明细级重置时刻(顶层缺失时回落到此)。
    #[serde(default)]
    pub next_date_reset: Option<f64>,
}

/// 奖励额度条目。
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Bonus {
    #[serde(default)]
    pub current_usage: f64,
    #[serde(default)]
    pub usage_limit: f64,
    /// 状态(`ACTIVE`/`EXPIRED`);仅 `ACTIVE` 计入累加。
    #[serde(default)]
    pub status: Option<String>,
}

/// 免费试用信息。
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct FreeTrialInfo {
    #[serde(default)]
    pub current_usage_with_precision: f64,
    #[serde(default)]
    pub usage_limit_with_precision: f64,
    /// 状态(`ACTIVE`/`EXPIRED`);仅 `ACTIVE` 计入累加。
    #[serde(default)]
    pub free_trial_status: Option<String>,
}

/// 判定某状态串是否为激活态(大小写敏感,与上游一致)。
fn is_active(status: &Option<String>) -> bool {
    status.as_deref() == Some("ACTIVE")
}

impl UsageLimitsResponse {
    /// 订阅标题(如 "KIRO PRO+"),无则 None。
    pub fn subscription_title(&self) -> Option<&str> {
        self.subscription_info
            .as_ref()
            .and_then(|s| s.subscription_title.as_deref())
    }

    /// 首条用量明细。
    fn primary(&self) -> Option<&UsageBreakdown> {
        self.usage_breakdown_list.first()
    }

    /// 总限额:基础 + 激活 free trial + 激活 bonus(精确值)。无明细 → 0。
    pub fn usage_limit(&self) -> f64 {
        let Some(b) = self.primary() else { return 0.0 };
        let mut total = b.usage_limit_with_precision;
        if let Some(t) = &b.free_trial_info
            && is_active(&t.free_trial_status)
        {
            total += t.usage_limit_with_precision;
        }
        for bonus in &b.bonuses {
            if is_active(&bonus.status) {
                total += bonus.usage_limit;
            }
        }
        total
    }

    /// 总已用:基础 + 激活 free trial + 激活 bonus(精确值)。无明细 → 0。
    pub fn current_usage(&self) -> f64 {
        let Some(b) = self.primary() else { return 0.0 };
        let mut total = b.current_usage_with_precision;
        if let Some(t) = &b.free_trial_info
            && is_active(&t.free_trial_status)
        {
            total += t.current_usage_with_precision;
        }
        for bonus in &b.bonuses {
            if is_active(&bonus.status) {
                total += bonus.current_usage;
            }
        }
        total
    }

    /// 剩余额度(限额 - 已用,下限 0)。
    pub fn remaining(&self) -> f64 {
        (self.usage_limit() - self.current_usage()).max(0.0)
    }

    /// 已用占比(百分数 0..=100);限额为 0 时返回 0。
    pub fn usage_percentage(&self) -> f64 {
        let limit = self.usage_limit();
        if limit <= 0.0 {
            0.0
        } else {
            ((self.current_usage() / limit) * 100.0).clamp(0.0, 100.0)
        }
    }

    /// 下次重置时刻(Unix 秒,截断为整数);顶层缺失时回落到首条明细。
    pub fn next_reset_at(&self) -> Option<i64> {
        self.next_date_reset
            .or_else(|| self.primary().and_then(|b| b.next_date_reset))
            .map(|v| v as i64)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const FULL: &str = r#"{
        "nextDateReset": 1784462400,
        "subscriptionInfo": { "subscriptionTitle": "KIRO PRO+" },
        "usageBreakdownList": [{
            "currentUsageWithPrecision": 10.5,
            "usageLimitWithPrecision": 100.0,
            "bonuses": [
                { "currentUsage": 1.0, "usageLimit": 5.0, "status": "ACTIVE" },
                { "currentUsage": 9.0, "usageLimit": 50.0, "status": "EXPIRED" }
            ],
            "freeTrialInfo": {
                "currentUsageWithPrecision": 2.0,
                "usageLimitWithPrecision": 20.0,
                "freeTrialStatus": "ACTIVE"
            }
        }]
    }"#;

    #[test]
    fn accumulates_active_bonus_and_trial() {
        let r: UsageLimitsResponse = serde_json::from_str(FULL).unwrap();
        // 限额:100 + 20(trial ACTIVE) + 5(bonus ACTIVE);50 的 EXPIRED bonus 不计
        assert_eq!(r.usage_limit(), 125.0);
        // 已用:10.5 + 2.0(trial) + 1.0(bonus ACTIVE)
        assert_eq!(r.current_usage(), 13.5);
        assert_eq!(r.remaining(), 111.5);
        assert_eq!(r.subscription_title(), Some("KIRO PRO+"));
        assert_eq!(r.next_reset_at(), Some(1_784_462_400));
    }

    #[test]
    fn percentage_and_clamp() {
        let r: UsageLimitsResponse = serde_json::from_str(FULL).unwrap();
        // 13.5 / 125 * 100 = 10.8
        assert!((r.usage_percentage() - 10.8).abs() < 1e-9);
    }

    #[test]
    fn empty_or_missing_fields_default_to_zero() {
        let r: UsageLimitsResponse = serde_json::from_str("{}").unwrap();
        assert_eq!(r.usage_limit(), 0.0);
        assert_eq!(r.current_usage(), 0.0);
        assert_eq!(r.remaining(), 0.0);
        assert_eq!(r.usage_percentage(), 0.0);
        assert_eq!(r.subscription_title(), None);
        assert_eq!(r.next_reset_at(), None);
    }

    #[test]
    fn remaining_never_negative() {
        let over = r#"{"usageBreakdownList":[{"currentUsageWithPrecision":200.0,"usageLimitWithPrecision":100.0}]}"#;
        let r: UsageLimitsResponse = serde_json::from_str(over).unwrap();
        assert_eq!(r.remaining(), 0.0);
        assert_eq!(r.usage_percentage(), 100.0);
    }

    #[test]
    fn next_reset_falls_back_to_breakdown() {
        let j = r#"{"usageBreakdownList":[{"usageLimitWithPrecision":1.0,"nextDateReset":42}]}"#;
        let r: UsageLimitsResponse = serde_json::from_str(j).unwrap();
        assert_eq!(r.next_reset_at(), Some(42));
    }
}
