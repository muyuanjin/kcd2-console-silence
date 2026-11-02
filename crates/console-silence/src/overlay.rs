//! 规则表的叠加与发布。
//!
//! **没有配置文件。** ASI 出厂一条规则都没有，也不写任何配置文件：规则只有两个来源——
//! 模组在运行时通过 Lua 调 [`crate::bridge`]（会话级，不落盘），或者重新发一版 ASI。
//!
//! 这里仍然是唯一的写侧：任何改动都走"改 spec → 编译 → [`crate::filter::publish`] 换快照"。
//!
//! 加锁顺序固定 `layers → filter`（[`publish_locked`] 与 [`list`] 都遵守），反过来会 ABBA。

use std::sync::{Mutex, OnceLock};

use crate::filter;
use crate::rules::Rules;
use crate::spec::RuleSpec;

/// 规则上限（超出后 `mute` 返回错误）。上限管的是编译时间与内存，不是匹配时间。
pub const MAX_RULES: usize = 64;

struct Layers {
    rules: Vec<RuleSpec>,
    /// 总开关（`false` = 钩子还在，但一行都不吞）。
    enabled: bool,
    /// 观察模式：把每一行落盘（定位"到底哪一行在刷"用）。
    observe: bool,
    /// 只看每行前 N 字节。
    scan_limit: usize,
}

fn defaults() -> Layers {
    Layers {
        rules: Vec::new(),
        enabled: true,
        observe: false,
        scan_limit: 512,
    }
}

static LAYERS: OnceLock<Mutex<Layers>> = OnceLock::new();

fn layers() -> &'static Mutex<Layers> {
    LAYERS.get_or_init(|| Mutex::new(defaults()))
}

/// 持锁发布：调用方已经拿着 [`layers`] 的 guard。
///
/// 所有内部改动都走这个入口，让"持锁再自取锁"这种自锁死结构上不可能出现。
fn publish_locked(layers: &Layers) -> usize {
    let rules = build(&layers.rules, layers);
    let count = rules.rules.len();
    filter::publish(rules);
    count
}

/// 发布当前状态（启动时调一次；此后每次改动都会自己发布）。
pub fn publish() -> usize {
    let guard = layers().lock().unwrap_or_else(|err| err.into_inner());
    publish_locked(&guard)
}

/// spec → 快照。命中计数按 id 从当前快照继承，改规则不会把计数清零。
fn build(specs: &[RuleSpec], layers: &Layers) -> Rules {
    let previous = filter::snapshot_handles();
    let mut rules = Rules {
        enabled: layers.enabled,
        observe: layers.observe,
        scan_limit: layers.scan_limit,
        rules: Vec::new(),
    };
    for spec in specs {
        match spec.compile() {
            Ok(rule) => {
                if let Some((_, hits, last_pass)) =
                    previous.iter().find(|(id, _, _)| *id == spec.id)
                {
                    rule.seed(*hits, *last_pass);
                }
                rules.rules.push(rule);
            }
            Err(message) => kcd2_abi::log::warn(&format!(
                "rule '{}' ({}) not compiled: {message}",
                spec.label, spec.pattern
            )),
        }
    }
    rules
}

/// 加一条规则（同 owner+pattern 已存在时幂等返回旧 id）。
pub fn add(spec: RuleSpec) -> Result<u32, &'static str> {
    let mut guard = layers().lock().unwrap_or_else(|err| err.into_inner());
    if let Some(existing) = guard
        .rules
        .iter()
        .find(|rule| rule.owner == spec.owner && rule.pattern == spec.pattern)
    {
        return Ok(existing.id);
    }
    if guard.rules.len() >= MAX_RULES {
        return Err("full");
    }
    let id = spec.id;
    guard.rules.push(spec);
    publish_locked(&guard);
    Ok(id)
}

/// 删一条规则；幂等（不存在也返回 true）。
pub fn remove(id: u32) -> bool {
    let mut guard = layers().lock().unwrap_or_else(|err| err.into_inner());
    let before = guard.rules.len();
    guard.rules.retain(|rule| rule.id != id);
    if before != guard.rules.len() {
        publish_locked(&guard);
    }
    true
}

/// 清规则（可按 owner）；返回清掉几条。
pub fn clear(owner: Option<&str>) -> usize {
    let mut guard = layers().lock().unwrap_or_else(|err| err.into_inner());
    let before = guard.rules.len();
    match owner {
        Some(owner) => guard.rules.retain(|rule| rule.owner != owner),
        None => guard.rules.clear(),
    }
    let removed = before - guard.rules.len();
    if removed > 0 {
        publish_locked(&guard);
    }
    removed
}

/// 清掉已过期的规则；有变化时重发快照。返回清掉几条（监督线程调用）。
pub fn expire_due(now_ms: u64) -> usize {
    let mut guard = layers().lock().unwrap_or_else(|err| err.into_inner());
    let before = guard.rules.len();
    guard.rules.retain(|rule| !rule.expired(now_ms));
    let removed = before - guard.rules.len();
    if removed > 0 {
        kcd2_abi::log::info(&format!("{removed} rule(s) expired"));
        publish_locked(&guard);
    }
    removed
}

/// 总开关。`false` = 一行都不吞（钩子仍在，便于对比）。
pub fn set_enabled(enabled: bool) {
    let mut guard = layers().lock().unwrap_or_else(|err| err.into_inner());
    guard.enabled = enabled;
    publish_locked(&guard);
}

/// 重复行折叠（同文本连续刷屏的通用兜底）。默认关闭。
pub fn set_fold(enabled: bool) {
    crate::repeat::set_enabled(enabled);
}

/// 观察模式：把每一行写进 `consolesilence.log`。
pub fn set_observe(observe: bool) {
    let mut guard = layers().lock().unwrap_or_else(|err| err.into_inner());
    guard.observe = observe;
    publish_locked(&guard);
}

/// `list()` 的一行：spec 的字段 + 当前命中数。
#[derive(Clone, Debug)]
pub struct Listed {
    /// 稳定句柄。
    pub id: u32,
    /// 识别方式。
    pub kind: &'static str,
    /// 动作。
    pub action: String,
    /// 模式原文。
    pub pattern: String,
    /// 人话名字。
    pub label: String,
    /// 归属。
    pub owner: String,
    /// 寿命（0 = 永久）。
    pub ttl_ms: u64,
    /// 命中次数。
    pub hits: u64,
}

/// 当前生效的规则清单（含命中数），按生效顺序。
#[must_use]
pub fn list() -> Vec<Listed> {
    // 加锁顺序固定 `layers → filter`（与 `publish_locked` 一致）。
    let guard = layers().lock().unwrap_or_else(|err| err.into_inner());
    let hits = filter::snapshot_handles();
    guard
        .rules
        .iter()
        .map(|spec| Listed {
            id: spec.id,
            kind: spec.kind.as_str(),
            action: spec.action.as_text(),
            pattern: spec.pattern.clone(),
            label: spec.label.clone(),
            owner: spec.owner.clone(),
            ttl_ms: spec.ttl_ms,
            hits: hits
                .iter()
                .find(|(id, _, _)| *id == spec.id)
                .map_or(0, |(_, hits, _)| *hits),
        })
        .collect()
}

/// 这一行会被哪条规则吞（只读，非热路径）。
#[must_use]
pub fn which(line: &str) -> Option<u32> {
    filter::which(line.as_bytes())
}

/// 规则条数。
#[must_use]
pub fn count() -> usize {
    layers()
        .lock()
        .unwrap_or_else(|err| err.into_inner())
        .rules
        .len()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::rules::{Action, MatchKind};

    fn spec(owner: &str, pattern: &str) -> RuleSpec {
        RuleSpec::new(MatchKind::Contains, Action::Drop, pattern, None, owner, 0)
    }

    fn reset() -> std::sync::MutexGuard<'static, ()> {
        let serial = filter::serial();
        let mut guard = layers().lock().unwrap_or_else(|err| err.into_inner());
        *guard = defaults();
        publish_locked(&guard);
        serial
    }

    #[test]
    fn the_asi_ships_with_no_rules_at_all() {
        let _serial = reset();
        assert_eq!(count(), 0);
        assert!(list().is_empty());
        // 出厂状态什么都不吞（剩下那层"重复行折叠"不是规则，另算）。
        assert_eq!(filter::decide(b"anything at all"), filter::Outcome::Pass);
    }

    #[test]
    fn rules_take_effect_immediately() {
        let _serial = reset();
        let id = add(spec("", "spam-a")).expect("added");
        assert_eq!(filter::decide(b"xx spam-a yy"), filter::Outcome::Drop(id));
        assert_eq!(filter::decide(b"nothing"), filter::Outcome::Pass);
    }

    #[test]
    fn same_owner_and_pattern_is_idempotent() {
        let _serial = reset();
        let first = add(spec("m1", "same")).expect("added");
        let second = add(spec("m1", "same")).expect("added");
        assert_eq!(first, second);
        assert_eq!(count(), 1);
    }

    #[test]
    fn clear_only_touches_the_named_owner() {
        let _serial = reset();
        let keep = add(spec("mod-a", "a-line")).expect("added");
        add(spec("mod-b", "b-line")).expect("added");
        assert_eq!(clear(Some("mod-b")), 1);
        assert_eq!(filter::decide(b"a-line"), filter::Outcome::Drop(keep));
        assert_eq!(filter::decide(b"b-line"), filter::Outcome::Pass);
    }

    #[test]
    fn expiry_drops_the_rule() {
        let _serial = reset();
        let mut expiring = spec("", "short-lived");
        expiring.ttl_ms = 100;
        expiring.born_ms = 0;
        let id = add(expiring).expect("added");
        assert_eq!(filter::decide(b"short-lived"), filter::Outcome::Drop(id));
        assert_eq!(expire_due(1000), 1);
        assert_eq!(filter::decide(b"short-lived"), filter::Outcome::Pass);
    }

    #[test]
    fn which_is_a_mirror_not_a_probe() {
        let _serial = reset();
        let id = add(spec("", "mirror")).expect("added");
        assert_eq!(which("mirror"), Some(id));
        let before = list().iter().find(|row| row.id == id).map(|row| row.hits);
        let _ = which("mirror");
        let after = list().iter().find(|row| row.id == id).map(|row| row.hits);
        assert_eq!(before, after, "which() must not bump hit counters");
    }

    #[test]
    fn the_switch_turns_everything_off() {
        let _serial = reset();
        add(spec("", "off-me")).expect("added");
        set_enabled(false);
        assert_eq!(filter::decide(b"off-me"), filter::Outcome::Pass);
        set_enabled(true);
        assert!(matches!(
            filter::decide(b"off-me"),
            filter::Outcome::Drop(_)
        ));
    }

    #[test]
    fn rule_cap_is_enforced() {
        let _serial = reset();
        for index in 0..MAX_RULES {
            add(spec("", &format!("p{index}"))).expect("within cap");
        }
        assert_eq!(add(spec("", "one-too-many")), Err("full"));
    }
}
