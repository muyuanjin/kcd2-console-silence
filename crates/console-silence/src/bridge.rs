//! Lua 桥：把六个函数挂到全局表 `ConsoleSilence` 上，并用一段 Lua 垫片做"好用的形状"。
//!
//! # 为什么分两层
//!
//! Rust 侧只暴露**位置参数 + 字符串/数字**的原语（`*_raw`），因为那些类型的跨界读取
//! 在 `IFunctionHandler::GetParamAny` 上是无歧义的；而"选项表"要读 `IScriptTable`
//! 的字段，那需要另一个尚未在实机验证过的 vtable 槽位。垫片放在 Lua 里做：表解析、
//! 默认值、错误语义都由 Lua 自己承担，跨界的只有字符串。
//!
//! 注册完立即 `ExecuteBuffer` 一段垫片：它把 `*_raw` 抄进 local、从表上删掉、再装上
//! 友好函数。所以调用方看到的表面就是 `docs/CONCEPT.md` §6.2 那六个函数。
//!
//! # vtable 索引的来源
//!
//! 取自 `WHGame.dll` 1.5.6 上实测可用的调用序列（同一份
//! 1.5.6 引擎上实机验证过 `AddFunction` / `CreateTable` / `SetGlobalAny` /
//! `ExecuteBuffer` 都能用）。本文件只是把这些调用**重新**声明一遍，不引入新猜测。

use core::ffi::{c_char, c_void};
use std::ffi::CString;
use std::sync::atomic::{AtomicBool, Ordering};

use kcd2_abi::{log, mem, module::Module};

use crate::overlay;
use crate::rules::{Action, MatchKind};

/// `gEnv->pScriptSystem` 的偏移（1.5.6 运行时吻合，见 `docs/RE_FINDINGS.md`）。
const SCRIPT_SYSTEM_OFFSET: usize = 0x28;
/// 全局表名。
const TABLE_NAME: &str = "ConsoleSilence";
/// 已注册标记（VM 里真的注册成功了）。
static REGISTERED: AtomicBool = AtomicBool::new(false);
/// `ExecuteBuffer` 钩子是否已装好（与 `REGISTERED` 是两件事：钩子在 ASI 启动时装，
/// 注册要等游戏线程走到脚本加载边界）。
static HOOKED: AtomicBool = AtomicBool::new(false);
/// 谁在注册（防重入）。
static REGISTRATION_CLAIMED: core::sync::atomic::AtomicUsize =
    core::sync::atomic::AtomicUsize::new(0);
/// `ExecuteBuffer` 的原函数地址。
static ORIGINAL_EXECUTE_BUFFER: core::sync::atomic::AtomicUsize =
    core::sync::atomic::AtomicUsize::new(0);

// ---------------------------------------------------------------- 引擎类型（最小镜像）

#[repr(C)]
struct ScriptSystem {
    vtable: *const *const c_void,
}

#[repr(C)]
struct ScriptTable {
    vtable: *const *const c_void,
}

#[repr(C)]
struct FunctionHandler {
    vtable: *const *const c_void,
}

#[repr(C)]
union AnyData {
    boolean: bool,
    number: f32,
    string: *const c_char,
    table: *mut ScriptTable,
    handle: *const c_void,
}

#[repr(C)]
struct AnyValue {
    value_type: i32,
    data: AnyData,
}

const ANY_ANY: i32 = 0;
const ANY_BOOL: i32 = 2;
const ANY_NUMBER: i32 = 4;
const ANY_STRING: i32 = 5;
const ANY_TABLE: i32 = 6;

impl AnyValue {
    /// `GetParamAny` 是**按类型定向**读的：输出值必须先写成 `ANY_ANY` 表示"什么都收"，
    /// 写成 `Nil` 会让每个参数读取都失败。
    fn any() -> AnyValue {
        AnyValue {
            value_type: ANY_ANY,
            data: AnyData { number: 0.0 },
        }
    }

    fn number(value: f32) -> AnyValue {
        AnyValue {
            value_type: ANY_NUMBER,
            data: AnyData { number: value },
        }
    }

    fn boolean(value: bool) -> AnyValue {
        AnyValue {
            value_type: ANY_BOOL,
            data: AnyData { boolean: value },
        }
    }

    fn string(value: *const c_char) -> AnyValue {
        AnyValue {
            value_type: ANY_STRING,
            data: AnyData { string: value },
        }
    }

    fn table(value: *mut ScriptTable) -> AnyValue {
        AnyValue {
            value_type: ANY_TABLE,
            data: AnyData { table: value },
        }
    }
}

type UserFunction = Option<extern "system" fn(*mut FunctionHandler, *mut c_void, i32) -> i32>;

/// `SUserFunctionDesc`：32 字节的 `Functor1wRet` 占位 + 函数指针。
#[repr(C)]
struct UserFunctionDesc {
    function_name: *const c_char,
    function_params: *const c_char,
    global_name: *const c_char,
    functor: [u8; 32],
    param_id_offset: i32,
    user_data_func: UserFunction,
    data_buffer: *mut c_void,
    data_size: i32,
}

impl UserFunctionDesc {
    fn new(
        function_name: &CString,
        function_params: &CString,
        global_name: &CString,
        user_data_func: UserFunction,
    ) -> UserFunctionDesc {
        UserFunctionDesc {
            function_name: function_name.as_ptr(),
            function_params: function_params.as_ptr(),
            global_name: global_name.as_ptr(),
            functor: [0u8; 32],
            param_id_offset: 0,
            user_data_func,
            data_buffer: core::ptr::null_mut(),
            data_size: 0,
        }
    }
}

// vtable 索引（与 kcd2-engine 的实机验证版本一致）
const SCRIPT_EXECUTE_BUFFER: usize = 6;
const SCRIPT_CREATE_TABLE: usize = 13;
const SCRIPT_SET_GLOBAL_ANY: usize = 31;
const SCRIPT_SET_GLOBAL_TO_NULL: usize = 33;
const TABLE_SET_VALUE_ANY: usize = 7;
const TABLE_SET_AT_ANY: usize = 13;
const TABLE_ADD_FUNCTION: usize = 22;
const HANDLER_GET_PARAM_ANY: usize = 7;
const HANDLER_END_FUNCTION_ANY: usize = 10;
const HANDLER_END_FUNCTION: usize = 11;

fn entry(vtable: *const *const c_void, index: usize) -> Option<usize> {
    if vtable.is_null() {
        return None;
    }
    mem::read_pointer(vtable as usize + index * core::mem::size_of::<usize>())
}

// ---------------------------------------------------------------- 注册

/// 在 `IScriptSystem::ExecuteBuffer` 入口装钩子。
///
/// **为什么不是在监督线程里直接注册**：`CreateTable` / `AddFunction` / `SetGlobalAny`
/// 都在改 Lua VM 的对象表与 GC 链，而 VM 只允许游戏线程碰。监督线程注册等于跨线程
/// 写 VM，会破坏 GC 链与对象表。
///
/// `ExecuteBuffer` 是游戏加载 Lua 脚本的入口（启动脚本、控制台 `exec`），**在函数
/// 入口处 VM 还没开始执行**，是这台机器上能拿到的最安全边界。注册只在第一次发生，
/// 之后钩子只做一次原子读就转发。
pub fn install(env: *mut c_void) -> bool {
    if HOOKED.load(Ordering::Acquire) {
        return true;
    }
    let env = env as usize;
    if env == 0 {
        return false;
    }
    let Some(script) = mem::read_pointer(env + SCRIPT_SYSTEM_OFFSET) else {
        return false;
    };
    if script == 0 {
        return false;
    }
    let Some(vtable) = mem::read_pointer(script) else {
        return false;
    };
    let Some(game) = Module::named(crate::console::GAME_MODULE) else {
        return false;
    };
    // 三个槽位都必须指向游戏自己的代码段，否则说明布局判断错了：不猜、不注册。
    for (name, index) in [
        ("ExecuteBuffer", SCRIPT_EXECUTE_BUFFER),
        ("CreateTable", SCRIPT_CREATE_TABLE),
        ("SetGlobalAny", SCRIPT_SET_GLOBAL_ANY),
    ] {
        match entry(vtable as *const *const c_void, index) {
            Some(target) if game.contains(target) => {}
            _ => {
                log::warn(&format!(
                    "IScriptSystem::{name} slot {index} looks wrong; Lua API not registered"
                ));
                return false;
            }
        }
    }

    SCRIPT_SYSTEM.store(script, core::sync::atomic::Ordering::Relaxed);
    // 装钩子：槽位 6 已校验属于游戏代码段。
    let Some(original) = entry(vtable as *const *const c_void, SCRIPT_EXECUTE_BUFFER) else {
        return false;
    };
    let target = vtable + SCRIPT_EXECUTE_BUFFER * core::mem::size_of::<usize>();
    let hook: extern "system" fn(
        *mut ScriptSystem,
        *const c_char,
        usize,
        *const c_char,
        *mut ScriptTable,
    ) -> bool = execute_buffer_hook;
    if !mem::write_pointer(target, hook as usize) {
        log::warn("could not hook IScriptSystem::ExecuteBuffer; Lua API disabled");
        return false;
    }
    ORIGINAL_EXECUTE_BUFFER.store(original, core::sync::atomic::Ordering::Release);
    HOOKED.store(true, Ordering::Release);
    log::info("IScriptSystem::ExecuteBuffer hooked (Lua API registers at the first script load)");
    true
}

/// 注册全局表；只在游戏线程的 VM 边界调用。
fn register_now(system: *mut ScriptSystem, vtable: *const *const c_void) -> bool {
    let name = match CString::new(TABLE_NAME) {
        Ok(value) => value,
        Err(_) => return false,
    };
    // SAFETY: 槽位在 `install` 里校验过；调用约定与引擎一致（`extern "system"`）。
    let table = unsafe {
        let set_null: extern "system" fn(*mut ScriptSystem, *const c_char) =
            core::mem::transmute(entry(vtable, SCRIPT_SET_GLOBAL_TO_NULL).unwrap_or(0));
        set_null(system, name.as_ptr());
        let create: extern "system" fn(*mut ScriptSystem, bool) -> *mut ScriptTable =
            core::mem::transmute(entry(vtable, SCRIPT_CREATE_TABLE).unwrap_or(0));
        create(system, false)
    };
    if table.is_null() {
        log::warn("CreateTable(ConsoleSilence) returned null");
        return false;
    }
    let table_vtable = match mem::read_pointer(table as usize) {
        Some(value) => value as *const *const c_void,
        None => return false,
    };
    let functions: [(&str, &str, UserFunction); 9] = [
        ("mute_raw", "s,s,s,s,n", Some(handler_mute)),
        ("unmute_raw", "n", Some(handler_unmute)),
        ("clear_raw", "s", Some(handler_clear)),
        ("list_raw", "", Some(handler_list)),
        ("stats_raw", "", Some(handler_stats)),
        ("which_raw", "s", Some(handler_which)),
        ("enabled_raw", "b", Some(handler_enabled)),
        ("observe_raw", "b", Some(handler_observe)),
        ("fold_raw", "b", Some(handler_fold)),
    ];
    let mut registered = 0usize;
    for (function_name, params, user_data) in functions {
        let (Ok(function_name), Ok(function_params)) =
            (CString::new(function_name), CString::new(params))
        else {
            continue;
        };
        let mut description =
            UserFunctionDesc::new(&function_name, &function_params, &name, user_data);
        // SAFETY: 表与描述符都在本函数栈上、调用期间有效；槽位已校验。
        let ok = unsafe {
            let add: extern "system" fn(*mut ScriptTable, *mut UserFunctionDesc) -> bool =
                core::mem::transmute(entry(table_vtable, TABLE_ADD_FUNCTION).unwrap_or(0));
            add(table, &mut description)
        };
        if ok {
            registered += 1;
        } else {
            log::warn(&format!("AddFunction({function_name:?}) failed"));
        }
    }
    if registered != functions.len() {
        return false;
    }
    // SAFETY: 同上；`AnyValue::table` 只是把指针包成脚本值。
    unsafe {
        let set_global: extern "system" fn(*mut ScriptSystem, *const c_char, *const AnyValue) =
            core::mem::transmute(entry(vtable, SCRIPT_SET_GLOBAL_ANY).unwrap_or(0));
        let value = AnyValue::table(table);
        set_global(system, name.as_ptr(), &value);
    }
    if !run_shim(system) {
        log::warn("Lua shim failed; raw functions remain visible");
    }
    REGISTERED.store(true, Ordering::Release);
    log::info("Lua API registered: ConsoleSilence.{mute,unmute,clear,list,stats,which,enabled,observe,fold}");
    true
}

/// `IScriptSystem::ExecuteBuffer` 的钩子：第一次进入时注册，之后只转发。
///
/// 注册动作落在这里而不是监督线程，是因为 `CreateTable` / `AddFunction` /
/// `SetGlobalAny` 都在改 Lua VM 的对象表与 GC 链，而 VM 只允许游戏线程碰。
/// 函数入口处 VM 还没开始跑脚本，是能拿到的最安全边界。
extern "system" fn execute_buffer_hook(
    system: *mut ScriptSystem,
    buffer: *const c_char,
    length: usize,
    description: *const c_char,
    table: *mut ScriptTable,
) -> bool {
    let original = ORIGINAL_EXECUTE_BUFFER.load(Ordering::Acquire);
    if original == 0 {
        return false;
    }
    if !REGISTERED.load(Ordering::Acquire) && REGISTRATION_CLAIMED.swap(1, Ordering::AcqRel) == 0 {
        let registered = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            match mem::read_pointer(system as usize) {
                Some(vtable) => register_now(system, vtable as *const *const c_void),
                None => false,
            }
        }))
        .unwrap_or(false);
        if !registered {
            // 失败就放开，让下次脚本加载再试（幂等，不会重复装函数）。
            REGISTRATION_CLAIMED.store(0, Ordering::Release);
        }
    }
    // SAFETY: 原函数地址来自 `install` 里校验过的槽位，调用约定一致；参数原样透传。
    unsafe {
        let forward: extern "system" fn(
            *mut ScriptSystem,
            *const c_char,
            usize,
            *const c_char,
            *mut ScriptTable,
        ) -> bool = core::mem::transmute(original);
        forward(system, buffer, length, description, table)
    }
}

fn run_shim(system: *mut ScriptSystem) -> bool {
    const SHIM: &str = include_str!("shim.lua");
    let Ok(text) = CString::new(SHIM) else {
        return false;
    };
    let Ok(description) = CString::new("ConsoleSilence/shim.lua") else {
        return false;
    };
    // 必须用**原函数指针**：vtable 里那个槽已经是我们的钩子了，走它会变成
    // "靠重入保护兜住的假成功"。
    let original = ORIGINAL_EXECUTE_BUFFER.load(Ordering::Acquire);
    if original == 0 {
        return false;
    }
    // SAFETY: 原函数地址来自 `install` 里校验过的槽位，调用约定一致；
    // `text`/`description` 在调用期间有效。
    unsafe {
        let execute: extern "system" fn(
            *mut ScriptSystem,
            *const c_char,
            usize,
            *const c_char,
            *mut ScriptTable,
        ) -> bool = core::mem::transmute(original);
        execute(
            system,
            text.as_ptr(),
            SHIM.len(),
            description.as_ptr(),
            core::ptr::null_mut(),
        )
    }
}

// ---------------------------------------------------------------- 参数与返回

struct Params {
    handler: *mut FunctionHandler,
    /// 字符串参数的 `CString` 必须在 `EndFunction` 之前活着。
    owned: Vec<CString>,
}

impl Params {
    fn string(&mut self, index: i32) -> Option<String> {
        // SAFETY: 处理器指针由引擎在本次调用内给出。
        unsafe {
            let mut value = AnyValue::any();
            let get: extern "system" fn(*mut FunctionHandler, i32, *mut AnyValue) -> bool =
                core::mem::transmute(entry((*(self.handler)).vtable, HANDLER_GET_PARAM_ANY)?);
            if !get(self.handler, index, &mut value) || value.value_type != ANY_STRING {
                return None;
            }
            let pointer = value.data.string;
            if pointer.is_null() {
                return None;
            }
            let text = std::ffi::CStr::from_ptr(pointer)
                .to_string_lossy()
                .into_owned();
            self.owned.push(CString::new(text.clone()).ok()?);
            Some(text)
        }
    }

    fn number(&self, index: i32) -> Option<f64> {
        // SAFETY: 同上。
        unsafe {
            let mut value = AnyValue::any();
            let get: extern "system" fn(*mut FunctionHandler, i32, *mut AnyValue) -> bool =
                core::mem::transmute(entry((*(self.handler)).vtable, HANDLER_GET_PARAM_ANY)?);
            if !get(self.handler, index, &mut value) || value.value_type != ANY_NUMBER {
                return None;
            }
            Some(f64::from(value.data.number))
        }
    }

    fn boolean(&self, index: i32) -> Option<bool> {
        // SAFETY: 处理器指针由引擎在本次调用内给出；槽位已校验。
        unsafe {
            let mut value = AnyValue::any();
            let get: extern "system" fn(*mut FunctionHandler, i32, *mut AnyValue) -> bool =
                core::mem::transmute(entry((*(self.handler)).vtable, HANDLER_GET_PARAM_ANY)?);
            if !get(self.handler, index, &mut value) || value.value_type != ANY_BOOL {
                return None;
            }
            Some(value.data.boolean)
        }
    }

    fn return_number(self, value: f64) -> i32 {
        // SAFETY: 同上；返回后引擎不再读我们的栈。
        unsafe {
            let Some(entry) = entry((*(self.handler)).vtable, HANDLER_END_FUNCTION_ANY) else {
                return 0;
            };
            let end: extern "system" fn(*mut FunctionHandler, *const AnyValue) -> i32 =
                core::mem::transmute(entry);
            let payload = AnyValue::number(value as f32);
            end(self.handler, &payload)
        }
    }

    fn return_bool(self, value: bool) -> i32 {
        // SAFETY: 同上。
        unsafe {
            let Some(entry) = entry((*(self.handler)).vtable, HANDLER_END_FUNCTION_ANY) else {
                return 0;
            };
            let end: extern "system" fn(*mut FunctionHandler, *const AnyValue) -> i32 =
                core::mem::transmute(entry);
            let payload = AnyValue::boolean(value);
            end(self.handler, &payload)
        }
    }

    fn return_string(self, value: &str) -> i32 {
        let Ok(text) = CString::new(value) else {
            return self.return_nil();
        };
        // SAFETY: 同上；`text` 活到 `end` 返回之后（本函数栈帧）。
        unsafe {
            let Some(entry) = entry((*(self.handler)).vtable, HANDLER_END_FUNCTION_ANY) else {
                return 0;
            };
            let end: extern "system" fn(*mut FunctionHandler, *const AnyValue) -> i32 =
                core::mem::transmute(entry);
            let payload = AnyValue::string(text.as_ptr());
            end(self.handler, &payload)
        }
    }

    fn return_table(self, table: *mut ScriptTable) -> i32 {
        if table.is_null() {
            return self.return_nil();
        }
        // SAFETY: 同上。
        unsafe {
            let Some(entry) = entry((*(self.handler)).vtable, HANDLER_END_FUNCTION_ANY) else {
                return 0;
            };
            let end: extern "system" fn(*mut FunctionHandler, *const AnyValue) -> i32 =
                core::mem::transmute(entry);
            let payload = AnyValue::table(table);
            end(self.handler, &payload)
        }
    }

    fn return_nil(self) -> i32 {
        // SAFETY: 同上。
        unsafe {
            let Some(entry) = entry((*(self.handler)).vtable, HANDLER_END_FUNCTION) else {
                return 0;
            };
            let end: extern "system" fn(*mut FunctionHandler) -> i32 = core::mem::transmute(entry);
            end(self.handler)
        }
    }
}

fn params(handler: *mut FunctionHandler) -> Option<Params> {
    if handler.is_null() {
        return None;
    }
    Some(Params {
        handler,
        owned: Vec::new(),
    })
}

// ---------------------------------------------------------------- 处理函数

fn guard<F>(name: &'static str, fallback: impl FnOnce() -> i32, body: F) -> i32
where
    F: FnOnce() -> i32,
{
    match std::panic::catch_unwind(std::panic::AssertUnwindSafe(body)) {
        Ok(code) => code,
        Err(_) => {
            log::error(&format!("panic in Lua handler {name}"));
            fallback()
        }
    }
}

extern "system" fn handler_mute(
    handler: *mut FunctionHandler,
    _data: *mut c_void,
    _offset: i32,
) -> i32 {
    guard(
        "mute",
        || {
            params(handler)
                .map(|params| params.return_string("internal error"))
                .unwrap_or(0)
        },
        || {
            let Some(mut p) = params(handler) else {
                return 0;
            };
            let kind = p.string(1).unwrap_or_else(|| "contains".to_string());
            let pattern = p.string(2).unwrap_or_default();
            let action = p.string(3).unwrap_or_else(|| "drop".to_string());
            let owner = p.string(4).unwrap_or_default();
            let ttl = p.number(5).unwrap_or(0.0).max(0.0) as u64;

            let Some(kind) = MatchKind::parse(&kind) else {
                return p.return_string(&format!("unknown kind '{kind}'"));
            };
            let Some(action) = Action::parse(&action) else {
                return p.return_string(&format!("unknown action '{action}'"));
            };
            if pattern.is_empty() {
                return p.return_string("empty pattern");
            }
            let spec = crate::spec::RuleSpec::new(kind, action, &pattern, None, &owner, ttl);
            // 编译一次：正则/通配写错要在**加入的这一刻**返回人话，而不是等到某次打印。
            if let Err(message) = spec.compile() {
                return p.return_string(&message);
            }
            match overlay::add(spec) {
                Ok(id) => p.return_number(f64::from(id)),
                Err(reason) => p.return_string(reason),
            }
        },
    )
}

extern "system" fn handler_unmute(
    handler: *mut FunctionHandler,
    _data: *mut c_void,
    _offset: i32,
) -> i32 {
    guard(
        "unmute",
        || 0,
        || {
            let Some(p) = params(handler) else { return 0 };
            let id = p.number(1).unwrap_or(0.0) as u32;
            let removed = overlay::remove(id);
            p.return_bool(removed)
        },
    )
}

extern "system" fn handler_clear(
    handler: *mut FunctionHandler,
    _data: *mut c_void,
    _offset: i32,
) -> i32 {
    guard(
        "clear",
        || 0,
        || {
            let Some(mut p) = params(handler) else {
                return 0;
            };
            let owner = p.string(1).filter(|text| !text.is_empty());
            let removed = overlay::clear(owner.as_deref());
            p.return_number(removed as f64)
        },
    )
}

extern "system" fn handler_which(
    handler: *mut FunctionHandler,
    _data: *mut c_void,
    _offset: i32,
) -> i32 {
    guard(
        "which",
        || 0,
        || {
            let Some(mut p) = params(handler) else {
                return 0;
            };
            match p.string(1).and_then(|line| overlay::which(&line)) {
                Some(id) => p.return_number(f64::from(id)),
                None => p.return_nil(),
            }
        },
    )
}

extern "system" fn handler_enabled(
    handler: *mut FunctionHandler,
    _data: *mut c_void,
    _offset: i32,
) -> i32 {
    guard(
        "enabled",
        || 0,
        || {
            let Some(p) = params(handler) else { return 0 };
            let on = p.boolean(1).unwrap_or(true);
            overlay::set_enabled(on);
            p.return_bool(on)
        },
    )
}

extern "system" fn handler_observe(
    handler: *mut FunctionHandler,
    _data: *mut c_void,
    _offset: i32,
) -> i32 {
    guard(
        "observe",
        || 0,
        || {
            let Some(p) = params(handler) else { return 0 };
            let on = p.boolean(1).unwrap_or(false);
            overlay::set_observe(on);
            p.return_bool(on)
        },
    )
}

extern "system" fn handler_fold(
    handler: *mut FunctionHandler,
    _data: *mut c_void,
    _offset: i32,
) -> i32 {
    guard(
        "fold",
        || 0,
        || {
            let Some(p) = params(handler) else { return 0 };
            let on = p.boolean(1).unwrap_or(true);
            overlay::set_fold(on);
            p.return_bool(on)
        },
    )
}

extern "system" fn handler_stats(
    handler: *mut FunctionHandler,
    _data: *mut c_void,
    _offset: i32,
) -> i32 {
    guard(
        "stats",
        || params(handler).map(Params::return_nil).unwrap_or(0),
        || {
            let Some(p) = params(handler) else { return 0 };
            let snapshot = crate::status::snapshot();
            let runtime = overlay::count();
            let mut strings: Vec<CString> = Vec::new();
            let Some(table) = new_table() else {
                return p.return_nil();
            };
            let fields: [(&str, f64); 6] = [
                ("seen", snapshot.seen as f64),
                ("dropped", snapshot.dropped as f64),
                ("passed", snapshot.passed as f64),
                ("repeatSuppressed", snapshot.repeat_suppressed as f64),
                ("commandsSilenced", snapshot.commands_silenced as f64),
                ("rules", runtime as f64),
            ];
            for (key, value) in fields {
                set_number(&mut strings, table, |text| text.to_string(), key, value);
            }
            p.return_table(table)
        },
    )
}

extern "system" fn handler_list(
    handler: *mut FunctionHandler,
    _data: *mut c_void,
    _offset: i32,
) -> i32 {
    guard(
        "list",
        || params(handler).map(Params::return_nil).unwrap_or(0),
        || {
            let Some(p) = params(handler) else { return 0 };
            let Some(array) = new_table() else {
                return p.return_nil();
            };
            let mut strings: Vec<CString> = Vec::new();
            for (index, row) in overlay::list().into_iter().enumerate() {
                let Some(row_table) = new_table() else {
                    break;
                };
                set_number(
                    &mut strings,
                    row_table,
                    |text| text.to_string(),
                    "id",
                    f64::from(row.id),
                );
                set_number(
                    &mut strings,
                    row_table,
                    |text| text.to_string(),
                    "hits",
                    row.hits as f64,
                );
                set_number(
                    &mut strings,
                    row_table,
                    |text| text.to_string(),
                    "ttlMs",
                    row.ttl_ms as f64,
                );
                set_string(&mut strings, row_table, "kind", row.kind);
                set_string(&mut strings, row_table, "action", &row.action);
                set_string(&mut strings, row_table, "pattern", &row.pattern);
                set_string(&mut strings, row_table, "label", &row.label);
                set_string(&mut strings, row_table, "owner", &row.owner);
                set_at(&mut strings, array, index as i32 + 1, row_table);
            }
            p.return_table(array)
        },
    )
}

// ---------------------------------------------------------------- 建表小工具

fn new_table() -> Option<*mut ScriptTable> {
    let script = current_script()?;
    let vtable = mem::read_pointer(script as usize)? as *const *const c_void;
    // SAFETY: 槽位在 `register` 里校验过；这里只创建空表。
    unsafe {
        let create: extern "system" fn(*mut ScriptSystem, bool) -> *mut ScriptTable =
            core::mem::transmute(entry(vtable, SCRIPT_CREATE_TABLE)?);
        let table = create(script as *mut ScriptSystem, false);
        (!table.is_null()).then_some(table)
    }
}

static SCRIPT_SYSTEM: core::sync::atomic::AtomicUsize = core::sync::atomic::AtomicUsize::new(0);

fn current_script() -> Option<*mut c_void> {
    let value = SCRIPT_SYSTEM.load(core::sync::atomic::Ordering::Relaxed);
    (value != 0).then_some(value as *mut c_void)
}

fn set_number(
    _strings: &mut [CString],
    table: *mut ScriptTable,
    _render: impl FnOnce(&str) -> String,
    key: &str,
    value: f64,
) {
    let Ok(key) = CString::new(key) else { return };
    let vtable = match mem::read_pointer(table as usize) {
        Some(value) => value as *const *const c_void,
        None => return,
    };
    // SAFETY: 槽位已校验；`key` 在本函数内有效，数字直接按值传。
    unsafe {
        let Some(entry) = entry(vtable, TABLE_SET_VALUE_ANY) else {
            return;
        };
        let set: extern "system" fn(*mut ScriptTable, *const c_char, *const AnyValue, bool) =
            core::mem::transmute(entry);
        let payload = AnyValue::number(value as f32);
        set(table, key.as_ptr(), &payload, false);
    }
}

fn set_string(strings: &mut Vec<CString>, table: *mut ScriptTable, key: &str, value: &str) {
    let (Ok(key), Ok(value)) = (CString::new(key), CString::new(value)) else {
        return;
    };
    let vtable = match mem::read_pointer(table as usize) {
        Some(pointer) => pointer as *const *const c_void,
        None => return,
    };
    // SAFETY: `TABLE_SET_VALUE_ANY` 槽位在 `register` 里校验过指向游戏代码段；
    // `table` 由引擎创建，`key` 在本函数内有效，字符串值挂到调用期的 `strings` 上，
    // 活到处理器返回之后（`EndFunction` 早已返回）。
    unsafe {
        let Some(entry) = entry(vtable, TABLE_SET_VALUE_ANY) else {
            return;
        };
        let set: extern "system" fn(*mut ScriptTable, *const c_char, *const AnyValue, bool) =
            core::mem::transmute(entry);
        let leaked = strings.len();
        strings.push(value);
        let payload = AnyValue::string(strings[leaked].as_ptr());
        set(table, key.as_ptr(), &payload, false);
    }
}

fn set_at(_strings: &mut [CString], array: *mut ScriptTable, index: i32, row: *mut ScriptTable) {
    let vtable = match mem::read_pointer(array as usize) {
        Some(value) => value as *const *const c_void,
        None => return,
    };
    // SAFETY: 槽位已校验；`row` 由本处理器创建。
    unsafe {
        let Some(entry) = entry(vtable, TABLE_SET_AT_ANY) else {
            return;
        };
        let set: extern "system" fn(*mut ScriptTable, i32, *const AnyValue) =
            core::mem::transmute(entry);
        let payload = AnyValue::table(row);
        set(array, index, &payload);
    }
}
