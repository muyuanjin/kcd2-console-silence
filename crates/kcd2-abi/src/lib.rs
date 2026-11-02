//! KCD2 native binding cage（L0）——宿主与引擎绑定的底层边界。
//!
//! 这一层是 `docs/ABSTRACTION_DESIGN.md` §3/§6 的落地：Win64 FFI、模块与
//! RVA 解析、受保护的内存读取、inline hook / vtable 补丁机制、日志与 SHA-256。
//! 它**不认识装备语义**（不知道"槽""预设""环"是什么）；引擎对象模型与实机
//! 后端在 `kcd2-engine`。
//!
//! 纪律：
//! - 只允许在 Windows x86_64 目标编译（其它目标上空 crate，便于门禁在任意
//!   宿主上跑 `cargo check`；Windows 非 x86_64 目标由 `compile_error!` 拒绝）；
//! - 一切读路径走 [`mem`]（`ReadProcessMemory` 为守卫，失败给 `None`，绝不
//!   解引用未验证指针、绝不 panic）；
//! - 一切写路径只有两类：宿主补丁（[`hook`]，带恢复记录）与引擎自身函数
//!   （在 `kcd2-engine` 里经绑定表调用）——本 crate 不提供通用内存写原语。

#![cfg(windows)]
#![deny(missing_docs)]
#![deny(unsafe_op_in_unsafe_fn)]
#![deny(clippy::undocumented_unsafe_blocks)]
#![deny(rustdoc::broken_intra_doc_links)]
// pedantic 稳定子集（§7.4）：查询结果与构造结果不允许被静默丢弃。
#![deny(clippy::must_use_candidate)]
#![deny(clippy::manual_let_else)]

/// 非 x86_64 的 Windows 目标不支持（x64 调用约定与 hook 机器码）。
#[cfg(not(target_arch = "x86_64"))]
compile_error!("kcd2-abi 只支持 x86_64-pc-windows-msvc");

pub mod env;
pub mod guard;
pub mod hook;
pub mod log;
pub mod mem;
pub mod module;
pub mod rva;
pub mod sha256;
pub mod win;
