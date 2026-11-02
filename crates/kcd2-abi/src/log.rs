//! 文件日志（跟随 DLL 所在目录）与日志目录单一来源。
//!
//! 只写文件：写引擎控制台在控制台处于"标记"选择模式时会阻塞调用线程
//! ，所以后台线程永远不碰控制台。

use std::fs::{File, OpenOptions};
use std::io::Write;
use std::path::PathBuf;
use std::sync::{Mutex, OnceLock};

static LOG_FILE: OnceLock<Mutex<Option<File>>> = OnceLock::new();
static LOG_DIRECTORY: OnceLock<PathBuf> = OnceLock::new();

fn file_slot() -> &'static Mutex<Option<File>> {
    LOG_FILE.get_or_init(|| Mutex::new(None))
}

/// 初始化日志文件（幂等；失败静默——日志是尽力而为）。
pub fn init(path: PathBuf) {
    if let Some(parent) = path.parent() {
        let _ = LOG_DIRECTORY.set(parent.to_path_buf());
    }
    let mut handle = file_slot().lock().unwrap_or_else(|err| err.into_inner());
    if handle.is_some() {
        return;
    }
    if let Some(parent) = path.parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    if let Ok(file) = OpenOptions::new().create(true).append(true).open(&path) {
        *handle = Some(file);
    }
}

/// 日志/命令通道目录（DLL 所在目录；未初始化 = `None`）。
pub fn directory() -> Option<&'static PathBuf> {
    LOG_DIRECTORY.get()
}

fn write_line(level: &str, message: &str) {
    let line = format!("[{level}] {message}");
    let mut handle = file_slot().lock().unwrap_or_else(|err| err.into_inner());
    if let Some(file) = handle.as_mut() {
        let _ = writeln!(file, "{line}");
        let _ = file.flush();
    }
}

/// INFO 级。
pub fn info(message: &str) {
    write_line("INFO", message);
}

/// DEBUG 级。
pub fn debug(message: &str) {
    write_line("DEBUG", message);
}

/// WARN 级。
pub fn warn(message: &str) {
    write_line("WARN", message);
}

/// ERROR 级。
pub fn error(message: &str) {
    write_line("ERROR", message);
}
