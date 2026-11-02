//! 受保护的内存读取原语。
//!
//! 契约（`docs/ABSTRACTION_DESIGN.md` §6.4）：一切 native 读都走这里。
//! `ReadProcessMemory`（RPM）作为守卫：目标页未映射/不可读时返回失败而不是
//! 触发访问违规（`IsBadReadPtr` 在 x64 上不可靠，已弃用）。调用方把 `None`
//! 映射为 `Error::Unreadable`/`Error::NotFound`。
//!
//! 写只有一个原语：[`write_pointer`]——指针大小的**数据**补丁（vtable 槽位），
//! 它读完原值后临时改页保护、写回、立刻恢复保护；与 [`crate::hook`] 的代码补丁
//! 同属"带恢复记录的宿主补丁"这一类。代码字节仍然只能经 `hook` 写。

use core::ffi::c_void;

use crate::win::{GetCurrentProcess, ReadProcessMemory, VirtualProtect, PAGE_READWRITE};

/// 用户区最低可寻址页（0 与首页永远不是合法对象）。
pub const MIN_VALID_ADDRESS: usize = 0x1_0000;

/// RPM 批量读：整段可读才成功。
pub fn read_bytes(address: usize, out: &mut [u8]) -> bool {
    if address < MIN_VALID_ADDRESS || out.is_empty() {
        return false;
    }
    let mut got = 0usize;
    // SAFETY: 只读自己的进程；未映射页由 RPM 返回失败，不会触发访问违规。
    let ok = unsafe {
        ReadProcessMemory(
            GetCurrentProcess(),
            address as *const c_void,
            out.as_mut_ptr() as *mut c_void,
            out.len(),
            &mut got,
        )
    };
    ok != 0 && got == out.len()
}

/// 守卫式 8 字节读。
#[must_use]
pub fn read_u64(address: usize) -> Option<u64> {
    let mut buffer = [0u8; 8];
    read_bytes(address, &mut buffer).then(|| u64::from_le_bytes(buffer))
}

/// 守卫式 4 字节读。
#[must_use]
pub fn read_u32(address: usize) -> Option<u32> {
    let mut buffer = [0u8; 4];
    read_bytes(address, &mut buffer).then(|| u32::from_le_bytes(buffer))
}

/// 守卫式 2 字节读。
#[must_use]
pub fn read_u16(address: usize) -> Option<u16> {
    let mut buffer = [0u8; 2];
    read_bytes(address, &mut buffer).then(|| u16::from_le_bytes(buffer))
}

/// 守卫式 1 字节读。
#[must_use]
pub fn read_u8(address: usize) -> Option<u8> {
    let mut buffer = [0u8; 1];
    read_bytes(address, &mut buffer).then(|| buffer[0])
}

/// 守卫式指针读（返回值小于最小合法地址时视为"未设置"）。
#[must_use]
pub fn read_pointer(address: usize) -> Option<usize> {
    read_u64(address)
        .map(|value| value as usize)
        .filter(|value| *value >= MIN_VALID_ADDRESS)
}

/// 一段内存区域（`VirtualQuery` 的 checked 视图；大小至少一页）。
#[derive(Clone, Copy, Debug)]
pub struct MemoryRegion {
    /// 区域基址（页对齐）。
    pub base: usize,
    /// 区域大小（至少一页）。
    pub size: usize,
    /// `MEM_*` 状态。
    pub state: u32,
    /// 页保护（`PAGE_*`）。
    pub protect: u32,
}

impl MemoryRegion {
    /// 区域是否处于提交状态（`MEM_COMMIT`）。
    #[must_use]
    pub fn committed(&self) -> bool {
        self.state == crate::win::MEM_COMMIT
    }
}

/// 查询地址所在区域；`VirtualQuery` 失败（未映射）返回 `None`。
#[must_use]
pub fn region_at(address: usize) -> Option<MemoryRegion> {
    let mut info = crate::win::MemoryBasicInformation {
        base_address: core::ptr::null_mut(),
        allocation_base: core::ptr::null_mut(),
        allocation_protect: 0,
        partition_id: 0,
        region_size: 0,
        state: 0,
        protect: 0,
        type_: 0,
    };
    // SAFETY: 输出结构体完整初始化；`VirtualQuery` 只读映射信息。
    let written = unsafe {
        crate::win::VirtualQuery(
            address as *const c_void,
            &mut info,
            core::mem::size_of::<crate::win::MemoryBasicInformation>(),
        )
    };
    if written == 0 {
        return None;
    }
    Some(MemoryRegion {
        base: info.base_address as usize,
        size: info.region_size.max(0x1000),
        state: info.state,
        protect: info.protect,
    })
}

/// 把 `[start, start+len)` 切成已提交子区间（跳过空洞与未提交段）。
#[must_use]
pub fn committed_subranges(start: usize, len: usize) -> Vec<(usize, usize)> {
    let mut out = Vec::new();
    let mut cursor = start;
    let end = start.saturating_add(len);
    while cursor < end {
        let Some(region) = region_at(cursor) else {
            break;
        };
        let region_end = region.base.saturating_add(region.size.max(0x1000));
        if !region.committed() {
            if region_end <= cursor {
                break;
            }
            cursor = region_end;
            continue;
        }
        let chunk_end = region_end.min(end);
        if chunk_end <= cursor {
            break;
        }
        out.push((cursor, chunk_end - cursor));
        cursor = chunk_end;
    }
    out
}

/// 守卫式 NUL 结尾 ASCII 字符串读（上限 256 字节；非可打印字符即拒绝）。
#[must_use]
pub fn read_cstr(address: usize, limit: usize) -> Option<String> {
    let mut buffer = [0u8; 256];
    let want = limit.min(buffer.len());
    let mut got = 0usize;
    if address < MIN_VALID_ADDRESS || want == 0 {
        return None;
    }
    // SAFETY: 只读自己的进程；未映射页由 RPM 返回失败，不会触发访问违规。
    let ok = unsafe {
        ReadProcessMemory(
            GetCurrentProcess(),
            address as *const c_void,
            buffer.as_mut_ptr() as *mut c_void,
            want,
            &mut got,
        )
    };
    if ok == 0 || got == 0 {
        return None;
    }
    let mut end = 0usize;
    while end < got && buffer[end] != 0 {
        end += 1;
    }
    if end == 0 || end == got {
        return None;
    }
    for value in &buffer[..end] {
        if !(0x20..0x7F).contains(value) {
            return None;
        }
    }
    String::from_utf8(buffer[..end].to_vec()).ok()
}

/// 指针大小的**数据**补丁（vtable 槽位）：临时把目标页改成可写、写入、立刻恢复
/// 原来的页保护。
///
/// 只有"把一个函数指针换成另一个函数指针"这一种写意图被支持：地址必须已经映射
/// 且当前可读（用 [`read_pointer`] 先验证），写入失败时返回 `false` 而不是猜一个
/// 部分成功的状态。调用方负责记住原值以便恢复。
#[must_use]
pub fn write_pointer(address: usize, value: usize) -> bool {
    if read_pointer(address).is_none() {
        return false;
    }
    let mut previous = 0u32;
    // SAFETY: 目标页刚被 RPM 读过（已映射）；只放宽为可写，写后立即恢复原保护。
    let opened = unsafe {
        VirtualProtect(
            address as *mut c_void,
            core::mem::size_of::<usize>(),
            PAGE_READWRITE,
            &mut previous,
        )
    };
    if opened == 0 {
        return false;
    }
    // SAFETY: 页已被显式改成可写，且地址来自上面的映射检查。
    unsafe { core::ptr::write_volatile(address as *mut usize, value) };
    let mut restored = 0u32;
    // SAFETY: 恢复刚才保存的页保护；失败只影响保护状态，写入已经完成。
    let closed = unsafe {
        VirtualProtect(
            address as *mut c_void,
            core::mem::size_of::<usize>(),
            previous,
            &mut restored,
        )
    };
    closed != 0
}

/// Token-free 子串搜索（供扫描器复用）。
#[must_use]
pub fn find_bytes(haystack: &[u8], needle: &[u8]) -> Option<usize> {
    if needle.is_empty() || haystack.len() < needle.len() {
        return None;
    }
    let first = needle[0];
    let limit = haystack.len() - needle.len();
    let mut index = 0usize;
    while index <= limit {
        if haystack[index] == first && &haystack[index..index + needle.len()] == needle {
            return Some(index);
        }
        index += 1;
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn low_pages_are_never_readable() {
        assert!(!read_bytes(0, &mut [0u8; 8]));
        assert_eq!(read_u64(0x100), None);
        assert_eq!(read_pointer(0), None);
        assert_eq!(read_cstr(0x10, 32), None);
    }

    #[test]
    fn own_process_data_reads_back() {
        let value: u64 = 0x1122_3344_5566_7788;
        let address = &value as *const u64 as usize;
        assert_eq!(read_u64(address), Some(value));
    }

    #[test]
    fn write_pointer_swaps_a_slot_and_rejects_unmapped() {
        // `read_pointer` 的过滤条件是「值必须像用户区地址」（>= 0x10000），
        // 所以夹具用两个合法量级的指针值，而不是 0x1111 这种哨兵。
        let mut slot: usize = 0x1000_1111;
        let address = &mut slot as *mut usize as usize;
        assert_eq!(
            read_pointer(address),
            Some(0x1000_1111),
            "probe read failed"
        );
        assert!(write_pointer(address, 0x1000_2222), "write_pointer refused");
        assert_eq!(read_pointer(address), Some(0x1000_2222));
        assert!(
            !write_pointer(0x10, 0x1000_3333),
            "unmapped page must be refused"
        );
        // 写入本身不校验"新值像不像指针"（那是调用方的语义）：写进去以后
        // `read_pointer` 的地址量级过滤才把它挡下来。
        assert!(write_pointer(address, 0x10));
        assert_eq!(read_pointer(address), None);
    }

    #[test]
    fn find_bytes_finds_and_misses() {
        assert_eq!(find_bytes(b"hello world", b"world"), Some(6));
        assert_eq!(find_bytes(b"hello", b"zzz"), None);
        assert_eq!(find_bytes(b"", b"x"), None);
        assert_eq!(find_bytes(b"x", b""), None);
    }
}
