//! 引擎控制台：`gEnv->pConsole` → `IConsole` vtable 的两个槽位。
//!
//! 两个拦截点，各管一条路（`docs/RE_FINDINGS.md` §2、§3 有反编译证据）：
//!
//! | 槽 | 方法 | 拦什么 |
//! |---|---|---|
//! | 26 | `PrintLine(const char*)` | 走进控制台行缓冲的**文本行**（`System.LogAlways`、引擎告警等） |
//! | 35 | `ExecuteString(cmd, bSilent, bDefer)` | **命令回显**：脚本每 tick 执行一次 cvar 命令时，命令本身被打印进控制台 |
//!
//! 为什么必须两个都钩：`CXConsole::ExecuteString`
//! 的工作函数里那句回显是
//!
//! ```c
//! if (!bSilentMode && this->GetStatus()) PrintLine(command);   // vtable+224 = 槽 28
//! ```
//!
//! ——它**直接调用** `sub_1808D9350`（`PrintLine` 的真实实现），不经过 vtable 槽 26，
//! 所以只补槽位 26 的钩子看不见它；而且 `GetStatus()`（`*(u8*)(this+476)`，由
//! `ShowConsole` 置位）为假时根本不回显，这就是"控制台没打开时测不出刷屏"的原因。
//!
//! 槽 35 的处置不是"丢掉命令"（那会让模组的 LUT 保护失效），而是**把 `bSilentMode`
//! 翻成真**：命令照常执行，回显不再发生。语义与 SDK 对这个参数的说明一致
//! （"suppresses log in error case and logging the command to the console"）。

use core::ffi::{c_char, c_void, CStr};
use core::sync::atomic::{AtomicUsize, Ordering};

use kcd2_abi::{log, mem, module::Module};

use crate::filter;

/// `WHGame.dll` 的模块名（`GetModuleHandle` 不区分大小写）。
pub const GAME_MODULE: &str = "WHGame.DLL";
/// `IConsole::PrintLine` 的 vtable 下标。
pub const PRINT_LINE_SLOT: usize = 26;
/// `IConsole::ExecuteString` 的 vtable 下标。
pub const EXECUTE_STRING_SLOT: usize = 35;
/// `IConsole::GetStatus` 的 vtable 下标（控制台是否显示）。
pub const GET_STATUS_SLOT: usize = 28;

/// 引擎侧 `void IConsole::PrintLine(const char*)`。
type PrintLineFn = unsafe extern "system" fn(*mut c_void, *const c_char);
/// 引擎侧 `void IConsole::ExecuteString(const char*, bool, bool)`。
type ExecuteStringFn = unsafe extern "system" fn(*mut c_void, *const c_char, u8, u8);
/// 引擎侧 `bool IConsole::GetStatus()`。
type GetStatusFn = unsafe extern "system" fn(*mut c_void) -> u8;

/// 钩子状态机。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum State {
    /// 还没装（等 `pConsole` 出现）。
    Pending,
    /// 两个槽位都是我们的钩子。
    Installed,
    /// 装过但槽位被别人改回去了（下一轮监督重装）。
    Lost,
    /// 装了也没用（模块/槽位校验失败），不再重试。
    Failed,
}

static STATE: AtomicUsize = AtomicUsize::new(0);
static PRINT_LINE_ORIGINAL: AtomicUsize = AtomicUsize::new(0);
static PRINT_LINE_ADDRESS: AtomicUsize = AtomicUsize::new(0);
static EXECUTE_STRING_ORIGINAL: AtomicUsize = AtomicUsize::new(0);
static EXECUTE_STRING_ADDRESS: AtomicUsize = AtomicUsize::new(0);
static CONSOLE_OBJECT: AtomicUsize = AtomicUsize::new(0);

fn state_from_raw(raw: usize) -> State {
    match raw {
        1 => State::Installed,
        2 => State::Lost,
        3 => State::Failed,
        _ => State::Pending,
    }
}

fn store(state: State) {
    let raw = match state {
        State::Pending => 0,
        State::Installed => 1,
        State::Lost => 2,
        State::Failed => 3,
    };
    STATE.store(raw, Ordering::SeqCst);
}

/// 当前钩子状态。
#[must_use]
pub fn state() -> State {
    state_from_raw(STATE.load(Ordering::SeqCst))
}

/// 原 `PrintLine` 地址（0 = 未知）。
#[must_use]
pub fn original() -> usize {
    PRINT_LINE_ORIGINAL.load(Ordering::SeqCst)
}

/// 原 `ExecuteString` 地址（0 = 未知）。
#[must_use]
pub fn original_execute_string() -> usize {
    EXECUTE_STRING_ORIGINAL.load(Ordering::SeqCst)
}

/// 当前 `PrintLine` 槽位里的地址（诊断用）。
#[must_use]
pub fn current_slot() -> usize {
    let address = PRINT_LINE_ADDRESS.load(Ordering::SeqCst);
    if address == 0 {
        return 0;
    }
    mem::read_pointer(address).unwrap_or(0)
}

/// 已定位好的控制台对象。
#[derive(Clone, Copy, Debug)]
pub struct Console {
    /// `IConsole` 对象地址。
    pub object: usize,
    /// vtable 地址。
    pub vtable: usize,
    /// `PrintLine` 槽位地址。
    pub print_line_slot: usize,
    /// `ExecuteString` 槽位地址。
    pub execute_string_slot: usize,
}

/// 从 `gEnv` 读 `pConsole` 并解析出两个槽位。
///
/// 任何一步读不到或看起来不像就返回 `None`（不猜地址）。
#[must_use]
pub fn locate(env: *mut c_void) -> Option<Console> {
    let env_address = env as usize;
    if env_address == 0 {
        return None;
    }
    let console = mem::read_pointer(env_address + kcd2_abi::env::CONSOLE_OFFSET)?;
    if console == 0 {
        return None;
    }
    let vtable = mem::read_pointer(console)?;
    if vtable == 0 {
        return None;
    }
    let entry = |index: usize| vtable + index * core::mem::size_of::<usize>();
    let print_line_slot = entry(PRINT_LINE_SLOT);
    let execute_string_slot = entry(EXECUTE_STRING_SLOT);
    // 两个槽位都必须指向游戏自己的代码段：出现堆地址/低地址说明布局判断错了。
    let game = Module::named(GAME_MODULE)?;
    for (name, slot) in [
        ("PrintLine", print_line_slot),
        ("ExecuteString", execute_string_slot),
    ] {
        let target = mem::read_pointer(slot)?;
        if target == 0 || !game.contains(target) {
            log::warn(&format!(
                "{name} slot 0x{slot:X} points outside {GAME_MODULE} (0x{target:X}); not hooking"
            ));
            return None;
        }
    }
    Some(Console {
        object: console,
        vtable,
        print_line_slot,
        execute_string_slot,
    })
}

/// 装钩子。幂等：已经指向本钩子时直接成功。
pub fn install(console: &Console) -> Result<(), &'static str> {
    store_object(console.object);
    let mut missing = 0;
    if mem::read_pointer(console.print_line_slot) != Some(print_line_hook_address()) {
        missing += 1;
        let original =
            mem::read_pointer(console.print_line_slot).ok_or("unreadable PrintLine slot")?;
        if original == 0 || !mem::write_pointer(console.print_line_slot, print_line_hook_address())
        {
            return Err("writing the PrintLine slot failed");
        }
        PRINT_LINE_ORIGINAL.store(original, Ordering::SeqCst);
        PRINT_LINE_ADDRESS.store(console.print_line_slot, Ordering::SeqCst);
        log::info(&format!(
            "PrintLine hooked: console=0x{:X} slot=0x{:X} original=0x{:X} hook=0x{:X}",
            console.object,
            console.print_line_slot,
            original,
            print_line_hook_address()
        ));
    }
    if mem::read_pointer(console.execute_string_slot) != Some(execute_string_hook_address()) {
        missing += 1;
        let original = mem::read_pointer(console.execute_string_slot)
            .ok_or("unreadable ExecuteString slot")?;
        if original == 0
            || !mem::write_pointer(console.execute_string_slot, execute_string_hook_address())
        {
            return Err("writing the ExecuteString slot failed");
        }
        EXECUTE_STRING_ORIGINAL.store(original, Ordering::SeqCst);
        EXECUTE_STRING_ADDRESS.store(console.execute_string_slot, Ordering::SeqCst);
        log::info(&format!(
            "ExecuteString hooked: console=0x{:X} vtable=0x{:X} slot=0x{:X} original=0x{:X} hook=0x{:X} \
             (matching commands run with bSilentMode=true, so they are not echoed)",
            console.object,
            console.vtable,
            console.execute_string_slot,
            original,
            execute_string_hook_address()
        ));
    }
    store(State::Installed);
    if missing > 0 {
        crate::status::count_install();
    }
    Ok(())
}

/// 每轮监督调用：两个槽位还是不是我们的钩子？
///
/// 没了就重装（游戏自己还原、另一个 ASI 覆盖、或控制台对象被重建）。返回
/// `true` = 两个都是我们装的。
pub fn verify(console: &Console) -> bool {
    let print_line_ok =
        mem::read_pointer(console.print_line_slot) == Some(print_line_hook_address());
    let execute_string_ok =
        mem::read_pointer(console.execute_string_slot) == Some(execute_string_hook_address());
    if print_line_ok && execute_string_ok {
        if state() != State::Installed {
            store(State::Installed);
        }
        return true;
    }
    if state() == State::Installed {
        log::warn(&format!(
            "console hook lost (PrintLine ok={print_line_ok}, ExecuteString ok={execute_string_ok}); reinstalling"
        ));
        crate::status::count_rearm();
    }
    store(State::Lost);
    false
}

/// 恢复原函数指针（DLL 卸载时调用）。
pub fn restore() {
    for (slot, original, hook, what) in [
        (
            PRINT_LINE_ADDRESS.load(Ordering::SeqCst),
            PRINT_LINE_ORIGINAL.load(Ordering::SeqCst),
            print_line_hook_address(),
            "PrintLine",
        ),
        (
            EXECUTE_STRING_ADDRESS.load(Ordering::SeqCst),
            EXECUTE_STRING_ORIGINAL.load(Ordering::SeqCst),
            execute_string_hook_address(),
            "ExecuteString",
        ),
    ] {
        if slot == 0 || original == 0 {
            continue;
        }
        if mem::read_pointer(slot) == Some(hook) {
            if mem::write_pointer(slot, original) {
                log::info(&format!("{what} slot restored to the engine function"));
            } else {
                log::error(&format!("{what} slot restore failed"));
            }
        }
    }
    store(State::Pending);
}

/// 控制台当前是否显示（`IConsole::GetStatus`）。
///
/// 这个标志决定"命令回显"到底发生不发生，所以状态文件里要能看见它。
#[must_use]
pub fn console_open() -> bool {
    let object = object();
    if object == 0 {
        return false;
    }
    let Some(vtable) = mem::read_pointer(object) else {
        return false;
    };
    let Some(entry) = mem::read_pointer(vtable + GET_STATUS_SLOT * core::mem::size_of::<usize>())
    else {
        return false;
    };
    if Module::named(GAME_MODULE).is_none_or(|game| !game.contains(entry)) {
        return false;
    }
    // SAFETY: 槽位 28 已由 IDA 反编译确认为 `GetStatus()`（读 `*(u8*)(this+476)`），
    // 传入的 `object` 是引擎自己的 `IConsole` 实例。
    let status: GetStatusFn = unsafe { core::mem::transmute(entry) };
    // SAFETY: 同上。
    unsafe { status(object as *mut c_void) != 0 }
}

/// 把一行文本交给引擎原来的 `PrintLine`（监督线程补汇总行用）。
///
/// 这条路径**不经过**过滤器：汇总行是我们自己写的，必须真的出现在控制台。
pub fn forward_raw(text: &str) {
    let original = PRINT_LINE_ORIGINAL.load(Ordering::SeqCst);
    if original == 0 {
        return;
    }
    let Ok(wide) = std::ffi::CString::new(text) else {
        return;
    };
    let console = CONSOLE_OBJECT.load(Ordering::SeqCst);
    if console == 0 {
        return;
    }
    // SAFETY: `original` 是安装时从槽位读出的引擎函数，`console` 是同一个对象。
    let forward: PrintLineFn = unsafe { core::mem::transmute(original) };
    // SAFETY: 控制台对象与函数都来自引擎：`console` 是安装时记下的 `IConsole` 实例，
    // `wide` 是本函数持有的 NUL 结尾缓冲，调用期间不会移动。
    unsafe { forward(console as *mut c_void, wide.as_ptr()) };
}

fn store_object(object: usize) {
    CONSOLE_OBJECT.store(object, Ordering::SeqCst);
}

/// 控制台对象地址（0 = 未知）。
#[must_use]
pub fn object() -> usize {
    CONSOLE_OBJECT.load(Ordering::SeqCst)
}

/// 读控制台行缓冲的最后 `count` 行（诊断转储用）。
///
/// 走 `IConsole::GetLineCount`（vtable 22）+ `GetLineNo`（vtable 21）：这是"用户在
/// 控制台里能看见什么"的权威来源，用来证明钩子没有漏掉别的写入路径。
#[must_use]
pub fn dump_lines(limit: usize) -> Vec<String> {
    type GetLineCountFn = unsafe extern "system" fn(*mut c_void) -> i32;
    type GetLineNoFn = unsafe extern "system" fn(*mut c_void, i32, *mut u8, i32) -> u8;

    let mut out = Vec::new();
    let object = object();
    if object == 0 {
        return out;
    }
    let Some(vtable) = mem::read_pointer(object) else {
        return out;
    };
    let read_entry =
        |index: usize| mem::read_pointer(vtable + index * core::mem::size_of::<usize>());
    let (Some(count_entry), Some(line_entry)) = (read_entry(22), read_entry(21)) else {
        return out;
    };
    let game = match Module::named(GAME_MODULE) {
        Some(module) => module,
        None => return out,
    };
    if !game.contains(count_entry) || !game.contains(line_entry) {
        log::warn("console dump skipped: vtable entries are not inside the game module");
        return out;
    }
    // SAFETY: 槽位 21/22 已由 IDA 反编译确认为 GetLineNo/GetLineCount，并按头文件签名调用。
    let count_fn: GetLineCountFn = unsafe { core::mem::transmute(count_entry) };
    // SAFETY: 同上。
    let line_fn: GetLineNoFn = unsafe { core::mem::transmute(line_entry) };
    // SAFETY: 传入的 `object` 是引擎自己的 `IConsole` 实例。
    let total = unsafe { count_fn(object as *mut c_void) };
    if total <= 0 {
        return out;
    }
    let want = (total as usize).min(limit);
    let mut buffer = vec![0u8; 4096];
    for index in 0..want {
        buffer[0] = 0;
        // SAFETY: 同上；`buffer` 是可写的 4096 字节目标缓冲。
        let ok = unsafe {
            line_fn(
                object as *mut c_void,
                index as i32,
                buffer.as_mut_ptr(),
                buffer.len() as i32,
            )
        };
        if ok == 0 {
            break;
        }
        let end = buffer
            .iter()
            .position(|byte| *byte == 0)
            .unwrap_or(buffer.len());
        out.push(String::from_utf8_lossy(&buffer[..end]).into_owned());
    }
    out
}

/// `PrintLine` 钩子地址（函数项到整数的转换只在这里发生）。
fn print_line_hook_address() -> usize {
    print_line_hook as *const () as usize
}

/// `ExecuteString` 钩子地址。
fn execute_string_hook_address() -> usize {
    execute_string_hook as *const () as usize
}

/// 去掉结尾的 CR/LF：引擎传进来的文本常带换行，而它自己入缓冲前也会裁掉
/// （`RE_FINDINGS` §2.2）。不裁的话 `equals` 规则永远匹配不上。
fn trim_line(bytes: &[u8]) -> &[u8] {
    match bytes
        .iter()
        .rposition(|byte| *byte != b'\n' && *byte != b'\r')
    {
        Some(last) => &bytes[..=last],
        None => bytes,
    }
}

/// 引擎每打一行控制台文本就走这里。
///
/// 契约：绝不 panic、绝不分配（除非这一行要落日志）、`text` 为 NUL 结尾。
unsafe extern "system" fn print_line_hook(this: *mut c_void, text: *const c_char) {
    let original = PRINT_LINE_ORIGINAL.load(Ordering::SeqCst);
    // 钩子自己必须兜住 panic：`guard::catch` 只包了 FFI 入口，panic 从
    // `extern "system"` 逸出会直接 abort 进程。兜住之后一律 fail-open（放行这一行）。
    let swallow = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        if text.is_null() {
            return false;
        }
        // SAFETY: 引擎按 `const char*` 传行文本，调用方保证 NUL 结尾。
        let bytes = unsafe { CStr::from_ptr(text) }.to_bytes();
        !matches!(filter::decide(trim_line(bytes)), filter::Outcome::Pass)
    }))
    .unwrap_or_else(|_| {
        kcd2_abi::log::error("panic inside print_line_hook; forwarding the line");
        false
    });
    if swallow || original == 0 {
        return;
    }
    // SAFETY: `original` 是安装时从同一槽位读出的引擎函数地址，签名一致。
    let forward: PrintLineFn = unsafe { core::mem::transmute(original) };
    // SAFETY: 转发是原样调用引擎函数：`this`/`text` 就是本钩子收到的实参，
    // 签名与安装时读出的槽位值一致（`void(*)(IConsole*, const char*)`）。
    unsafe { forward(this, text) };
}

/// 引擎每执行一条控制台命令就走这里（包括脚本的 `System.ExecuteCommand`）。
///
/// 命中规则时**不吞命令**，只把 `bSilentMode` 翻成真：命令照常执行，回显消失。
unsafe extern "system" fn execute_string_hook(
    this: *mut c_void,
    command: *const c_char,
    silent: u8,
    defer: u8,
) {
    let original = EXECUTE_STRING_ORIGINAL.load(Ordering::SeqCst);
    if original == 0 {
        return;
    }
    // 同上：钩子自带护栏，panic 一律 fail-open（按原样转发，不静音）。
    let silence = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        if command.is_null() {
            return false;
        }
        // SAFETY: 引擎按 `const char*` 传命令文本，调用方保证 NUL 结尾。
        let bytes = unsafe { CStr::from_ptr(command) }.to_bytes();
        filter::matches_command(trim_line(bytes))
    }))
    .unwrap_or_else(|_| {
        kcd2_abi::log::error("panic inside execute_string_hook; forwarding the command");
        false
    });
    if silence {
        crate::status::count_command_silenced();
    }
    // SAFETY: `original` 是安装时从同一槽位读出的引擎函数地址，签名一致。
    let forward: ExecuteStringFn = unsafe { core::mem::transmute(original) };
    // SAFETY: 转发原样调用引擎函数；被静音时只改第二个布尔参数（`bSilentMode`）。
    unsafe { forward(this, command, if silence { 1 } else { silent }, defer) };
}
