//! `Rva` / `Va` 新类型：地址算术的唯一直说方式。
//!
//! 契约（`docs/ABSTRACTION_DESIGN.md` §6）：所有绑定表项用 `Rva`（相对模块基址）
//! 表达；运行期调用一律先经 [`crate::module::Module::resolve`] 转成 `Va`，失败即拒绝——不存在
//! "猜一个地址继续跑"。

use core::fmt;

/// 模块内相对地址（相对镜像基址）。
#[derive(Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct Rva(u32);

impl Rva {
    /// 从原始读数构造。
    #[must_use]
    pub const fn new(raw: u32) -> Self {
        Rva(raw)
    }

    /// 原始读数。
    #[must_use]
    pub const fn get(self) -> u32 {
        self.0
    }
}

impl fmt::Debug for Rva {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "Rva(0x{:X})", self.0)
    }
}

impl fmt::Display for Rva {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "0x{:X}", self.0)
    }
}

/// 已解析的进程内地址。
#[derive(Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct Va(usize);

impl Va {
    /// 从原始地址构造；用户区最低页（< 0x10000）与 0 一律拒绝。
    #[must_use]
    pub fn new(raw: usize) -> Option<Self> {
        (raw >= 0x10000).then_some(Va(raw))
    }

    /// 原始地址。
    #[must_use]
    pub const fn get(self) -> usize {
        self.0
    }

    /// 偏移一个字节数（溢出 = 拒绝）。
    #[must_use]
    pub fn offset(self, bytes: usize) -> Option<Self> {
        Va::new(self.0.checked_add(bytes)?)
    }
}

impl fmt::Debug for Va {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "Va(0x{:X})", self.0)
    }
}

impl fmt::Display for Va {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "0x{:X}", self.0)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn va_rejects_null_and_low_pages() {
        assert!(Va::new(0).is_none());
        assert!(Va::new(0xFFFF).is_none());
        assert_eq!(Va::new(0x10000).unwrap().get(), 0x10000);
    }

    #[test]
    fn va_offset_checks_overflow() {
        let va = Va::new(0x10000).unwrap();
        assert_eq!(va.offset(0x10).unwrap().get(), 0x10010);
        assert!(Va::new(usize::MAX).unwrap().offset(1).is_none());
    }
}
