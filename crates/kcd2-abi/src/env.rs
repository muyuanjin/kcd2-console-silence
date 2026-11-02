//! Locate `SSystemGlobalEnvironment` inside WHGame.dll.
//!
//! Algorithm (same as the shipped `kcd2db` ASI and its Rust port):
//!  1. find the literal `"exec autoexec.cfg"`.
//!  2. find the `lea rdx,[rip+disp]` that references it.
//!  3. inspect the 7 bytes 7 before that LEA to tell V1.4+ from older layouts.
//!  4. the preceding `mov [rip+disp],rcx` stores `pConsole`; back off 0xA8 to get gEnv.

use core::ffi::c_void;

use crate::log;
use crate::mem;
use crate::win::{find_bytes_rpm, find_lea_to, module_range, sleep_ms};

const GAME_MODULE: &str = "WHGame.DLL";
const ANCHOR: &[u8] = b"exec autoexec.cfg";
const NEW_CONTEXT: [u8; 7] = [0x4C, 0x8B, 0x92, 0x18, 0x01, 0x00, 0x00];
const OLD_CONTEXT: [u8; 3] = [0x48, 0x8B, 0x0D];
/// `pConsole` 相对 `gEnv` 的偏移（1.5.6）。
pub const CONSOLE_OFFSET: usize = 0xA8;

/// 等待 `WHGame.DLL` 加载并返回 `(基址, 镜像大小)`。
#[must_use]
pub fn wait_for_module(timeout_ms: u32) -> Option<(usize, usize)> {
    let mut waited = 0u32;
    loop {
        if let Some(range) = module_range(GAME_MODULE) {
            return Some(range);
        }
        if waited >= timeout_ms {
            return None;
        }
        sleep_ms(50);
        waited += 50;
    }
}

/// 在模块镜像内定位 `gEnv`（锚点字符串 -> `lea` -> `pConsole` 存储点）。
#[must_use]
pub fn locate(base: usize, size: usize) -> Option<*mut c_void> {
    let anchor = find_bytes_rpm(base, size, ANCHOR)?;
    log::debug(&format!("anchor string at 0x{anchor:X}"));
    let lea = find_lea_to(base, size, anchor)?;
    log::debug(&format!("lea referencing anchor at 0x{lea:X}"));

    let console_mov = detect_context(lea)?;
    log::debug(&format!("pConsole store at 0x{console_mov:X}"));

    let displacement = mem::read_u32(console_mov + 3)? as i32;
    let console_pointer = (console_mov as isize + 7 + displacement as isize) as usize;
    let environment = console_pointer.checked_sub(CONSOLE_OFFSET)?;
    log::info(&format!(
        "gEnv=0x{environment:X} (pConsole=0x{console_pointer:X})"
    ));
    Some(environment as *mut c_void)
}

fn detect_context(lea: usize) -> Option<usize> {
    let context_address = lea.checked_sub(7)?;
    let mut buffer = [0u8; 7];
    if !mem::read_bytes(context_address, &mut buffer) {
        log::error("unreadable version context around the anchor LEA");
        return None;
    }
    if buffer == NEW_CONTEXT {
        log::debug("version context: New (V1.4+)");
        return lea.checked_sub(0x17);
    }
    if buffer[..3] == OLD_CONTEXT {
        log::debug("version context: Old (<= V1.3)");
        return Some(context_address);
    }
    log::error("unrecognised version context around the anchor LEA");
    None
}
