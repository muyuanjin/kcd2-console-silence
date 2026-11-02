//! 宿主 FFI 包装 + 模块/模式扫描/命名管道助手。
//!
//! Win32 绑定来自 `windows-sys`（通用基础设施不造轮子）；本模块只保留项目特有的
//! checked 包装、PE/模式扫描与管道安全/读写助手。
//!
//! 这一层是"裸调用"（§6.7 的 `raw`）：不加守卫、不做语义；带守卫的读面在
//! [`crate::mem`]，模块模型在 [`crate::module`]。所有 `unsafe` 调用点都必须
//! 自带 `SAFETY` 论证。
//!
//! 可见性即分层：裸绑定一律 `pub(crate)`（禁止跨 crate 泄漏），上层只能使用本
//! 模块导出的 checked 包装与助手。

// Win32 函数名原样保留（与 MSDN/其它绑定一致，便于比对）。
#![allow(non_snake_case)]

use core::ffi::c_void;

use windows_sys::Win32::Foundation::{
    CloseHandle as SysCloseHandle, GetLastError as SysGetLastError, LocalFree,
    ERROR_ALREADY_EXISTS, ERROR_PIPE_BUSY, ERROR_PIPE_CONNECTED, GENERIC_READ, GENERIC_WRITE,
    HANDLE, SYSTEMTIME,
};
use windows_sys::Win32::Globalization::GetUserDefaultUILanguage as SysGetUserDefaultUILanguage;
use windows_sys::Win32::Security::Authorization::{
    ConvertStringSecurityDescriptorToSecurityDescriptorW, SDDL_REVISION_1,
};
use windows_sys::Win32::Security::SECURITY_ATTRIBUTES;
use windows_sys::Win32::Storage::FileSystem::{
    CreateFileW as SysCreateFileW, FlushFileBuffers as SysFlushFileBuffers,
    ReadFile as SysReadFile, WriteFile as SysWriteFile, FILE_ATTRIBUTE_NORMAL, OPEN_EXISTING,
};
use windows_sys::Win32::System::Diagnostics::Debug::ReadProcessMemory as SysReadProcessMemory;
use windows_sys::Win32::System::LibraryLoader::{
    GetModuleFileNameA as SysGetModuleFileNameA, GetModuleHandleA as SysGetModuleHandleA,
};
use windows_sys::Win32::System::Memory::{
    VirtualAlloc as SysVirtualAlloc, VirtualFree as SysVirtualFree,
    VirtualProtect as SysVirtualProtect, VirtualQuery as SysVirtualQuery, MEMORY_BASIC_INFORMATION,
    MEM_COMMIT as SYS_MEM_COMMIT, MEM_RELEASE as SYS_MEM_RELEASE, MEM_RESERVE,
    PAGE_EXECUTE_READWRITE as SYS_PAGE_EXECUTE_READWRITE,
};
use windows_sys::Win32::System::Pipes::{
    ConnectNamedPipe, CreateNamedPipeW, DisconnectNamedPipe, WaitNamedPipeW, PIPE_READMODE_BYTE,
    PIPE_TYPE_BYTE, PIPE_UNLIMITED_INSTANCES, PIPE_WAIT,
};
use windows_sys::Win32::System::SystemInformation::GetLocalTime as SysGetLocalTime;
use windows_sys::Win32::System::Threading::{
    CreateMutexW as SysCreateMutexW, CreateThread as SysCreateThread,
    GetCurrentProcess as SysGetCurrentProcess, GetCurrentProcessId as SysGetCurrentProcessId,
    GetCurrentThreadId as SysGetCurrentThreadId, Sleep as SysSleep,
};
use windows_sys::Win32::UI::Input::KeyboardAndMouse::GetAsyncKeyState as SysGetAsyncKeyState;
use windows_sys::Win32::UI::WindowsAndMessaging::{GetForegroundWindow, GetWindowThreadProcessId};

/// 线程入口签名（`CreateThread`）。
pub(crate) type ThreadEntry = extern "system" fn(*mut c_void) -> u32;

/// `ERROR_ALREADY_EXISTS` —— `CreateMutexW` 后 `GetLastError` 的返回值（单实例检测）。
pub const ERROR_ALREADY_EXISTS_CODE: u32 = ERROR_ALREADY_EXISTS;

/// `CloseHandle`。
pub(crate) unsafe fn CloseHandle(handle: *mut c_void) -> i32 {
    // SAFETY: 直通系统调用；句柄由调用方保证来自本模块的创建函数或加载器。
    unsafe { SysCloseHandle(handle as HANDLE) }
}

/// `GetLastError`。
pub(crate) fn GetLastError() -> u32 {
    // SAFETY: 无前置条件。
    unsafe { SysGetLastError() }
}

/// `VirtualProtect`。
pub(crate) unsafe fn VirtualProtect(
    address: *mut c_void,
    size: usize,
    new_protect: u32,
    old: *mut u32,
) -> i32 {
    // SAFETY: 直通；调用方保证地址/长度落在已提交页内。
    unsafe { SysVirtualProtect(address, size, new_protect, old) }
}

/// `CreateThread`（默认属性/栈，挂起标志 0）。
pub(crate) unsafe fn CreateThread(start: ThreadEntry) -> *mut c_void {
    // SAFETY: 入口是 `extern "system"` 函数；属性/栈/参数/标志均为默认值。
    unsafe {
        SysCreateThread(
            core::ptr::null(),
            0,
            Some(start),
            core::ptr::null_mut(),
            0,
            core::ptr::null_mut(),
        )
    }
}

/// `CreateMutexW`（名字以 NUL 结尾的 UTF-16）。
fn create_mutex_wide(name: *const u16) -> *mut c_void {
    // SAFETY: 名字在调用期间存活且以 NUL 结尾。
    unsafe { SysCreateMutexW(core::ptr::null(), 0, name) }
}

/// `VirtualQuery`（写入项目内的 `MemoryBasicInformation` 视图）。
pub(crate) unsafe fn VirtualQuery(
    address: *const c_void,
    buffer: *mut MemoryBasicInformation,
    length: usize,
) -> usize {
    if length < core::mem::size_of::<MemoryBasicInformation>() {
        return 0;
    }
    // SAFETY: `MEMORY_BASIC_INFORMATION` 是 POD；全零是合法的初值（系统会覆写）。
    let mut info = unsafe { core::mem::zeroed::<MEMORY_BASIC_INFORMATION>() };
    // SAFETY: 输出结构体完整初始化；`VirtualQuery` 只读映射信息。
    let written = unsafe {
        SysVirtualQuery(
            address,
            &mut info,
            core::mem::size_of::<MEMORY_BASIC_INFORMATION>(),
        )
    };
    if written == 0 {
        return 0;
    }
    // SAFETY: 调用方保证 `buffer` 指向足够大的 `MemoryBasicInformation`（上面的长度检查）。
    unsafe {
        core::ptr::write(
            buffer,
            MemoryBasicInformation {
                base_address: info.BaseAddress,
                allocation_base: info.AllocationBase,
                allocation_protect: info.AllocationProtect,
                partition_id: info.PartitionId,
                region_size: info.RegionSize,
                state: info.State,
                protect: info.Protect,
                type_: info.Type,
            },
        );
    }
    written
}

/// `ReadProcessMemory`。
pub(crate) unsafe fn ReadProcessMemory(
    process: *mut c_void,
    address: *const c_void,
    buffer: *mut c_void,
    size: usize,
    read: *mut usize,
) -> i32 {
    // SAFETY: 直通；调用方保证缓冲区有效，目标可读性由系统返回失败表达。
    unsafe { SysReadProcessMemory(process, address, buffer, size, read) }
}

/// `GetCurrentProcess`。
pub(crate) unsafe fn GetCurrentProcess() -> *mut c_void {
    // SAFETY: 无前置条件（伪句柄）。
    unsafe { SysGetCurrentProcess() }
}

/// `VirtualAlloc`。
pub(crate) unsafe fn VirtualAlloc(
    address: *mut c_void,
    size: usize,
    kind: u32,
    protect: u32,
) -> *mut c_void {
    // SAFETY: 直通；调用方保证参数合法。
    unsafe { SysVirtualAlloc(address, size, kind, protect) }
}

/// `VirtualFree`。
pub(crate) unsafe fn VirtualFree(address: *mut c_void, size: usize, kind: u32) -> i32 {
    // SAFETY: 直通；调用方保证地址来自 `VirtualAlloc`。
    unsafe { SysVirtualFree(address, size, kind) }
}

/// 创建/打开一个命名互斥体；返回 `(handle, 是否已存在)`。
#[must_use]
pub fn create_or_open_mutex(name: &str) -> (*mut c_void, bool) {
    let wide = wide(name);
    let handle = create_mutex_wide(wide.as_ptr());
    let existed = GetLastError() == ERROR_ALREADY_EXISTS_CODE;
    (handle, existed)
}

/// `MEMORY_BASIC_INFORMATION` 的 x64 布局（项目内视图，便于 `mem` 层直读字段）。
#[repr(C)]
pub(crate) struct MemoryBasicInformation {
    /// 区域基址。
    pub base_address: *mut c_void,
    /// 分配基址。
    pub allocation_base: *mut c_void,
    /// 分配保护。
    pub allocation_protect: u32,
    /// 分区 id。
    pub partition_id: u16,
    /// 区域大小。
    pub region_size: usize,
    /// 状态。
    pub state: u32,
    /// 保护。
    pub protect: u32,
    /// 类型。
    pub type_: u32,
}

/// `MEM_COMMIT`。
pub(crate) const MEM_COMMIT: u32 = SYS_MEM_COMMIT;
/// `MEM_COMMIT | MEM_RESERVE`。
pub(crate) const MEM_COMMIT_RESERVE: u32 = SYS_MEM_COMMIT | MEM_RESERVE;
/// `MEM_RELEASE`。
pub(crate) const MEM_RELEASE: u32 = SYS_MEM_RELEASE;
/// `PAGE_EXECUTE_READWRITE`。
pub(crate) const PAGE_EXECUTE_READWRITE: u32 = SYS_PAGE_EXECUTE_READWRITE;
/// `PAGE_READWRITE`（vtable 槽位这类**数据**补丁用；可写不可执行）。
pub(crate) const PAGE_READWRITE: u32 = 0x04;

/// 睡眠（毫秒）。
pub fn sleep_ms(milliseconds: u32) {
    // SAFETY: `Sleep` 无前置条件。
    unsafe { SysSleep(milliseconds) };
}

/// 当前进程 id。
#[must_use]
pub fn current_process_id() -> u32 {
    // SAFETY: 无前置条件。
    unsafe { SysGetCurrentProcessId() }
}

/// 当前线程 id。
#[must_use]
pub fn current_thread_id() -> u32 {
    // SAFETY: 无前置条件。
    unsafe { SysGetCurrentThreadId() }
}

/// 系统默认 UI 语言 LANGID（低 10 位 = 主语言）。
#[must_use]
pub fn user_default_ui_language() -> u16 {
    // SAFETY: 无前置条件。
    unsafe { SysGetUserDefaultUILanguage() }
}

/// 前台窗口所属进程 id（无前台窗口时为 0）。
#[must_use]
pub fn foreground_process_id() -> u32 {
    // SAFETY: `GetForegroundWindow` 无前置条件；句柄只作查询用。
    let window = unsafe { GetForegroundWindow() };
    if window.is_null() {
        return 0;
    }
    let mut process_id = 0u32;
    // SAFETY: 输出参数有效。
    unsafe { GetWindowThreadProcessId(window, &mut process_id) };
    process_id
}

/// 虚拟键当前是否按下（`GetAsyncKeyState` 高位）。
#[must_use]
pub fn key_down(virtual_key: i32) -> bool {
    // SAFETY: 无前置条件。
    (unsafe { SysGetAsyncKeyState(virtual_key) } as u16 & 0x8000) != 0
}

/// 启动一个分离线程；成功时关闭线程句柄（线程继续运行）。
#[must_use]
pub fn spawn_detached_thread(entry: ThreadEntry) -> bool {
    // SAFETY: 入口是 `extern "system"` 函数；线程句柄由调用返回。
    let handle = unsafe { CreateThread(entry) };
    if handle.is_null() {
        return false;
    }
    // SAFETY: 句柄由上面的调用返回；关闭不影响已启动线程。
    unsafe { CloseHandle(handle) };
    true
}

/// 本地墙上时钟 `YYYY-MM-DD HH:MM:SS`（审计日志用）。
#[must_use]
pub fn timestamp() -> String {
    // SAFETY: `SYSTEMTIME` 是 POD；全零是合法初值（系统会覆写）。
    let mut time = unsafe { core::mem::zeroed::<SYSTEMTIME>() };
    // SAFETY: 结构体是 `out` 参数且完整初始化。
    unsafe { SysGetLocalTime(&mut time) };
    format!(
        "{:04}-{:02}-{:02} {:02}:{:02}:{:02}",
        time.wYear, time.wMonth, time.wDay, time.wHour, time.wMinute, time.wSecond
    )
}

/// `module` 句柄对应的完整文件路径（DLL 自定位用）。
///
/// # Safety
/// `module` 必须是加载器给出的模块句柄（`DllMain` 的 `hModule` 或
/// `GetModuleHandle*` 的返回值），或为 null（表示当前进程的映像）。
#[must_use]
pub unsafe fn module_file_name(module: *mut c_void) -> Option<std::path::PathBuf> {
    let mut buffer = [0u8; 1024];
    // SAFETY: 缓冲区有效；`module` 由调用方保证来自加载器。
    let length = unsafe { SysGetModuleFileNameA(module, buffer.as_mut_ptr(), buffer.len() as u32) };
    if length == 0 {
        return None;
    }
    let text = String::from_utf8_lossy(&buffer[..length as usize]).to_string();
    Some(std::path::PathBuf::from(text))
}

/// 主可执行文件所在目录（游戏安装的 Bin 目录；`WHGame.dll` 与其同级）。
///
/// 构建身份检查必须对着游戏目录里的 `WHGame.dll`，而不是本 DLL 自己的目录——
/// 开发注入副本可能从任意目录加载（如构建产物目录）。
#[must_use]
pub fn main_module_directory() -> Option<std::path::PathBuf> {
    // SAFETY: `NULL` 模块句柄 = 主可执行文件；缓冲区在调用期间有效。
    let path = unsafe { module_file_name(core::ptr::null_mut()) }?;
    path.parent().map(std::path::Path::to_path_buf)
}

/// 解析已加载模块的基址与镜像大小。
#[must_use]
pub fn module_range(name: &str) -> Option<(usize, usize)> {
    let mut cname = Vec::with_capacity(name.len() + 1);
    cname.extend_from_slice(name.as_bytes());
    cname.push(0);
    // SAFETY: 名字以 NUL 结尾。
    let handle = unsafe { SysGetModuleHandleA(cname.as_ptr()) };
    if handle.is_null() {
        return None;
    }
    let base = handle as usize;
    // SAFETY: 基址来自 `GetModuleHandleA`，PE 头解析全程走 RPM（失败即 None）。
    let size = unsafe { image_size(base) }?;
    Some((base, size))
}

/// 通过 PE 头取 `SizeOfImage`（RPM 守卫）。
///
/// # Safety
/// `base` 必须是模块基址（调用方经 `module_range`/`GetModuleHandleA` 获得）。
unsafe fn image_size(base: usize) -> Option<usize> {
    if crate::mem::read_u16(base)? != 0x5A4D {
        return None;
    }
    let nt = base + crate::mem::read_u32(base + 0x3C)? as usize;
    if crate::mem::read_u32(nt)? != 0x0000_4550 {
        return None;
    }
    Some(crate::mem::read_u32(nt + 0x50)? as usize)
}

/// 段表：`(名字, 虚拟地址, 虚拟大小, 文件偏移, 文件大小)`。
///
/// # Safety
/// `base` 必须是模块基址。
#[must_use]
pub unsafe fn sections(base: usize) -> Vec<(String, usize, usize, usize, usize)> {
    let Some(nt_displacement) = crate::mem::read_u32(base + 0x3C) else {
        return Vec::new();
    };
    let nt = base + nt_displacement as usize;
    let Some(section_count) = crate::mem::read_u16(nt + 6) else {
        return Vec::new();
    };
    let Some(optional_size) = crate::mem::read_u16(nt + 20) else {
        return Vec::new();
    };
    let section_count = section_count as usize;
    let optional_size = optional_size as usize;
    let section_table = nt + 24 + optional_size;
    let mut out = Vec::with_capacity(section_count);
    for index in 0..section_count {
        let entry = section_table + index * 40;
        let mut raw_name = [0u8; 8];
        if !crate::mem::read_bytes(entry, &mut raw_name) {
            break;
        }
        let mut name = String::new();
        for byte in raw_name {
            if byte == 0 {
                break;
            }
            name.push(byte as char);
        }
        let (Some(vsize), Some(vaddr), Some(raw_size), Some(raw_off)) = (
            crate::mem::read_u32(entry + 8),
            crate::mem::read_u32(entry + 12),
            crate::mem::read_u32(entry + 16),
            crate::mem::read_u32(entry + 20),
        ) else {
            break;
        };
        out.push((
            name,
            vaddr as usize,
            vsize as usize,
            raw_off as usize,
            raw_size as usize,
        ));
    }
    out
}

/// 可写、不可执行的段（对象扫描的唯一猎场）。
///
/// # Safety
/// `base` 必须是模块基址。
#[must_use]
pub unsafe fn writable_sections(base: usize) -> Vec<(String, usize, usize)> {
    let mut out = Vec::new();
    // SAFETY: 调用方保证 `base` 是模块基址。
    for (name, vaddr, vsize, _raw_off, raw_size) in unsafe { sections(base) } {
        let size = vsize.max(raw_size);
        if !name_is_executable_or_readonly(&name) {
            out.push((name, base + vaddr, size));
        }
    }
    out
}

fn name_is_executable_or_readonly(name: &str) -> bool {
    matches!(
        name,
        ".text" | ".rdata" | ".pdata" | ".reloc" | ".idata" | ".edata" | ".tls" | ".rsrc"
    )
}

/// 分块、只经 RPM 的字节模式扫描（跨块重叠 `needle.len() - 1` 字节）。
#[must_use]
pub(crate) fn find_bytes_rpm(start: usize, size: usize, needle: &[u8]) -> Option<usize> {
    const CHUNK: usize = 1 << 20;
    if needle.is_empty() || size < needle.len() {
        return None;
    }
    let mut buffer = vec![0u8; CHUNK + needle.len()];
    let end = start.saturating_add(size);
    let mut cursor = start;
    while cursor < end {
        let want = CHUNK.min(end - cursor);
        let readable = want + (needle.len() - 1).min(end - cursor - want);
        let mut got = 0usize;
        // SAFETY: 只读自己的进程；未映射页由 RPM 返回失败，不会触发访问违规。
        let ok = unsafe {
            ReadProcessMemory(
                GetCurrentProcess(),
                cursor as *const c_void,
                buffer.as_mut_ptr() as *mut c_void,
                readable,
                &mut got,
            )
        };
        if ok == 0 || got < needle.len() {
            cursor = (cursor + CHUNK) & !0xFFF;
            continue;
        }
        let slice = &buffer[..got];
        if let Some(position) = crate::mem::find_bytes(slice, needle) {
            return Some(cursor + position);
        }
        cursor += want;
    }
    None
}

/// 扫描 `48 8D 15 ?? ?? ?? ??`（`lea rdx,[rip+disp32]`）找出目标地址的引用点。
#[must_use]
pub(crate) fn find_lea_to(base: usize, size: usize, target: usize) -> Option<usize> {
    const CHUNK: usize = 1 << 20;
    let end = base.saturating_add(size);
    let mut cursor = base;
    let mut buffer = vec![0u8; CHUNK + 16];
    while cursor < end {
        let want = CHUNK.min(end - cursor);
        let mut got = 0usize;
        // SAFETY: 只读自己的进程；失败即跳过该块。
        let ok = unsafe {
            ReadProcessMemory(
                GetCurrentProcess(),
                cursor as *const c_void,
                buffer.as_mut_ptr() as *mut c_void,
                want,
                &mut got,
            )
        };
        if ok == 0 || got < 7 {
            cursor = (cursor + CHUNK) & !0xFFF;
            continue;
        }
        let slice = &buffer[..got];
        for index in 0..slice.len().saturating_sub(7) {
            if slice[index] == 0x48 && slice[index + 1] == 0x8D && slice[index + 2] == 0x15 {
                let disp = i32::from_le_bytes([
                    slice[index + 3],
                    slice[index + 4],
                    slice[index + 5],
                    slice[index + 6],
                ]);
                let here = cursor + index;
                let resolved = (here as isize + 7 + disp as isize) as usize;
                if resolved == target {
                    return Some(here);
                }
            }
        }
        cursor += want;
    }
    None
}

// ── 命名管道（W4.1 控制通道）────────────────────────────────────────────

/// 管道句柄（`INVALID_HANDLE_VALUE` 用 [`invalid_handle`] 判断）。
pub type PipeHandle = *mut c_void;

/// `PIPE_ACCESS_DUPLEX`（windows-sys 未导出此别名时项目内自持该值）。
const PIPE_ACCESS_DUPLEX: u32 = 0x0000_0003;

/// `INVALID_HANDLE_VALUE`。
#[must_use]
pub fn invalid_handle() -> PipeHandle {
    usize::MAX as PipeHandle
}

/// 把 Rust 字符串编码成带 NUL 的 UTF-16（Win32 W 系 API 用）。
#[must_use]
pub fn wide(text: &str) -> Vec<u16> {
    text.encode_utf16().chain(std::iter::once(0)).collect()
}

/// `\\.\pipe\<name>` 的宽字符形式。
#[must_use]
pub fn named_pipe_path(name: &str) -> Vec<u16> {
    wide(&format!(r"\\.\pipe\{name}"))
}

/// 管道安全描述符（当前用户 / System / Administrators 可访问）。
///
/// SDDL：`D:P(A;;GA;;;SY)(A;;GA;;;BA)(A;;GA;;;OW)`——`OW` = Owner Rights，
/// 即对象属主（本进程用户）。其它用户连不上。
pub struct PipeSecurity {
    attributes: Vec<u8>,
    descriptor: *mut c_void,
}

impl PipeSecurity {
    /// 构造当前用户专属安全属性；失败返回 `None`。
    #[must_use]
    pub fn current_user_only() -> Option<PipeSecurity> {
        let sddl = wide(PIPE_SDDL);
        let mut descriptor: *mut c_void = core::ptr::null_mut();
        let mut size: u32 = 0;
        // SAFETY: SDDL 是 NUL 结尾 UTF-16；输出指针有效。
        let ok = unsafe {
            ConvertStringSecurityDescriptorToSecurityDescriptorW(
                sddl.as_ptr(),
                SDDL_REVISION_1,
                &mut descriptor,
                &mut size,
            )
        };
        if ok == 0 || descriptor.is_null() {
            return None;
        }
        let mut attributes = Vec::with_capacity(core::mem::size_of::<SECURITY_ATTRIBUTES>());
        // SAFETY: `SECURITY_ATTRIBUTES` 是 POD；缓冲区容量按尺寸预留。
        unsafe {
            let attributes_ptr = attributes.as_mut_ptr() as *mut SECURITY_ATTRIBUTES;
            core::ptr::write(
                attributes_ptr,
                SECURITY_ATTRIBUTES {
                    nLength: core::mem::size_of::<SECURITY_ATTRIBUTES>() as u32,
                    lpSecurityDescriptor: descriptor,
                    bInheritHandle: 0,
                },
            );
            attributes.set_len(core::mem::size_of::<SECURITY_ATTRIBUTES>());
        }
        Some(PipeSecurity {
            attributes,
            descriptor,
        })
    }

    /// 传给 `CreateNamedPipeW` 的属性指针。
    #[must_use]
    pub fn attributes_ptr(&self) -> *mut c_void {
        self.attributes.as_ptr() as *mut c_void
    }
}

impl Drop for PipeSecurity {
    fn drop(&mut self) {
        if !self.descriptor.is_null() {
            // SAFETY: `descriptor` 由 SDDL 转换分配（LocalAlloc 家族）。
            unsafe {
                LocalFree(self.descriptor);
            }
        }
    }
}

// SAFETY: `descriptor` 是本结构独占持有的本地堆分配（LocalAlloc），生命周期由
// `Drop` 管理；把整个结构移动到另一个线程不产生别名或生命周期问题。
unsafe impl Send for PipeSecurity {}

/// 创建一个管道服务端实例（未连接状态）；失败返回 `invalid_handle()`。
#[must_use]
pub fn create_pipe_instance(path: &[u16], security: *mut c_void) -> PipeHandle {
    // SAFETY: 路径以 NUL 结尾；安全属性由调用方在整个调用期间保活。
    unsafe {
        CreateNamedPipeW(
            path.as_ptr(),
            PIPE_ACCESS_DUPLEX,
            PIPE_TYPE_BYTE | PIPE_READMODE_BYTE | PIPE_WAIT,
            PIPE_UNLIMITED_INSTANCES,
            64 * 1024,
            64 * 1024,
            0,
            security as *const SECURITY_ATTRIBUTES,
        )
    }
}

/// 等待客户端连接（含"客户端已连上"竞态）。
///
/// # Safety
/// `handle` 必须是 [`create_pipe_instance`] 返回的服务端实例。
#[must_use]
pub unsafe fn accept_pipe(handle: PipeHandle) -> bool {
    // SAFETY: 同步模式（无 OVERLAPPED），句柄来自 `create_pipe_instance`。
    let ok = unsafe { ConnectNamedPipe(handle, core::ptr::null_mut()) };
    ok != 0 || GetLastError() == ERROR_PIPE_CONNECTED
}

/// 断开连接（允许下一实例复用句柄）。
///
/// # Safety
/// `handle` 必须是 [`create_pipe_instance`] 返回的服务端实例。
pub unsafe fn disconnect_pipe(handle: PipeHandle) {
    // SAFETY: 句柄来自 `create_pipe_instance`。
    unsafe {
        DisconnectNamedPipe(handle);
    }
}

/// 读取一段（阻塞）；`None` = 连接结束/错误。
///
/// # Safety
/// `handle` 必须来自 [`create_pipe_instance`]/[`connect_pipe`]。
#[must_use]
pub unsafe fn read_pipe(handle: PipeHandle, buffer: &mut [u8]) -> Option<usize> {
    let mut read: u32 = 0;
    // SAFETY: 缓冲区有效且长度已截断到 u32。
    let ok = unsafe {
        SysReadFile(
            handle,
            buffer.as_mut_ptr(),
            buffer.len().min(u32::MAX as usize) as u32,
            &mut read,
            core::ptr::null_mut(),
        )
    };
    if ok == 0 {
        return None;
    }
    Some(read as usize)
}

/// 写入完整一段并 flush；失败返回 `false`。
///
/// # Safety
/// `handle` 必须来自 [`create_pipe_instance`]/[`connect_pipe`]。
#[must_use]
pub unsafe fn write_pipe(handle: PipeHandle, bytes: &[u8]) -> bool {
    let mut written: u32 = 0;
    // SAFETY: 缓冲区有效且长度已截断到 u32。
    let ok = unsafe {
        SysWriteFile(
            handle,
            bytes.as_ptr(),
            bytes.len().min(u32::MAX as usize) as u32,
            &mut written,
            core::ptr::null_mut(),
        )
    };
    if ok == 0 || written as usize != bytes.len() {
        return false;
    }
    // SAFETY: 句柄有效。
    unsafe {
        SysFlushFileBuffers(handle);
    }
    true
}

/// 客户端连接（先 `WaitNamedPipeW` 等实例，再 `CreateFileW`）；失败返回 `invalid_handle()`。
#[must_use]
pub fn connect_pipe(path: &[u16], timeout_ms: u32) -> PipeHandle {
    let deadline = std::time::Instant::now() + std::time::Duration::from_millis(timeout_ms.into());
    loop {
        // SAFETY: 路径以 NUL 结尾。
        unsafe {
            WaitNamedPipeW(path.as_ptr(), 100);
        }
        // SAFETY: 路径以 NUL 结尾；无安全属性/模板。
        let handle = unsafe {
            SysCreateFileW(
                path.as_ptr(),
                GENERIC_READ | GENERIC_WRITE,
                0,
                core::ptr::null(),
                OPEN_EXISTING,
                FILE_ATTRIBUTE_NORMAL,
                core::ptr::null_mut(),
            )
        };
        if !handle.is_null() && handle != invalid_handle() {
            return handle;
        }
        if std::time::Instant::now() >= deadline {
            return invalid_handle();
        }
        sleep_ms(25);
    }
}

/// 关闭管道句柄。
///
/// # Safety
/// `handle` 必须来自 [`create_pipe_instance`]/[`connect_pipe`]，且不得重复关闭。
pub unsafe fn close_pipe(handle: PipeHandle) {
    if handle.is_null() || handle == invalid_handle() {
        return;
    }
    // SAFETY: 句柄由本模块的创建/连接函数返回。
    unsafe {
        SysCloseHandle(handle as HANDLE);
    }
}

/// `ERROR_PIPE_BUSY`（客户端连接重试判定）。
#[must_use]
pub fn last_error_is_pipe_busy() -> bool {
    GetLastError() == ERROR_PIPE_BUSY
}

/// 管道 DACL（见 [`PipeSecurity::current_user_only`] 的说明）。
pub(crate) const PIPE_SDDL: &str = "D:P(A;;GA;;;SY)(A;;GA;;;BA)(A;;GA;;;OW)";

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pipe_dacl_is_owner_restricted() {
        assert_eq!(PIPE_SDDL, "D:P(A;;GA;;;SY)(A;;GA;;;BA)(A;;GA;;;OW)");
        assert!(PIPE_SDDL.contains("OW"), "属主（当前用户）可访问");
        assert!(!PIPE_SDDL.contains("WD"), "不得授予 Everyone");
        assert!(!PIPE_SDDL.contains(";;;BU"), "不得授予 Users 组");
    }
}
