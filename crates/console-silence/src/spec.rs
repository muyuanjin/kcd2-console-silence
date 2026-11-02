//! 规则**声明**（spec）：可克隆的纯数据，是三层叠加的真相。
//!
//! 与 [`crate::rules::Rule`] 的分工：spec 是"作者写了什么"，`Rule` 是"编译出来的
//! 匹配器"。spec 可以随便存、改、清；`Rule` 只在发布快照时构建，构建失败（正则写错）
//! 在**加入的那一刻**就报错，不会留到某次打印才炸。

use crate::rules::{next_id, Action, MatchKind, Rule};

/// 一条规则的声明。
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RuleSpec {
    /// 稳定句柄。
    pub id: u32,
    /// 怎么认。
    pub kind: MatchKind,
    /// 认出来怎么办。
    pub action: Action,
    /// 认什么。
    pub pattern: String,
    /// 人话名字。
    pub label: String,
    /// 归属（`clear(owner)` 只清自己的）。
    pub owner: String,
    /// 寿命：`0` = 永不结束；否则从 [`RuleSpec::born_ms`] 起算。
    pub ttl_ms: u64,
    /// 创建时刻（`filter::now_ms()` 口径）。
    pub born_ms: u64,
}

impl RuleSpec {
    /// 造一条新规则的声明（自动分配 id 与出生时刻）。
    #[must_use]
    pub fn new(
        kind: MatchKind,
        action: Action,
        pattern: &str,
        label: Option<&str>,
        owner: &str,
        ttl_ms: u64,
    ) -> RuleSpec {
        RuleSpec {
            id: next_id(),
            kind,
            action,
            pattern: pattern.to_string(),
            label: label.map_or_else(|| pattern.to_string(), str::to_string),
            owner: owner.to_string(),
            ttl_ms,
            born_ms: crate::filter::now_ms(),
        }
    }

    /// 这条规则此刻是否已经过期。
    #[must_use]
    pub fn expired(&self, now_ms: u64) -> bool {
        self.ttl_ms > 0 && now_ms.saturating_sub(self.born_ms) >= self.ttl_ms
    }

    /// 编译成匹配器；模式非法时返回人话原因（调用方当场拒绝，不装半条）。
    pub fn compile(&self) -> Result<Rule, String> {
        Rule::new(
            self.id,
            self.kind,
            self.action,
            &self.pattern,
            Some(&self.label),
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn spec(ttl_ms: u64) -> RuleSpec {
        RuleSpec::new(MatchKind::Contains, Action::Drop, "spam", None, "", ttl_ms)
    }

    #[test]
    fn ttl_zero_never_expires() {
        let rule = spec(0);
        assert!(!rule.expired(u64::MAX / 2));
    }

    #[test]
    fn ttl_expires_at_the_deadline() {
        let mut rule = spec(1000);
        rule.born_ms = 500;
        assert!(!rule.expired(1499));
        assert!(rule.expired(1500));
    }

    #[test]
    fn ids_come_from_one_space() {
        let first = spec(0);
        let second = spec(0);
        assert_ne!(first.id, second.id);
    }

    #[test]
    fn label_defaults_to_pattern() {
        assert_eq!(spec(0).label, "spam");
    }

    #[test]
    fn compile_reports_bad_patterns() {
        let mut rule = spec(0);
        rule.kind = MatchKind::Regex;
        rule.pattern = "([".to_string();
        assert!(rule.compile().unwrap_err().contains("bad pattern"));
    }
}
