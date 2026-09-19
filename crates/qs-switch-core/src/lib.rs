//! Qoder Switch 核心库。分层与命名对照参考实现 `workbuddy-switch`（MIT, changexbc）。
//!
//! 本 crate 刻意不依赖 Tauri，以便桌面端与 HTTP server 两个宿主复用同一套逻辑。

pub mod modules;

pub use modules::{bundle, config, snapshot, variant};

/// 核心层统一错误类型。面向 UI 的中文消息，不做错误码分类。
pub type Result<T> = std::result::Result<T, String>;
