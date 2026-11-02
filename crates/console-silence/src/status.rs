//! 常开计数与状态文件（`ConsoleSilence.status.json`）。
//!
//! ASI 不能要求用户去翻控制台才能知道"它到底有没有在工作"，所以这里把
//! 进/出/吞的条数、钩子状态、每条规则的命中数与几条被吞样本写成一份 JSON，
//! 由后台监督线程周期性刷新。钩子热路径只做 `fetch_add`（无锁、无分配）。

use core::sync::atomic::{AtomicU64, Ordering};

static SEEN: AtomicU64 = AtomicU64::new(0);
static DROPPED: AtomicU64 = AtomicU64::new(0);
static PASSED: AtomicU64 = AtomicU64::new(0);
static INSTALLS: AtomicU64 = AtomicU64::new(0);
static REARMED: AtomicU64 = AtomicU64::new(0);
static COMMANDS_SILENCED: AtomicU64 = AtomicU64::new(0);

/// 记一条被吞的行（热路径）。
///
/// 这里**只**做两次原子加：曾经还在这里加样本文本（全局 Mutex + 逐字比较 + `to_string`），
/// 那是把一把锁和一次分配放进了最热的分支。想看"到底哪行被吞"用观察模式（`observe = 1`），
/// 它本来就把每一行落盘。
pub fn count_dropped() {
    SEEN.fetch_add(1, Ordering::Relaxed);
    DROPPED.fetch_add(1, Ordering::Relaxed);
}

/// 记一条被"重复行折叠"吃掉的行（热路径；只加 `seen`，折叠计数在 `repeat`）。
pub fn count_folded() {
    SEEN.fetch_add(1, Ordering::Relaxed);
}

/// 记一条放行的行（热路径）。
pub fn count_passed() {
    SEEN.fetch_add(1, Ordering::Relaxed);
    PASSED.fetch_add(1, Ordering::Relaxed);
}

/// 记一次钩子安装。
pub fn count_install() {
    INSTALLS.fetch_add(1, Ordering::Relaxed);
}

/// 记一次"钩子被挤掉后重新装回"。
pub fn count_rearm() {
    REARMED.fetch_add(1, Ordering::Relaxed);
}

/// 记一条被静音执行的控制台命令（`ExecuteString` 的 `bSilentMode` 被翻成真）。
pub fn count_command_silenced() {
    COMMANDS_SILENCED.fetch_add(1, Ordering::Relaxed);
}

/// 计数器快照。
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Snapshot {
    /// 被"重复行折叠"吃掉的行数。
    pub repeat_suppressed: u64,
    /// 被静音执行（不回显）的控制台命令数。
    pub commands_silenced: u64,
    /// 被"重复行折叠"吃掉的行数（已含在 `repeat_suppressed` 之外）。
    /// 进过钩子的行数。
    pub seen: u64,
    /// 被吞的行数。
    pub dropped: u64,
    /// 放行的行数。
    pub passed: u64,
    /// 钩子安装次数。
    pub installs: u64,
    /// 钩子被外力还原后的重装次数。
    pub rearmed: u64,
}

/// 读当前计数。
#[must_use]
pub fn snapshot() -> Snapshot {
    Snapshot {
        repeat_suppressed: crate::repeat::suppressed(),
        commands_silenced: COMMANDS_SILENCED.load(Ordering::Relaxed),
        seen: SEEN.load(Ordering::Relaxed),
        dropped: DROPPED.load(Ordering::Relaxed),
        passed: PASSED.load(Ordering::Relaxed),
        installs: INSTALLS.load(Ordering::Relaxed),
        rearmed: REARMED.load(Ordering::Relaxed),
    }
}

/// 渲染状态 JSON 所需的额外上下文（由监督线程提供）。
pub struct Context<'a> {
    /// 钩子状态（`installed` / `pending` / `disabled` / `lost`）。
    pub hook_state: &'a str,
    /// 原 `PrintLine` 地址（0 = 未知）。
    pub original: usize,
    /// 原 `ExecuteString` 地址（0 = 未装命令钩子）。
    pub original_command: usize,
    /// 当前 vtable 槽位里的地址。
    pub current: usize,
    /// `WHGame.dll` 基址（0 = 未加载）。
    pub module_base: usize,

    /// 规则开关快照。
    pub enabled: bool,
    /// 观察模式是否打开。
    pub observe: bool,
    /// 是否真的在过滤（配置可读且至少一条规则）。
    pub rule_count: usize,
    /// 控制台当前是否显示（`IConsole::GetStatus`）——命令回显只在显示时发生。
    pub console_open: bool,
    /// 每规则命中数。
    pub rule_hits: &'a [(String, u64)],
}

/// 把计数与上下文渲染成 JSON（不引入 serde：字段固定、无嵌套）。
#[must_use]
pub fn render(snapshot: &Snapshot, context: &Context<'_>) -> String {
    let mut text = String::with_capacity(1024);
    text.push_str("{\n");
    text.push_str("  \"schema\": 1,\n");
    text.push_str(&format!(
        "  \"hookState\": {},\n",
        quote(context.hook_state)
    ));
    text.push_str(&format!(
        "  \"hookOriginal\": \"0x{:X}\",\n  \"hookCommand\": \"0x{:X}\",\n  \"hookCurrent\": \"0x{:X}\",\n  \"moduleBase\": \"0x{:X}\",\n",
        context.original, context.original_command, context.current, context.module_base
    ));

    text.push_str(&format!(
        "  \"enabled\": {},\n  \"observe\": {},\n  \"ruleCount\": {},\n  \"consoleOpen\": {},\n",
        context.enabled, context.observe, context.rule_count, context.console_open
    ));
    text.push_str(&format!(
        "  \"seen\": {},\n  \"dropped\": {},\n  \"passed\": {},\n  \"repeatSuppressed\": {},\n  \"commandsSilenced\": {},\n  \"installs\": {},\n  \"rearmed\": {},\n",
        snapshot.seen,
        snapshot.dropped,
        snapshot.passed,
        snapshot.repeat_suppressed,
        snapshot.commands_silenced,
        snapshot.installs,
        snapshot.rearmed
    ));
    text.push_str("  \"ruleHits\": {");
    for (index, (label, hits)) in context.rule_hits.iter().enumerate() {
        if index > 0 {
            text.push_str(", ");
        }
        text.push_str(&format!("{}: {}", quote(label), hits));
    }
    text.push_str("},\n");
    text.push_str("}\n");
    text
}

fn quote(value: &str) -> String {
    let mut out = String::with_capacity(value.len() + 2);
    out.push('"');
    for ch in value.chars() {
        match ch {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            other if (other as u32) < 0x20 => out.push_str(&format!("\\u{:04x}", other as u32)),
            other => out.push(other),
        }
    }
    out.push('"');
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn render_is_parseable_shape() {
        let snapshot = Snapshot {
            repeat_suppressed: 3,
            commands_silenced: 12,
            seen: 7,
            dropped: 5,
            passed: 2,
            installs: 1,
            rearmed: 0,
        };
        let hits = vec![("rule one".to_string(), 5u64)];
        let context = Context {
            hook_state: "installed",
            original: 0x1800_1234,
            original_command: 0x1800_5678,
            current: 0x7FF6_5678,
            module_base: 0x1800_0000,
            enabled: true,
            observe: false,
            rule_count: 1,
            console_open: true,
            rule_hits: &hits,
        };
        let text = render(&snapshot, &context);
        assert!(text.starts_with('{'));
        assert!(text.contains("\"seen\": 7"));
        assert!(text.contains("\"repeatSuppressed\": 3"));
        assert!(text.contains("\"commandsSilenced\": 12"));
        assert!(text.contains("\"consoleOpen\": true"));
        assert!(text.contains("\"hookOriginal\": \"0x18001234\""));

        assert!(text.contains("\"ruleHits\": {\"rule one\": 5}"));
        assert!(text.trim_end().ends_with('}'));
    }

    #[test]
    fn counters_accumulate() {
        let before = snapshot();
        count_passed();
        count_dropped();
        let after = snapshot();
        assert_eq!(after.passed, before.passed + 1);
        assert_eq!(after.dropped, before.dropped + 1);
        assert_eq!(after.seen, before.seen + 2);
    }
}
