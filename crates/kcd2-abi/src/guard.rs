//! FFI 边界 panic 护栏（`docs/ABSTRACTION_DESIGN.md` §5.10）。
//!
//! panic 穿越 FFI 是未定义行为：release 构建以 `panic = "abort"` 直接终止进程，
//! unwind 构建（debug / test）必须在这里收敛。所有 `extern "system"` 出口的最外层
//! 统一经 [`catch`]；Lua 桥在应用层把捕获结果转成 `{ok=false, code="internal"}`，
//! 其余出口回退到调用方给定的安全默认值并记日志。

/// 在 FFI 出口最外层捕获 panic：正常返回 `f()`，panic 时记日志并返回 `fallback`。
///
/// 契约：`fallback` 必须是"让宿主继续跑"的安全值（如 `1`、`()`、错误对象）；
/// panic 之后引擎状态未知，`f` 内部不得再进入可重入逻辑。`panic = "abort"`
/// 的发布构建不会走到 `fallback` 分支（进程直接终止）。
///
/// 不标 `#[must_use]`：需要返回值的出口（DllMain/bootstrap/钩子）在类型层面
/// 已被强制消费结果，单元用途（副作用护栏）则以语句调用为正常形态。
#[allow(clippy::must_use_candidate)]
pub fn catch<R>(what: &str, fallback: R, f: impl FnOnce() -> R) -> R {
    match std::panic::catch_unwind(std::panic::AssertUnwindSafe(f)) {
        Ok(value) => value,
        Err(_) => {
            crate::log::error(&format!("panic caught at FFI boundary: {what}"));
            fallback
        }
    }
}

#[cfg(test)]
mod tests {
    use super::catch;

    #[test]
    fn passes_value_through() {
        assert_eq!(catch("ok", 0, || 42), 42);
    }

    #[test]
    fn catches_panic_and_returns_fallback() {
        assert_eq!(catch("boom", -1, || panic!("test panic")), -1);
    }
}
