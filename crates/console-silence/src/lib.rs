//! ConsoleSilence —— 在引擎的控制台打印层吞掉指定行。
//!
//! 动机：模组的 "LUT guard"
//! 在用户切过 LUT 之后每 16 ms 重新执行一次 `r_ColorGradingChartImage <path>`，
//! 用来对抗引擎把 chart 重置回默认值；引擎对每次执行都往控制台打一行，于是控制台
//! 被同一条消息刷满。模组内部改脚本压不掉引擎自己打的那一行，所以要在 native 层
//! 拦（见 `docs/RE_FINDINGS.md`）。
//!
//! 结构：
//! - [`console`]：定位 `gEnv->pConsole`，把 `IConsole::PrintLine`（vtable 槽位 26）
//!   换成 [`console::print_line_hook`]；
//! - [`filter`] / [`rules`]：规则与快照发布（纯逻辑，离线可测）；
//! - [`status`]：常开计数与 `ConsoleSilence.status.json`。
//!
//! 纪律：
//! - 钩子只吞**命中规则**的行，其余原样转发给引擎函数——功能上等价于没装；
//! - 只在 `IConsole::PrintLine` 这一层动手：`kcd.log` 一字不动；
//! - 一遍遍校验槽位，被游戏或别的 ASI 改回去就重装；DLL 卸载时还原；
//! - 所有 FFI 出口经 [`kcd2_abi::guard::catch`] 收口，panic 不穿越引擎边界。

#![cfg(windows)]
#![deny(missing_docs)]
#![deny(unsafe_op_in_unsafe_fn)]
#![deny(clippy::undocumented_unsafe_blocks)]

#[cfg(not(target_arch = "x86_64"))]
compile_error!("ConsoleSilence 只支持 x86_64-pc-windows-msvc（x64 调用约定）");

mod bridge;
mod console;
mod filter;
mod overlay;
mod repeat;
mod rules;
mod spec;
mod status;

use core::ffi::c_void;
use std::path::{Path, PathBuf};
use std::sync::{Mutex, OnceLock};

use kcd2_abi::module::Module;
use kcd2_abi::{env, guard, log, mem, win};

/// 日志文件名（与 ASI 同目录）。
const LOG_FILE: &str = "consolesilence.log";
/// 配置文件（与 ASI 同目录；只在缺失时生成）。
/// 状态文件（与 ASI 同目录；周期性刷新）。
const STATUS_FILE: &str = "ConsoleSilence.status.json";
/// 存在这个文件就转储控制台行缓冲（用户/作者现场取证用；读完即删）。
const DUMP_TRIGGER: &str = "ConsoleSilence.dump";
/// 转储落盘的文件名。
const DUMP_FILE: &str = "ConsoleSilence.console.txt";

/// 单实例护栏（重复注入的第二份只记录、不抢槽位）。
const MUTEX_NAME: &str = "Global\\KCD2ConsoleSilence";

/// ASI 所在目录（`DllMain` 解析一次）。
static INSTALL_DIR: OnceLock<PathBuf> = OnceLock::new();
/// 已定位但可能被重建的控制台对象。
static CONSOLE: Mutex<Option<console::Console>> = Mutex::new(None);

/// ASI 入口。
///
/// # Safety
/// 由 Windows 加载器调用；`module` 是本次映射的句柄，`reason` 是加载事件。
#[no_mangle]
pub unsafe extern "system" fn DllMain(
    module: *mut c_void,
    reason: u32,
    _reserved: *mut c_void,
) -> i32 {
    const DLL_PROCESS_ATTACH: u32 = 1;
    const DLL_PROCESS_DETACH: u32 = 0;
    // SAFETY: 加载器给出的 `module` 就是本模块句柄；file name 查询失败只是没有路径。
    let directory = unsafe { win::module_file_name(module) }
        .and_then(|path| path.parent().map(Path::to_path_buf));
    guard::catch("DllMain", 1, || {
        if reason == DLL_PROCESS_ATTACH {
            if let Some(directory) = directory {
                let _ = INSTALL_DIR.set(directory);
            }
            log::init(log_path(LOG_FILE));
            log::info(&format!(
                "ConsoleSilence {} attaching (dir={})",
                env!("CARGO_PKG_VERSION"),
                INSTALL_DIR
                    .get()
                    .map_or_else(|| "<unknown>".to_string(), |dir| dir.display().to_string())
            ));
            let (_, already_resident) = win::create_or_open_mutex(MUTEX_NAME);
            if already_resident {
                log::warn("another ConsoleSilence instance is resident; this copy stays idle");
                return 1;
            }
            if !win::spawn_detached_thread(bootstrap) {
                log::error("CreateThread(bootstrap) failed");
            }
        } else if reason == DLL_PROCESS_DETACH {
            console::restore();
            log::info("ConsoleSilence detaching");
        }
        1
    })
}

fn log_path(file: &str) -> PathBuf {
    match INSTALL_DIR.get() {
        Some(directory) => directory.join(file),
        None => PathBuf::from(file),
    }
}

extern "system" fn bootstrap(_param: *mut c_void) -> u32 {
    guard::catch("bootstrap thread", 1, || {
        let Some((base, size)) = env::wait_for_module(120_000) else {
            log::error("WHGame.dll did not load within 120 s; nothing to hook");
            return 1;
        };
        log::info(&format!("WHGame.dll base=0x{base:X} size=0x{size:X}"));
        let Some(environment) = env::locate(base, size) else {
            log::error("could not locate gEnv; console hook not installed");
            return 1;
        };
        // 出厂状态：零规则。发布一次，让 enabled/observe/scan_limit 落到快照上。
        overlay::publish();
        // Lua 面：只在 `ExecuteBuffer` 入口装钩子，注册动作留给游戏线程走到那个边界。
        bridge::install(environment);

        // pConsole 在 ASI 加载时可能还没建好：轮询到出现为止（每 5 s 记一次）。
        let mut waited_ms = 0u32;
        let console = loop {
            if let Some(found) = console::locate(environment) {
                break Some(found);
            }
            if waited_ms.is_multiple_of(5_000) {
                log::debug(&format!("waiting for gEnv->pConsole ({waited_ms} ms)"));
            }
            if waited_ms >= 300_000 {
                log::error("gEnv->pConsole did not appear within 300 s");
                break None;
            }
            win::sleep_ms(100);
            waited_ms += 100;
        };
        let Some(console) = console else {
            return 1;
        };
        {
            let mut slot = CONSOLE.lock().unwrap_or_else(|err| err.into_inner());
            *slot = Some(console);
        }
        if let Err(message) = console::install(&console) {
            log::error(&format!("installing the PrintLine hook failed: {message}"));
        }
        supervise(environment, console);
        0
    })
}

/// 常驻监督：控制台对象重建 / 槽位被改回 / 配置改动 / 状态刷新。
fn supervise(environment: *mut c_void, mut console: console::Console) {
    let mut last_status = 0u64;
    let mut last_console_check = 0u64;
    let mut last_dump_check = 0u64;
    let mut last_summary = 0u64;
    let mut last_expiry = 0u64;
    let mut reported_suppressed = 0u64;
    let mut logged_failure = false;
    loop {
        let now = filter::now_ms();
        if now.saturating_sub(last_console_check) >= 2_000 {
            last_console_check = now;
            // 每一步都用 `guard::catch` 隔离：监督线程死掉会让钩子失去校验与状态文件，
            // 症状是"钩子还在、但 status/dump 全没了"。
            console = guard::catch("supervise/hook", console, || {
                check_and_rearm(environment, console, &mut logged_failure)
            });
        }
        // `pScriptSystem` 可能比我们还晚出现：重试装钩子（只写一个 vtable 槽位，
        // 不碰 Lua VM，所以任何线程都安全；VM 里的注册由游戏线程在脚本加载时自己做）。
        guard::catch("supervise/lua", (), || {
            bridge::install(environment);
        });

        // 旧快照的回收必须在**没有读者**的时刻做，钩子线程里绝不做析构。
        guard::catch("supervise/retire", (), || {
            filter::collect_retired();
        });
        // 运行期规则可以带寿命：到期在这里清（不在打印路径上判断）。
        if now.saturating_sub(last_expiry) >= 1_000 {
            last_expiry = now;
            guard::catch("supervise/expiry", (), || {
                overlay::expire_due(now);
            });
        }
        if now.saturating_sub(last_status) >= 2_000 {
            last_status = now;
            guard::catch("supervise/status", (), write_status);
        }
        if now.saturating_sub(last_dump_check) >= 500 {
            last_dump_check = now;
            guard::catch("supervise/dump", (), maybe_dump_console);
        }
        // 折叠汇总：不打印任何东西会让用户以为控制台坏了，所以每 5 s 最多补一行。
        let suppressed = repeat::suppressed();
        if suppressed > reported_suppressed && now.saturating_sub(last_summary) >= 5_000 {
            let folded = suppressed - reported_suppressed;
            reported_suppressed = suppressed;
            last_summary = now;
            console::forward_raw(&format!(
                "[ConsoleSilence] folded {folded} repeated console line(s) (repeat window {} ms)",
                repeat::policy().window_ms
            ));
        }
        win::sleep_ms(200);
    }
}

/// 读配置（`force = true` 时无条件读一次并生成默认文件）。
/// 触发文件在场时把控制台行缓冲转储到 `ConsoleSilence.console.txt`，然后删掉触发文件。
fn maybe_dump_console() {
    let trigger = log_path(DUMP_TRIGGER);
    if !trigger.exists() {
        return;
    }
    let lines = console::dump_lines(400);
    let path = log_path(DUMP_FILE);
    let body = lines.join("\n");
    match std::fs::write(&path, format!("{}\n", body)) {
        Ok(()) => log::info(&format!(
            "console dumped to {} ({})",
            path.display(),
            lines.len()
        )),
        Err(error) => log::warn(&format!("console dump failed: {error}")),
    }
    let _ = std::fs::remove_file(&trigger);
}

/// 控制台对象可能被重建、槽位可能被别人改回去：这里负责"发现并装回去"。
///
/// 返回当前应继续使用的 [`console::Console`]（对象搬家时换成新的）。
fn check_and_rearm(
    environment: *mut c_void,
    mut console: console::Console,
    logged_failure: &mut bool,
) -> console::Console {
    let live_object = mem::read_pointer(environment as usize + env::CONSOLE_OFFSET).unwrap_or(0);
    if live_object != console.object {
        if let Some(found) = console::locate(environment) {
            log::info(&format!(
                "console object moved 0x{:X} -> 0x{:X}; re-hooking",
                console.object, found.object
            ));
            console = found;
            let mut slot = CONSOLE.lock().unwrap_or_else(|err| err.into_inner());
            *slot = Some(console);
        }
    }
    if !console::verify(&console) {
        match console::install(&console) {
            Err(message) => {
                if !*logged_failure {
                    *logged_failure = true;
                    log::error(&format!(
                        "re-installing the console hooks failed: {message}"
                    ));
                }
            }
            Ok(()) => *logged_failure = false,
        }
    }
    console
}

fn write_status() {
    let snapshot = status::snapshot();
    let (enabled, observe, rule_count) = filter::shape();
    let hits = filter::rule_hits();
    let hook_state = match console::state() {
        console::State::Pending => "pending",
        console::State::Installed => "installed",
        console::State::Lost => "lost",
        console::State::Failed => "failed",
    };
    let module_base = Module::named(console::GAME_MODULE).map_or(0, |found| found.base().get());
    let text = status::render(
        &snapshot,
        &status::Context {
            hook_state,
            original: console::original(),
            original_command: console::original_execute_string(),
            current: console::current_slot(),
            module_base,
            enabled,
            observe,
            rule_count,
            console_open: console::console_open(),
            rule_hits: &hits,
        },
    );
    let path = log_path(STATUS_FILE);
    match std::fs::write(&path, text) {
        Ok(()) => {}
        Err(error) => log::warn(&format!("writing {} failed: {error}", path.display())),
    }
}
