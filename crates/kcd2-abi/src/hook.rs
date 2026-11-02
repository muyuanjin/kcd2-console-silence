//! x86-64 inline hook 与指针补丁（无外部依赖）。
//!
//! 机制：从目标入口偷 `STEAL`（默认 7）个字节到可执行跳板，入口改成
//! `E9` 跳到**近块**（在目标 ±2 GB 内新分配），近块用绝对间接跳转进入本 crate
//! 的静态存根；存根保存参数寄存器、调用 Rust 处理器、然后尾跳进跳板（同样在
//! 近块内），跳板重放被偷指令并 `E9` 回 `entry+steal`，原函数照常执行。
//!
//! 可达性是硬约束：`E9` 只能表达 rel32。本 crate 的静态存根随 DLL 加载在任意
//! 地址——DLL 与游戏模块相距可超过 2 GB（实测一次注入相距 3.87 GB，入口直接
//! `E9` 到存根会让位移截断、游戏跳到垃圾地址 NX 崩溃），因此入口与回跳一律只
//! 指向近块；近块与静态存根之间走绝对寻址。安装前逐项校验可达性，越界即
//! fail-closed 拒绝安装。
//!
//! 契约（§6.4/§5.7）：
//! - 每个补丁都记录原始字节，[`HookGuard`] 或 [`restore_all`] 可以逐字节恢复；
//! - 目标入口已被 `E9` 补丁过（另一个实例/上次注入）时拒绝安装（不叠跳板）；
//! - 处理器函数指针必须在安装前经 [`set_handler`] 注入到对应槽位——存根经槽位
//!   间接调用，这样跳板/存根可以待在本 crate，而处理器归引擎绑定层。

use core::ffi::c_void;
use core::sync::atomic::{AtomicUsize, Ordering};

use crate::log;
use crate::win::{
    VirtualAlloc, VirtualFree, VirtualProtect, MEM_COMMIT_RESERVE, MEM_RELEASE,
    PAGE_EXECUTE_READWRITE,
};

/// 3 参数处理器（rcx/rdx/r8）。
pub type Handler3 = extern "system" fn(usize, usize, usize);
/// 4 参数处理器（rcx/rdx/r8/r9）。
pub type Handler4 = extern "system" fn(usize, usize, usize, usize);

/// 窃取字节数：等号入口前两条指令是 3 + 4 字节，正好可整取。
pub const STEAL: usize = 7;

// —— 存根经这些槽位间接调用处理器（值由 set_handler 注入）——
static HANDLER3: AtomicUsize = AtomicUsize::new(0);
static HANDLER4: AtomicUsize = AtomicUsize::new(0);
static HANDLER4B: AtomicUsize = AtomicUsize::new(0);
static HANDLER_EQ: AtomicUsize = AtomicUsize::new(0);
static HANDLER_SAVE: AtomicUsize = AtomicUsize::new(0);

// 跳板地址（0 = 未安装）。
static TRAMPOLINE: AtomicUsize = AtomicUsize::new(0);
static TRAMPOLINE4: AtomicUsize = AtomicUsize::new(0);
static TRAMPOLINE4B: AtomicUsize = AtomicUsize::new(0);
static TRAMPOLINE_EQ: AtomicUsize = AtomicUsize::new(0);
static TRAMPOLINE_SAVE: AtomicUsize = AtomicUsize::new(0);

core::arch::global_asm!(
    ".globl kcd2_hook_stub3",
    ".p2align 4",
    "kcd2_hook_stub3:",
    "push rcx",
    "push rdx",
    "push r8",
    "push r9",
    "sub rsp, 0x28",
    "mov rcx, [rsp+0x40]",
    "mov rdx, [rsp+0x38]",
    "mov r8, [rsp+0x30]",
    "lea rax, [rip + {handler}]",
    "mov rax, [rax]",
    "call rax",
    "add rsp, 0x28",
    "pop r9",
    "pop r8",
    "pop rdx",
    "pop rcx",
    "jmp qword ptr [rip + {tramp}]",
    handler = sym HANDLER3,
    tramp = sym TRAMPOLINE,
);

core::arch::global_asm!(
    ".globl kcd2_hook_stub4",
    ".p2align 4",
    "kcd2_hook_stub4:",
    "push rcx",
    "push rdx",
    "push r8",
    "push r9",
    "sub rsp, 0x28",
    "mov rcx, [rsp+0x40]",
    "mov rdx, [rsp+0x38]",
    "mov r8, [rsp+0x30]",
    "mov r9, [rsp+0x28]",
    "lea rax, [rip + {handler}]",
    "mov rax, [rax]",
    "call rax",
    "add rsp, 0x28",
    "pop r9",
    "pop r8",
    "pop rdx",
    "pop rcx",
    "jmp qword ptr [rip + {tramp}]",
    handler = sym HANDLER4,
    tramp = sym TRAMPOLINE4,
);

// 第二个独立的 4 参数存根：一次探针可以同时观察两个引擎函数。
core::arch::global_asm!(
    ".globl kcd2_hook_stub4b",
    ".p2align 4",
    "kcd2_hook_stub4b:",
    "push rcx",
    "push rdx",
    "push r8",
    "push r9",
    "sub rsp, 0x28",
    "mov rcx, [rsp+0x40]",
    "mov rdx, [rsp+0x38]",
    "mov r8, [rsp+0x30]",
    "mov r9, [rsp+0x28]",
    "lea rax, [rip + {handler}]",
    "mov rax, [rax]",
    "call rax",
    "add rsp, 0x28",
    "pop r9",
    "pop r8",
    "pop rdx",
    "pop rcx",
    "jmp qword ptr [rip + {tramp}]",
    handler = sym HANDLER4B,
    tramp = sym TRAMPOLINE4B,
);

// 存档观察存根（产品持久化专用槽位）：处理器读存档记录的 SaveId 与 playline。
core::arch::global_asm!(
    ".globl kcd2_hook_stub_save",
    ".p2align 4",
    "kcd2_hook_stub_save:",
    "push rcx",
    "push rdx",
    "push r8",
    "push r9",
    "sub rsp, 0x28",
    "mov rcx, [rsp+0x40]",
    "mov rdx, [rsp+0x38]",
    "mov r8, [rsp+0x30]",
    "mov r9, [rsp+0x28]",
    "lea rax, [rip + {handler}]",
    "mov rax, [rax]",
    "call rax",
    "add rsp, 0x28",
    "pop r9",
    "pop r8",
    "pop rdx",
    "pop rcx",
    "jmp qword ptr [rip + {tramp}]",
    handler = sym HANDLER_SAVE,
    tramp = sym TRAMPOLINE_SAVE,
);

// worker 追踪存根：额外捕获调用者的返回地址（原 `[rsp]`），
// 这样一份 golden sample 能回答"是哪个引擎函数发起了这次 equip/unequip"。
core::arch::global_asm!(
    ".globl kcd2_hook_stub_eq",
    ".p2align 4",
    "kcd2_hook_stub_eq:",
    "push rcx",
    "push rdx",
    "push r8",
    "push r9",
    "sub rsp, 0x28",
    "mov rcx, [rsp+0x40]",
    "mov rdx, [rsp+0x38]",
    "mov r8, [rsp+0x30]",
    "mov r9, [rsp+0x48]",
    "lea rax, [rip + {handler}]",
    "mov rax, [rax]",
    "call rax",
    "add rsp, 0x28",
    "pop r9",
    "pop r8",
    "pop rdx",
    "pop rcx",
    "jmp qword ptr [rip + {tramp}]",
    handler = sym HANDLER_EQ,
    tramp = sym TRAMPOLINE_EQ,
);

extern "C" {
    fn kcd2_hook_stub3();
    fn kcd2_hook_stub4();
    fn kcd2_hook_stub4b();
    fn kcd2_hook_stub_eq();
    fn kcd2_hook_stub_save();
}

/// 每个存根槽位。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum StubKind {
    Three,
    Four,
    FourB,
    EquipWorker,
    SaveObserver,
}

struct PatchRecord {
    entry: usize,
    length: usize,
    original: [u8; 15],
}

static PATCHES: std::sync::Mutex<Vec<PatchRecord>> = std::sync::Mutex::new(Vec::new());
static TRAMPOLINES: std::sync::Mutex<Vec<usize>> = std::sync::Mutex::new(Vec::new());

/// 注入一个存根的处理器（必须在安装前调用）。
///
/// 合同：`handler` 的签名必须与存根匹配（3 参数存根 = [`Handler3`]，
/// 其余 = [`Handler4`]），且必须永不返回（存根把它的返回值当跳板地址用）。
/// 存根只在对应入口被补丁后才会执行，因此"设置错误的处理器"只可能发生在
/// 我们自己的安装序列里——用函数指针类型而不是 `unsafe` 表达这个约束。
pub fn set_handler(kind_slot: u8, handler: usize) {
    let slot = match kind_slot {
        0 => &HANDLER3,
        1 => &HANDLER4,
        2 => &HANDLER4B,
        3 => &HANDLER_EQ,
        4 => &HANDLER_SAVE,
        _ => return,
    };
    slot.store(handler, Ordering::SeqCst);
}

/// 安装 3 参数追踪钩子。
///
/// # Safety
/// `entry` 必须是函数入口且前 [`STEAL`] 字节构成完整指令；处理器经
/// [`set_handler`]（槽位 0）注入。
pub unsafe fn install_trace3(entry: usize, handler: Handler3) -> Result<HookGuard, &'static str> {
    set_handler(0, handler as usize);
    install_inner(
        entry,
        STEAL,
        kcd2_hook_stub3 as *const c_void,
        StubKind::Three,
    )
}

/// 安装可定制窃取长度的 4 参数追踪钩子（跳板回跳到 `entry + steal`，长度必须
/// 落在指令边界上；允许 5..=15）。
///
/// # Safety
/// 同 [`install_trace3`]（槽位 1）。
pub unsafe fn install_trace4(
    entry: usize,
    steal: usize,
    handler: Handler4,
) -> Result<HookGuard, &'static str> {
    set_handler(1, handler as usize);
    install_inner(
        entry,
        steal,
        kcd2_hook_stub4 as *const c_void,
        StubKind::Four,
    )
}

/// 第二个 4 参数追踪槽（槽位 2）。
///
/// # Safety
/// 同 [`install_trace4`]。
pub unsafe fn install_trace4b(
    entry: usize,
    steal: usize,
    handler: Handler4,
) -> Result<HookGuard, &'static str> {
    set_handler(2, handler as usize);
    install_inner(
        entry,
        steal,
        kcd2_hook_stub4b as *const c_void,
        StubKind::FourB,
    )
}

/// equip worker 追踪（槽位 3；处理器的第 4 个参数是调用者返回地址）。
///
/// # Safety
/// 同 [`install_trace4`]。
pub unsafe fn install_equip_trace(
    entry: usize,
    steal: usize,
    handler: Handler4,
) -> Result<HookGuard, &'static str> {
    set_handler(3, handler as usize);
    install_inner(
        entry,
        steal,
        kcd2_hook_stub_eq as *const c_void,
        StubKind::EquipWorker,
    )
}

/// 安装存档观察钩子（槽位 4，产品持久化专用；与诊断槽位隔离）。
///
/// # Safety
/// 同 [`install_trace4`]：`entry` 必须是函数入口，`steal`（5..=15）落在指令边界上。
pub unsafe fn install_save_observer(
    entry: usize,
    steal: usize,
    handler: Handler4,
) -> Result<HookGuard, &'static str> {
    set_handler(4, handler as usize);
    install_inner(
        entry,
        steal,
        kcd2_hook_stub_save as *const c_void,
        StubKind::SaveObserver,
    )
}

/// 已探针的目标 `(入口, 窃取长度)`（审计日志用）。
pub fn patched_sites() -> Vec<(usize, usize)> {
    let patches = PATCHES.lock().unwrap_or_else(|err| err.into_inner());
    patches
        .iter()
        .map(|record| (record.entry, record.length))
        .collect()
}

/// 恢复全部探针入口并释放跳板；返回人类可读的审计文本。
pub fn restore_all() -> String {
    let mut out = String::new();
    let records: Vec<PatchRecord> = {
        let mut patches = PATCHES.lock().unwrap_or_else(|err| err.into_inner());
        std::mem::take(&mut *patches)
    };
    for record in &records {
        out.push_str(&restore_entry(record));
    }
    let trampolines: Vec<usize> = {
        let mut list = TRAMPOLINES.lock().unwrap_or_else(|err| err.into_inner());
        std::mem::take(&mut *list)
    };
    for trampoline in trampolines {
        // SAFETY: 跳板由 `VirtualAlloc` 分配、已不再被引用。
        unsafe { VirtualFree(trampoline as *mut c_void, 0, MEM_RELEASE) };
    }
    for slot in [
        &TRAMPOLINE,
        &TRAMPOLINE4,
        &TRAMPOLINE4B,
        &TRAMPOLINE_EQ,
        &TRAMPOLINE_SAVE,
    ] {
        slot.store(0, Ordering::SeqCst);
    }
    out
}

fn restore_entry(record: &PatchRecord) -> String {
    let mut old = 0u32;
    // SAFETY: 入口点来自已安装的补丁记录，长度 5..=15；恢复的是我们改写前的字节。
    let protect_ok = unsafe {
        VirtualProtect(
            record.entry as *mut c_void,
            record.length,
            PAGE_EXECUTE_READWRITE,
            &mut old,
        )
    } != 0;
    if !protect_ok {
        return format!("FAILED to restore 0x{:X} (VirtualProtect)\n", record.entry);
    }
    // SAFETY: 目标是本进程已加载代码页，`original` 是原字节副本。
    unsafe {
        core::ptr::copy_nonoverlapping(
            record.original.as_ptr(),
            record.entry as *mut u8,
            record.length,
        );
    }
    let mut restored = 0u32;
    // SAFETY: 恢复原保护属性。
    unsafe {
        VirtualProtect(
            record.entry as *mut c_void,
            record.length,
            old,
            &mut restored,
        );
    }
    format!(
        "restored inline patch at 0x{:X} ({} bytes)\n",
        record.entry, record.length
    )
}

/// 补丁的 RAII 句柄：`Drop` 时恢复入口（§5.7 把"忘记恢复"降级为不可发生）。
#[must_use = "drop 该句柄即恢复补丁；忘记持有等于补丁悬空"]
pub struct HookGuard {
    entry: usize,
    armed: bool,
}

impl HookGuard {
    /// 被补丁的入口地址。
    #[must_use]
    pub fn entry(&self) -> usize {
        self.entry
    }

    /// 显式恢复（等价于 `drop`，但返回审计文本）。
    #[must_use]
    pub fn restore(mut self) -> String {
        let text = self.restore_inner();
        self.armed = false;
        text
    }

    fn restore_inner(&mut self) -> String {
        let records: Vec<PatchRecord> = {
            let mut patches = PATCHES.lock().unwrap_or_else(|err| err.into_inner());
            let mut taken = Vec::new();
            let mut rest = Vec::new();
            for record in patches.drain(..) {
                if record.entry == self.entry {
                    taken.push(record);
                } else {
                    rest.push(record);
                }
            }
            *patches = rest;
            taken
        };
        records
            .iter()
            .map(restore_entry)
            .collect::<Vec<_>>()
            .join("")
    }
}

impl Drop for HookGuard {
    fn drop(&mut self) {
        if self.armed {
            self.armed = false;
            let _ = self.restore_inner();
        }
    }
}

/// 该入口是否已被 `E9` 补丁（另一个实例/上次注入）——重复挂钩会叠跳板。
///
/// # Safety
/// `entry` 必须指向已加载代码（调用方保证）。
#[must_use]
pub unsafe fn is_patched(entry: usize) -> bool {
    // SAFETY: 调用方保证 entry 是代码地址。
    unsafe { *(entry as *const u8) == 0xE9 }
}

/// rel32 相对跳转的安全窗（留余量；`E9` 实际极限是 ±2 GiB）。
const NEAR_LIMIT: usize = 0x7000_0000;

/// 编码 `E9 rel32`（从 `op` 跳到 `to`）；超出 rel32 可达范围返回 `None`。
///
/// fail-closed 的唯一入口：位移越界绝不允许截断成错误地址（历史上这样把游戏
/// 跳到过 0x7FFD7D8010C0 的 NX 页）。
fn encode_e9(op: usize, to: usize) -> Option<[u8; 5]> {
    let rel = to as i64 - (op as i64 + 5);
    let rel32 = i32::try_from(rel).ok()?;
    let mut bytes = [0u8; 5];
    bytes[0] = 0xE9;
    bytes[1..].copy_from_slice(&rel32.to_le_bytes());
    Some(bytes)
}

/// 编码近跳块：`jmp qword ptr [rip+0]` + 8 字节绝对目标（对任意距离可达）。
fn encode_indirect_jump(target: usize) -> [u8; 14] {
    let mut bytes = [0u8; 14];
    bytes[..6].copy_from_slice(&[0xFF, 0x25, 0x00, 0x00, 0x00, 0x00]);
    bytes[6..].copy_from_slice(&(target as u64).to_le_bytes());
    bytes
}

/// 在离 `entry` ±[`NEAR_LIMIT`] 内分配一页可执行内存；找不到返回 `None`。
fn allocate_near(entry: usize) -> Option<*mut c_void> {
    let mut delta = 0x10000usize;
    while delta < NEAR_LIMIT {
        for candidate in [entry.wrapping_sub(delta), entry.wrapping_add(delta)] {
            // SAFETY: 申请一页可执行内存；失败返回 null。
            let allocated = unsafe {
                VirtualAlloc(
                    candidate as *mut c_void,
                    0x1000,
                    MEM_COMMIT_RESERVE,
                    PAGE_EXECUTE_READWRITE,
                )
            };
            if !allocated.is_null() {
                let distance = (allocated as i64 - entry as i64).unsigned_abs() as usize;
                if distance < NEAR_LIMIT {
                    return Some(allocated);
                }
                // SAFETY: 刚分配、未被引用。
                unsafe { VirtualFree(allocated, 0, MEM_RELEASE) };
            }
        }
        delta <<= 1;
    }
    None
}

fn install_inner(
    entry: usize,
    steal: usize,
    stub: *const c_void,
    kind: StubKind,
) -> Result<HookGuard, &'static str> {
    if entry == 0 || !(5..=15).contains(&steal) {
        return Err("bad entry");
    }
    // 幂等：同一入口不重复挂钩（再次窃取已补丁的字节会嵌套跳板）。
    {
        let patches = PATCHES.lock().unwrap_or_else(|err| err.into_inner());
        if patches.iter().any(|record| record.entry == entry) {
            return Ok(HookGuard {
                entry,
                armed: false,
            });
        }
    }
    // 分配一个离入口 ±2 GB 内（rel32 窗口）的可执行块，供近跳块与跳板共用。
    let Some(page) = allocate_near(entry) else {
        return Err("no near trampoline allocation");
    };
    let page = page as *mut u8;
    // 布局：[0..14) 近跳块（`jmp qword ptr [rip]` + 8B 绝对地址）；[0x10..)
    // 跳板（被偷字节 + `jmp entry+steal`）。两段都在同一近页内，入口只依赖
    // 本页可达；到静态存根的距离不再有任何约束。
    let near_jump = page;
    let trampoline = (page as usize + 0x10) as *mut u8;

    // 近跳块 -> 静态存根（绝对寻址）。
    let block = encode_indirect_jump(stub as usize);
    // SAFETY: 新分配页（RWX）内的 14 字节写入；目标区间不重叠。
    unsafe {
        core::ptr::copy_nonoverlapping(block.as_ptr(), near_jump, block.len());
    }

    // 跳板：重放被偷字节 + `jmp entry+steal`（rel32；分配窗口保证可达）。
    let resume = entry + steal;
    let Some(resume_jump) = encode_e9(trampoline as usize + steal, resume) else {
        // SAFETY: 刚分配、未被引用。
        unsafe { VirtualFree(page as *mut c_void, 0, MEM_RELEASE) };
        return Err("resume jump out of rel32 range");
    };
    // SAFETY: 拷贝入口处的 steal 个字节到新分配的可执行页（两段内存互不重叠），
    // 页容量 0x1000 > 0x10 + steal + 5。
    unsafe {
        core::ptr::copy_nonoverlapping(entry as *const u8, trampoline, steal);
        core::ptr::copy_nonoverlapping(
            resume_jump.as_ptr(),
            trampoline.add(steal),
            resume_jump.len(),
        );
    }
    trampoline_slot(kind).store(trampoline as usize, Ordering::SeqCst);
    {
        let mut list = TRAMPOLINES.lock().unwrap_or_else(|err| err.into_inner());
        list.push(page as usize);
    }

    // 改写入口：`jmp near_jump` + NOP 填充（可达性再次显式校验，越界即拒绝）。
    let Some(entry_jump) = encode_e9(entry, near_jump as usize) else {
        // SAFETY: 页刚分配、未被入口引用，可直接回收。
        unsafe { VirtualFree(page as *mut c_void, 0, MEM_RELEASE) };
        return Err("near jump block out of rel32 range");
    };
    let mut old = 0u32;
    // SAFETY: 入口是代码页；`steal` 字节落在两条完整指令上（调用方保证）。
    let protect_ok = unsafe {
        VirtualProtect(
            entry as *mut c_void,
            steal,
            PAGE_EXECUTE_READWRITE,
            &mut old,
        )
    } != 0;
    if !protect_ok {
        return Err("VirtualProtect (entry) failed");
    }
    let mut original = [0u8; 15];
    // SAFETY: 同上；备份原字节供恢复。
    unsafe {
        core::ptr::copy_nonoverlapping(entry as *const u8, original.as_mut_ptr(), steal);
    }
    {
        let mut patches = PATCHES.lock().unwrap_or_else(|err| err.into_inner());
        patches.push(PatchRecord {
            entry,
            length: steal,
            original,
        });
    }
    let stub_address = stub as usize;
    // SAFETY: 同上的代码页改写。
    unsafe {
        core::ptr::copy_nonoverlapping(entry_jump.as_ptr(), entry as *mut u8, entry_jump.len());
        for index in 5..steal {
            *((entry + index) as *mut u8) = 0x90;
        }
    }
    let mut restored = 0u32;
    // SAFETY: 恢复原保护。
    unsafe {
        VirtualProtect(entry as *mut c_void, steal, old, &mut restored);
    }
    log::info(&format!(
        "hook: installed at 0x{entry:X} -> near 0x{near:X} -> stub 0x{stub_address:X} \
         (trampoline 0x{trampoline:X})",
        near = near_jump as usize,
        trampoline = trampoline as usize
    ));
    Ok(HookGuard { entry, armed: true })
}

fn trampoline_slot(kind: StubKind) -> &'static AtomicUsize {
    match kind {
        StubKind::Three => &TRAMPOLINE,
        StubKind::Four => &TRAMPOLINE4,
        StubKind::FourB => &TRAMPOLINE4B,
        StubKind::EquipWorker => &TRAMPOLINE_EQ,
        StubKind::SaveObserver => &TRAMPOLINE_SAVE,
    }
}

/// 把 vtable 的一个槽位指针换成新值；返回原值（数据补丁，不改指令）。
///
/// ```text
/// slot = *(vtable_address + slot_index * 8)
/// ```
///
/// # Safety
/// `slot_address` 必须指向一个 8 字节对齐、位于可写页（或可改保护页）的槽位。
#[must_use]
pub unsafe fn patch_pointer(slot_address: usize, replacement: usize) -> Option<usize> {
    let mut previous = 0u32;
    // SAFETY: 调用方保证槽位地址/对齐；只改一页保护，随即恢复。
    let protect_ok = unsafe {
        VirtualProtect(
            slot_address as *mut c_void,
            core::mem::size_of::<usize>(),
            PAGE_EXECUTE_READWRITE,
            &mut previous,
        )
    } != 0;
    if !protect_ok {
        return None;
    }
    // SAFETY: 调用方保证槽位可写且对齐。
    let original = unsafe { *(slot_address as *mut usize) };
    // SAFETY: 同上。
    unsafe {
        *(slot_address as *mut usize) = replacement;
    }
    let mut restored = 0u32;
    // SAFETY: 恢复原保护。
    unsafe {
        VirtualProtect(
            slot_address as *mut c_void,
            core::mem::size_of::<usize>(),
            previous,
            &mut restored,
        );
    }
    Some(original)
}

#[cfg(test)]
mod tests {
    //! 纯逻辑单测：E9 叠挂检测、参数闸门与指针槽补丁的往返。安装路径依赖可
    //! 执行页与真实目标，由实机套件（`unhook`/运行期钩子）覆盖。

    use super::{
        encode_e9, encode_indirect_jump, install_trace4, is_patched, patch_pointer, Handler4,
    };

    extern "system" fn dummy4(_: usize, _: usize, _: usize, _: usize) {}

    #[test]
    fn e9_detection_matches_only_the_patch_opcode() {
        let patched = [0xE9u8, 0, 0, 0, 0];
        let untouched = [0x48u8, 0x8B, 0x01, 0xC3, 0x90];
        // SAFETY: 只读测试内联缓冲区的首字节。
        assert!(unsafe { is_patched(patched.as_ptr() as usize) });
        // SAFETY: 同上，只读测试内联缓冲区的首字节。
        assert!(!unsafe { is_patched(untouched.as_ptr() as usize) });
    }

    #[test]
    fn bad_steal_is_rejected_before_touching_memory() {
        // SAFETY: entry=0 与越界 steal 在任何内存操作之前返回 Err。
        let result = unsafe { install_trace4(0, 4, dummy4 as Handler4) };
        assert_eq!(result.err(), Some("bad entry"));
    }

    #[test]
    fn patch_pointer_swaps_and_restores() {
        let mut slot: usize = 0x1122_3344_5566_7788;
        let address = &mut slot as *mut usize as usize;
        // SAFETY: 指向测试内对齐的局部变量；patch_pointer 只改一个指针宽度并恢复页保护。
        let first = unsafe { patch_pointer(address, 0xDEAD_BEEF) };
        assert_eq!(first, Some(0x1122_3344_5566_7788));
        assert_eq!(slot, 0xDEAD_BEEF);
        // SAFETY: 同上。
        let second = unsafe { patch_pointer(address, 0x1122_3344_5566_7788) };
        assert_eq!(second, Some(0xDEAD_BEEF));
        assert_eq!(slot, 0x1122_3344_5566_7788);
    }

    #[test]
    fn e9_encoding_rejects_out_of_rel32_range() {
        // 历史缺陷的值：入口 0x7FFD968FDA54 曾直接跳到 0x7FFE7D8010C0（相距
        // 3.87 GB），位移 as i32 截断后游戏跳到 0x7FFD7D8010C0 的 NX 页。
        assert!(
            encode_e9(0x7FFD_968F_DA54, 0x7FFE_7D80_10C0).is_none(),
            "越界位移必须被拒绝，不允许截断"
        );
        // 边界内（+1.75 GB）可编码且可逆解。
        let op = 0x1000_0000usize;
        let to = op + 0x7000_0000 - 0x10;
        let bytes = encode_e9(op, to).expect("near jump must encode");
        assert_eq!(bytes[0], 0xE9);
        let rel = i32::from_le_bytes(bytes[1..].try_into().unwrap()) as i64;
        assert_eq!(op as i64 + 5 + rel, to as i64);
        // 边界外（+2 GiB 以上）拒绝（0x8000_0000-5 仍在 i32 正区间内，用留量值）。
        assert!(encode_e9(op, op + 0x8000_1000).is_none());
    }

    #[test]
    fn indirect_block_carries_the_absolute_target() {
        let target = 0x7FFE_7D80_10C0usize;
        let bytes = encode_indirect_jump(target);
        assert_eq!(&bytes[..6], &[0xFF, 0x25, 0x00, 0x00, 0x00, 0x00]);
        let decoded = u64::from_le_bytes(bytes[6..].try_into().unwrap());
        assert_eq!(decoded, target as u64);
    }
}
