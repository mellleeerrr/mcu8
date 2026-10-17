//! Синхронная обёртка над async (для `#[test]` SDK без tokio runtime).
//!
//! SDK (WASM) — async; в нативных тестах используем `block_on` через
//! single-thread tokio runtime. В фазе 8 (Dioxus) — этот helper заменяется
//! реальным async-потоком SDK.

use std::future::Future;

/// Запускает future синхронно (single-thread tokio runtime на native).
#[cfg(not(target_arch = "wasm32"))]
pub fn block_on<F: Future>(fut: F) -> F::Output {
    let rt = tokio::runtime::Builder::new_current_thread().enable_all().build().expect("build tokio runtime");
    rt.block_on(fut)
}

/// В wasm32 синхронный block_on не поддерживается (нет потоков/event-loop блокировки).
#[cfg(target_arch = "wasm32")]
pub fn block_on<F: Future>(_fut: F) -> F::Output {
    panic!("block_on is not supported in wasm32; use async methods instead");
}
