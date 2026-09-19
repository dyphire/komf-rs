//! komf-core —— 领域模型、配置、匹配工具与元数据 provider 框架。
//!
//! 对应 Kotlin 工程的 `komf-core` Gradle 模块（`snd.komf` 包）：
//! - `model`    -> `snd.komf.model`
//! - `providers` -> `snd.komf.providers`
//! - `util`     -> `snd.komf.util`

pub mod config;
pub mod model;
pub mod providers;
pub mod util;

pub use model::*;
