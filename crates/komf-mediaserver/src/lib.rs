//! komf-mediaserver —— Komga/Kavita 客户端、元数据服务与任务追踪。
//!
//! 对应 Kotlin 工程的 `komf-mediaserver` Gradle 模块（`snd.komf.mediaserver` 包）。
pub mod client;
pub mod comic_info;
pub mod config;
pub mod event_listener;
pub mod jobs;
pub mod kavita;
pub mod kavita_signalr;
pub mod komga;
pub mod media_server_module;
pub mod metadata_mapper;
pub mod mylar;
pub mod metadata_merger;
pub mod metadata_post_processor;
pub mod metadata_service;
pub mod metadata_updater;
pub mod model;
pub mod stump;
pub mod stump_event;

pub use client::{MediaServerClient, MediaServerError};
pub use media_server_module::MediaServerModule;
pub use metadata_service::MetadataServiceProvider;
pub use model::*;
