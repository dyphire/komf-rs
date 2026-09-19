//! API 模型 crate —— 对应 Kotlin 工程 `snd.komf.api` 包。
//!
//! 这些 DTO 是 komf 对外 HTTP API 的请求/响应结构，与
//! `komf-api-models` Gradle 模块一一对应。

pub mod common;
pub mod config;
pub mod job;
pub mod mediaserver;
pub mod metadata;
pub mod notifications;

pub use common::*;
