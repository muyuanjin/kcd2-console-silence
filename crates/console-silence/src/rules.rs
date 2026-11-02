//! 行匹配规则（纯逻辑，无 FFI、可离线单测）。
//!
//! 一条规则回答一个问题：**这一行要不要吞**。识别方式从便宜到贵：
//!
//! | kind | 匹配方式 | 代价 |
//! |---|---|---|
//! | `contains` / `prefix` / `suffix` / `equals` | 字面比较（大小写不敏感） | 零分配字节扫描 |
//! | `wildcard` | `*`（任意串）`?`（任意单字符），其余字符按**字面量**处理 | 建规则时翻译成正则 |
//! | `regex` | 完整正则（大小写不敏感、线性时间） | 建规则时编译一次 |
//!
//! 通配与正则都在 [`Rule::new`] 里编译一次，匹配期不分配、不编译。`regex` crate
//! 保证线性时间（无回溯爆炸），所以放在引擎打印路径上是安全的；但它始终是"兜底"，
//! 字面规则永远先跑。
//!
//! 动作有两种：
//! - [`Action::Drop`]：命中即吞，完全不出现在控制台；
//! - [`Action::Throttle`]：命中后按窗口放行，窗口内其余丢弃——用于"确实要让用户看见
//!   一次，但不要每 16 ms 一次"的告警类刷屏（例如"chart 加载失败"）。

use core::sync::atomic::{AtomicU32, AtomicU64, Ordering};

/// 规则 id 分配器：cfg 与 Lua 共用同一个空间，id 稳定（删掉别人不会改变它）。
static NEXT_ID: AtomicU32 = AtomicU32::new(1);

/// 取一个新的规则 id。
pub fn next_id() -> u32 {
    NEXT_ID.fetch_add(1, Ordering::Relaxed)
}

/// `last_pass_ms` 的初值：从没放行过。
const NEVER: u64 = u64::MAX;

/// 命中后怎么处理。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Action {
    /// 直接丢弃。
    Drop,
    /// 每个窗口只放行第一条。
    Throttle {
        /// 放行窗口（毫秒）。
        window_ms: u64,
    },
}

impl Action {
    /// 配置里的写法（`drop` / `throttle:<ms>`）。
    #[must_use]
    pub fn parse(text: &str) -> Option<Action> {
        let text = text.trim().to_ascii_lowercase();
        if text == "drop" {
            return Some(Action::Drop);
        }
        let window_ms = text.strip_prefix("throttle:")?.parse::<u64>().ok()?;
        (window_ms > 0).then_some(Action::Throttle { window_ms })
    }

    /// 反过来写成文本（状态文件 / `List()` 用）。
    #[must_use]
    pub fn as_text(&self) -> String {
        match self {
            Action::Drop => "drop".to_string(),
            Action::Throttle { window_ms } => format!("throttle:{window_ms}"),
        }
    }
}

/// 匹配方式。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum MatchKind {
    /// 子串。
    Contains,
    /// 前缀。
    Prefix,
    /// 后缀。
    Suffix,
    /// 整行相等。
    Equals,
    /// `*` / `?` 通配，其余字符字面量。
    Wildcard,
    /// 完整正则（大小写不敏感）。
    Regex,
}

impl MatchKind {
    /// 配置里的写法。
    #[must_use]
    pub fn parse(text: &str) -> Option<MatchKind> {
        match text.trim().to_ascii_lowercase().as_str() {
            "contains" => Some(MatchKind::Contains),
            "prefix" => Some(MatchKind::Prefix),
            "suffix" => Some(MatchKind::Suffix),
            "equals" | "equal" => Some(MatchKind::Equals),
            "wildcard" | "glob" => Some(MatchKind::Wildcard),
            "regex" | "regexp" => Some(MatchKind::Regex),
            _ => None,
        }
    }

    /// 反过来写成文本。
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            MatchKind::Contains => "contains",
            MatchKind::Prefix => "prefix",
            MatchKind::Suffix => "suffix",
            MatchKind::Equals => "equals",
            MatchKind::Wildcard => "wildcard",
            MatchKind::Regex => "regex",
        }
    }
}

/// 编译好的匹配器：字面量走字节比较，通配/正则走自动机。
#[derive(Debug)]
enum Matcher {
    Contains(Vec<u8>),
    Prefix(Vec<u8>),
    Suffix(Vec<u8>),
    Equals(Vec<u8>),
    Compiled(regex::bytes::Regex),
}

/// 一条规则。
#[derive(Debug)]
pub struct Rule {
    id: u32,
    action: Action,
    matcher: Matcher,
    label: String,
    hits: AtomicU64,
    last_pass_ms: AtomicU64,
}

impl Rule {
    /// 构造一条规则；通配/正则在**这里**编译一次，失败返回人话原因。
    ///
    /// id 用 [`next_id`] 取。
    pub fn new(
        id: u32,
        kind: MatchKind,
        action: Action,
        pattern: &str,
        label: Option<&str>,
    ) -> Result<Rule, String> {
        if pattern.is_empty() {
            return Err("empty pattern".to_string());
        }
        let matcher = match kind {
            MatchKind::Contains => Matcher::Contains(pattern.as_bytes().to_vec()),
            MatchKind::Prefix => Matcher::Prefix(pattern.as_bytes().to_vec()),
            MatchKind::Suffix => Matcher::Suffix(pattern.as_bytes().to_vec()),
            MatchKind::Equals => Matcher::Equals(pattern.as_bytes().to_vec()),
            MatchKind::Wildcard => Matcher::Compiled(compile(&wildcard_to_regex(pattern))?),
            MatchKind::Regex => Matcher::Compiled(compile(pattern)?),
        };
        Ok(Rule {
            id,
            action,
            matcher,
            label: label.map_or_else(|| pattern.to_string(), str::to_string),
            hits: AtomicU64::new(0),
            last_pass_ms: AtomicU64::new(NEVER),
        })
    }

    /// 稳定句柄。
    #[must_use]
    pub fn id(&self) -> u32 {
        self.id
    }

    /// 命中次数。
    #[must_use]
    pub fn hits(&self) -> u64 {
        self.hits.load(Ordering::Relaxed)
    }

    /// 上次因 throttle 放行的时刻（`NEVER` = 还没放过）。
    #[must_use]
    pub fn last_pass(&self) -> u64 {
        self.last_pass_ms.load(Ordering::Relaxed)
    }

    /// 重建快照时继承计数与节流窗口：热重载不该把"已经吞了多少行"清零。
    pub fn seed(&self, hits: u64, last_pass: u64) {
        self.hits.store(hits, Ordering::Relaxed);
        self.last_pass_ms.store(last_pass, Ordering::Relaxed);
    }

    /// 这一行是否命中本规则（只看 `line` 的前 `scan_limit` 字节）。
    fn matches(&self, line: &[u8]) -> bool {
        match &self.matcher {
            Matcher::Contains(needle) => contains_ci(line, needle),
            Matcher::Prefix(needle) => {
                line.len() >= needle.len() && eq_ci(&line[..needle.len()], needle)
            }
            Matcher::Suffix(needle) => {
                line.len() >= needle.len() && eq_ci(&line[line.len() - needle.len()..], needle)
            }
            Matcher::Equals(needle) => line.len() == needle.len() && eq_ci(line, needle),
            Matcher::Compiled(regex) => regex.is_match(line),
        }
    }

    /// 规则给出裁决：`true` = 吞掉这一行。
    fn verdict(&self, now_ms: u64) -> bool {
        self.hits.fetch_add(1, Ordering::Relaxed);
        match self.action {
            Action::Drop => true,
            Action::Throttle { window_ms } => {
                let last = self.last_pass_ms.load(Ordering::Relaxed);
                let due = last == NEVER || now_ms.saturating_sub(last) >= window_ms;
                if due {
                    self.last_pass_ms.store(now_ms, Ordering::Relaxed);
                }
                !due
            }
        }
    }
}

/// 把 `*` / `?` 通配翻译成正则，其余字符一律转义——所以 `[ERVWHDO LUT Guard]`
/// 这种"看着像正则"的写法在通配里就是字面量。这是通配相对正则的关键好处。
fn wildcard_to_regex(pattern: &str) -> String {
    let mut out = String::with_capacity(pattern.len() + 8);
    out.push_str("(?s)^");
    for ch in pattern.chars() {
        match ch {
            '*' => out.push_str(".*"),
            '?' => out.push('.'),
            other => out.push_str(&regex::escape(&other.to_string())),
        }
    }
    out.push('$');
    out
}

fn compile(pattern: &str) -> Result<regex::bytes::Regex, String> {
    regex::bytes::RegexBuilder::new(pattern)
        .case_insensitive(true)
        .size_limit(1 << 20)
        .build()
        .map_err(|error| format!("bad pattern: {error}"))
}

/// 整套规则 + 全局开关（配置热重载 / Lua 修改时整体替换）。
#[derive(Debug)]
pub struct Rules {
    /// 总开关（关掉 = 钩子仍然在，但一行都不吞）。
    pub enabled: bool,
    /// 观察模式：把每一行进出的行都写进日志（定位"到底哪一行在刷"用）。
    pub observe: bool,
    /// 只看每行的前 N 字节（引擎行有上限；把比较成本钉死）。
    pub scan_limit: usize,
    /// 规则表（按顺序，第一条命中即定案）。
    pub rules: Vec<Rule>,
}

impl Default for Rules {
    fn default() -> Rules {
        Rules {
            enabled: true,
            observe: false,
            scan_limit: 512,
            rules: Vec::new(),
        }
    }
}

/// 一行文本的裁决结果。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[cfg(test)]
pub enum Verdict {
    /// 放行。
    Pass,
    /// 吞掉；带命中的规则下标。
    Drop(usize),
}

impl Rules {
    /// 裁决一行（`line` 是去掉 NUL 的字节）。
    #[must_use]
    #[cfg(test)]
    pub fn evaluate(&self, line: &[u8], now_ms: u64) -> Verdict {
        if !self.enabled {
            return Verdict::Pass;
        }
        let window = &line[..line.len().min(self.scan_limit)];
        for (index, rule) in self.rules.iter().enumerate() {
            if rule.matches(window) {
                return if rule.verdict(now_ms) {
                    Verdict::Drop(index)
                } else {
                    Verdict::Pass
                };
            }
        }
        Verdict::Pass
    }

    /// 裁决一行：匹配 + 动作（会推进 throttle 窗口、加命中数）。
    #[must_use]
    pub fn decide(&self, line: &[u8], now_ms: u64) -> Option<&Rule> {
        if !self.enabled {
            return None;
        }
        let window = &line[..line.len().min(self.scan_limit)];
        self.rules
            .iter()
            .find(|rule| rule.matches(window) && rule.verdict(now_ms))
    }

    /// 只匹配，不改任何状态（命令面与 `which()` 用）。
    #[must_use]
    pub fn match_only(&self, line: &[u8]) -> Option<&Rule> {
        if !self.enabled {
            return None;
        }
        let window = &line[..line.len().min(self.scan_limit)];
        self.rules.iter().find(|rule| rule.matches(window))
    }

    /// 每条规则的 `(id, 命中数, 上次放行时刻)`。
    #[must_use]
    pub fn handles(&self) -> Vec<(u32, u64, u64)> {
        self.rules
            .iter()
            .map(|rule| (rule.id, rule.hits(), rule.last_pass()))
            .collect()
    }

    /// 所有规则的命中计数快照（状态文件用）。
    #[must_use]
    pub fn hits(&self) -> Vec<(String, u64)> {
        self.rules
            .iter()
            .map(|rule| (rule.label.clone(), rule.hits()))
            .collect()
    }

    /// 按 id 找规则下标。
    #[must_use]
    #[cfg(test)]
    pub fn index_of(&self, id: u32) -> Option<usize> {
        self.rules.iter().position(|rule| rule.id == id)
    }
}

fn eq_ci(left: &[u8], right: &[u8]) -> bool {
    left.eq_ignore_ascii_case(right)
}

fn contains_ci(haystack: &[u8], needle: &[u8]) -> bool {
    if needle.is_empty() || haystack.len() < needle.len() {
        return false;
    }
    haystack
        .windows(needle.len())
        .any(|window| window.eq_ignore_ascii_case(needle))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rule(kind: MatchKind, action: Action, pattern: &str) -> Rule {
        Rule::new(next_id(), kind, action, pattern, None).expect("valid rule")
    }

    fn rules(kind: MatchKind, action: Action, pattern: &str) -> Rules {
        Rules {
            rules: vec![rule(kind, action, pattern)],
            ..Rules::default()
        }
    }

    #[test]
    fn contains_is_case_insensitive() {
        let set = rules(
            MatchKind::Contains,
            Action::Drop,
            "r_colorgradingchartimage",
        );
        assert_eq!(
            set.evaluate(b"Setting R_ColorGradingChartImage to x", 0),
            Verdict::Drop(0)
        );
        assert_eq!(set.evaluate(b"Loading level", 0), Verdict::Pass);
    }

    #[test]
    fn prefix_and_equals_are_distinct() {
        let prefix = rules(MatchKind::Prefix, Action::Drop, "$4[ERVWHDO");
        assert_eq!(prefix.evaluate(b"$4[ERVWHDO LUT] x", 0), Verdict::Drop(0));
        assert_eq!(prefix.evaluate(b" x $4[ERVWHDO", 0), Verdict::Pass);

        let equals = rules(MatchKind::Equals, Action::Drop, "abc");
        assert_eq!(equals.evaluate(b"ABC", 0), Verdict::Drop(0));
        assert_eq!(equals.evaluate(b"ABCD", 0), Verdict::Pass);
    }

    #[test]
    fn suffix_matches_tail_only() {
        let set = rules(MatchKind::Suffix, Action::Drop, "chart image");
        assert_eq!(
            set.evaluate(b"r_ColorGradingChartImage chart image", 0),
            Verdict::Drop(0)
        );
        assert_eq!(set.evaluate(b"chart image loaded", 0), Verdict::Pass);
    }

    #[test]
    fn first_matching_rule_wins() {
        let set = Rules {
            rules: vec![
                rule(MatchKind::Contains, Action::Drop, "keep"),
                rule(MatchKind::Contains, Action::Drop, "keep me"),
            ],
            ..Rules::default()
        };
        assert_eq!(set.evaluate(b"keep me", 30), Verdict::Drop(0));
    }

    #[test]
    fn disabled_rules_pass_everything() {
        let set = Rules {
            enabled: false,
            rules: vec![rule(MatchKind::Contains, Action::Drop, "x")],
            ..Rules::default()
        };
        assert_eq!(set.evaluate(b"xxx", 0), Verdict::Pass);
        assert_eq!(set.hits()[0].1, 0);
    }

    #[test]
    fn throttle_passes_one_line_per_window() {
        let set = rules(
            MatchKind::Contains,
            Action::Throttle { window_ms: 1000 },
            "spam",
        );
        assert_eq!(set.evaluate(b"spam 1", 0), Verdict::Pass);
        assert_eq!(set.evaluate(b"spam 2", 10), Verdict::Drop(0));
        assert_eq!(set.evaluate(b"spam 3", 999), Verdict::Drop(0));
        assert_eq!(set.evaluate(b"spam 4", 1000), Verdict::Pass);
        assert_eq!(set.hits()[0].1, 4);
    }

    #[test]
    fn scan_limit_bounds_the_inspected_prefix() {
        let set = Rules {
            scan_limit: 8,
            rules: vec![rule(MatchKind::Contains, Action::Drop, "needle")],
            ..Rules::default()
        };
        assert_eq!(
            set.evaluate(b"0123456789needle", 0),
            Verdict::Pass,
            "a pattern that only occurs past scan_limit must not match"
        );
        assert_eq!(set.evaluate(b"needle 0123456789", 0), Verdict::Drop(0));
    }

    #[test]
    fn empty_pattern_is_rejected() {
        assert!(Rule::new(next_id(), MatchKind::Contains, Action::Drop, "", None).is_err());
    }

    #[test]
    fn wildcard_treats_brackets_as_literals() {
        // 关键用例：`[ERVWHDO LUT Guard]` 在正则里是字符类，在通配里必须是字面量。
        let set = rules(MatchKind::Wildcard, Action::Drop, "[ERVWHDO LUT Guard]*");
        assert_eq!(
            set.evaluate(b"[ERVWHDO LUT Guard] stopped", 0),
            Verdict::Drop(0)
        );
        assert_eq!(set.evaluate(b"E stopped", 0), Verdict::Pass);
    }

    #[test]
    fn wildcard_question_mark_matches_one_character() {
        let set = rules(MatchKind::Wildcard, Action::Drop, "a?c");
        assert_eq!(set.evaluate(b"abc", 0), Verdict::Drop(0));
        assert_eq!(set.evaluate(b"ac", 0), Verdict::Pass);
        assert_eq!(set.evaluate(b"abbc", 0), Verdict::Pass);
    }

    #[test]
    fn regex_matches_with_case_insensitivity() {
        let set = rules(MatchKind::Regex, Action::Drop, r"chart.*\.dds$");
        assert_eq!(
            set.evaluate(b"r_ColorGradingChartImage DATA//x/3.A24.DDS", 0),
            Verdict::Drop(0)
        );
        assert_eq!(set.evaluate(b"nothing here", 0), Verdict::Pass);
    }

    #[test]
    fn invalid_regex_is_rejected_with_a_reason() {
        let error = Rule::new(next_id(), MatchKind::Regex, Action::Drop, "([", None).unwrap_err();
        assert!(error.contains("bad pattern"), "got: {error}");
    }

    #[test]
    fn ids_are_stable_and_independent_of_position() {
        let first = rule(MatchKind::Contains, Action::Drop, "a");
        let second = rule(MatchKind::Contains, Action::Drop, "b");
        let (first_id, second_id) = (first.id(), second.id());
        assert_ne!(first_id, second_id);
        let set = Rules {
            rules: vec![first, second],
            ..Rules::default()
        };
        assert_eq!(set.index_of(second_id), Some(1));
        assert_eq!(set.index_of(u32::MAX), None);
    }
}
