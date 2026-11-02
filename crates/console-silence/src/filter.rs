//! 活规则表的**发布点**：单指针快照 + 读侧守卫 + 退休队列。
//!
//! 打印路径只做三件事：读一次指针、匹配、加减一个原子计数。没有锁、没有分配、
//! 没有 `from_utf8_lossy`、没有 IO（观察模式是唯一的例外，且是显式 opt-in）。
//!
//! 内存回收：旧快照绝不可以在钩子线程里释放（`RegexSet` 析构是几百 µs 量级）。
//! 释放由监督线程在 [`collect_retired`] 里做，判据是"此刻没有读者"——读者在**读指针
//! 之前**自增、读完之后自减，所以 `READERS == 0` 时不可能有人还握着旧指针。

use core::sync::atomic::{AtomicPtr, AtomicU64, AtomicUsize, Ordering};
use std::sync::{Mutex, OnceLock};
use std::time::Instant;

use crate::repeat;
use crate::rules::Rules;
use crate::status;

/// 当前快照（`null` = 还没发布过）。
static LIVE: AtomicPtr<Rules> = AtomicPtr::new(core::ptr::null_mut());
/// 正在读快照的线程数（含钩子）。
static READERS: AtomicUsize = AtomicUsize::new(0);
/// 等待回收的旧快照。
///
/// 裸指针默认不是 `Send`；这里包一层并**用纪律**保证线程安全：队列只在 [`publish`]
/// （持锁 push）与 [`collect_retired`]（持锁 drain，且已确认 `READERS == 0`）里出现，
/// 指针本身从不跨线程解引用。
struct Retired(Vec<*mut Rules>);
// SAFETY: 见 `Retired` 的文档——指针只在上述两处持锁访问，且释放前确认无读者。
unsafe impl Send for Retired {}

static RETIRED: Mutex<Retired> = Mutex::new(Retired(Vec::new()));
/// 观察模式的落盘预算。
static OBSERVED_LINES: AtomicU64 = AtomicU64::new(0);
static OBSERVE_CLOSED: AtomicUsize = AtomicUsize::new(0);

/// 观察模式最多往日志里灌多少字节（防止用户忘了关，把盘写满）。
const OBSERVE_BUDGET_BYTES: u64 = 8 * 1024 * 1024;

/// 进程内单调毫秒（throttle 与 TTL 都用它）。
#[must_use]
pub fn now_ms() -> u64 {
    static ORIGIN: OnceLock<Instant> = OnceLock::new();
    ORIGIN.get_or_init(Instant::now).elapsed().as_millis() as u64
}

struct ReaderGuard;

impl ReaderGuard {
    fn new() -> ReaderGuard {
        READERS.fetch_add(1, Ordering::Acquire);
        ReaderGuard
    }
}

impl Drop for ReaderGuard {
    fn drop(&mut self) {
        // unwind 也要减：否则一次 panic 会让退休队列永远不回收。
        READERS.fetch_sub(1, Ordering::Release);
    }
}

/// 发布一份新快照；旧快照进退休队列（由 [`collect_retired`] 释放）。
pub fn publish(rules: Rules) {
    let boxed = Box::into_raw(Box::new(rules));
    // SAFETY: `boxed` 刚由 `Box::into_raw` 产出，指向有效的 `Rules`。
    let previous = LIVE.swap(boxed, Ordering::AcqRel);
    if !previous.is_null() {
        RETIRED
            .lock()
            .unwrap_or_else(|err| err.into_inner())
            .0
            .push(previous);
    }
}

/// 释放已退休的快照；只在"此刻没有读者"时动手。返回释放了几份（监督线程调用）。
pub fn collect_retired() -> usize {
    if READERS.load(Ordering::Acquire) != 0 {
        return 0;
    }
    let retired: Vec<*mut Rules> = {
        let mut guard = RETIRED.lock().unwrap_or_else(|err| err.into_inner());
        if guard.0.is_empty() {
            return 0;
        }
        std::mem::take(&mut guard.0)
    };
    let count = retired.len();
    for pointer in retired {
        // SAFETY: 指针来自 `publish` 的 `Box::into_raw`，且刚刚确认没有读者；
        // 每份只在这里释放一次（drain 已经把它从队列里拿走）。
        drop(unsafe { Box::from_raw(pointer) });
    }
    count
}

fn with_snapshot<R>(f: impl FnOnce(&Rules) -> R) -> Option<R> {
    let _reader = ReaderGuard::new();
    let pointer = LIVE.load(Ordering::Acquire);
    if pointer.is_null() {
        return None;
    }
    // SAFETY: 指针由 `publish` 发布；读者守卫在读取期间阻止它被回收。
    Some(f(unsafe { &*pointer }))
}

/// 一行的裁决结果。
///
/// 三态是必要的：**"被折叠"与"放行"都不是"命中规则"**，但它们对钩子的处置完全相反。
/// 用一个返回值同时表达它们，会让折叠计数增长而行仍然打进控制台。

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Outcome {
    /// 放行。
    Pass,
    /// 被某条规则吞掉；带规则 id。
    Drop(u32),
    /// 被"重复行折叠"吞掉（不属于任何一条规则）。
    Fold,
}

/// 裁决一行，并记计数。
///
/// 契约：绝不 panic、绝不分配（观察模式除外）、绝不取锁。
pub fn decide(text: &[u8]) -> Outcome {
    let now = now_ms();
    let matched =
        with_snapshot(|rules| rules.decide(text, now).map(crate::rules::Rule::id)).flatten();
    if let Some(id) = matched {
        status::count_dropped();
        return Outcome::Drop(id);
    }
    // 规则没命中：还有一层"重复行折叠"（全局策略，不属于任何一条规则）。
    if repeat::should_suppress(text, now) {
        status::count_folded();
        return Outcome::Fold;
    }
    status::count_passed();
    observe(text);
    Outcome::Pass
}

/// 控制台**命令**是否命中规则（只匹配，不改任何状态）。
///
/// 命令侧只有一种处置：静音执行。它**不**推进 throttle 窗口、**不**加命中数——
/// 否则"一行被节流"会连带影响"一条命令是否回显"，那是两个面。
#[must_use]
pub fn matches_command(text: &[u8]) -> bool {
    with_snapshot(|rules| rules.match_only(text).is_some()).unwrap_or(false)
}

/// 这一行会被哪条规则吞（只读镜子：不推进 throttle、不加命中数）。
#[must_use]
pub fn which(text: &[u8]) -> Option<u32> {
    with_snapshot(|rules| rules.match_only(text).map(crate::rules::Rule::id)).flatten()
}

/// 当前快照里每条规则的 `(id, 命中数, 上次放行时刻)`（重建时继承计数用）。
#[must_use]
pub fn snapshot_handles() -> Vec<(u32, u64, u64)> {
    with_snapshot(Rules::handles).unwrap_or_default()
}

/// 当前规则面：`(总开关, 观察模式, 规则条数)`。
#[must_use]
pub fn shape() -> (bool, bool, usize) {
    with_snapshot(|rules| (rules.enabled, rules.observe, rules.rules.len()))
        .unwrap_or((true, false, 0))
}

/// 每条规则的命中数（状态文件用）。
#[must_use]
pub fn rule_hits() -> Vec<(String, u64)> {
    with_snapshot(Rules::hits).unwrap_or_default()
}

fn observe(text: &[u8]) {
    let live = with_snapshot(|rules| rules.observe).unwrap_or(false);
    if !live || OBSERVE_CLOSED.load(Ordering::Relaxed) != 0 {
        return;
    }
    let seen = OBSERVED_LINES.fetch_add(1, Ordering::Relaxed) + 1;
    if seen * 160 > OBSERVE_BUDGET_BYTES {
        OBSERVE_CLOSED.store(1, Ordering::Relaxed);
        kcd2_abi::log::warn(&format!(
            "observe mode stopped after {seen} lines (budget {OBSERVE_BUDGET_BYTES} bytes); set observe = 0"
        ));
        return;
    }
    // 观察模式是显式排障模式：这里付出一次格式化 + 一次写盘，是为了让用户不必猜。
    kcd2_abi::log::debug(&format!("pass: {}", String::from_utf8_lossy(text)));
}

/// 测试串行锁：快照与三层规则都是进程级状态，并行跑测试会互相拆台
/// 并行执行会互相干扰。
#[cfg(test)]
pub fn serial() -> std::sync::MutexGuard<'static, ()> {
    static LOCK: Mutex<()> = Mutex::new(());
    LOCK.lock().unwrap_or_else(|err| err.into_inner())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::rules::{Action, MatchKind, Rule};

    fn install(pattern: &str) -> u32 {
        let rule = Rule::new(
            crate::rules::next_id(),
            MatchKind::Contains,
            Action::Drop,
            pattern,
            None,
        )
        .expect("valid rule");
        let id = rule.id();
        publish(Rules {
            rules: vec![rule],
            ..Rules::default()
        });
        id
    }

    #[test]
    fn decide_reports_the_rule_and_commands_match_without_state() {
        let _serial = serial();
        let id = install("spam");
        assert_eq!(decide(b"xx spam yy"), Outcome::Drop(id));
        assert_eq!(decide(b"clean line"), Outcome::Pass);
        let before = rule_hits()[0].1;
        assert!(matches_command(b"spam"));
        assert_eq!(which(b"spam"), Some(id));
        assert_eq!(
            rule_hits()[0].1,
            before,
            "commands/which must not bump hits"
        );
    }

    #[test]
    fn retired_snapshots_are_reclaimed_when_no_readers() {
        let _serial = serial();
        for pattern in ["a", "b", "c"] {
            install(pattern);
        }
        assert!(collect_retired() >= 2);
        assert_eq!(collect_retired(), 0, "queue is empty now");
    }

    #[test]
    fn handles_carry_hits_for_the_same_id() {
        let _serial = serial();
        let id = install("carry");
        let _ = decide(b"carry me");
        let handles = snapshot_handles();
        assert_eq!(handles.len(), 1);
        assert_eq!(handles[0].0, id);
        assert_eq!(handles[0].1, 1);
    }

    #[test]
    fn disabled_snapshot_passes_everything() {
        let _serial = serial();
        publish(Rules {
            enabled: false,
            rules: vec![Rule::new(
                crate::rules::next_id(),
                MatchKind::Contains,
                Action::Drop,
                "x",
                None,
            )
            .expect("valid")],
            ..Rules::default()
        });
        assert_eq!(decide(b"xxx"), Outcome::Pass);
        assert!(!matches_command(b"xxx"));
    }
}
