//! 已加载模块的定位与 RVA 解析（含 PE 段表）。

use crate::mem;
use crate::rva::{Rva, Va};
use crate::win;

/// 一个已加载的模块镜像。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Module {
    base: Va,
    size: usize,
}

impl Module {
    /// 按模块名（如 `WHGame.DLL`）定位；未加载返回 `None`。
    #[must_use]
    pub fn named(name: &str) -> Option<Module> {
        let (base, size) = win::module_range(name)?;
        Some(Module {
            base: Va::new(base)?,
            size,
        })
    }

    /// 模块基址。
    #[must_use]
    pub fn base(&self) -> Va {
        self.base
    }

    /// 模块映射大小（PE `SizeOfImage`）。
    #[must_use]
    pub fn size(&self) -> usize {
        self.size
    }

    /// RVA -> 进程内地址（界内检查；越界 = 拒绝）。
    #[must_use]
    pub fn resolve(&self, rva: Rva) -> Option<Va> {
        let offset = rva.get() as usize;
        if offset == 0 || offset >= self.size {
            return None;
        }
        self.base.offset(offset)
    }

    /// 地址是否落在本模块镜像内。
    #[must_use]
    pub fn contains(&self, address: usize) -> bool {
        let base = self.base.get();
        address > base && address < base + self.size
    }

    /// PE 段表（名字, RVA, 虚拟大小, 文件偏移, 文件大小）。
    #[must_use]
    pub fn sections(&self) -> Vec<(String, usize, usize, usize, usize)> {
        // SAFETY: `self.base` 来自模块定位（`GetModuleHandleA`），满足 `sections` 前置。
        unsafe { win::sections(self.base.get()) }
    }

    /// 可写且不可执行的段（对象扫描的唯一猎场）。
    #[must_use]
    pub fn writable_sections(&self) -> Vec<(String, usize, usize)> {
        // SAFETY: 同上；`writable_sections` 只读模块段表。
        unsafe { win::writable_sections(self.base.get()) }
    }

    /// 地址指向的内存是否可读（8 字节探针；RPM 守卫）。
    #[must_use]
    pub fn readable(address: usize) -> bool {
        mem::read_u64(address).is_some()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn resolve_rejects_out_of_range_rvas() {
        let module = Module {
            base: Va::new(0x1_0000_0000).unwrap(),
            size: 0x1000,
        };
        assert_eq!(module.resolve(Rva::new(0)), None);
        assert_eq!(module.resolve(Rva::new(0x1000)), None);
        assert_eq!(
            module.resolve(Rva::new(0x800)).unwrap().get(),
            0x1_0000_0800
        );
        assert!(module.contains(0x1_0000_0001));
        assert!(!module.contains(0x1_0000_1000));
    }
}
