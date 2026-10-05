//! 媒体服务器配置 —— 对应 `snd.komf.mediaserver.config` 包。
use komf_core::model::{MediaType, ReadingDirection, UpdateMode};
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct KomgaConfig {
    pub base_uri: String,
    pub komga_user: String,
    pub komga_password: String,
    /// Komga 服务端生成的 API key（`X-API-Key` 头）；非空时优先于账号密码。
    pub api_key: String,
    pub thumbnail_size_limit: i64,
    pub event_listener: EventListenerConfig,
    pub metadata_update: MetadataUpdateConfig,
}

impl Default for KomgaConfig {
    fn default() -> Self {
        Self {
            base_uri: "http://localhost:25600".to_string(),
            komga_user: "admin@example.org".to_string(),
            komga_password: "admin".to_string(),
            api_key: String::new(),
            thumbnail_size_limit: 1048575,
            event_listener: EventListenerConfig::default(),
            metadata_update: MetadataUpdateConfig::default(),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct KavitaConfig {
    pub base_uri: String,
    pub api_key: String,
    pub event_listener: EventListenerConfig,
    pub metadata_update: MetadataUpdateConfig,
}

impl Default for KavitaConfig {
    fn default() -> Self {
        Self {
            base_uri: "http://localhost:5000".to_string(),
            api_key: String::new(),
            event_listener: EventListenerConfig {
                enabled: false,
                ..Default::default()
            },
            metadata_update: MetadataUpdateConfig::default(),
        }
    }
}

/// Stump 媒体服务器配置（Rust 扩展）。
/// 认证优先 `api_key`（Bearer 直用）；为空时用账号密码登录换取 JWT。
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct StumpConfig {
    pub base_uri: String,
    pub username: String,
    pub password: String,
    /// Stump API Key（`stump_...` 前缀）；非空时优先于账号密码。
    pub api_key: String,
    pub event_listener: EventListenerConfig,
    pub metadata_update: MetadataUpdateConfig,
}

impl Default for StumpConfig {
    fn default() -> Self {
        Self {
            base_uri: "http://localhost:10801".to_string(),
            username: String::new(),
            password: String::new(),
            api_key: String::new(),
            event_listener: EventListenerConfig {
                enabled: false,
                ..Default::default()
            },
            metadata_update: MetadataUpdateConfig::default(),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct EventListenerConfig {
    pub enabled: bool,
    pub metadata_library_filter: Vec<String>,
    pub metadata_series_exclude_filter: Vec<String>,
    pub notifications_library_filter: Vec<String>,
}

impl Default for EventListenerConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            metadata_library_filter: Vec::new(),
            metadata_series_exclude_filter: Vec::new(),
            notifications_library_filter: Vec::new(),
        }
    }
}

/// 元数据更新配置。YAML 中 `library` 条目为**在 `default` 上的部分覆盖**：
/// 未写字段继承 default 节的值（而非硬编码默认值），与 WebUI PATCH 的合并语义一致。
/// 实现：反序列化时先把每个库级条目与 default 的序列化值深合并，再按完整结构解析。
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct MetadataUpdateConfig {
    pub default: MetadataProcessingConfig,
    pub library: std::collections::HashMap<String, MetadataProcessingConfig>,
}

impl<'de> Deserialize<'de> for MetadataUpdateConfig {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        #[derive(Deserialize)]
        #[serde(default, rename_all = "camelCase")]
        struct Helper {
            default: MetadataProcessingConfig,
            library: std::collections::HashMap<String, serde_yaml::Value>,
        }
        impl Default for Helper {
            fn default() -> Self {
                Self {
                    default: MetadataProcessingConfig::default(),
                    library: Default::default(),
                }
            }
        }

        let helper = Helper::deserialize(deserializer)?;
        let mut library = std::collections::HashMap::with_capacity(helper.library.len());
        for (library_id, value) in helper.library {
            // default 序列化为 YAML 值作为合并底：库级条目显式给出的键覆盖之，
            // 未给出的键保留 default 的值；序列化无 skip 字段，底是完整的。
            let mut merged =
                serde_yaml::to_value(&helper.default).map_err(serde::de::Error::custom)?;
            deep_merge_yaml(&mut merged, value);
            let config =
                MetadataProcessingConfig::deserialize(merged).map_err(serde::de::Error::custom)?;
            library.insert(library_id, config);
        }
        Ok(Self {
            default: helper.default,
            library,
        })
    }
}

/// 深合并 overlay 到 base：两边都是 mapping 时按键递归合并（overlay 优先），
/// 其余情况（标量/序列/null）整体替换——序列不拼接（如 cleanupRegex 显式给出即替换）。
fn deep_merge_yaml(base: &mut serde_yaml::Value, overlay: serde_yaml::Value) {
    match (base, overlay) {
        (serde_yaml::Value::Mapping(base_map), serde_yaml::Value::Mapping(overlay_map)) => {
            for (key, value) in overlay_map {
                match base_map.get_mut(&key) {
                    Some(existing) => deep_merge_yaml(existing, value),
                    None => {
                        base_map.insert(key, value);
                    }
                }
            }
        }
        (slot, value) => *slot = value,
    }
}

impl Default for MetadataUpdateConfig {
    fn default() -> Self {
        Self {
            default: MetadataProcessingConfig::default(),
            library: Default::default(),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct MetadataProcessingConfig {
    pub library_type: MediaType,
    pub aggregate: bool,
    /// Rust 扩展：aggregate=true 时，将媒体服务器上该系列当前的原始 genres
    /// 合并进聚合结果（并集去重排序），避免 provider 聚合数据整体覆盖服务器
    /// 已有体裁。仅聚合模式生效；默认 false（行为不变）。
    pub aggregate_genres: bool,
    /// Rust 扩展：aggregate=true 时，将媒体服务器上该系列/书籍当前的原始 tags
    /// 合并进聚合结果（并集去重排序）。仅聚合模式生效；默认 false（行为不变）。
    pub aggregate_tags: bool,
    pub merge_tags: bool,
    pub merge_genres: bool,
    pub book_covers: bool,
    pub series_covers: bool,
    pub override_existing_covers: bool,
    pub lock_covers: bool,
    pub update_modes: Vec<UpdateMode>,
    pub override_comic_info: bool,
    /// Rust 扩展：UpdateMode::MylarSeriesJson 导出 series.json 时同时下载系列封面
    /// （cover.jpg / <系列名>.cover.jpg，已存在跳过）。默认 false（对齐 py 脚本 --save-cover 需显式开启）。
    pub mylar_covers: bool,
    /// Rust 扩展：mylar series.json 导出根目录（对齐 py --output）。null = 系列原目录。
    /// 库根目录由内部从媒体服务器 API 自动获取（get_library().roots），用于还原相对目录结构。
    pub mylar_output_dir: Option<String>,
    pub post_processing: MetadataPostProcessingConfig,
    /// 搜索标题提取（括号式标题提取，可配置化）
    /// enabled 默认 false = 行为不变；除 symbolNormalizeRegex 外均无默认正则，
    /// 未配置任何正则时行为仅剩符号归一，与原 removeParentheses 并存。
    #[serde(default)]
    pub search_title_extraction: SearchTitleExtractionConfig,
    /// Auto-Identify Library 匹配失败的系列加入的收藏夹名称（Rust 扩展，Kotlin 无）。
    /// 非空时生效；收藏夹按库级建（名称 = 配置名 + `[库名]` 后缀）。
    /// Auto-Identify Library 自动跳过已在收藏夹内的条目；Auto-Identify Series /
    /// Identify 成功时自动从收藏夹删除。
    #[serde(default)]
    pub failed_match_collection_name: Option<String>,
    /// Rust 扩展：简繁转换（Kotlin 无）。应用范围：搜索 / 自动匹配 / 元数据更新
    /// （更新内容可自定义字段：标题、体裁、标签、简介等）。
    #[serde(default)]
    pub chinese_conversion: ChineseConversionConfig,
}

/// 搜索标题提取配置：enabled 时对系列名生成额外搜索候选（可配置版）
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct SearchTitleExtractionConfig {
    /// 是否启用括号式标题提取。false → 仅保留现有 remove_parentheses 行为
    pub enabled: bool,
    /// 括号内容提取正则（如 `\[([^\[\]]+)\]`）。不配置 → 不提取括号段
    pub bracket_regex: Option<String>,
    /// 作者识别分隔符（**正则**，如 `×`、`[×xX]`）。括号段命中该正则 → 视为作者，拆开并排除出标题
    pub author_separator: Option<String>,
    /// 标题拆分符列表（如 `_`）。**支持正则**（如 `[_\-]`），编译失败退回字面量。提取出的标题段按每个分隔符拆分，每段一个候选
    pub title_splitters: Vec<String>,
    /// 符号归一正则：所有候选匹配部分替换为空格（默认  `[:：•·․,，。'’?？!！~⁓～]`）。
    #[serde(default = "default_symbol_normalize_regex")]
    pub symbol_normalize_regex: String,
    /// 单字符映射表（如 "／" → "/"）。**支持正则键**（全局替换，可含捕获组如 `"(\\d+)" → "[$1]"`），编译失败退回字面量替换。不配置 → 无替换。
    pub char_mappings: Vec<(String, String)>,
    /// 整名清洗链。按序对系列名 replace ""，结果作为前置候选
    pub cleanup_regex: Vec<String>,
}

impl Default for SearchTitleExtractionConfig {
    fn default() -> Self {
        Self {
            // 默认 false = 行为不变（仅保留原 remove_parentheses）；用户可显式开启。
            enabled: false,
            bracket_regex: None,
            author_separator: None,
            title_splitters: Vec::new(),
            symbol_normalize_regex: default_symbol_normalize_regex(),
            char_mappings: Vec::new(),
            cleanup_regex: Vec::new(),
        }
    }
}

fn default_symbol_normalize_regex() -> String {
    "[:：•·․,，。'’?？!！~⁓～]".to_string()
}

/// 简繁转换配置（Rust 扩展，Kotlin 无）。
/// 应用范围：`search`（搜索关键词）/ `matching`（自动匹配）/ `update`（元数据更新）。
/// `update.fields` 控制更新时应用到的内容字段。
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct ChineseConversionConfig {
    /// 总开关。false = 完全禁用（默认）。
    pub enabled: bool,
    /// 转换方向：`t2s`（繁体→简体，默认）/ `s2t`（简体→繁体）。
    pub direction: komf_core::util::ChineseDirection,
    /// 是否应用于搜索关键词（provider 搜索 API 前转换）。
    pub search: bool,
    /// 是否应用于自动匹配（匹配归一时对 query 与候选双向应用）。
    pub matching: bool,
    /// 元数据更新应用配置。
    pub update: ChineseUpdateConfig,
}

/// 元数据更新中的简繁转换配置。
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct ChineseUpdateConfig {
    /// 元数据更新时是否应用转换。false = 仅搜索/匹配生效。
    pub enabled: bool,
    /// 应用字段（空 = 全部字段不转换）。
    pub fields: Vec<ChineseField>,
}

/// 简繁转换应用的内容字段。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ChineseField {
    /// 系列标题（SeriesTitle.name）
    Title,
    /// 体裁（genres）
    Genres,
    /// 标签（tags）
    Tags,
    /// 简介（summary）
    Summary,
}

impl Default for ChineseConversionConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            direction: komf_core::util::ChineseDirection::T2s,
            search: true,
            matching: true,
            update: ChineseUpdateConfig::default(),
        }
    }
}

impl Default for ChineseUpdateConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            fields: vec![ChineseField::Title],
        }
    }
}

impl Default for MetadataProcessingConfig {
    fn default() -> Self {
        Self {
            library_type: MediaType::Manga,
            aggregate: false,
            aggregate_genres: false,
            aggregate_tags: false,
            merge_tags: false,
            merge_genres: false,
            book_covers: false,
            series_covers: false,
            override_existing_covers: true,
            lock_covers: true,
            update_modes: vec![UpdateMode::Api],
            override_comic_info: false,
            mylar_covers: false,
            mylar_output_dir: None,
            post_processing: MetadataPostProcessingConfig::default(),
            search_title_extraction: SearchTitleExtractionConfig::default(),
            failed_match_collection_name: None,
            chinese_conversion: ChineseConversionConfig::default(),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct MetadataPostProcessingConfig {
    pub series_title: bool,
    pub series_title_language: Option<String>,
    pub alternative_series_titles: bool,
    pub alternative_series_title_languages: Vec<String>,
    pub fallback_to_alt_title: bool,
    pub order_books: bool,
    pub reading_direction_value: Option<ReadingDirection>,
    pub language_value: Option<String>,
    pub score_tag_name: Option<String>,
    pub original_publisher_tag_name: Option<String>,
    pub publisher_tag_names: Vec<PublisherTagNameConfig>,
    pub alternate_title_labels: AlternateTitleLabelsConfig,
    /// Rust 扩展：系列 links 命中任一 provider 识别特征时，整个系列跳过自动匹配（默认启用）。
    pub links_skip_enabled: bool,
    /// Rust 扩展：linksSkipEnabled=false 时，若 links 中存在可解析的 provider 链接，
    /// 直接用该链接拉取元数据更新，不再执行额外搜索（默认启用）。
    pub links_match_enabled: bool,
}

impl Default for MetadataPostProcessingConfig {
    fn default() -> Self {
        Self {
            series_title: false,
            series_title_language: Some("en".to_string()),
            alternative_series_titles: false,
            alternative_series_title_languages: vec![
                "en".to_string(),
                "ja".to_string(),
                "ja-ro".to_string(),
            ],
            fallback_to_alt_title: false,
            order_books: false,
            reading_direction_value: None,
            language_value: None,
            score_tag_name: None,
            original_publisher_tag_name: None,
            publisher_tag_names: Vec::new(),
            alternate_title_labels: AlternateTitleLabelsConfig::default(),
            links_skip_enabled: true,
            links_match_enabled: true,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PublisherTagNameConfig {
    pub tag_name: String,
    pub language: String,
}

/// alternate titles 的 ROMAJI / NATIVE / LOCALIZED label 本地化映射。
///
/// None = 使用默认标签（Romaji / Native / Localized；LOCALIZED 无语言时回退 Localized）。
/// 例如 `romaji: "罗马音"`、`native: "原名"`、`localized: "别名"`。
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct AlternateTitleLabelsConfig {
    pub romaji: Option<String>,
    pub native: Option<String>,
    pub localized: Option<String>,
}

impl Default for AlternateTitleLabelsConfig {
    fn default() -> Self {
        Self {
            romaji: None,
            native: None,
            localized: None,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct DatabaseConfig {
    pub file: String,
}

impl Default for DatabaseConfig {
    fn default() -> Self {
        Self {
            file: "./database.sqlite".to_string(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 库级条目未给出的字段继承 default 节（而非硬编码默认值）。
    #[test]
    fn library_entry_inherits_default() {
        let yaml = r#"
default:
  aggregate: true
  postProcessing:
    linksSkipEnabled: false
    linksMatchEnabled: false
    seriesTitleLanguage: zh
library:
  lib-a:
    aggregate: false
"#;
        let config: MetadataUpdateConfig = serde_yaml::from_str(yaml).unwrap();
        let lib = &config.library["lib-a"];
        // 显式覆盖生效
        assert!(!lib.aggregate);
        // 未写字段继承 default
        assert!(!lib.post_processing.links_skip_enabled);
        assert!(!lib.post_processing.links_match_enabled);
        assert_eq!(
            lib.post_processing.series_title_language.as_deref(),
            Some("zh")
        );
        // default 本身不受影响
        assert!(config.default.aggregate);
    }

    /// 库级条目显式给出的值优先于 default。
    #[test]
    fn library_entry_explicit_value_wins() {
        let yaml = r#"
default:
  postProcessing:
    linksSkipEnabled: false
library:
  lib-a:
    postProcessing:
      linksSkipEnabled: true
"#;
        let config: MetadataUpdateConfig = serde_yaml::from_str(yaml).unwrap();
        assert!(config.library["lib-a"].post_processing.links_skip_enabled);
    }

    /// 嵌套 map 递归合并：库级只给 postProcessing 的一个键，其余键继承 default。
    #[test]
    fn library_entry_nested_partial_merge() {
        let yaml = r#"
default:
  postProcessing:
    seriesTitle: true
    seriesTitleLanguage: zh
    orderBooks: true
library:
  lib-a:
    postProcessing:
      orderBooks: false
"#;
        let config: MetadataUpdateConfig = serde_yaml::from_str(yaml).unwrap();
        let pp = &config.library["lib-a"].post_processing;
        assert!(pp.series_title);
        assert_eq!(pp.series_title_language.as_deref(), Some("zh"));
        assert!(!pp.order_books);
    }

    /// 序列整体替换而非拼接：库级显式给出的列表替换 default 的列表。
    #[test]
    fn library_entry_sequence_replaces() {
        let yaml = r#"
default:
  searchTitleExtraction:
    cleanupRegex:
      - 'default-pattern'
library:
  lib-a:
    searchTitleExtraction:
      cleanupRegex:
        - 'lib-pattern'
"#;
        let config: MetadataUpdateConfig = serde_yaml::from_str(yaml).unwrap();
        assert_eq!(
            config.library["lib-a"]
                .search_title_extraction
                .cleanup_regex,
            vec!["lib-pattern".to_string()]
        );
    }

    /// 空 library / 缺省 metadataUpdate 段：与旧行为一致。
    #[test]
    fn empty_library_and_missing_section() {
        let config: MetadataUpdateConfig = serde_yaml::from_str("default: {}").unwrap();
        assert!(config.library.is_empty());
        assert_eq!(config.default.library_type, MediaType::Manga);
        // metadataUpdate 段整体缺失时由外层 serde(default) 回退 Default
        #[derive(Deserialize)]
        #[serde(default)]
        struct Outer {
            metadata_update: MetadataUpdateConfig,
        }
        impl Default for Outer {
            fn default() -> Self {
                Self {
                    metadata_update: MetadataUpdateConfig::default(),
                }
            }
        }
        let outer: Outer = serde_yaml::from_str("{}").unwrap();
        assert!(outer.metadata_update.library.is_empty());
    }
}
