//! 媒体服务器相关 API DTO —— 对应 `snd.komf.api.mediaserver` 包。
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct KomfMediaServerConnectionResponse {
    pub success: bool,
    pub http_status_code: Option<u16>,
    pub error_message: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct KomfMediaServerLibrary {
    pub id: KomfMediaServerLibraryId,
    pub name: String,
    pub roots: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(transparent)]
pub struct KomfMediaServerLibraryId(pub String);
