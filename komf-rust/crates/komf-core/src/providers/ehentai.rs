//! EHentai provider —— 对应 Kotlin `providers/ehentai` 包（上游 PR #284，3 commits）。
//!
//! 搜索：e-hentai.org HTML 搜索（`?f_search=`，≤3 页 + `var nexturl`），正则提取
//! `/g/(\d+)/([a-f0-9]+)` gid/token，再调 gdata API（25 个/请求分块）取完整元数据。
//! 匹配：`bookQualifier.name` 优先；本地标题变体 × 远程变体 any 匹配（NameSimilarityMatcher）。
//! 元数据：标题 parseTitle（去前后缀/`|` 最右、残留括号清理）、title_jpn 非空 → Native、
//! BCP47 语言映射、作者解析（标题驱动 `[circle (artist)]` 优先 title_jpn +
//! gdata 标签回退 + 汉化组，clean_author_names 清理）、标签命名空间白名单/黑名单/male-only 过滤、
//! rating 四舍五入、ageRating（极端标签/分类映射）、status=ENDED。

use serde::{Deserialize, Deserializer};
use std::collections::HashMap;
use std::time::Duration;

use crate::config::EHentaiConfig;
use crate::model::{
    Author, AuthorRole, Image, MatchQuery, ProviderBookId, ProviderBookMetadata, ProviderSeriesId,
    ProviderSeriesMetadata, ReadingDirection, ReleaseDate, SeriesMetadata, SeriesSearchResult,
    SeriesStatus, SeriesTitle, TitleType, WebLink,
};
use crate::providers::ehentai_archive::{EHentaiArchiveService, GalleryRow};
use crate::providers::{CoreProviders, MetadataProvider, ProviderError};
use crate::util::NameSimilarityMatcher;

const API_URL: &str = "https://api.e-hentai.org/api.php";
const MAX_PAGES: usize = 3;

// ---------------------------------------------------------------------------
// 模型 —— 对应 Kotlin model/EHentaiResponse.kt、model/EHentaiBook.kt
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default)]
pub(crate) struct EHentaiResponse {
    gmetadata: Vec<EHentaiBook>,
    gid: Option<i32>,
    error: Option<String>,
}

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default)]
pub(crate) struct EHentaiBook {
    gid: i32,
    token: String,
    #[serde(deserialize_with = "de_html_unescape")]
    title: String,
    #[serde(rename = "title_jpn", deserialize_with = "de_html_unescape_opt")]
    title_jpn: Option<String>,
    category: Option<String>,
    thumb: Option<String>,
    uploader: Option<String>,
    /// epoch 秒（字符串），Kotlin `Instant`；null 跳过序列化器。
    #[serde(deserialize_with = "de_epoch_seconds_opt")]
    posted: Option<i64>,
    #[serde(rename = "filecount")]
    file_count: Option<String>,
    #[serde(rename = "filesize")]
    file_size: Option<i64>,
    expunged: Option<bool>,
    /// Kotlin StringToRoundedDoubleSerializer：字符串 → f64 → round()。
    #[serde(deserialize_with = "de_rating_rounded_opt")]
    rating: Option<f64>,
    #[serde(rename = "torrentcount")]
    torrent_count: Option<String>,
    torrents: Option<Vec<EHentaiTorrent>>,
    tags: Option<Vec<String>>,
    #[serde(rename = "parent_gid")]
    parent_gid: Option<String>,
    #[serde(rename = "parent_key")]
    parent_key: Option<String>,
    #[serde(rename = "current_gid")]
    current_gid: Option<String>,
    #[serde(rename = "current_key")]
    current_key: Option<String>,
    #[serde(rename = "first_gid")]
    first_gid: Option<String>,
    #[serde(rename = "first_key")]
    first_key: Option<String>,
    error: Option<String>,
}

impl EHentaiBook {
    /// 从 e-hentai-db 离线库 GalleryRow 构造（gdata 同字段语义）：
    /// title/title_jpn 为 gdata 原始 HTML 实体编码 → 复用 html_unescape 解码；
    /// rating 字符串 → round；空 title_jpn → None；空 tags → None。
    pub(crate) fn from_archive_row(row: GalleryRow) -> Self {
        Self {
            gid: row.gid,
            token: row.token,
            title: html_unescape(&row.title),
            title_jpn: if row.title_jpn.is_empty() {
                None
            } else {
                Some(html_unescape(&row.title_jpn))
            },
            category: Some(row.category),
            thumb: Some(row.thumb),
            uploader: row.uploader,
            posted: Some(row.posted),
            file_count: Some(row.file_count.to_string()),
            file_size: Some(row.file_size),
            expunged: Some(row.expunged),
            rating: row.rating.parse::<f64>().ok().map(|v| v.round()),
            tags: if row.tags.is_empty() {
                None
            } else {
                Some(row.tags)
            },
            ..Default::default()
        }
    }
}

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default)]
pub(crate) struct EHentaiTorrent {
    hash: String,
    /// epoch 秒（字符串）
    #[serde(deserialize_with = "de_epoch_seconds")]
    added: i64,
    #[serde(deserialize_with = "de_html_unescape")]
    name: String,
    #[serde(rename = "tsize")]
    t_size: String,
    #[serde(rename = "fsize")]
    f_size: String,
}

/// 对应 Kotlin model/EHentaiParsedTitle.kt。
struct EHentaiParsedTitle {
    /// 匹配标题（去掉前后缀后的主体）——对应 Kotlin `match` 字段，保留以对齐结构。
    #[allow(dead_code)]
    match_title: String,
    /// `Title1 | Title2 | Title3` 中最右侧的标题。
    best_match: String,
}

// ---------------------------------------------------------------------------
// 自定义反序列化 —— 对应 Kotlin EHentaiBook.kt 的序列化器
// ---------------------------------------------------------------------------

/// Kotlin HtmlUnescapeStringSerializer 增强：先解码数字实体（`&#123;`/`&#x1F;`），
/// 再解码 6 种命名实体。数字实体在前避免 `&amp;#123;` 被二次解码（与 Python html.unescape 一致）。
fn html_unescape(s: &str) -> String {
    static NUMERIC_RE: std::sync::OnceLock<regex::Regex> = std::sync::OnceLock::new();
    let numeric_re =
        NUMERIC_RE.get_or_init(|| regex::Regex::new(r"&#(?:x([0-9a-fA-F]+)|(\d+));").unwrap());
    let s = numeric_re
        .replace_all(s, |caps: &regex::Captures| {
            let code = if let Some(h) = caps.get(1) {
                u32::from_str_radix(h.as_str(), 16).ok()
            } else {
                caps.get(2).and_then(|d| d.as_str().parse::<u32>().ok())
            };
            match code.and_then(char::from_u32) {
                Some(c) => c.to_string(),
                None => caps
                    .get(0)
                    .map(|m| m.as_str().to_string())
                    .unwrap_or_default(),
            }
        })
        .into_owned();
    s.replace("&amp;", "&")
        .replace("&quot;", "\"")
        .replace("&#039;", "'")
        .replace("&#39;", "'")
        .replace("&lt;", "<")
        .replace("&gt;", ">")
}

fn de_html_unescape<'de, D>(d: D) -> Result<String, D::Error>
where
    D: Deserializer<'de>,
{
    let s = String::deserialize(d)?;
    Ok(html_unescape(&s))
}

fn de_html_unescape_opt<'de, D>(d: D) -> Result<Option<String>, D::Error>
where
    D: Deserializer<'de>,
{
    let s: Option<String> = Option::deserialize(d)?;
    Ok(s.map(|s| html_unescape(&s)))
}

/// Kotlin StringToRoundedDoubleSerializer：`"4.68"` → `round(4.68)` = 5.0。
fn de_rating_rounded_opt<'de, D>(d: D) -> Result<Option<f64>, D::Error>
where
    D: Deserializer<'de>,
{
    let s: Option<String> = Option::deserialize(d)?;
    Ok(s.and_then(|s| s.parse::<f64>().ok()).map(|v| v.round()))
}

fn de_epoch_seconds<'de, D>(d: D) -> Result<i64, D::Error>
where
    D: Deserializer<'de>,
{
    let s = String::deserialize(d)?;
    s.parse::<i64>().map_err(serde::de::Error::custom)
}

fn de_epoch_seconds_opt<'de, D>(d: D) -> Result<Option<i64>, D::Error>
where
    D: Deserializer<'de>,
{
    let s: Option<String> = Option::deserialize(d)?;
    Ok(s.and_then(|s| s.parse::<i64>().ok()))
}

// ---------------------------------------------------------------------------
// Parser —— 对应 Kotlin EHentaiParser.kt
// ---------------------------------------------------------------------------

struct EHentaiParser;

/// `^\[(pixiv|fanbox|fantia|patreon|gumroad|ci-en)([\s/,&|]+(pixiv|fanbox|fantia|patreon|gumroad|ci-en))*]`（忽略大小写）
const PLATFORM_PREFIX_REGEX: &str = r"(?i)^\[(pixiv|fanbox|fantia|patreon|gumroad|ci-en)([\s/,&|]+(pixiv|fanbox|fantia|patreon|gumroad|ci-en))*]";
/// 尾部 `[xxx]` 标签（可多个，允许前置空白）
const TRAILING_REGEX: &str = r"(?:\s*\[[^]]+])+$";
/// 头部 `(convention)` 可选 + `[circle/artist]` 可选
const LEADING_REGEX: &str = r"^(?:\([^)]+\)\s*)?(?:\[[^]]+]\s*)?";
/// 纯日期数字/画师图包
const DATE_ONLY_REGEX: &str = r"^[\d\-.\s]+$";
/// 尾部括号 `(xxx)`（全角/半角，可多个）
const CORE_TITLE_REGEX: &str = r"(?:\s*[（(][^)）]+[)）])+$";
/// 增强：残留括号 `[..]`/`（..）`/`(..)`（parse_filename 对齐，非回退路径）
const REMNANT_BRACKET_REGEX: &str = r"\[[^\[\]]*\]|（[^（）]*）|\([^()]*\)";
/// 标题驱动作者：标题前最后一个 `[xxx]`
const AUTHOR_BRACKET_REGEX: &str = r"\[([^\]]+)\]\s*$";
/// `circle (artist)` 拆分
const CIRCLE_ARTIST_REGEX: &str = r"(.+?)\s*\((.+?)\)";

impl EHentaiParser {
    /// Kotlin `parseGid`：`"gid;token"` 拆分；格式非法抛异常。
    fn parse_gid(series_id: &ProviderSeriesId) -> Result<(i32, String), ProviderError> {
        let parts: Vec<&str> = series_id.0.splitn(2, ';').collect();
        if parts.len() != 2 {
            return Err(ProviderError::message(format!(
                "Invalid E-Hentai ID format: {}",
                series_id.0
            )));
        }
        let gid = parts[0].parse::<i32>().map_err(|_| {
            ProviderError::message(format!("Invalid GID (Not a number) in ID: {}", series_id.0))
        })?;
        Ok((gid, parts[1].to_string()))
    }

    /// Kotlin `parseTitle`：去尾部 `[xxx]` → 去头部 `(convention)[circle]` →
    /// 纯数字/平台前缀回退 → `|` 取最右 → 空回退。
    fn parse_title(raw_title: &str) -> EHentaiParsedTitle {
        if raw_title.trim().is_empty() {
            return EHentaiParsedTitle {
                match_title: String::new(),
                best_match: String::new(),
            };
        }

        let trailing_re = regex::Regex::new(TRAILING_REGEX).unwrap();
        let without_trailing = trailing_re.replace_all(raw_title, "").trim().to_string();

        let leading_re = regex::Regex::new(LEADING_REGEX).unwrap();
        let mut base_title = leading_re
            .replace_all(&without_trailing, "")
            .trim()
            .to_string();

        let date_only_re = regex::Regex::new(DATE_ONLY_REGEX).unwrap();
        let platform_re = regex::Regex::new(PLATFORM_PREFIX_REGEX).unwrap();
        let reverted = date_only_re.is_match(&base_title) || platform_re.is_match(raw_title);
        if reverted {
            base_title = without_trailing;
        }

        // 取 `|` 最右侧的翻译名/原名
        let mut best_match = if base_title.contains('|') {
            base_title
                .rsplit('|')
                .next()
                .map(|s| s.trim().to_string())
                .unwrap_or_else(|| base_title.clone())
        } else {
            base_title.clone()
        };

        // 增强（parse_filename 对齐）：非回退路径清除残留括号 `[..]`/`(..)`/`（..）`
        // 括号替换为单个空格再压缩，避免两侧词粘连（如 "Title（番外）End" → "Title End"）
        if !reverted && !best_match.trim().is_empty() {
            let remnant_re = regex::Regex::new(REMNANT_BRACKET_REGEX).unwrap();
            let spaced = remnant_re.replace_all(&best_match, " ");
            let collapse_re = regex::Regex::new(r"\s+").unwrap();
            let cleaned = collapse_re.replace_all(&spaced, " ").trim().to_string();
            if !cleaned.is_empty() {
                best_match = cleaned;
            }
        }

        if base_title.trim().is_empty() {
            base_title = raw_title.to_string();
        }

        EHentaiParsedTitle {
            match_title: base_title.clone(),
            best_match: if best_match.trim().is_empty() {
                base_title
            } else {
                best_match
            },
        }
    }

    /// Kotlin `getSearchQueries`：最多 3 个搜索变体
    /// （rawTitle → bestMatch → 去尾部括号的 coreTitle），trim + 去空 + 保序去重。
    fn get_search_queries(raw_title: &str) -> Vec<String> {
        let parsed = Self::parse_title(raw_title);
        let core_title_re = regex::Regex::new(CORE_TITLE_REGEX).unwrap();
        let core_title = core_title_re
            .replace_all(&parsed.best_match, "")
            .trim()
            .to_string();
        let final_core_title = if core_title.is_empty() {
            parsed.best_match.clone()
        } else {
            core_title
        };

        let mut out = Vec::new();
        for s in [raw_title.to_string(), parsed.best_match, final_core_title] {
            let s = s.trim().to_string();
            if !s.is_empty() && !out.contains(&s) {
                out.push(s);
            }
        }
        out
    }

    /// `parse_filename` 对齐：标题前最后一个 `[circle (artist)]`。
    /// 返回 (writer 社团, penciller 画师)，均未清理（由调用方 `clean_author_names` 处理）。
    fn parse_authors_from_title(title: &str) -> (Option<String>, Option<String>) {
        let remove_re = regex::Regex::new(REMNANT_BRACKET_REGEX).unwrap();
        let cleaned = remove_re.replace_all(title, "").trim().to_string();
        if cleaned.is_empty() {
            return (None, None);
        }
        // 找到 title 在原始 text 中的起始位置，截取 title 前的文本
        let Some(idx) = title.find(&cleaned) else {
            return (None, None);
        };
        let before_title = &title[..idx];
        // 匹配紧挨标题的前一个 [] 内的内容（EH 命名规范中它总是作者信息）
        let author_re = regex::Regex::new(AUTHOR_BRACKET_REGEX).unwrap();
        let Some(caps) = author_re.captures(before_title) else {
            return (None, None);
        };
        let author_str = caps.get(1).map(|m| m.as_str().trim()).unwrap_or("");
        if author_str.is_empty() {
            return (None, None);
        }
        // `circle (artist)` → 社团为 writer，画师为 penciller；否则两者同值。
        // 多人保留增强：`[Circle (A, B)]` 中括号内多人全部保留在
        // penciller（不把第一段并入 writer——参考实现会把第一段视为"原著作者"并入
        // writer，导致 `[Poki no Ie (Pochikin, Chinpoki)]` 这种罗马音变体场景被错误拆成
        // "Poki no Ie, Pochikin" + "Chinpoki"）；多人拆分交由调用方 `clean_author_names` 完成。
        let circle_re = regex::Regex::new(CIRCLE_ARTIST_REGEX).unwrap();
        match circle_re.captures(author_str) {
            Some(c) => (
                c.get(1).map(|m| m.as_str().to_string()),
                c.get(2).map(|m| m.as_str().to_string()),
            ),
            None => (Some(author_str.to_string()), Some(author_str.to_string())),
        }
    }

    /// `find_translator` 对齐：从标题中识别汉化组（无翻译模块，仅识别文本）。
    /// `keywords` 来自配置 translatorKeywords（空时无匹配）。
    /// Jinja2 风格轻量模板（ComicInfo 字段模板对齐）：
    /// 支持 `{{var}}` 变量替换与 `{% if var %}...{% endif %}` 条件块（变量非空真值；
    /// if 块内可再含 `{{var}}`，不支持嵌套 if）。未定义/空变量替换为空串。
    pub(crate) fn render_title_template(
        template: &str,
        vars: &std::collections::HashMap<String, String>,
    ) -> String {
        let if_re = regex::Regex::new(
            r"(?s)\{%\s*if\s+([A-Za-z_][A-Za-z0-9_]*)\s*%\}(.*?)\{%\s*endif\s*%\}",
        )
        .expect("valid if regex");
        let var_re =
            regex::Regex::new(r"\{\{\s*([A-Za-z_][A-Za-z0-9_]*)\s*\}\}").expect("valid var regex");
        // 先展开条件块（逐轮迭代直到稳定；if 内再含 if 时按最内层处理）
        let mut out = template.to_string();
        loop {
            let next = if_re.replace_all(&out, |caps: &regex::Captures| {
                let name = &caps[1];
                let present = vars
                    .get(name)
                    .map(|v| !v.trim().is_empty())
                    .unwrap_or(false);
                if present {
                    caps[2].to_string()
                } else {
                    String::new()
                }
            });
            let next = next.into_owned();
            if next == out {
                break;
            }
            out = next;
        }
        // 再替换变量
        var_re
            .replace_all(&out, |caps: &regex::Captures| {
                vars.get(&caps[1]).cloned().unwrap_or_default()
            })
            .into_owned()
    }

    fn find_translator(title: &str, keywords: &[String]) -> Option<String> {
        if keywords.is_empty() {
            return None;
        }
        let alternation = keywords
            .iter()
            .map(|k| regex::escape(k))
            .collect::<Vec<_>>()
            .join("|");
        let pattern = format!(
            r"[\[\(【]([^\]\)】]*?(?:{alternation})[^\]\)】]*)[\]\)】]|\s+([^\[\(【\]\)】\s]*(?:{alternation})[^\[\(【\]\)】\s]*)"
        );
        let re = regex::Regex::new(&pattern).ok()?;
        let caps = re.captures(title)?;
        let group = caps.get(1).or_else(|| caps.get(2))?;
        let s = group.as_str().trim();
        if s.is_empty() {
            None
        } else {
            Some(s.to_string())
        }
    }
}

// ---------------------------------------------------------------------------
// 标签过滤常量 —— parse_eh_tags 对齐（无翻译模块）
// ---------------------------------------------------------------------------

/// 垃圾标签黑名单（other/tag 命名空间排除）
/// 默认汉化组关键词（可配置 translatorKeywords 覆盖）；用于 forced-language 检测与 Translator 作者解析。
/// 含日文"翻訳"（[中国翻訳] 等日文标题汉化组标记，与 forced-language 的"中国翻訳"特征一致）。
const DEFAULT_TRANSLATOR_KEYWORDS: [&str; 10] = [
    "汉化", "漢化", "翻译", "翻譯", "翻訳", "机翻", "機翻", "渣翻", "个汉", "個漢",
];

const EH_TAG_BLACKLIST: &[&str] = &[
    "extraneous ads",
    "already uploaded",
    "missing cover",
    "forbidden content",
    "replaced",
    "compilation",
    "incomplete",
    "caption",
];

// ---------------------------------------------------------------------------
// EhTagTranslation 标签翻译 —— hentai-assistant src/providers/ehtranslator.py 对齐
// ---------------------------------------------------------------------------

/// EhTagTranslation 数据库（db.text.json）：
/// ```json
/// {"data": [{"namespace": "female", "data": {"anal": {"name": "肛门", ...}}}]}
/// ```
/// 按 namespace(小写) → tag(小写) → 中文名 组织；`name` 缺失时无翻译项。
#[derive(Debug, Clone, Default)]
pub(crate) struct TagTranslator {
    tagsdict: std::collections::HashMap<String, std::collections::HashMap<String, String>>,
}

impl TagTranslator {
    /// 从 db.text.json 流式加载；文件缺失/解析失败/空库 → None（等效禁用，调用方用原名）。
    ///
    /// 流式解析（serde_json::from_reader + 派生结构，仅读 name 字段，忽略 intro 等）：
    /// 直接构建最终双层 HashMap（组 data 经 into_iter move 转移，无双份拷贝）
    pub(crate) fn load(path: &std::path::Path) -> Option<Self> {
        #[derive(serde::Deserialize)]
        struct TagInfo {
            name: Option<String>,
        }
        #[derive(serde::Deserialize)]
        struct NamespaceGroup {
            namespace: String,
            #[serde(default)]
            data: std::collections::HashMap<String, TagInfo>,
        }
        #[derive(serde::Deserialize)]
        struct Db {
            #[serde(default)]
            data: Vec<NamespaceGroup>,
        }
        let file = std::fs::File::open(path).ok()?;
        let reader = std::io::BufReader::new(file);
        let db: Db = serde_json::from_reader(reader).ok()?;
        let mut tagsdict = std::collections::HashMap::new();
        for group in db.data {
            let namespace = group.namespace.trim().to_lowercase();
            if namespace.is_empty() {
                continue;
            }
            let mut map = std::collections::HashMap::new();
            for (tag, info) in group.data {
                let name = info.name.as_deref().map(remove_emoji).and_then(|n| {
                    let n = n.trim().to_string();
                    if n.is_empty() { None } else { Some(n) }
                });
                if let Some(name) = name {
                    map.insert(tag.trim().to_lowercase(), name);
                }
            }
            if !map.is_empty() {
                tagsdict.insert(namespace, map);
            }
        }
        if tagsdict.is_empty() {
            None
        } else {
            Some(Self { tagsdict })
        }
    }

    /// 翻译（对齐 ehtranslator.get_translation）：
    /// 先按 namespace 精确查；未命中（或 namespace=None）全局遍历所有 namespace。
    /// 命中返回中文名；未命中返回 None（调用方保留原名）。
    pub(crate) fn translate(&self, text: &str, namespace: Option<&str>) -> Option<String> {
        let key = text.trim().to_lowercase();
        if key.is_empty() {
            return None;
        }
        if let Some(ns) = namespace.map(|n| n.trim().to_lowercase()) {
            if let Some(name) = self.tagsdict.get(&ns).and_then(|m| m.get(&key)) {
                return Some(name.clone());
            }
        }
        for map in self.tagsdict.values() {
            if let Some(name) = map.get(&key) {
                return Some(name.clone());
            }
        }
        None
    }

    /// 库是否为空（无任何命名空间）。
    #[allow(dead_code)]
    pub(crate) fn is_empty(&self) -> bool {
        self.tagsdict.is_empty()
    }
}

/// 去除 emoji（对齐 hentai-assistant utils.remove_emoji；按常见 emoji Unicode 块/变体过滤）。
fn remove_emoji(s: &str) -> String {
    s.chars()
        .filter(|c| {
            let cp = u32::from(*c);
            !(matches!(cp,
                // 表情符号主区块 U+1F000–U+1FAFF、装饰 U+2600–U+27BF、杂项 U+2B00–U+2BFF、
                // 变体选择符-16（U+FE0F）、零宽连接符（U+200D）
                0x1F000..=0x1FAFF | 0x2600..=0x27BF | 0x2B00..=0x2BFF | 0xFE0F | 0x200D))
        })
        .collect()
}

// ---------------------------------------------------------------------------
// 标签翻译库更新机制（应用内部统一接管；对齐 hentai-assistant ehtranslator.py）
// ---------------------------------------------------------------------------

/// 默认下载地址（EhTagTranslation 官方 release；对齐 hentai-assistant DB_URL）。
pub(crate) const DEFAULT_TAG_TRANSLATION_URL: &str =
    "https://github.com/EhTagTranslation/Database/releases/latest/download/db.text.json";

/// 定期检查间隔：每 24 小时（对齐 hentai-assistant CHECK_INTERVAL_HOURS=24）。
pub(crate) const TAG_TRANSLATION_UPDATE_HOURS: u64 = 24;

/// 翻译库缓存路径（缺省 workDir/ehentai/db.text.json）。
fn tag_translation_db_path(work_dir: Option<&std::path::Path>) -> std::path::PathBuf {
    work_dir
        .map(|d| d.join("ehentai").join("db.text.json"))
        .unwrap_or_default()
}

/// 翻译库 meta 路径（记录 last_checked，供过期判断）。
fn tag_translation_meta_path(work_dir: Option<&std::path::Path>) -> std::path::PathBuf {
    work_dir
        .map(|d| d.join("ehentai").join("db_meta.json"))
        .unwrap_or_default()
}

/// meta 是否在 interval 内更新过（对齐 hentai-assistant load_or_update_on_startup；
/// 文件缺失/解析失败 → false=需要更新）。
fn tag_translation_meta_fresh(meta_path: &std::path::Path, interval: std::time::Duration) -> bool {
    let raw = match std::fs::read_to_string(meta_path) {
        Ok(s) => s,
        Err(_) => return false,
    };
    let json: serde_json::Value = match serde_json::from_str(&raw) {
        Ok(v) => v,
        Err(_) => return false,
    };
    let ts = match json.get("last_checked").and_then(|v| v.as_str()) {
        Some(s) => s,
        None => return false,
    };
    let checked = match chrono::DateTime::parse_from_rfc3339(ts) {
        Ok(t) => t.with_timezone(&chrono::Utc),
        Err(_) => return false,
    };
    match chrono::Utc::now().signed_duration_since(checked).to_std() {
        Ok(elapsed) => elapsed < interval,
        Err(_) => false,
    }
}

/// 下载翻译库到 db_path 并写 meta；返回是否成功。
/// 失败（网络/非 200/非 JSON）→ false，调用方回退本地已有缓存。
async fn download_tag_translation_db(
    client: &reqwest::Client,
    url: &str,
    db_path: &std::path::Path,
    meta_path: &std::path::Path,
) -> bool {
    let resp = match client.get(url).send().await {
        Ok(r) if r.status().is_success() => r,
        Ok(r) => {
            tracing::warn!("tag translation download HTTP {}", r.status());
            return false;
        }
        Err(e) => {
            tracing::warn!("tag translation download failed: {e}");
            return false;
        }
    };
    let bytes = match resp.bytes().await {
        Ok(b) => b,
        Err(e) => {
            tracing::warn!("tag translation read body failed: {e}");
            return false;
        }
    };
    // 校验 JSON 可解析（避免把坏内容写入缓存）
    if serde_json::from_slice::<serde_json::Value>(&bytes).is_err() {
        tracing::warn!("tag translation download is not valid JSON, ignored");
        return false;
    }
    if let Some(dir) = db_path.parent() {
        let _ = std::fs::create_dir_all(dir);
    }
    if std::fs::write(db_path, &bytes).is_err() {
        tracing::warn!("tag translation db write failed");
        return false;
    }
    if let Some(dir) = meta_path.parent() {
        let _ = std::fs::create_dir_all(dir);
    }
    let meta = serde_json::json!({ "last_checked": chrono::Utc::now().to_rfc3339() });
    let _ = std::fs::write(meta_path, meta.to_string());
    true
}

/// ehwiki Fetish_Listing 的 male-only 标签（内置回退；json 文件缺失/下载失败时使用。
/// 优先从 male_only_taglist.json 读取，一致）
const DEFAULT_MALE_ONLY_TAGS: &[&str] = &[
    "dilf",
    "old man",
    "feminization",
    "giant",
    "miniguy",
    "tall man",
    "gyaru-oh",
    "bbm",
    "ssbbm",
    "dickgirl on male",
    "no balls",
    "cuntboy",
    "pegging",
    "alien",
    "bat boy",
    "bear boy",
    "bee boy",
    "bird boy",
    "bunny boy",
    "catboy",
    "cowman",
    "deer boy",
    "demon",
    "dog boy",
    "elephant boy",
    "fox boy",
    "frog boy",
    "giraffe boy",
    "hedgehog boy",
    "hippo boy",
    "horse boy",
    "hyena boy",
    "insect boy",
    "kangaroo boy",
    "lizard guy",
    "merman",
    "minotaur",
    "monkey boy",
    "monster",
    "moth boy",
    "mouse boy",
    "mushroom boy",
    "otter boy",
    "panda boy",
    "pig man",
    "plant boy",
    "raccoon boy",
    "rhinoceros boy",
    "shark boy",
    "sheep boy",
    "skunk boy",
    "slime boy",
    "snake boy",
    "spider boy",
    "squid boy",
    "squirrel boy",
    "wolf boy",
    "bull",
    "lion",
    "clothed female nude male",
    "mecha boy",
    "ninja",
    "policeman",
    "priest",
    "steward",
    "mmm threesome",
    "josou seme",
    "otokofutanari",
    "yaoi",
    "males only",
    "pussyboys only",
    "sole male",
    "sole pussyboy",
    "tomgirl",
    "virginity",
    "widower",
    "brother",
    "father",
    "grandfather",
    "uncle",
    "low shotacon",
];

/// 读取 male-only 标签 json（{"content": [...]} 与 male_only_taglist.json 一致）。
fn read_male_only_json(path: &std::path::Path) -> Option<Vec<String>> {
    let text = std::fs::read_to_string(path).ok()?;
    let value: serde_json::Value = serde_json::from_str(&text).ok()?;
    let content = value.get("content")?.as_array()?;
    let tags: Vec<String> = content
        .iter()
        .filter_map(|x| x.as_str().map(|s| s.to_string()))
        .filter(|s| !s.is_empty())
        .collect();
    if tags.is_empty() {
        None
    } else {
        Some(tags)
    }
}

/// ehwiki Fetish_Listing HTML 解析 male-only 标签（male_only_taglist 对齐）：
/// 抓取 `<a ...>Tag</a>♂` 形式的条目（a 标签后紧跟 ♂ 标记），去重保序。
fn parse_male_only_html(html: &str) -> Vec<String> {
    let re = regex::Regex::new(r#"<a[^>]*>([^<]*)</a>\s*([^<]*)♂"#).unwrap();
    let mut tags: Vec<String> = Vec::new();
    let mut seen = std::collections::HashSet::new();
    for cap in re.captures_iter(html) {
        let name = cap
            .get(1)
            .map(|m| m.as_str())
            .unwrap_or("")
            .trim()
            .trim_matches('\u{200e}')
            .trim();
        if !name.is_empty() && seen.insert(name.to_string()) {
            tags.push(name.to_string());
        }
    }
    tags
}

/// 下载 ehwiki Fetish_Listing 并解析 male-only 标签（male_only_taglist 下载逻辑对齐）。
async fn download_male_only_tags(client: &reqwest::Client) -> Result<Vec<String>, ProviderError> {
    let html = client
        .get("https://ehwiki.org/wiki/Fetish_Listing")
        .send()
        .await?
        .error_for_status()?
        .text()
        .await?;
    Ok(parse_male_only_html(&html))
}

/// 加载 male-only 标签：json 存在→读取；缺失→后台下载并保存 json，本次回退内置常量。
/// `path` 为空路径时直接用内置常量（不下载）。
fn load_male_only_tags(
    path: &std::path::Path,
    client: &reqwest::Client,
) -> std::sync::Arc<Vec<String>> {
    let fallback: Vec<String> = DEFAULT_MALE_ONLY_TAGS
        .iter()
        .map(|s| s.to_string())
        .collect();
    if path.as_os_str().is_empty() {
        return std::sync::Arc::new(fallback);
    }
    if let Some(tags) = read_male_only_json(path) {
        return std::sync::Arc::new(tags);
    }
    let client = client.clone();
    let path = path.to_path_buf();
    tokio::spawn(async move {
        match download_male_only_tags(&client).await {
            Ok(tags) if !tags.is_empty() => {
                if let Some(parent) = path.parent() {
                    let _ = std::fs::create_dir_all(parent);
                }
                let json = serde_json::json!({ "content": tags });
                let _ = std::fs::write(
                    &path,
                    serde_json::to_string_pretty(&json).unwrap_or_default(),
                );
                tracing::info!(
                    "male-only taglist downloaded and saved to {}",
                    path.display()
                );
            }
            Ok(_) => tracing::warn!("male-only taglist download returned empty list"),
            Err(e) => tracing::warn!("male-only taglist download failed: {e}"),
        }
    });
    std::sync::Arc::new(fallback)
}

/// 标签回退：按 prefixes 顺序取 `prefix:name` 的值（fill_field 对齐，无翻译）。
fn ehentai_authors_from_tags(tags: &[&str], prefixes: &[&str]) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    for prefix in prefixes {
        let needle = format!("{prefix}:");
        for t in tags {
            if let Some(rest) = t.strip_prefix(&needle) {
                let rest = rest.trim();
                if !rest.is_empty() && !out.iter().any(|o| o == rest) {
                    out.push(rest.to_string());
                }
            }
        }
    }
    out
}

/// bangumi.clean_author_names 对齐的文本清理：去括号 + 按常见分隔符拆分 + trim + 去空。
fn clean_author_names(value: &str) -> Vec<String> {
    static RE: std::sync::OnceLock<regex::Regex> = std::sync::OnceLock::new();
    let re = RE.get_or_init(|| {
        regex::Regex::new(r"[《【（\[\(（][^》】）\]\)）]*[》】）\]\)）]").unwrap()
    });
    let no_brackets = re.replace_all(value, "");
    no_brackets
        .split(['/', '／', '、', '_', '→', '・', ':', '×', '&', ',', '，'])
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(String::from)
        .collect()
}

// ---------------------------------------------------------------------------
// Tag mapper —— 对应 Kotlin EHentaiTagMapper.kt
// ---------------------------------------------------------------------------

struct EHentaiTagMapper;

/// 对应 Kotlin BCP47_MAP（72 种语言标签 → BCP47 码）。
const BCP47_MAP: &[(&str, &str)] = &[
    ("language:afrikaans", "af"),
    ("language:albanian", "sq"),
    ("language:arabic", "ar"),
    ("language:aramaic", "arc"),
    ("language:armenian", "hy"),
    ("language:bengali", "bn"),
    ("language:bosnian", "bs"),
    ("language:bulgarian", "bg"),
    ("language:burmese", "my"),
    ("language:catalan", "ca"),
    ("language:cebuano", "ceb"),
    ("language:chinese", "zh"),
    ("language:cree", "cr"),
    ("language:creole", "ht"),
    ("language:croatian", "hr"),
    ("language:czech", "cs"),
    ("language:danish", "da"),
    ("language:dutch", "nl"),
    ("language:english", "en"),
    ("language:esperanto", "eo"),
    ("language:estonian", "et"),
    ("language:finnish", "fi"),
    ("language:french", "fr"),
    ("language:georgian", "ka"),
    ("language:german", "de"),
    ("language:greek", "el"),
    ("language:gujarati", "gu"),
    ("language:hebrew", "he"),
    ("language:hindi", "hi"),
    ("language:hmong", "hmn"),
    ("language:hungarian", "hu"),
    ("language:icelandic", "is"),
    ("language:indonesian", "id"),
    ("language:irish", "ga"),
    ("language:italian", "it"),
    ("language:japanese", "ja"),
    ("language:javanese", "jv"),
    ("language:kannada", "kn"),
    ("language:kazakh", "kk"),
    ("language:khmer", "km"),
    ("language:korean", "ko"),
    ("language:kurdish", "ku"),
    ("language:ladino", "lad"),
    ("language:lao", "lo"),
    ("language:latin", "la"),
    ("language:latvian", "lv"),
    ("language:marathi", "mr"),
    ("language:mongolian", "mn"),
    ("language:ndebele", "nd"),
    ("language:nepali", "ne"),
    ("language:norwegian", "no"),
    ("language:oromo", "om"),
    ("language:papiamento", "pap"),
    ("language:pashto", "ps"),
    ("language:persian", "fa"),
    ("language:polish", "pl"),
    ("language:portuguese", "pt"),
    ("language:punjabi", "pa"),
    ("language:romanian", "ro"),
    ("language:russian", "ru"),
    ("language:sango", "sg"),
    ("language:sanskrit", "sa"),
    ("language:serbian", "sr"),
    ("language:shona", "sn"),
    ("language:slovak", "sk"),
    ("language:slovenian", "sl"),
    ("language:somali", "so"),
    ("language:spanish", "es"),
    ("language:swahili", "sw"),
    ("language:swedish", "sv"),
    ("language:tagalog", "tl"),
    ("language:tamil", "ta"),
    ("language:telugu", "te"),
    ("language:thai", "th"),
    ("language:tibetan", "bo"),
    ("language:tigrinya", "ti"),
    ("language:turkish", "tr"),
    ("language:ukrainian", "uk"),
    ("language:urdu", "ur"),
    ("language:vietnamese", "vi"),
    ("language:welsh", "cy"),
    ("language:yiddish", "yi"),
    ("language:zulu", "zu"),
];

impl EHentaiTagMapper {
    fn bcp47_map() -> HashMap<&'static str, &'static str> {
        BCP47_MAP.iter().copied().collect()
    }

    /// 取第一个 `language:` 标签映射为 BCP47 码。
    fn map_language(tags: &[String]) -> Option<String> {
        let map = Self::bcp47_map();
        tags.iter()
            .filter(|t| t.starts_with("language:"))
            .find_map(|t| map.get(t.as_str()).map(|v| v.to_string()))
    }

    /// Kotlin `mapAgeRating`：极端标签（guro/ryona/snuff/scat）→18；
    /// 分类 non-h →15，其余（doujinshi/manga/artist cg 等）→18。
    fn map_age_rating(category: Option<&str>, tags: &[String]) -> Option<i32> {
        let has_extreme = tags.iter().any(|tag| {
            let clean = tag.split_once(':').map(|(_, v)| v).unwrap_or(tag);
            matches!(clean, "guro" | "ryona" | "snuff" | "scat")
        });
        if has_extreme {
            return Some(18);
        }
        match category.map(|c| c.to_lowercase()).as_deref() {
            Some("non-h") => Some(15),
            Some("doujinshi") | Some("manga") | Some("artist cg") | Some("game cg")
            | Some("image set") | Some("western") | Some("cosplay") | Some("misc")
            | Some("private") => Some(18),
            _ => None,
        }
    }
}

/// 搜索域名规范化："exhentai" → "exhentai.org"，其余 → "e-hentai.org"（仅搜索用）。
fn ehentai_search_domain(domain: &str) -> &'static str {
    if domain.eq_ignore_ascii_case("exhentai") {
        "exhentai.org"
    } else {
        "e-hentai.org"
    }
}

// ---------------------------------------------------------------------------
// Client —— 对应 Kotlin EHentaiClient.kt（双 client：api 限流 + img 独立）
// ---------------------------------------------------------------------------

/// 令牌式限流 —— 对应 Ktor HttpRequestRateLimiter(interval=6s, events=4, allowBurst=true)
/// （近似：permit 间隔 = interval / events，超出排队等待）。
#[derive(Clone)]
struct RateLimiter {
    permit_duration: Duration,
    state: std::sync::Arc<tokio::sync::Mutex<RateLimiterState>>,
}

struct RateLimiterState {
    cursor: std::time::Instant,
}

impl RateLimiter {
    fn new(events_per_interval: u32, interval: Duration) -> Self {
        Self {
            permit_duration: interval / events_per_interval,
            state: std::sync::Arc::new(tokio::sync::Mutex::new(RateLimiterState {
                cursor: std::time::Instant::now(),
            })),
        }
    }

    async fn acquire(&self) {
        let wait = {
            let mut state = self.state.lock().await;
            let now = std::time::Instant::now();
            let base = if state.cursor > now {
                state.cursor
            } else {
                now
            };
            state.cursor = base + self.permit_duration;
            base.saturating_duration_since(now)
        };
        if !wait.is_zero() {
            tokio::time::sleep(wait).await;
        }
    }
}

#[derive(Clone)]
pub(crate) struct EHentaiClient {
    api: reqwest::Client,
    img: reqwest::Client,
    rate_limiter: RateLimiter,
    /// 搜索域名（"e-hentai.org" / "exhentai.org"，仅用于搜索）。
    search_domain: String,
    /// 运行时 cookie（ipb_member_id/ipb_pass_hash/sk/igneous）：请求自动携带，Set-Cookie 自动刷新
    /// （EHentaiTools session cookie 机制对齐）。
    cookies: std::sync::Arc<tokio::sync::Mutex<std::collections::HashMap<String, String>>>,
}

impl EHentaiClient {
    /// 默认构造：e-hentai.org、无登录 cookie（测试用；生产走 with_options）。
    #[cfg(test)]
    pub fn new(api: reqwest::Client, img: reqwest::Client) -> Self {
        Self::with_options(api, img, "e-hentai.org", None, None)
    }

    /// 完整构造：search_domain 主机名（已规范化）、ipb 登录 cookie（exhentai 必需）。
    pub fn with_options(
        api: reqwest::Client,
        img: reqwest::Client,
        search_domain: &str,
        ipb_member_id: Option<&str>,
        ipb_pass_hash: Option<&str>,
    ) -> Self {
        let mut cookies = std::collections::HashMap::new();
        if let Some(v) = ipb_member_id.filter(|v| !v.is_empty()) {
            cookies.insert("ipb_member_id".to_string(), v.to_string());
        }
        if let Some(v) = ipb_pass_hash.filter(|v| !v.is_empty()) {
            cookies.insert("ipb_pass_hash".to_string(), v.to_string());
        }
        Self {
            api,
            img,
            rate_limiter: RateLimiter::new(4, Duration::from_secs(6)),
            search_domain: search_domain.to_string(),
            cookies: std::sync::Arc::new(tokio::sync::Mutex::new(cookies)),
        }
    }

    /// 从 Set-Cookie 值解析 `name=value`（纯函数，便于测试）。
    fn parse_set_cookie(value: &str) -> Option<(String, String)> {
        let (name, rest) = value.split_once('=')?;
        let name = name.trim().to_string();
        let val = rest.split(';').next().unwrap_or("").trim().to_string();
        if name.is_empty() || val.is_empty() {
            None
        } else {
            Some((name, val))
        }
    }

    /// 响应 Set-Cookie → 更新会话 cookie 缓存（sk/igneous 自动获取与刷新）。
    async fn update_cookies(&self, response: &reqwest::Response) {
        let mut cookies = self.cookies.lock().await;
        for value in response.headers().get_all(reqwest::header::SET_COOKIE) {
            if let Ok(s) = value.to_str() {
                if let Some((name, val)) = Self::parse_set_cookie(s) {
                    if name == "sk"
                        || name == "igneous"
                        || name == "ipb_member_id"
                        || name == "ipb_pass_hash"
                    {
                        cookies.insert(name, val);
                    }
                }
            }
        }
    }

    /// 当前 cookie → Cookie header（空则无）。
    async fn cookie_header(&self) -> reqwest::header::HeaderMap {
        let mut headers = reqwest::header::HeaderMap::new();
        let value = {
            let cookies = self.cookies.lock().await;
            cookies
                .iter()
                .map(|(k, v)| format!("{k}={v}"))
                .collect::<Vec<_>>()
                .join("; ")
        };
        if !value.is_empty() {
            if let Ok(header) = reqwest::header::HeaderValue::from_str(&value) {
                headers.insert(reqwest::header::COOKIE, header);
            }
        }
        headers
    }

    /// GET 带 cookie；响应 Set-Cookie 自动更新缓存。
    async fn get(
        &self,
        client: &reqwest::Client,
        url: &str,
    ) -> Result<reqwest::Response, ProviderError> {
        let headers = self.cookie_header().await;
        let response = client.get(url).headers(headers).send().await?;
        self.update_cookies(&response).await;
        Ok(response)
    }

    /// GET 带 cookie + exhentai 403（cookie 失效）→ 刷新后重试一次。
    async fn get_with_refresh(&self, url: &str) -> Result<reqwest::Response, ProviderError> {
        let headers = self.cookie_header().await;
        let mut response = self.api.get(url).headers(headers).send().await?;
        self.update_cookies(&response).await;
        if response.status() == reqwest::StatusCode::FORBIDDEN
            && self.search_domain == "exhentai.org"
        {
            // cookie 过期 → 访问 uconfig 刷新（sk/igneous）后重试
            self.refresh_exhentai_cookie().await?;
            let headers = self.cookie_header().await;
            response = self.api.get(url).headers(headers).send().await?;
            self.update_cookies(&response).await;
        }
        Ok(response)
    }

    /// exhentai cookie 自动获取/刷新（is_valid_cookie 对齐）：
    /// e-hentai exchange（sk）+ exhentai uconfig（igneous），Set-Cookie 自动更新缓存。
    async fn refresh_exhentai_cookie(&self) -> Result<(), ProviderError> {
        let r1 = self
            .get(&self.api, "https://e-hentai.org/exchange.php?t=gp")
            .await?;
        let _ = r1.text().await;
        let r2 = self
            .get(&self.api, "https://exhentai.org/uconfig.php")
            .await?;
        let _ = r2.text().await;
        Ok(())
    }

    /// Kotlin `searchByGidList`：gidlist 每 25 个分块请求 gdata API；
    /// 合并 gmetadata，error 取第一个非空。
    pub(crate) async fn search_by_gid_list(
        &self,
        gid_list: &[(i32, String)],
    ) -> Result<EHentaiResponse, ProviderError> {
        if gid_list.is_empty() {
            return Ok(EHentaiResponse::default());
        }

        let mut all = Vec::new();
        let mut first_error: Option<String> = None;
        for chunk in gid_list.chunks(25) {
            self.rate_limiter.acquire().await;
            let body = serde_json::json!({
 "method": "gdata",
                "gidlist": chunk.iter().map(|(gid, token)| [gid.to_string(), token.clone()]).collect::<Vec<_>>(),
 "namespace": 1
 })
            .to_string();
            let headers = self.cookie_header().await;
            let response = self
                .api
                .post(API_URL)
                .headers(headers)
                .header("Content-Type", "application/json")
                .body(body)
                .send()
                .await?;
            self.update_cookies(&response).await;
            let status = response.status();
            if !status.is_success() {
                let text = response.text().await.unwrap_or_default();
                return Err(ProviderError::Status(CoreProviders::EHentai, status, text));
            }
            let text = response.text().await?;
            let parsed: EHentaiResponse = serde_json::from_str(&text)?;
            all.extend(parsed.gmetadata);
            if first_error.is_none() {
                first_error = parsed.error;
            }
        }
        Ok(EHentaiResponse {
            gmetadata: all,
            gid: None,
            error: first_error,
        })
    }

    /// Kotlin `searchByTitle`：HTML 搜索（f_search），最多翻 3 页（`var nexturl`），
    /// 收集 gid/token → distinct → gdata。
    pub(crate) async fn search_by_title(
        &self,
        title: &str,
    ) -> Result<EHentaiResponse, ProviderError> {
        let mut gid_list: Vec<(i32, String)> = Vec::new();
        let mut current_url: Option<String> = None;
        let mut current_page: usize = 0;

        let gallery_re = regex::Regex::new(r"/g/(\d+)/([a-f0-9]+)").unwrap();
        let next_url_re = regex::Regex::new(r#"var\s+nexturl="([^"]+)""#).unwrap();

        let base_url = format!("https://{}/", self.search_domain);
        while current_page < MAX_PAGES {
            self.rate_limiter.acquire().await;
            let html = if let Some(url) = &current_url {
                let response = self.get_with_refresh(url).await?;
                let status = response.status();
                if !status.is_success() {
                    let text = response.text().await.unwrap_or_default();
                    return Err(ProviderError::Status(CoreProviders::EHentai, status, text));
                }
                response.text().await?
            } else {
                let headers = self.cookie_header().await;
                let response = self
                    .api
                    .get(&base_url)
                    .headers(headers)
                    .query(&[("f_search", title)])
                    .send()
                    .await?;
                self.update_cookies(&response).await;
                let status = response.status();
                if status == reqwest::StatusCode::FORBIDDEN && self.search_domain == "exhentai.org"
                {
                    // 第一页 403 → cookie 失效 → 刷新后重试
                    self.refresh_exhentai_cookie().await?;
                    let headers = self.cookie_header().await;
                    let response = self
                        .api
                        .get(&base_url)
                        .headers(headers)
                        .query(&[("f_search", title)])
                        .send()
                        .await?;
                    self.update_cookies(&response).await;
                    let status = response.status();
                    if !status.is_success() {
                        let text = response.text().await.unwrap_or_default();
                        return Err(ProviderError::Status(CoreProviders::EHentai, status, text));
                    }
                    response.text().await?
                } else if !status.is_success() {
                    let text = response.text().await.unwrap_or_default();
                    return Err(ProviderError::Status(CoreProviders::EHentai, status, text));
                } else {
                    response.text().await?
                }
            };

            let page_gids: Vec<(i32, String)> = gallery_re
                .captures_iter(&html)
                .map(|c| (c[1].parse::<i32>().unwrap_or(0), c[2].to_string()))
                .collect();
            if page_gids.is_empty() {
                break;
            }
            gid_list.extend(page_gids);

            match next_url_re
                .captures(&html)
                .and_then(|c| c.get(1))
                .map(|m| m.as_str().replace("&amp;", "&"))
            {
                Some(url) => current_url = Some(url),
                None => break,
            }
            current_page += 1;
        }

        // distinct（保持插入序）
        let mut seen = std::collections::HashSet::new();
        let mut distinct: Vec<(i32, String)> = Vec::new();
        for g in gid_list {
            if seen.insert(g.clone()) {
                distinct.push(g);
            }
        }

        if distinct.is_empty() {
            return Ok(EHentaiResponse {
                error: Some(format!("Empty gidList for search: {title}")),
                ..Default::default()
            });
        }
        self.search_by_gid_list(&distinct).await
    }

    /// Kotlin `getThumbnail`：下载 thumb → Image；URL 为空返回 null；非 2xx 抛错。
    pub(crate) async fn get_thumbnail(
        &self,
        book: &EHentaiBook,
    ) -> Result<Option<Image>, ProviderError> {
        let Some(url) = book.thumb.as_deref().filter(|s| !s.is_empty()) else {
            return Ok(None);
        };
        let headers = self.cookie_header().await;
        let response = self.img.get(url).headers(headers).send().await?;
        self.update_cookies(&response).await;
        let status = response.status();
        if !status.is_success() {
            let text = response.text().await.unwrap_or_default();
            return Err(ProviderError::Status(CoreProviders::EHentai, status, text));
        }
        let bytes = response.bytes().await?;
        Ok(Some(Image::new(bytes.to_vec(), detect_mime(url))))
    }
}

fn detect_mime(url: &str) -> Option<String> {
    let lower = url.to_lowercase();
    if lower.ends_with(".png") {
        Some("image/png".into())
    } else if lower.ends_with(".webp") {
        Some("image/webp".into())
    } else if lower.ends_with(".gif") {
        Some("image/gif".into())
    } else {
        Some("image/jpeg".into())
    }
}

// ---------------------------------------------------------------------------
// Metadata mapper —— 对应 Kotlin EHentaiMetadataMapper.kt
// ---------------------------------------------------------------------------

pub struct EHentaiMetadataMapper {
    metadata_config: crate::config::SeriesMetadataConfig,
    author_roles: Vec<AuthorRole>,
    artist_roles: Vec<AuthorRole>,
    preferred_languages: Vec<String>,
    /// "jpn" → title_jpn 优先（默认）；"title" → 英文 title 优先。
    title_priority: String,
    /// 元数据标题模板（空=不启用，走 titlePriority 逻辑）。
    title_template: String,
    /// 汉化组关键词（配置为空时用默认 9 个）。
    translator_keywords: Vec<String>,
    /// male-only 标签（json 加载或内置回退）。
    male_only_tags: std::sync::Arc<Vec<String>>,
    /// EhTagTranslation 标签翻译共享槽（None=禁用；标签处理时命中则用中文名）。
    /// 应用内部定期更新机制会重建并替换槽内翻译器（同步 RwLock，短临界区）。
    tag_translator: Option<std::sync::Arc<std::sync::RwLock<Option<TagTranslator>>>>,
}

impl EHentaiMetadataMapper {
    /// 默认构造：title_priority="jpn"、内置汉化组关键词、内置 male-only 标签（测试/默认行为）。
    pub fn new(
        metadata_config: crate::config::SeriesMetadataConfig,
        author_roles: Vec<AuthorRole>,
        artist_roles: Vec<AuthorRole>,
        preferred_languages: Vec<String>,
    ) -> Self {
        Self::with_options(
            metadata_config,
            author_roles,
            artist_roles,
            preferred_languages,
            "jpn".to_string(),
            Vec::new(),
            std::sync::Arc::new(
                DEFAULT_MALE_ONLY_TAGS
                    .iter()
                    .map(|s| s.to_string())
                    .collect(),
            ),
            String::new(),
            None,
        )
    }

    /// 完整构造（生产路径）：title_priority、translator_keywords（空→默认）、male_only_tags。
    pub(crate) fn with_options(
        metadata_config: crate::config::SeriesMetadataConfig,
        author_roles: Vec<AuthorRole>,
        artist_roles: Vec<AuthorRole>,
        preferred_languages: Vec<String>,
        title_priority: String,
        translator_keywords: Vec<String>,
        male_only_tags: std::sync::Arc<Vec<String>>,
        title_template: String,
        tag_translator: Option<std::sync::Arc<std::sync::RwLock<Option<TagTranslator>>>>,
    ) -> Self {
        let translator_keywords = if translator_keywords.is_empty() {
            DEFAULT_TRANSLATOR_KEYWORDS
                .iter()
                .map(|s| s.to_string())
                .collect()
        } else {
            translator_keywords
        };
        let title_priority = if title_priority.is_empty() {
            "jpn".to_string()
        } else {
            title_priority
        };
        Self {
            metadata_config,
            author_roles,
            artist_roles,
            preferred_languages,
            title_priority,
            title_template,
            translator_keywords,
            male_only_tags,
            tag_translator,
        }
    }

    /// 特殊化（超 Kotlin）：搜索/匹配标题含「中国翻訳」→ 强制 zh；含「英訳」→ 强制 en。
    fn forced_language_from_search(name: &str) -> Option<&'static str> {
        if name.contains("中国翻訳")
            || name.contains("中國翻訳")
            || name.contains("中国翻译")
            || name.contains("中國翻譯")
            || name.contains("汉化")
            || name.contains("漢化")
            || name.contains("机翻")
            || name.contains("機翻")
            || name.contains("渣翻")
            || name.contains("个汉")
            || name.contains("個漢")
        {
            Some("zh")
        } else if name.contains("英訳") || name.contains("英译") || name.contains("英譯") {
            Some("en")
        } else {
            None
        }
    }

    /// 结果是否具备指定语言特征（强制语言优先匹配用）。
    /// zh：language:chinese 标签 / title_jpn 含中国翻訳/中国翻译/中國翻譯 / title 含汉化组关键词（配置 translatorKeywords）；
    /// en：language:english 标签 / title_jpn 含英訳/英译/英譯 / title 含 [English]/(English)/[en] 标记。
    fn book_matches_forced_language(&self, book: &EHentaiBook, lang: &str) -> bool {
        let tags: &[String] = book.tags.as_deref().unwrap_or_default();
        match lang {
            "zh" => {
                tags.iter().any(|t| *t == "language:chinese")
                    || book
                        .title_jpn
                        .as_deref()
                        .map(|t| {
                            t.contains("中国翻訳")
                                || t.contains("中國翻譯")
                                || t.contains("中国翻译")
                                || t.contains("中國翻譯")
                                || t.contains("汉化")
                                || t.contains("漢化")
                                || t.contains("机翻")
                                || t.contains("機翻")
                                || t.contains("渣翻")
                                || t.contains("个汉")
                                || t.contains("個漢")
                        })
                        .unwrap_or(false)
                    || self
                        .translator_keywords
                        .iter()
                        .any(|k| book.title.contains(k.as_str()))
            }
            "en" => {
                tags.iter().any(|t| *t == "language:english")
                    || book
                        .title_jpn
                        .as_deref()
                        .map(|t| t.contains("英訳") || t.contains("英译") || t.contains("英譯"))
                        .unwrap_or(false)
                    || book.title.contains("[English]")
                    || book.title.contains("(English)")
                    || book.title.contains("[en]")
            }
            _ => false,
        }
    }

    pub(crate) fn to_series_metadata(
        &self,
        book: &EHentaiBook,
        thumbnail: Option<Image>,
        forced_language: Option<&str>,
    ) -> ProviderSeriesMetadata {
        let cfg = &self.metadata_config;
        let raw_tags = book.tags.clone().unwrap_or_default();

        // 特殊化（超 Kotlin）：搜索/匹配标题含「中国翻訳」→ 语言强制写 zh；含「英訳」→ 强制写 en；
        // 否则按结果 language 标签推导。
        let language = if cfg.language {
            forced_language
                .map(|l| l.to_string())
                .or_else(|| EHentaiTagMapper::map_language(&raw_tags))
        } else {
            None
        };

        // 增强（parse_gmetadata/parse_filename 对齐，无翻译）：
        // ① 标题驱动 `[circle (artist)]` → writer=社团 × author_roles、penciller=画师 × artist_roles、`、`/`,` 多人拆分；
        // ② gdata 标签回退：Writer 缺失 ← group+artist、Penciller 缺失 ← artist+group；
        // ③ 汉化组 → Translator；全部经 clean_author_names 文本清理 + 去重。
        // 作者解析输入：title_jpn 非空优先，否则英文 title（PREFER_JAPANESE_TITLE 对齐）。
        let author_source: &str = book
            .title_jpn
            .as_deref()
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .unwrap_or(&book.title);
        let authors = if cfg.authors {
            let mut authors: Vec<Author> = Vec::new();
            let push_cleaned = |authors: &mut Vec<Author>, name: &str, role: AuthorRole| {
                for cleaned in clean_author_names(name) {
                    if !authors.iter().any(|a| a.name == cleaned && a.role == role) {
                        authors.push(Author {
                            name: cleaned,
                            role,
                        });
                    }
                }
            };
            // ① 标题驱动（EH 的 [circle (artist)] 规范；优先 title_jpn —— 英文 title 常为罗马音/翻译版
            // （如 [Poki no Ie (Pochikin, Chinpoki)]），日文原版才是规范 [circle (artist)]
            // （如 [ぽきの家 (ちんぽき)]））
            let (writer, penciller) = EHentaiParser::parse_authors_from_title(author_source);
            if let Some(w) = writer {
                for role in &self.author_roles {
                    push_cleaned(&mut authors, &w, *role);
                }
            }
            if let Some(p) = penciller {
                for role in &self.artist_roles {
                    push_cleaned(&mut authors, &p, *role);
                }
            }
            // ② gdata 标签回退（仅当对应角色缺失）
            let tag_refs: Vec<&str> = raw_tags.iter().map(String::as_str).collect();
            if !authors.iter().any(|a| a.role == AuthorRole::Writer) {
                for name in ehentai_authors_from_tags(&tag_refs, &["group", "artist"]) {
                    for role in &self.author_roles {
                        push_cleaned(&mut authors, &name, *role);
                    }
                }
            }
            if !authors.iter().any(|a| a.role == AuthorRole::Penciller) {
                for name in ehentai_authors_from_tags(&tag_refs, &["artist", "group"]) {
                    for role in &self.artist_roles {
                        push_cleaned(&mut authors, &name, *role);
                    }
                }
            }
            // ③ 汉化组 → Translator（find_translator，作用于英文标题）
            let translator_found =
                EHentaiParser::find_translator(&book.title, &self.translator_keywords);
            if let Some(translator) = &translator_found {
                push_cleaned(&mut authors, translator, AuthorRole::Translator);
            }
            authors
        } else {
            Vec::new()
        };

        // 增强（parse_eh_tags 对齐）：命名空间白名单 + 黑名单 + male-only 过滤 +
        // EhTagTranslation 标签翻译（tag_translation_enabled 时命中替换中文名）+ 保序去重；
        // language 走 BCP47 映射不进 tags；无命名空间标签丢弃。
        let tags: Vec<String> = if cfg.tags {
            let mut seen = std::collections::HashSet::new();
            raw_tags
                .iter()
                .filter_map(|t| {
                    let (ns, name) = t.split_once(':')?;
                    let ns_l = ns.to_lowercase();
                    let name_l = name.trim().to_lowercase();
                    if name_l.is_empty() {
                        return None;
                    }
                    // 翻译辅助：命中翻译库 → 中文名；未命中 → 原名。
                    // 对齐 ehtranslator.get_translation：tag 命名空间全局查（无 namespace），
                    // 其余按命名空间查，未命中回退全局遍历。
                    let tr = |n: &str, with_ns: Option<&str>| -> String {
                        if let Some(shared) = &self.tag_translator {
                            if let Ok(guard) = shared.read() {
                                if let Some(translator) = guard.as_ref() {
                                    if let Some(name) = translator.translate(n, with_ns) {
                                        return name;
                                    }
                                }
                            }
                        }
                        n.to_string()
                    };
                    match ns_l.as_str() {
                        "language" => None,
                        "parody" => {
                            if name_l == "original" || name_l == "various" {
                                None
                            } else {
                                Some(format!("parody:{}", tr(name, Some("parody"))))
                            }
                        }
                        "character" => Some(format!("character:{}", tr(name, Some("character")))),
                        "female" | "mixed" | "location" => Some(tr(name, Some(&ns_l))),
                        "male" => {
                            if self.male_only_tags.iter().any(|m| m == &name_l) {
                                Some(tr(name, Some("male")))
                            } else {
                                None
                            }
                        }
                        "other" | "tag" => {
                            if EH_TAG_BLACKLIST.iter().any(|b| *b == name_l) {
                                None
                            } else {
                                let with_ns = if ns_l == "tag" { None } else { Some(ns_l.as_str()) };
                                Some(tr(name, with_ns))
                            }
                        }
                        _ => None,
                    }
                })
                .filter(|t| seen.insert(t.clone()))
                .collect()
        } else {
            Vec::new()
        };

        // parse_eh_tags 对齐：不含 webtoon 标签 → 右到左（ComicInfo Manga=YesAndRightToLeft 映射）；
        // 含 webtoon → Webtoon 阅读方向（Kotlin ReadingDirection.Webtoon 映射；Komga "WEBTOON"）。
        // EH 的 webtoon 标签格式为 `other:webtoon`（有命名空间）；用 raw_tags 独立检查，
        // 不依赖过滤后的 tags——避免 seriesMetadata.tags=false 时 webtoon 检测失效。
        let reading_direction = if cfg.reading_direction {
            let has_webtoon = raw_tags.iter().any(|t| {
                t.eq_ignore_ascii_case("webtoon")
                    || t.rsplit_once(':')
                        .map(|(_, name)| name.eq_ignore_ascii_case("webtoon"))
                        .unwrap_or(false)
            });
            if has_webtoon {
                Some(ReadingDirection::Webtoon)
            } else {
                Some(ReadingDirection::RightToLeft)
            }
        } else {
            None
        };

        // 增强（超 Kotlin）：title_jpn 非空即用 → NATIVE（不再要求 language:japanese）；
        // 否则 type 依 language（null→null、ja→ROMAJI、其他→LOCALIZED）。
        // titlePriority 配置："title" → 始终用英文 title；"jpn"（默认）→ title_jpn 非空优先。
        let use_japanese_title = self.title_priority != "title"
            && book
                .title_jpn
                .as_deref()
                .map(|s| !s.trim().is_empty())
                .unwrap_or(false);
        let title = if use_japanese_title {
            SeriesTitle {
                name: EHentaiParser::parse_title(book.title_jpn.as_deref().unwrap_or(""))
                    .best_match,
                r#type: Some(TitleType::Native),
                language: Some("ja".to_string()),
            }
        } else {
            let r#type = match language.as_deref() {
                None => None,
                Some("ja") => Some(TitleType::Romaji),
                Some(_) => Some(TitleType::Localized),
            };
            SeriesTitle {
                name: EHentaiParser::parse_title(&book.title).best_match,
                r#type,
                language: language.clone(),
            }
        };
        // 增强（超 Kotlin）：titleTemplate 非空时按参考实现 模板渲染覆盖主标题
        // （变量：title=按 titlePriority 选出的主标题、translator=汉化组、writer/penciller=标题驱动作者）。
        let title = if !self.title_template.is_empty() {
            let mut vars = std::collections::HashMap::new();
            // title 变量即按 titlePriority 选出的主标题（title_jpn 非空且 priority=jpn → 日文，否则英文）
            vars.insert("title".to_string(), title.name.clone());
            // translator 变量：title_jpn 优先（汉化组标签常出现在日文标题，如 [中国翻訳]），再英文 title
            let translator_for_template = EHentaiParser::find_translator(
                book.title_jpn.as_deref().unwrap_or_default(),
                &self.translator_keywords,
            )
            .or_else(|| EHentaiParser::find_translator(&book.title, &self.translator_keywords));
            vars.insert(
                "translator".to_string(),
                translator_for_template.unwrap_or_default(),
            );
            let (template_writer, template_penciller) =
                EHentaiParser::parse_authors_from_title(author_source);
            vars.insert("writer".to_string(), template_writer.unwrap_or_default());
            vars.insert(
                "penciller".to_string(),
                template_penciller.unwrap_or_default(),
            );
            SeriesTitle {
                name: EHentaiParser::render_title_template(&self.title_template, &vars),
                r#type: title.r#type.clone(),
                language: title.language.clone(),
            }
        } else {
            title
        };
        let title_field = cfg.title.then_some(title.clone());

        // Kotlin MetadataConfigApplier.seriesTitles：title 禁用时保留全部标题仅清空 type/language
        let mut titles = vec![title.clone()];
        if !cfg.title {
            for t in titles.iter_mut() {
                t.r#type = None;
                t.language = None;
            }
        }

        let release_date = cfg
            .release_date
            .then(|| posted_to_release_date(book.posted))
            .flatten();
        let score = cfg.score.then_some(book.rating).flatten();
        let status = cfg.status.then_some(SeriesStatus::Ended);
        let age_rating = cfg
            .age_rating
            .then(|| EHentaiTagMapper::map_age_rating(book.category.as_deref(), &raw_tags))
            .flatten();
        let links = if cfg.links {
            vec![WebLink {
                label: "e-hentai".to_string(),
                url: format!("https://e-hentai.org/g/{}/{}", book.gid, book.token),
            }]
        } else {
            Vec::new()
        };

        let metadata = SeriesMetadata {
            status,
            title: title_field,
            titles,
            summary: None,
            publisher: None,
            alternative_publishers: Vec::new(),
            reading_direction,
            age_rating,
            language,
            // 非 non-h（SFW）内容 → hentai genre（non-h 标签格式：`non-h` / `misc:non-h`）
            genres: if raw_tags
                .iter()
                .any(|t| t == "non-h" || t == "misc:non-h")
            {
                Vec::new()
            } else {
                vec!["hentai".to_string()]
            },
            tags,
            total_book_count: None,
            authors,
            release_date,
            links,
            score,
            thumbnail,
        };

        ProviderSeriesMetadata {
            id: ProviderSeriesId(format!("{};{}", book.gid, book.token)),
            metadata,
            books: Vec::new(),
        }
    }

    pub(crate) fn to_series_search_result(
        &self,
        result: &EHentaiBook,
        _search_name: &str,
    ) -> SeriesSearchResult {
        // 增强（超 Kotlin）：titlePriority 配置 —— "jpn"（默认）优先使用日文标题；"title" 用英文 title
        let title = if self.title_priority != "title"
            && result
                .title_jpn
                .as_deref()
                .map(|s| !s.trim().is_empty())
                .unwrap_or(false)
        {
            result.title_jpn.clone().unwrap()
        } else {
            result.title.clone()
        };
        // 搜索结果语言：尊重原始解析，仅按结果自身 language: 标签映射 BCP47，
        // 不做强制改写（forced-language 仅用于最终元数据 to_series_metadata）。
        let language = EHentaiTagMapper::map_language(result.tags.as_deref().unwrap_or_default());
        SeriesSearchResult {
            result_id: format!("{};{}", result.gid, result.token),
            url: Some(format!(
                "https://e-hentai.org/g/{}/{}",
                result.gid, result.token
            )),
            image_url: result.thumb.clone(),
            title,
            provider: CoreProviders::EHentai.as_str().to_string(),
            media_type: None,
            language,
            nsfw: result.category.as_deref().map(|c| !c.eq_ignore_ascii_case("non-h")),
        }
    }

    /// Kotlin `applyLanguagePreference`：按 preferredLanguages 分组（组内 rating 降序），
    /// 无语言标签的排在末尾（rating 降序）；preferred 无效时原样返回；
    /// 两组皆空时返回空列表。
    pub(crate) fn apply_language_preference(&self, books: &[EHentaiBook]) -> Vec<EHentaiBook> {
        let map = EHentaiTagMapper::bcp47_map();
        let mut valid: Vec<String> = Vec::new();
        for lang in &self.preferred_languages {
            if map.values().any(|v| *v == lang) && !valid.contains(lang) {
                valid.push(lang.clone());
            }
        }
        if valid.is_empty() {
            return books.to_vec();
        }

        let mut grouped: HashMap<String, Vec<EHentaiBook>> = HashMap::new();
        for lang in &valid {
            grouped.insert(lang.clone(), Vec::new());
        }
        let mut no_language_books: Vec<EHentaiBook> = Vec::new();

        for book in books {
            let book_lang = book.tags.as_ref().and_then(|t| {
                t.iter()
                    .filter(|t| t.starts_with("language:"))
                    .find_map(|t| map.get(t.as_str()).map(|v| v.to_string()))
            });

            if let Some(lang) = &book_lang {
                if valid.contains(lang) {
                    grouped.get_mut(lang).unwrap().push(book.clone());
                }
            } else {
                // 无法映射（language:translated / 未收录语言 / 无语言标签）→ 归无语言组保留，不丢弃
                no_language_books.push(book.clone());
            }
        }

        let mut out = Vec::new();
        for lang in &valid {
            let mut list = grouped.remove(lang).unwrap();
            list.sort_by(|a, b| {
                b.rating
                    .unwrap_or(0.0)
                    .partial_cmp(&a.rating.unwrap_or(0.0))
                    .unwrap_or(std::cmp::Ordering::Equal)
            });
            out.extend(list);
        }
        no_language_books.sort_by(|a, b| {
            b.rating
                .unwrap_or(0.0)
                .partial_cmp(&a.rating.unwrap_or(0.0))
                .unwrap_or(std::cmp::Ordering::Equal)
        });

        if !out.is_empty() || !no_language_books.is_empty() {
            out.extend(no_language_books);
            return out;
        }
        Vec::new()
    }
}

/// Kotlin `book.posted?.toLocalDateTime(TimeZone.UTC)` → ReleaseDate。
fn posted_to_release_date(posted: Option<i64>) -> Option<ReleaseDate> {
    use chrono::Datelike;
    let posted = posted?;
    let dt = chrono::DateTime::<chrono::Utc>::from_timestamp(posted, 0)?;
    Some(ReleaseDate::new(
        Some(dt.year()),
        Some(dt.month()),
        Some(dt.day()),
    ))
}

// ---------------------------------------------------------------------------
// Provider —— 对应 Kotlin EHentaiMetadataProvider.kt
// ---------------------------------------------------------------------------

/// 简单 TTL 缓存 —— 对应 Kotlin cache4k expireAfterWrite(5.minutes)。
/// Kotlin cache4k 默认 maximumSize=10000：容量超限时淘汰最旧条目（近似 LRU）。
/// Rust 实现同样带上限，避免 Auto-Identify 扫库（gid 数量级可达数万）把缓存无限撑大。
struct TtlCache<K, V> {
    inner: tokio::sync::Mutex<HashMap<K, (V, std::time::Instant)>>,
    ttl: Duration,
    capacity: usize,
}

impl<K, V> TtlCache<K, V>
where
    K: Eq + std::hash::Hash + Clone,
    V: Clone,
{
    fn new(ttl: Duration) -> Self {
        Self {
            inner: tokio::sync::Mutex::new(HashMap::new()),
            ttl,
            capacity: 10_000,
        }
    }

    /// 命中且未过期直接返回；未命中执行 load，仅成功结果入缓存。
    async fn get_or_load<E, F, Fut>(&self, key: K, load: F) -> Result<V, E>
    where
        F: FnOnce() -> Fut,
        Fut: std::future::Future<Output = Result<V, E>>,
    {
        {
            let guard = self.inner.lock().await;
            if let Some((value, created)) = guard.get(&key) {
                if created.elapsed() < self.ttl {
                    return Ok(value.clone());
                }
            }
        }
        let value = load().await?;
        let mut guard = self.inner.lock().await;
        self.insert_limited(&mut guard, key, value.clone());
        Ok(value)
    }

    async fn put(&self, key: K, value: V) {
        let mut guard = self.inner.lock().await;
        self.insert_limited(&mut guard, key, value);
    }

    /// 插入并维持容量上限：先清过期项；仍超限则移除最旧的 excess 条（近似 LRU，
    /// 与 cache4k 的"超限淘汰最旧"语义一致）。HashMap 无序，按遍历顺序取
    /// 最早的 `excess` 个即可——容量只是软上限，淘汰顺序不影响正确性。
    fn insert_limited(
        &self,
        guard: &mut HashMap<K, (V, std::time::Instant)>,
        key: K,
        value: V,
    ) {
        if guard.len() >= self.capacity {
            let now = std::time::Instant::now();
            guard.retain(|_, (_, created)| now.duration_since(*created) < self.ttl);
        }
        guard.insert(key, (value, std::time::Instant::now()));
        if guard.len() > self.capacity {
            let excess = guard.len() - self.capacity;
            let oldest: Vec<K> = {
                let mut entries: Vec<(&K, &std::time::Instant)> =
                    guard.iter().map(|(k, (_, c))| (k, c)).collect();
                entries.sort_by_key(|(_, c)| **c);
                entries
                    .into_iter()
                    .take(excess)
                    .map(|(k, _)| k.clone())
                    .collect()
            };
            for k in oldest {
                guard.remove(&k);
            }
        }
    }
}

pub struct EHentaiMetadataProvider {
    client: EHentaiClient,
    metadata_mapper: EHentaiMetadataMapper,
    name_matcher: NameSimilarityMatcher,
    fetch_series_covers: bool,
    /// e-hentai-db 离线数据源（None=禁用；gid 精准查询离线优先，未命中回退在线）。
    archive: Option<std::sync::Arc<EHentaiArchiveService>>,
    /// 离线标题搜索的 category 白名单（空=不过滤）。
    archive_category_filter: Vec<String>,
    /// 离线标题搜索的 uploader 白名单（空=不过滤）。
    archive_uploader_filter: Vec<String>,
    /// 自动匹配仅 gid 匹配（Rust 扩展）：true → match 只做 gid 精准搜索，
    /// 无 gid / gid 无结果都跳过（不回落普通相似度搜索）；links 匹配不受影响。
    gid_only_match: bool,
    cache: TtlCache<ProviderSeriesId, EHentaiBook>,
}

pub fn create_provider(
    config: &EHentaiConfig,
    default_name_matcher: NameSimilarityMatcher,
    http_client: &reqwest::Client,
    work_dir: Option<&std::path::Path>,
) -> Option<EHentaiMetadataProvider> {
    if !config.enabled {
        return None;
    }
    let name_matcher = config.name_matching_mode.unwrap_or(default_name_matcher);
    // male-only 标签：配置路径优先，缺省 workDir/ehentai/male_only_taglist.json（一致）
    let male_tags_path = config
        .male_only_tags_file
        .as_ref()
        .map(std::path::PathBuf::from)
        .unwrap_or_else(|| {
            work_dir
                .map(|d| d.join("ehentai").join("male_only_taglist.json"))
                .unwrap_or_default()
        });
    let male_only_tags = load_male_only_tags(&male_tags_path, http_client);
    // EhTagTranslation 标签翻译（hentai-assistant 对齐）：enabled 时由应用内部统一管理更新。
    // 同步先加载本地缓存（workDir/ehentai/db.text.json）；后台任务每 24h 检查 meta 过期/缺文件
    // → 从配置 URL（缺省官方 release）下载 → 写缓存 → 重建翻译器 → 替换共享槽。
    let tag_translator = if config.tag_translation_enabled {
        let tt_path = tag_translation_db_path(work_dir);
        let tt_meta = tag_translation_meta_path(work_dir);
        let url = config
            .tag_translation_url
            .clone()
            .unwrap_or_else(|| DEFAULT_TAG_TRANSLATION_URL.to_string());
        let shared: std::sync::Arc<std::sync::RwLock<Option<TagTranslator>>> =
            std::sync::Arc::new(std::sync::RwLock::new(TagTranslator::load(&tt_path)));
        // 后台周期任务：立即检查一次，之后每 24h（对齐 CHECK_INTERVAL_HOURS）。
        let task_client = http_client.clone();
        let task_shared = shared.clone();
        let task_db = tt_path.clone();
        let task_meta = tt_meta.clone();
        let task_url = url.clone();
        tokio::spawn(async move {
            let interval = std::time::Duration::from_secs(TAG_TRANSLATION_UPDATE_HOURS * 3600);
            loop {
                let need = !tag_translation_meta_fresh(&task_meta, interval) || !task_db.exists();
                if need {
                    if download_tag_translation_db(&task_client, &task_url, &task_db, &task_meta)
                        .await
                    {
                        if let Some(tr) = TagTranslator::load(&task_db) {
                            if let Ok(mut guard) = task_shared.write() {
                                *guard = Some(tr);
                                tracing::info!("ehentai tag translation refreshed");
                            }
                        }
                    } else if !task_db.exists() {
                        tracing::warn!("ehentai tag translation download failed, will retry");
                    }
                }
                tokio::time::sleep(interval).await;
            }
        });
        Some(shared)
    } else {
        None
    };
    // 搜索域名规范化："exhentai" → exhentai.org，其余 → e-hentai.org（仅搜索用）
    let search_domain = ehentai_search_domain(&config.search_domain);
    let client = EHentaiClient::with_options(
        http_client.clone(),
        http_client.clone(),
        search_domain,
        config.ipb_member_id.as_deref(),
        config.ipb_pass_hash.as_deref(),
    );
    // exhentai：后台预热 cookie（自动获取/刷新 sk/igneous）
    if search_domain == "exhentai.org"
        && config.ipb_member_id.is_some()
        && config.ipb_pass_hash.is_some()
    {
        let warmup = client.clone();
        tokio::spawn(async move {
            match warmup.refresh_exhentai_cookie().await {
                Ok(()) => tracing::info!("exhentai cookie warm-up ok"),
                Err(e) => tracing::warn!("exhentai cookie warm-up failed: {e}"),
            }
        });
    }
    // e-hentai-db 离线数据源：enabled 时启动后台下载/解压（tokio::spawn 非阻塞）。
    // 数据文件：配置 dbFile → workDir/ehentai/e-hentai.db。
    let archive = if config.archive.enabled {
        Some(EHentaiArchiveService::start(
            &config.archive,
            http_client.clone(),
            work_dir,
        ))
    } else {
        None
    };
    Some(EHentaiMetadataProvider {
        client,
        metadata_mapper: EHentaiMetadataMapper::with_options(
            config.series_metadata.clone(),
            config.author_roles.clone(),
            config.artist_roles.clone(),
            config.preferred_languages.clone(),
            config.title_priority.clone(),
            config.translator_keywords.clone(),
            male_only_tags,
            config.title_template.clone(),
            tag_translator,
        ),
        name_matcher,
        fetch_series_covers: config.series_metadata.thumbnail,
        gid_only_match: config.gid_only_match,
        archive_category_filter: config.archive.search_category_filter.clone(),
        archive_uploader_filter: config.archive.search_uploader_filter.clone(),
        archive,
        cache: TtlCache::new(Duration::from_secs(5 * 60)),
    })
}

impl EHentaiMetadataProvider {
    /// Kotlin `getBookOrThrow`：gdata 单查；gallery 缺失/API error 抛错。
    /// Rust 扩展：e-hentai-db 离线库就绪时 gid 精准查询优先（未命中回退在线）。
    async fn get_book_or_throw(
        &self,
        series_id: &ProviderSeriesId,
    ) -> Result<EHentaiBook, ProviderError> {
        if let Some(archive) = &self.archive {
            if let Some(store) = archive.get() {
                if let Ok((gid, _)) = EHentaiParser::parse_gid(series_id) {
                    if let Some(row) = store.get_by_gid(gid) {
                        tracing::debug!("ehentai archive hit gid {gid}");
                        return Ok(EHentaiBook::from_archive_row(row));
                    }
                }
            }
        }
        let key = series_id.clone();
        self.cache
            .get_or_load(key, || async {
                let (gid, token) = EHentaiParser::parse_gid(series_id)?;
                let response = self.client.search_by_gid_list(&[(gid, token)]).await?;
                let book = response
                    .gmetadata
                    .into_iter()
                    .next()
                    .ok_or_else(|| ProviderError::message("Gallery not found"))?;
                if let Some(err) = book.error.as_deref() {
                    return Err(ProviderError::message(format!(
                        "E-Hentai API Error for {series_id}: {err}"
                    )));
                }
                Ok(book)
            })
            .await
    }

    /// Kotlin `matchesName`：本地变体 × 远程变体 any 匹配。
    fn matches_name(&self, name: &str, name_to_match: &str) -> bool {
        let local_variants = EHentaiParser::get_search_queries(name);
        let remote_variants = EHentaiParser::get_search_queries(name_to_match);
        local_variants
            .iter()
            .any(|local| self.name_matcher.matches(local, &remote_variants))
    }
}

/// 从标题末尾提取 eHentai gid：`[1234567]` / `(1234567)` / 末尾裸数字（≥5 位纯数字，避免卷号误判）。
fn extract_gid_from_title(title: &str) -> Option<String> {
    let re = regex::Regex::new(r"(?:\[(\d{5,})\]|\((\d{5,})\)|(\d{5,}))\s*$").ok()?;
    let caps = re.captures(title.trim_end())?;
    caps.get(1)
        .or_else(|| caps.get(2))
        .or_else(|| caps.get(3))
        .map(|m| m.as_str().to_string())
}

#[async_trait::async_trait]
impl MetadataProvider for EHentaiMetadataProvider {

    fn resolve_link_id(&self, query: &str) -> Option<String> {
        let re = regex::Regex::new(r"(?:e-hentai\.org|exhentai\.org)/g/(\d+)/([a-f0-9]+)").ok()?;
        re.captures(query)
            .map(|c| format!("{};{}", c.get(1).unwrap().as_str(), c.get(2).unwrap().as_str()))
    }
    fn provider_name(&self) -> CoreProviders {
        CoreProviders::EHentai
    }

    async fn resolve_link_search_result(&self, query: &str) -> Option<SeriesSearchResult> {
        let id = self.resolve_link_id(query)?;
        let book = self.get_book_or_throw(&ProviderSeriesId(id)).await.ok()?;
        Some(self.metadata_mapper.to_series_search_result(&book, query))
    }

    async fn get_series_metadata(
        &self,
        series_id: &ProviderSeriesId,
    ) -> Result<ProviderSeriesMetadata, ProviderError> {
        let book = self.get_book_or_throw(series_id).await?;
        let thumbnail = if self.fetch_series_covers {
            self.client.get_thumbnail(&book).await?
        } else {
            None
        };
        Ok(self
            .metadata_mapper
            .to_series_metadata(&book, thumbnail, None))
    }

    async fn get_series_cover(
        &self,
        series_id: &ProviderSeriesId,
    ) -> Result<Option<Image>, ProviderError> {
        let book = self.get_book_or_throw(series_id).await?;
        self.client.get_thumbnail(&book).await
    }

    async fn get_book_metadata(
        &self,
        _series_id: &ProviderSeriesId,
        _book_id: &ProviderBookId,
    ) -> Result<ProviderBookMetadata, ProviderError> {
        // Kotlin: throw UnsupportedOperationException()
        Err(ProviderError::message("Unsupported operation"))
    }

    async fn search_series(
        &self,
        series_name: &str,
        _limit: usize,
        _media_type: Option<crate::model::MediaType>,
    ) -> Result<Vec<SeriesSearchResult>, ProviderError> {
        let queries = EHentaiParser::get_search_queries(series_name);
        // e-hentai-db 离线优先：标题搜索（FTS5/LIKE）→ 候选；离线空 → 在线回退
        if let Some(archive) = &self.archive {
            let mut offline: Vec<EHentaiBook> = Vec::new();
            for query in &queries {
                let rows = archive.search_titles(
                    query,
                    50,
                    &self.archive_category_filter,
                    &self.archive_uploader_filter,
                );
                offline.extend(rows.into_iter().map(EHentaiBook::from_archive_row));
            }
            if !offline.is_empty() {
                let mut seen = std::collections::HashSet::new();
                let mut filtered: Vec<EHentaiBook> = Vec::new();
                for book in offline {
                    if book.error.is_some() {
                        continue;
                    }
                    if seen.insert(book.gid) {
                        filtered.push(book);
                    }
                }
                let processed = self.metadata_mapper.apply_language_preference(&filtered);
                let processed = match EHentaiMetadataMapper::forced_language_from_search(series_name)
                {
                    Some(lang) => {
                        let (matching, rest): (Vec<EHentaiBook>, Vec<EHentaiBook>) = processed
                            .into_iter()
                            .partition(|b| {
                                self.metadata_mapper.book_matches_forced_language(b, lang)
                            });
                        matching.into_iter().chain(rest).collect()
                    }
                    None => processed,
                };
                let mut out = Vec::new();
                for book in processed {
                    let result =
                        self.metadata_mapper.to_series_search_result(&book, series_name);
                    self.cache
                        .put(ProviderSeriesId(result.result_id.clone()), book)
                        .await;
                    out.push(result);
                }
                return Ok(out);
            }
        }

        let mut raw_results: Vec<EHentaiBook> = Vec::new();
        for query in &queries {
            let truncated: String = query.chars().take(400).collect();
            let response = self.client.search_by_title(&truncated).await?;
            raw_results.extend(response.gmetadata);
        }

        // filter error == null + distinctBy gid（保序）
        let mut seen = std::collections::HashSet::new();
        let mut filtered: Vec<EHentaiBook> = Vec::new();
        for book in raw_results {
            if book.error.is_some() {
                continue;
            }
            if seen.insert(book.gid) {
                filtered.push(book);
            }
        }

        let processed = self.metadata_mapper.apply_language_preference(&filtered);
        // 特殊化（超 Kotlin）：标题含「中国翻訳」→ 中文特征结果提到最前；含「英訳」→ 英文特征提到最前
        let processed = match EHentaiMetadataMapper::forced_language_from_search(series_name) {
            Some(lang) => {
                let (matching, rest): (Vec<EHentaiBook>, Vec<EHentaiBook>) = processed
                    .into_iter()
                    .partition(|b| self.metadata_mapper.book_matches_forced_language(b, lang));
                matching.into_iter().chain(rest).collect()
            }
            None => processed,
        };

        let mut out = Vec::new();
        for book in processed {
            let result = self.metadata_mapper.to_series_search_result(&book, series_name);
            self.cache
                .put(ProviderSeriesId(result.result_id.clone()), book)
                .await;
            out.push(result);
        }
        Ok(out)
    }

    async fn match_series_metadata(
        &self,
        match_query: &MatchQuery,
    ) -> Result<Option<ProviderSeriesMetadata>, ProviderError> {
        // Kotlin: matchQuery.bookQualifier?.name ?: matchQuery.seriesName
        let search_name = match_query
            .book_qualifier
            .as_ref()
            .map(|b| b.name.clone())
            .unwrap_or_else(|| match_query.series_name.clone());
        // 特殊化（超 Kotlin）：标题含「中国翻訳」→ 优先匹配中文结果；含「英訳」→ 优先英文结果
        let forced = EHentaiMetadataMapper::forced_language_from_search(&search_name);

        // Rust 扩展：从系列标题/目录名（oneshot 额外从唯一书籍标题/文件名）提取末尾 gid
        // （`[1234567]` / `(1234567)` / 裸数字）→ 用 `gid:xxx` 精准搜索取唯一结果直接匹配。
        let mut gid_candidates: Vec<String> = vec![match_query.series_name.clone()];
        if let Some(folder) = &match_query.series_folder {
            gid_candidates.push(folder.clone());
        }
        if match_query.oneshot {
            if let Some(bq) = &match_query.book_qualifier {
                gid_candidates.push(bq.name.clone());
            }
            if let Some(file_name) = &match_query.book_file_name {
                gid_candidates.push(file_name.clone());
            }
        }
        if let Some(gid) = gid_candidates.iter().find_map(|c| extract_gid_from_title(c)) {
            tracing::info!("found gid {} in match title, searching gid:{}", gid, gid);
            // e-hentai-db 离线优先：gid 精准查询（未命中 → 在线 gid 搜索）
            if let Some(archive) = &self.archive {
                if let Some(store) = archive.get() {
                    if let Ok(gid_num) = gid.parse::<i32>() {
                        if let Some(row) = store.get_by_gid(gid_num) {
                            let book = EHentaiBook::from_archive_row(row);
                            let cover = if self.fetch_series_covers {
                                self.client.get_thumbnail(&book).await?
                            } else {
                                None
                            };
                            let metadata = self
                                .metadata_mapper
                                .to_series_metadata(&book, cover, forced);
                            self.cache.put(metadata.id.clone(), book).await;
                            return Ok(Some(metadata));
                        }
                    }
                }
            }
            let response = self.client.search_by_title(&format!("gid:{gid}")).await?;
            if let Some(book) = response
                .gmetadata
                .into_iter()
                .find(|b| b.error.is_none() && b.gid.to_string() == gid)
            {
                let cover = if self.fetch_series_covers {
                    self.client.get_thumbnail(&book).await?
                } else {
                    None
                };
                let metadata = self
                    .metadata_mapper
                    .to_series_metadata(&book, cover, forced);
                self.cache.put(metadata.id.clone(), book).await;
                return Ok(Some(metadata));
            }
            if self.gid_only_match {
                // gid 匹配无结果 → 跳过（不回落普通相似度搜索）
                tracing::info!("gid {} matched nothing, skipping (gidOnlyMatch)", gid);
                return Ok(None);
            }
        } else if self.gid_only_match {
            // 无 gid → 跳过（不回落普通相似度搜索）
            tracing::info!("no gid found in match titles, skipping (gidOnlyMatch)");
            return Ok(None);
        }

        let queries = EHentaiParser::get_search_queries(&search_name);
        let mut raw_results: Vec<EHentaiBook> = Vec::new();

        // e-hentai-db 离线优先：标题搜索候选；离线空 → 在线搜索
        let mut offline_used = false;
        if let Some(archive) = &self.archive {
            for query in &queries {
                let rows = archive.search_titles(
                    query,
                    50,
                    &self.archive_category_filter,
                    &self.archive_uploader_filter,
                );
                if !rows.is_empty() {
                    offline_used = true;
                }
                raw_results.extend(rows.into_iter().map(EHentaiBook::from_archive_row));
            }
        }
        if raw_results.is_empty() {
        for query in &queries {
            let truncated: String = query.chars().take(400).collect();
            let response = self.client.search_by_title(&truncated).await?;
            raw_results.extend(response.gmetadata);
        }
        }

        let mut seen = std::collections::HashSet::new();
        let mut filtered: Vec<EHentaiBook> = Vec::new();
        for book in raw_results {
            if book.error.is_some() {
                continue;
            }
            if seen.insert(book.gid) {
                filtered.push(book);
            }
        }

        let processed = self.metadata_mapper.apply_language_preference(&filtered);

        let matches = |book: &EHentaiBook| {
            self.matches_name(&search_name, &book.title)
                || book
                    .title_jpn
                    .as_deref()
                    .map(|t| self.matches_name(&search_name, t))
                    .unwrap_or(false)
        };
        let mut matched = match forced {
            Some(lang) => processed
                .iter()
                .find(|book| {
                    self.metadata_mapper
                        .book_matches_forced_language(book, lang)
                        && matches(book)
                })
                .or_else(|| processed.iter().find(|book| matches(book)))
                .cloned(),
            None => processed.into_iter().find(|book| matches(book)),
        };


        // 离线候选非空但相似度未命中 → 在线回退再匹配（保证不漏）
        if matched.is_none() && offline_used {
            let mut raw: Vec<EHentaiBook> = Vec::new();
            for query in &queries {
                let truncated: String = query.chars().take(400).collect();
                let response = self.client.search_by_title(&truncated).await?;
                raw.extend(response.gmetadata);
            }
            let mut seen = std::collections::HashSet::new();
            let mut filtered: Vec<EHentaiBook> = Vec::new();
            for book in raw {
                if book.error.is_some() {
                    continue;
                }
                if seen.insert(book.gid) {
                    filtered.push(book);
                }
            }
            let processed = self.metadata_mapper.apply_language_preference(&filtered);
            matched = match forced {
                Some(lang) => processed
                    .iter()
                    .find(|book| {
                        self.metadata_mapper
                            .book_matches_forced_language(book, lang)
                            && matches(book)
                    })
                    .or_else(|| processed.iter().find(|book| matches(book)))
                    .cloned(),
                None => processed.into_iter().find(|book| matches(book)),
            };
        }
        let Some(book) = matched else { return Ok(None) };
        let cover = if self.fetch_series_covers {
            self.client.get_thumbnail(&book).await?
        } else {
            None
        };
        let metadata = self
            .metadata_mapper
            .to_series_metadata(&book, cover, forced);
        self.cache.put(metadata.id.clone(), book).await;
        Ok(Some(metadata))
    }
}

// ---------------------------------------------------------------------------
// 单测
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    #[cfg(test)]
    use crate::providers::ehentai_archive::EHentaiArchiveStore;
    /// [オンキュウ] 无 (artist) 括号结构 -> writer=penciller=社团名（直接使用）。
    #[test]
    fn parse_authors_from_title_no_artist_bracket() {
        let (writer, penciller) = EHentaiParser::parse_authors_from_title(
            "[オンキュウ] 生意気ギャルがわからせられる本3.0 [中国翻訳] [DL版]",
        );
        assert_eq!(writer.as_deref(), Some("オンキュウ"));
        assert_eq!(penciller.as_deref(), Some("オンキュウ"));
    }

    /// [circle (artist)] -> writer=社团、penciller=画师（多人由调用方拆分）。
    #[test]
    fn parse_authors_from_title_with_artist() {
        let (writer, penciller) =
            EHentaiParser::parse_authors_from_title("[Poki no Ie (Pochikin, Chinpoki)] Aisareru");
        assert_eq!(writer.as_deref(), Some("Poki no Ie"));
        assert_eq!(penciller.as_deref(), Some("Pochikin, Chinpoki"));
    }

    /// 完整 mapper 链路：title_jpn 含 [オンキュウ] -> 两层角色均写入。
    #[test]
    fn mapper_authors_title_jpn_no_artist() {
        use crate::config::SeriesMetadataConfig;
        let mut cfg = SeriesMetadataConfig::default();
        cfg.authors = true;
        let mapper = EHentaiMetadataMapper::with_options(
            cfg,
            vec![AuthorRole::Writer],
            vec![AuthorRole::Penciller],
            vec!["zh".to_string(), "ja".to_string()],
            "jpn".to_string(),
            Vec::new(),
            std::sync::Arc::new(Vec::new()),
            String::new(),
            None,
        );
        let book = EHentaiBook {
            gid: 4190146,
            token: "90f7fd33fa".to_string(),
            title: "[Onkyu] Namaiki JK ga Wakaraserareru Hon 3.0 [Chinese] [Digital]".to_string(),
            title_jpn: Some(
                "[オンキュウ] 生意気ギャルがわからせられる本3.0 [中国翻訳] [DL版]".to_string(),
            ),
            category: Some("Doujinshi".to_string()),
            rating: Some(2.0),
            tags: None,
            ..Default::default()
        };
        let meta = mapper.to_series_metadata(&book, None, None);
        assert!(
            meta.metadata
                .authors
                .iter()
                .any(|a| a.name == "オンキュウ" && a.role == AuthorRole::Writer),
            "expected writer オンキュウ, got {:?}",
            meta.metadata.authors
        );
        assert!(
            meta.metadata
                .authors
                .iter()
                .any(|a| a.name == "オンキュウ" && a.role == AuthorRole::Penciller),
            "expected penciller オンキュウ, got {:?}",
            meta.metadata.authors
        );
    }

    /// 集成：真实 e-hentai.db（../../ehentai/e-hentai.db）-> from_archive_row ->
    /// to_series_metadata 验证 authors 从 title_jpn 的 [オンキュウ] 解析（--ignored 运行）。
    #[test]
    #[ignore]
    fn archive_real_db_authors() {
        use crate::config::SeriesMetadataConfig;
        let path = std::path::Path::new("../../ehentai/e-hentai.db");
        if !path.exists() {
            eprintln!("db not found, skipping");
            return;
        }
        let store = EHentaiArchiveStore::open(path, 0).expect("open real db");
        let row = store.get_by_gid(4190146).expect("row 4190146");
        let book = EHentaiBook::from_archive_row(row);
        let mut cfg = SeriesMetadataConfig::default();
        cfg.authors = true;
        let mapper = EHentaiMetadataMapper::with_options(
            cfg,
            vec![AuthorRole::Writer],
            vec![AuthorRole::Penciller],
            vec!["zh".to_string(), "ja".to_string()],
            "jpn".to_string(),
            Vec::new(),
            std::sync::Arc::new(Vec::new()),
            String::new(),
            None,
        );
        let meta = mapper.to_series_metadata(&book, None, None);
        eprintln!("authors={:?}", meta.metadata.authors);
        assert!(
            meta.metadata
                .authors
                .iter()
                .any(|a| a.role == AuthorRole::Writer),
            "expected writer from title, got {:?}",
            meta.metadata.authors
        );
    }

    /// 临时 db.text.json（EhTagTranslation 格式：{"data":[{"namespace":..,"data":{..}}]}）。
    fn write_test_db() -> std::path::PathBuf {
        let path = std::env::temp_dir().join(format!(
            "ehentai_test_db_{}.json",
            std::process::id()
        ));
        let json = r#"{"data":[
            {"namespace":"female","data":{
                "anal":{"name":"肛门","intro":""},
                "ahegao":{"name":"阿嘿颜"}
            }},
            {"namespace":"parody","data":{
                "original":{"name":"原创"}
            }},
            {"namespace":"character","data":{
                "hatsune miku":{"name":"初音ミク 🎵"}
            }},
            {"namespace":"male","data":{
                "dilf":{"name":"大叔"}
            }},
            {"namespace":"reclass","data":{
                "manga":{"name":"漫画"}
            }}
        ]}"#;
        std::fs::write(&path, json).unwrap();
        path
    }

    #[test]
    fn tag_translator_load_and_translate() {
        let path = write_test_db();
        let tr = TagTranslator::load(&path).expect("load db");
        // 按 namespace 精确查
        assert_eq!(tr.translate("anal", Some("female")).as_deref(), Some("肛门"));
        // 大小写/空白容错
        assert_eq!(
            tr.translate("  AHEGAO ", Some("Female")).as_deref(),
            Some("阿嘿颜")
        );
        // namespace 未命中 → 全局遍历
        assert_eq!(tr.translate("hatsune miku", None).as_deref(), Some("初音ミク"));
        assert_eq!(
            tr.translate("hatsune miku", Some("female")).as_deref(),
            Some("初音ミク")
        );
        // 未命中 → None
        assert_eq!(tr.translate("nonexistent", Some("female")), None);
        assert_eq!(tr.translate("nonexistent", None), None);
        // 去 emoji（对齐 hentai-assistant remove_emoji）
        assert_eq!(
            tr.translate("hatsune miku", Some("character")).as_deref(),
            Some("初音ミク")
        );
        // 空 key → None
        assert_eq!(tr.translate("   ", Some("female")), None);
        // 文件缺失 → None（等效禁用）
        assert!(TagTranslator::load(std::path::Path::new("no_such_db.json")).is_none());
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn tag_translator_malformed_db() {
        let path = std::env::temp_dir().join(format!(
            "ehentai_test_db_bad_{}.json",
            std::process::id()
        ));
        std::fs::write(&path, "{not json").unwrap();
        assert!(TagTranslator::load(&path).is_none());
        // 空 data → None
        std::fs::write(&path, r#"{"data":[]}"#).unwrap();
        assert!(TagTranslator::load(&path).is_none());
        let _ = std::fs::remove_file(&path);
    }

    fn mapper_with_translator() -> EHentaiMetadataMapper {
        let path = write_test_db();
        let tr = TagTranslator::load(&path).expect("load db");
        EHentaiMetadataMapper::with_options(
            crate::config::SeriesMetadataConfig {
                tags: true,
                ..Default::default()
            },
            vec![AuthorRole::Writer],
            vec![AuthorRole::Penciller],
            vec!["en".to_string(), "ja".to_string()],
            "jpn".to_string(),
            Vec::new(),
            std::sync::Arc::new(vec!["dilf".to_string()]),
            String::new(),
            Some(std::sync::Arc::new(std::sync::RwLock::new(Some(tr)))),
        )
    }

    #[test]
    fn to_series_metadata_tags_translated() {
        let mapper = mapper_with_translator();
        let mut b = book_with_jpn(1, "Title", None);
        b.tags = Some(vec![
            "parody:original".to_string(),      // 黑名单 → 丢弃
            "parody:to love ru".to_string(),    // 未命中 → 原名
            "female:anal".to_string(),          // 命中 → 肛门（无前缀）
            "character:hatsune miku".to_string(), // 命中 → 初音ミク（character 前缀）
            "male:dilf".to_string(),            // male-only 命中 → 大叔
            "male:catgirl".to_string(),         // 非 male-only → 丢弃
            "other:extraneous ads".to_string(), // 黑名单 → 丢弃
            "tag:gore".to_string(),             // 未命中 → 原名
            "language:chinese".to_string(),     // 不进 tags
        ]);
        let meta = mapper.to_series_metadata(&b, None, None);
        assert_eq!(
            meta.metadata.tags,
            vec![
                "parody:to love ru",
                "肛门",
                "character:初音ミク",
                "大叔",
                "gore",
            ]
        );
    }

    #[test]
    fn tag_translation_meta_fresh_checks() {
        let dir = std::env::temp_dir().join(format!("ehentai_meta_test_{}", std::process::id()));
        let _ = std::fs::create_dir_all(&dir);
        let meta = dir.join("db_meta.json");
        let hour = std::time::Duration::from_secs(3600);
        // 缺失 → 需要更新
        assert!(!tag_translation_meta_fresh(&meta, hour));
        // 1 小时前 → fresh（间隔 24h）
        let t = (chrono::Utc::now() - chrono::Duration::hours(1)).to_rfc3339();
        std::fs::write(&meta, format!(r#"{{"last_checked":"{t}"}}"#)).unwrap();
        assert!(tag_translation_meta_fresh(&meta, hour * 24));
        // 48 小时前 → 过期
        let t2 = (chrono::Utc::now() - chrono::Duration::hours(48)).to_rfc3339();
        std::fs::write(&meta, format!(r#"{{"last_checked":"{t2}"}}"#)).unwrap();
        assert!(!tag_translation_meta_fresh(&meta, hour * 24));
        // 坏格式 → 需要更新
        std::fs::write(&meta, "{bad").unwrap();
        assert!(!tag_translation_meta_fresh(&meta, hour * 24));
        // 缺 last_checked 字段 → 需要更新
        std::fs::write(&meta, r#"{"foo":1}"#).unwrap();
        assert!(!tag_translation_meta_fresh(&meta, hour * 24));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn tag_translation_db_paths_defaults() {
        let work = std::path::Path::new("/tmp/work");
        assert_eq!(
            tag_translation_db_path(Some(work)),
            std::path::PathBuf::from("/tmp/work/ehentai/db.text.json")
        );
        assert_eq!(
            tag_translation_meta_path(Some(work)),
            std::path::PathBuf::from("/tmp/work/ehentai/db_meta.json")
        );
        assert_eq!(
            DEFAULT_TAG_TRANSLATION_URL,
            "https://github.com/EhTagTranslation/Database/releases/latest/download/db.text.json"
        );
    }

    #[test]
    fn to_series_metadata_tags_untranslated_when_disabled() {
        // 无 translator（默认）→ 全部原名
        let mapper = EHentaiMetadataMapper::with_options(
            crate::config::SeriesMetadataConfig {
                tags: true,
                ..Default::default()
            },
            vec![AuthorRole::Writer],
            vec![AuthorRole::Penciller],
            vec!["en".to_string(), "ja".to_string()],
            "jpn".to_string(),
            Vec::new(),
            std::sync::Arc::new(vec!["dilf".to_string()]),
            String::new(),
            None,
        );
        let mut b = book_with_jpn(1, "Title", None);
        b.tags = Some(vec![
            "female:anal".to_string(),
            "male:dilf".to_string(),
        ]);
        let meta = mapper.to_series_metadata(&b, None, None);
        assert_eq!(meta.metadata.tags, vec!["anal", "dilf"]);
    }

    fn book(gid: i32, token: &str, title: &str, rating: Option<f64>) -> EHentaiBook {
        EHentaiBook {
            gid,
            token: token.to_string(),
            title: title.to_string(),
            ..Default::default()
        }
        .into_with_rating(rating)
    }

    fn book_with_jpn(gid: i32, title: &str, title_jpn: Option<&str>) -> EHentaiBook {
        EHentaiBook {
            gid,
            token: "t".to_string(),
            title: title.to_string(),
            title_jpn: title_jpn.map(|s| s.to_string()),
            ..Default::default()
        }
    }

    trait IntoWithRating {
        fn into_with_rating(self, rating: Option<f64>) -> EHentaiBook;
    }
    impl IntoWithRating for EHentaiBook {
        fn into_with_rating(mut self, rating: Option<f64>) -> EHentaiBook {
            self.rating = rating;
            self
        }
    }

    #[test]
    fn parse_title_removes_trailing_and_leading() {
        // (Convention)[Artist/Circle] Title [Language] [Tags] -> Title
        let p = EHentaiParser::parse_title("(COMIC1)[Circle X] My Title [English] [Digital]");
        assert_eq!(p.match_title, "My Title");
        assert_eq!(p.best_match, "My Title");
    }

    #[test]
    fn parse_title_takes_rightmost_after_pipe() {
        let p = EHentaiParser::parse_title("Title1 | Title2 | Title3");
        assert_eq!(p.best_match, "Title3");
        assert_eq!(p.match_title, "Title1 | Title2 | Title3");
    }

    #[test]
    fn parse_title_platform_prefix_reverts() {
        // 平台前缀（画师图包）→ 回退到 withoutTrailing 原文
        let p = EHentaiParser::parse_title("[pixiv] 2024.08.24 [Artist]");
        assert_eq!(p.match_title, "[pixiv] 2024.08.24");
    }

    #[test]
    fn parse_title_date_only_reverts() {
        // 去前缀后为纯日期 → 回退到 withoutTrailing 原文
        let p = EHentaiParser::parse_title("[Circle] 2024.08.24");
        assert_eq!(p.match_title, "[Circle] 2024.08.24");
    }

    #[test]
    fn get_search_queries_variants() {
        let qs = EHentaiParser::get_search_queries("[C] Series (Vol. 1) [en]");
        // rawTitle / bestMatch（去前后缀 + 残留括号清理）
        assert_eq!(qs[0], "[C] Series (Vol. 1) [en]");
        assert_eq!(qs[1], "Series");
    }

    #[test]
    fn get_search_queries_dedup_and_trim() {
        let qs = EHentaiParser::get_search_queries(" Series ");
        assert_eq!(qs.len(), 1);
        assert_eq!(qs[0], "Series");
    }

    #[test]
    fn extract_gid_variants() {
        assert_eq!(
            extract_gid_from_title("[Poki no Ie (Pochikin)] Aisareru Shikaku [1234567]").as_deref(),
            Some("1234567")
        );
        assert_eq!(extract_gid_from_title("Title (7654321)").as_deref(), Some("7654321"));
        assert_eq!(extract_gid_from_title("Title 1234567").as_deref(), Some("1234567"));
        assert_eq!(extract_gid_from_title("[Circle] Title [Vol.1]").as_deref(), None);
        assert_eq!(extract_gid_from_title("Title [1]").as_deref(), None);
        assert_eq!(extract_gid_from_title("Vol. 3").as_deref(), None);
        assert_eq!(extract_gid_from_title("Title [123]").as_deref(), None);
    }

    #[test]
    fn parse_gid_ok_and_invalid() {
        let ok = EHentaiParser::parse_gid(&ProviderSeriesId("12345;abcdef".to_string())).unwrap();
        assert_eq!(ok, (12345, "abcdef".to_string()));
        assert!(EHentaiParser::parse_gid(&ProviderSeriesId("invalid".to_string())).is_err());
        assert!(EHentaiParser::parse_gid(&ProviderSeriesId("abc;def".to_string())).is_err());
    }

    #[test]
    fn html_unescape_all_entities() {
        assert_eq!(
            html_unescape("a&amp;b&quot;c&#039;d&#39;e&lt;f&gt;g"),
            "a&b\"c'd'e<f>g"
        );
        // 数字实体（增强）；&amp;#123; 不得二次解码
        assert_eq!(
            html_unescape("&#12354;&#x1F600;&amp;#123;"),
            "\u{3042}\u{1F600}&#123;"
        );
        // 无效数字实体保持原样
        assert_eq!(html_unescape("&#xZZ;"), "&#xZZ;");
    }

    #[test]
    fn rating_rounds() {
        let json = r#"{"gid":1,"token":"t","title":"X","rating":"4.68"}"#;
        let b: EHentaiBook = serde_json::from_str(json).unwrap();
        assert_eq!(b.rating, Some(5.0));
    }

    #[test]
    fn title_jpn_unescaped() {
        let json = r#"{"gid":1,"token":"t","title":"X","title_jpn":"A&amp;B"}"#;
        let b: EHentaiBook = serde_json::from_str(json).unwrap();
        assert_eq!(b.title_jpn.as_deref(), Some("A&B"));
    }

    #[test]
    fn map_age_rating_extreme_tags() {
        let tags = vec!["parody:xxx".to_string(), "guro".to_string()];
        assert_eq!(
            EHentaiTagMapper::map_age_rating(Some("non-h"), &tags),
            Some(18)
        );
        assert_eq!(
            EHentaiTagMapper::map_age_rating(Some("non-h"), &[]),
            Some(15)
        );
        assert_eq!(
            EHentaiTagMapper::map_age_rating(Some("doujinshi"), &[]),
            Some(18)
        );
        assert_eq!(EHentaiTagMapper::map_age_rating(Some("unknown"), &[]), None);
    }

    #[test]
    fn map_language_bcp47() {
        let tags = vec!["language:chinese".to_string(), "male:xxx".to_string()];
        assert_eq!(EHentaiTagMapper::map_language(&tags).as_deref(), Some("zh"));
        assert_eq!(EHentaiTagMapper::map_language(&[]), None);
    }

    #[test]
    fn apply_language_preference_keeps_book_when_translated_precedes_language() {
        // language:translated 排在 language:chinese 前：不能因首个 language:* 无法映射而丢弃
        let mapper = EHentaiMetadataMapper::new(
            crate::config::SeriesMetadataConfig::default(),
            vec![AuthorRole::Writer],
            vec![AuthorRole::Penciller],
            vec!["zh".to_string(), "ja".to_string(), "en".to_string()],
        );
        let book = EHentaiBook {
            gid: 4190146,
            token: "t".to_string(),
            title: "T".to_string(),
            title_jpn: Some("J".to_string()),
            category: Some("Doujinshi".to_string()),
            tags: Some(vec![
                "female:big breasts".to_string(),
                "language:translated".to_string(),
                "language:chinese".to_string(),
            ]),
            ..Default::default()
        };
        let out = mapper.apply_language_preference(&[book]);
        assert_eq!(out.len(), 1);
        assert_eq!(out[0].gid, 4190146);
    }
    #[test]
    fn apply_language_preference_sorts_by_preference_then_rating() {
        let mapper = EHentaiMetadataMapper::new(
            crate::config::SeriesMetadataConfig::default(),
            vec![AuthorRole::Writer],
            vec![AuthorRole::Penciller],
            vec!["en".to_string(), "ja".to_string()],
        );
        let mut b_en1 = book(1, "a", "T1", Some(4.0));
        b_en1.tags = Some(vec!["language:english".to_string()]);
        let mut b_ja1 = book(2, "b", "T2", Some(5.0));
        b_ja1.tags = Some(vec!["language:japanese".to_string()]);
        let b_nolang = book(3, "c", "T3", Some(3.0));
        let mut b_zh = book(4, "d", "T4", Some(5.0));
        b_zh.tags = Some(vec!["language:chinese".to_string()]);

        let sorted = mapper.apply_language_preference(&[b_zh, b_nolang, b_ja1, b_en1]);
        let gids: Vec<i32> = sorted.iter().map(|b| b.gid).collect();
        // en 组 → ja 组 → 无语言组；zh（非 preferred）被丢弃
        assert_eq!(gids, vec![1, 2, 3]);
    }

    #[test]
    fn apply_language_preference_invalid_preferences_returns_all() {
        let mapper = EHentaiMetadataMapper::new(
            crate::config::SeriesMetadataConfig::default(),
            vec![AuthorRole::Writer],
            vec![AuthorRole::Penciller],
            vec!["xx".to_string()],
        );
        let b1 = book(1, "a", "T1", None);
        let b2 = book(2, "b", "T2", None);
        assert_eq!(
            mapper
                .apply_language_preference(&[b1.clone(), b2.clone()])
                .len(),
            2
        );
    }

    /// 非 non-h（SFW）内容 → genres=["hentai"]；含 non-h / misc:non-h → 空
    #[test]
    fn to_series_metadata_genres_hentai_unless_noh() {
        let mapper = EHentaiMetadataMapper::new(
            crate::config::SeriesMetadataConfig::default(),
            vec![AuthorRole::Writer],
            vec![AuthorRole::Penciller],
            vec!["en".to_string()],
        );
        // 无 no-h → hentai
        let mut b = book(1, "t", "Title", None);
        b.tags = Some(vec!["artist:ArtistX".to_string()]);
        let meta = mapper.to_series_metadata(&b, None, None);
        assert_eq!(meta.metadata.genres, vec!["hentai"]);
        // non-h → 空
        let mut b2 = book(2, "t", "SFW Title", None);
        b2.tags = Some(vec!["non-h".to_string()]);
        let meta2 = mapper.to_series_metadata(&b2, None, None);
        assert_eq!(meta2.metadata.genres, Vec::<String>::new());
        // misc:non-h → 空
        let mut b3 = book(3, "t", "SFW Title 2", None);
        b3.tags = Some(vec!["misc:non-h".to_string()]);
        let meta3 = mapper.to_series_metadata(&b3, None, None);
        assert_eq!(meta3.metadata.genres, Vec::<String>::new());
    }

    #[test]
    fn to_series_metadata_maps_fields() {
        let mapper = EHentaiMetadataMapper::new(
            crate::config::SeriesMetadataConfig::default(),
            vec![AuthorRole::Writer],
            vec![AuthorRole::Penciller],
            vec!["en".to_string()],
        );
        let mut b = book(123, "token", "Title [en]", Some(4.5));
        b.tags = Some(vec![
            "language:english".to_string(),
            "artist:ArtistX".to_string(),
            "parody:Original".to_string(),
        ]);
        b.posted = Some(1700000000);
        let meta = mapper.to_series_metadata(&b, None, None);
        assert_eq!(meta.id.0, "123;token");
        assert_eq!(meta.metadata.title.as_ref().unwrap().name, "Title");
        assert_eq!(
            meta.metadata.title.as_ref().unwrap().r#type,
            Some(TitleType::Localized)
        );
        assert_eq!(
            meta.metadata.title.as_ref().unwrap().language.as_deref(),
            Some("en")
        );
        assert_eq!(meta.metadata.language.as_deref(), Some("en"));
        assert_eq!(meta.metadata.status, Some(SeriesStatus::Ended));
        // 直构 4.5 未走反序列化 round（round 由 rating_rounds 测试覆盖序列化路径）
        assert_eq!(meta.metadata.score, Some(4.5));
        // 标题无 [circle] 前缀 → 标题驱动作者为空 → gdata 标签回退：Writer←artist、Penciller←artist
        assert_eq!(meta.metadata.authors.len(), 2);
        assert!(meta
            .metadata
            .authors
            .iter()
            .any(|a| a.role == AuthorRole::Writer && a.name == "ArtistX"));
        assert!(meta
            .metadata
            .authors
            .iter()
            .any(|a| a.role == AuthorRole::Penciller && a.name == "ArtistX"));
        // 标签：language/artist 不进 tags；parody:Original 小写后 = original（原创标记）→ 按规则丢弃
        assert_eq!(meta.metadata.tags, Vec::<String>::new());
        // 无 webtoon 标签 → 右到左
        assert_eq!(
            meta.metadata.reading_direction,
            Some(ReadingDirection::RightToLeft)
        );
        assert_eq!(meta.metadata.links.len(), 1);
        assert_eq!(
            meta.metadata.links[0].url,
            "https://e-hentai.org/g/123/token"
        );
        assert!(meta.metadata.release_date.is_some());
        assert_eq!(meta.metadata.release_date.unwrap().year, Some(2023));
        assert!(meta.books.is_empty());
    }

    #[test]
    fn to_series_metadata_japanese_title_native() {
        let mapper = EHentaiMetadataMapper::new(
            crate::config::SeriesMetadataConfig::default(),
            vec![AuthorRole::Writer],
            vec![AuthorRole::Penciller],
            vec!["ja".to_string()],
        );
        let mut b = book(1, "t", "English Title", None);
        b.tags = Some(vec!["language:japanese".to_string()]);
        b.title_jpn = Some("日本語タイトル".to_string());
        let meta = mapper.to_series_metadata(&b, None, None);
        let title = meta.metadata.title.unwrap();
        assert_eq!(title.name, "日本語タイトル");
        assert_eq!(title.r#type, Some(TitleType::Native));
        assert_eq!(title.language.as_deref(), Some("ja"));
    }

    #[test]
    fn titles_cleared_when_title_disabled() {
        let mut cfg = crate::config::SeriesMetadataConfig::default();
        cfg.title = false;
        let mapper = EHentaiMetadataMapper::new(
            cfg,
            vec![AuthorRole::Writer],
            vec![AuthorRole::Penciller],
            vec![],
        );
        let meta = mapper.to_series_metadata(&book(1, "t", "Title", None), None, None);
        let t = meta.metadata.titles.first().unwrap();
        assert_eq!(t.name, "Title");
        assert_eq!(t.r#type, None);
        assert_eq!(t.language, None);
        assert!(meta.metadata.title.is_none());
    }

    #[test]
    fn parse_title_removes_remnant_brackets() {
        // 增强：非回退路径清除残留括号
        let p = EHentaiParser::parse_title("[C] Series (Vol. 1) [en]");
        assert_eq!(p.best_match, "Series");
        let p = EHentaiParser::parse_title("Title（番外）End");
        assert_eq!(p.best_match, "Title End");
        // 多括号与连续空白压缩
        let p = EHentaiParser::parse_title("A [x] (y) B");
        assert_eq!(p.best_match, "A B");
    }

    #[test]
    fn parse_title_revert_keeps_brackets() {
        // 平台前缀/纯日期回退路径不做残留括号清理
        let p = EHentaiParser::parse_title("[pixiv] 2024.08.24 [Artist]");
        assert_eq!(p.best_match, "[pixiv] 2024.08.24");
        let p = EHentaiParser::parse_title("[Circle] 2024.08.24");
        assert_eq!(p.best_match, "[Circle] 2024.08.24");
    }

    #[test]
    fn parse_authors_from_title_circle_artist() {
        let (w, p) =
            EHentaiParser::parse_authors_from_title("(COMIC1)[Circle X (Artist Y)] My Title [en]");
        assert_eq!(w.as_deref(), Some("Circle X"));
        assert_eq!(p.as_deref(), Some("Artist Y"));
    }

    #[test]
    fn parse_authors_from_title_plain_circle() {
        let (w, p) = EHentaiParser::parse_authors_from_title("[Circle X] My Title");
        assert_eq!(w.as_deref(), Some("Circle X"));
        assert_eq!(p.as_deref(), Some("Circle X"));
    }

    #[test]
    fn parse_authors_from_title_multi_split() {
        // 多人保留增强（增强）：penciller 保留全部多人，不并入 writer；
        // 具体拆分由调用方 clean_author_names 完成
        let (w, p) = EHentaiParser::parse_authors_from_title("[A、B, C (X、Y)] My Title");
        assert_eq!(w.as_deref(), Some("A、B, C"));
        assert_eq!(p.as_deref(), Some("X、Y"));
    }

    #[test]
    fn parse_authors_from_title_none() {
        assert_eq!(
            EHentaiParser::parse_authors_from_title("My Title [en]"),
            (None, None)
        );
        assert_eq!(
            EHentaiParser::parse_authors_from_title("No Brackets Title"),
            (None, None)
        );
    }

    #[test]
    fn render_title_template_basic() {
        let mut vars = std::collections::HashMap::new();
        vars.insert(
            "title".to_string(),
            "愛される資格は過去に落としてきました".to_string(),
        );
        vars.insert("translator".to_string(), "漢化組A".to_string());
        vars.insert("writer".to_string(), "ぽきの家".to_string());
        assert_eq!(
            EHentaiParser::render_title_template("{{title}}", &vars),
            "愛される資格は過去に落としてきました"
        );
        // if translator → [translator] 后缀
        assert_eq!(
            EHentaiParser::render_title_template(
                "{{title}}{% if translator %} [{{ translator }}]{% endif %}",
                &vars
            ),
            "愛される資格は過去に落としてきました [漢化組A]"
        );
        // translator 为空 → if 块消失
        let mut no_tr = vars.clone();
        no_tr.insert("translator".to_string(), String::new());
        assert_eq!(
            EHentaiParser::render_title_template(
                "{{title}}{% if translator %} [{{ translator }}]{% endif %}",
                &no_tr
            ),
            "愛される資格は過去に落としてきました"
        );
        // 未定义变量 → 空串
        assert_eq!(
            EHentaiParser::render_title_template("[{{ writer }}]{{ title }}", &vars),
            "[ぽきの家]愛される資格は過去に落としてきました"
        );
        // if 块内再含 {{var}}
        assert_eq!(
            EHentaiParser::render_title_template(
                "{% if writer %}{{ writer }}: {% endif %}{{title}}",
                &vars
            ),
            "ぽきの家: 愛される資格は過去に落としてきました"
        );
        // 空模板
        assert_eq!(EHentaiParser::render_title_template("", &vars), "");
    }

    #[test]
    fn to_series_metadata_title_template_applied() {
        let book = EHentaiBook {
            gid: 4196879,
            token: "1e1a4d157c".to_string(),
            title: "[Poki no Ie (Pochikin)] Aisareru Shikaku [Chinese]".to_string(),
            title_jpn: Some(
                "[ぽきの家 (ちんぽき)] 愛される資格は過去に落としてきました [中国翻訳]".to_string(),
            ),
            category: Some("Manga".to_string()),
            thumb: None,
            uploader: None,
            posted: Some(1_600_000_000),
            file_count: None,
            file_size: None,
            expunged: None,
            rating: Some(4.5),
            torrent_count: None,
            torrents: None,
            tags: Some(vec![
                "language:chinese".to_string(),
                "artist:chinpoki".to_string(),
            ]),
            parent_gid: None,
            parent_key: None,
            current_gid: None,
            current_key: None,
            first_gid: None,
            first_key: None,
            error: None,
        };
        let cfg = crate::config::SeriesMetadataConfig {
            title: true,
            ..Default::default()
        };
        let mapper = EHentaiMetadataMapper::with_options(
            cfg,
            vec![AuthorRole::Writer],
            vec![AuthorRole::Penciller],
            vec!["en".to_string(), "ja".to_string()],
            "jpn".to_string(),
            Vec::new(),
            std::sync::Arc::new(vec![]),
            "{{title}}{% if translator %} [{{ translator }}]{% endif %}".to_string(),
            None,
        );
        // 调试：直接验证 find_translator 对 title_jpn 的行为
        let tr_debug = EHentaiParser::find_translator(
            "[ぽきの家 (ちんぽき)] 愛される資格は過去に落としてきました [中国翻訳]",
            &DEFAULT_TRANSLATOR_KEYWORDS
                .iter()
                .map(|s| s.to_string())
                .collect::<Vec<_>>(),
        );
        assert_eq!(
            tr_debug.as_deref(),
            Some("中国翻訳"),
            "debug translator: {tr_debug:?}"
        );
        let meta = mapper.to_series_metadata(&book, None, None);
        let t = meta.metadata.title.expect("title");
        // 内置关键词含"翻訳"子串 → [中国翻訳] 识别为汉化组 → 模板追加 [中国翻訳]
        assert_eq!(t.name, "愛される資格は過去に落としてきました [中国翻訳]");
        // 自定义 translatorKeywords 含"中国翻訳" → [中国翻訳] 后缀
        let mapper2 = EHentaiMetadataMapper::with_options(
            crate::config::SeriesMetadataConfig::default(),
            vec![AuthorRole::Writer],
            vec![AuthorRole::Penciller],
            vec!["en".to_string(), "ja".to_string()],
            "jpn".to_string(),
            vec!["中国翻訳".to_string()],
            std::sync::Arc::new(vec![]),
            "{{title}}{% if translator %} [{{ translator }}]{% endif %}".to_string(),
            None,
        );
        let meta2 = mapper2.to_series_metadata(&book, None, None);
        let t2 = meta2.metadata.title.expect("title");
        // 自定义关键词同样命中
        assert_eq!(t2.name, "愛される資格は過去に落としてきました [中国翻訳]");
        // 无 translator 时 if 块消失（英文 title 无汉化组特征）
        let book_en = EHentaiBook {
            gid: 4002642,
            token: "6aabf5a796".to_string(),
            title: "[Poki no Ie (Pochikin)] Aisareru Shikaku".to_string(),
            title_jpn: Some(
                "[ぽきの家 (ちんぽき)] 愛される資格は過去に落としてきました".to_string(),
            ),
            category: Some("Manga".to_string()),
            thumb: None,
            uploader: None,
            posted: Some(1_600_000_000),
            file_count: None,
            file_size: None,
            expunged: None,
            rating: Some(4.5),
            torrent_count: None,
            torrents: None,
            tags: Some(vec!["artist:chinpoki".to_string()]),
            parent_gid: None,
            parent_key: None,
            current_gid: None,
            current_key: None,
            first_gid: None,
            first_key: None,
            error: None,
        };
        let meta_en = mapper.to_series_metadata(&book_en, None, None);
        let t_en = meta_en.metadata.title.expect("title");
        assert_eq!(t_en.name, "愛される資格は過去に落としてきました");
    }

    #[test]
    fn find_translator_detects() {
        let keywords: Vec<String> = DEFAULT_TRANSLATOR_KEYWORDS
            .iter()
            .map(|s| s.to_string())
            .collect();
        assert_eq!(
            EHentaiParser::find_translator("[Circle] Title [汉化组]", &keywords).as_deref(),
            Some("汉化组")
        );
        assert_eq!(
            EHentaiParser::find_translator("[Circle] Title 汉化组X", &keywords).as_deref(),
            Some("汉化组X")
        );
        assert_eq!(
            EHentaiParser::find_translator("[Circle] Title [en]", &keywords),
            None
        );
        // 自定义关键词
        let custom: Vec<String> = vec!["汉化组A".to_string()];
        assert_eq!(
            EHentaiParser::find_translator("[Circle] Title [汉化组A]", &custom).as_deref(),
            Some("汉化组A")
        );
        assert_eq!(
            EHentaiParser::find_translator("[Circle] Title [汉化组]", &custom),
            None
        );
        assert_eq!(
            EHentaiParser::find_translator("[Circle] Title", &Vec::<String>::new()),
            None
        );
    }

    #[test]
    fn to_series_metadata_title_driven_authors_and_translator() {
        let mapper = EHentaiMetadataMapper::new(
            crate::config::SeriesMetadataConfig::default(),
            vec![AuthorRole::Writer],
            vec![AuthorRole::Penciller],
            vec![],
        );
        let mut b = book(9, "t", "[Circle X (Artist Y)] My Title [汉化组] [en]", None);
        b.tags = Some(vec![
            "artist:ArtistY".to_string(),
            "group:CircleX".to_string(),
        ]);
        let meta = mapper.to_series_metadata(&b, None, None);
        // 标题驱动：Circle X → Writer；Artist Y → Penciller；汉化组 → Translator
        assert!(meta
            .metadata
            .authors
            .iter()
            .any(|a| a.role == AuthorRole::Writer && a.name == "Circle X"));
        assert!(meta
            .metadata
            .authors
            .iter()
            .any(|a| a.role == AuthorRole::Penciller && a.name == "Artist Y"));
        assert!(meta
            .metadata
            .authors
            .iter()
            .any(|a| a.role == AuthorRole::Translator && a.name == "汉化组"));
        // 标题驱动已给出 Writer/Penciller → 不回退 tags（artist/group 不进作者）
        assert_eq!(meta.metadata.authors.len(), 3);
        // 标题含 [en] → parse_title 后 best_match 清理
        assert_eq!(meta.metadata.title.as_ref().unwrap().name, "My Title");
    }

    #[test]
    fn to_series_metadata_authors_prefer_title_jpn() {
        // PREFER_JAPANESE_TITLE 对齐：英文 title 是罗马音/翻译版
        // （[Poki no Ie (Pochikin, Chinpoki)]），title_jpn 才是规范 [ぽきの家 (ちんぽき)]。
        let mapper = EHentaiMetadataMapper::new(
            crate::config::SeriesMetadataConfig::default(),
            vec![AuthorRole::Writer],
            vec![AuthorRole::Penciller],
            vec![],
        );
        let mut b = book(
            4002642,
            "6aabf5a796",
            "[Poki no Ie (Pochikin, Chinpoki)] Aisareru Shikaku wa Kako ni Otoshite Kimashita",
            None,
        );
        b.title_jpn =
            Some("[ぽきの家 (ちんぽき)] 愛される資格は過去に落としてきました".to_string());
        b.tags = Some(vec!["artist:chinpoki".to_string()]);
        let meta = mapper.to_series_metadata(&b, None, None);
        // 标题驱动优先 title_jpn：writer=ぽきの家、penciller=ちんぽき（非英文罗马音版）
        assert!(meta
            .metadata
            .authors
            .iter()
            .any(|a| a.role == AuthorRole::Writer && a.name == "ぽきの家"));
        assert!(meta
            .metadata
            .authors
            .iter()
            .any(|a| a.role == AuthorRole::Penciller && a.name == "ちんぽき"));
        assert!(
            !meta.metadata.authors.iter().any(|a| a.name.contains("Poki")
                || a.name.contains("Pochikin")
                || a.name.contains("Chinpoki"))
        );
        assert_eq!(meta.metadata.authors.len(), 2);
    }

    #[test]
    fn to_series_metadata_authors_multi_kept_in_penciller() {
        // 多人保留增强：英文 title `[Poki no Ie (Pochikin, Chinpoki)]`（无 title_jpn）
        // → writer=Poki no Ie、penciller=[Pochikin, Chinpoki] 两条（不并入 writer）
        let mapper = EHentaiMetadataMapper::new(
            crate::config::SeriesMetadataConfig::default(),
            vec![AuthorRole::Writer],
            vec![AuthorRole::Penciller],
            vec![],
        );
        let b = book(
            4002642,
            "6aabf5a796",
            "[Poki no Ie (Pochikin, Chinpoki)] Aisareru Shikaku wa Kako ni Otoshite Kimashita",
            None,
        );
        let meta = mapper.to_series_metadata(&b, None, None);
        let writers: Vec<&str> = meta
            .metadata
            .authors
            .iter()
            .filter(|a| a.role == AuthorRole::Writer)
            .map(|a| a.name.as_str())
            .collect();
        let pencillers: Vec<&str> = meta
            .metadata
            .authors
            .iter()
            .filter(|a| a.role == AuthorRole::Penciller)
            .map(|a| a.name.as_str())
            .collect();
        assert_eq!(writers, vec!["Poki no Ie"]);
        assert_eq!(pencillers, vec!["Pochikin", "Chinpoki"]);
    }

    #[test]
    fn to_series_metadata_authors_fallback_title_when_no_title_jpn() {
        // 无 title_jpn → 回退英文 title 解析（原行为不变）
        let mapper = EHentaiMetadataMapper::new(
            crate::config::SeriesMetadataConfig::default(),
            vec![AuthorRole::Writer],
            vec![AuthorRole::Penciller],
            vec![],
        );
        let b = book(7, "t", "[Circle X (Artist Y)] My Title [en]", None);
        let meta = mapper.to_series_metadata(&b, None, None);
        assert!(meta
            .metadata
            .authors
            .iter()
            .any(|a| a.role == AuthorRole::Writer && a.name == "Circle X"));
        assert!(meta
            .metadata
            .authors
            .iter()
            .any(|a| a.role == AuthorRole::Penciller && a.name == "Artist Y"));
    }

    #[test]
    fn to_series_metadata_title_jpn_native_without_ja_language() {
        // 增强（超 Kotlin）：title_jpn 非空即 Native，不要求 language:japanese
        let mapper = EHentaiMetadataMapper::new(
            crate::config::SeriesMetadataConfig::default(),
            vec![AuthorRole::Writer],
            vec![AuthorRole::Penciller],
            vec!["en".to_string()],
        );
        let mut b = book(2, "t", "English Title", None);
        b.tags = Some(vec!["language:english".to_string()]);
        b.title_jpn = Some("日本語タイトル".to_string());
        let meta = mapper.to_series_metadata(&b, None, None);
        let title = meta.metadata.title.unwrap();
        assert_eq!(title.name, "日本語タイトル");
        assert_eq!(title.r#type, Some(TitleType::Native));
        assert_eq!(title.language.as_deref(), Some("ja"));
        assert_eq!(meta.metadata.language.as_deref(), Some("en"));
    }

    #[test]
    fn to_series_metadata_reading_direction_webtoon() {
        // EH webtoon 标签为 `other:webtoon` → 不设置阅读方向（None）；
        // 无 webtoon → RightToLeft
        let mapper = EHentaiMetadataMapper::new(
            crate::config::SeriesMetadataConfig::default(),
            vec![AuthorRole::Writer],
            vec![AuthorRole::Penciller],
            vec![],
        );
        let mut b = book(11, "t", "Title", None);
        b.tags = Some(vec![
            "other:webtoon".to_string(),
            "language:english".to_string(),
        ]);
        let meta = mapper.to_series_metadata(&b, None, None);
        // 含 webtoon → Webtoon 阅读方向（不是 None 也不是 RightToLeft）
        assert_eq!(
            meta.metadata.reading_direction,
            Some(ReadingDirection::Webtoon)
        );
        // 标签白名单仍保留 webtoon（other 命名空间、不在黑名单）
        assert!(meta.metadata.tags.iter().any(|t| t == "webtoon"));
    }

    #[test]
    fn to_series_metadata_reading_direction_webtoon_independent_of_tags_flag() {
        // seriesMetadata.tags=false 时 webtoon 检测仍生效（raw_tags 独立检查）
        let mut cfg = crate::config::SeriesMetadataConfig::default();
        cfg.tags = false;
        cfg.reading_direction = true;
        let mapper = EHentaiMetadataMapper::new(
            cfg,
            vec![AuthorRole::Writer],
            vec![AuthorRole::Penciller],
            vec![],
        );
        let mut b = book(12, "t", "Title", None);
        b.tags = Some(vec!["other:webtoon".to_string()]);
        let meta = mapper.to_series_metadata(&b, None, None);
        assert_eq!(
            meta.metadata.reading_direction,
            Some(ReadingDirection::Webtoon)
        );
        assert!(meta.metadata.tags.is_empty());
    }

    #[test]
    fn to_series_metadata_tags_hentai_assistant_filter() {
        let mapper = EHentaiMetadataMapper::new(
            crate::config::SeriesMetadataConfig::default(),
            vec![AuthorRole::Writer],
            vec![AuthorRole::Penciller],
            vec![],
        );
        let mut b = book(3, "t", "Title", None);
        b.tags = Some(vec![
            "language:english".to_string(),
            "artist:X".to_string(),
            "group:G".to_string(),
            "parody:Original".to_string(),
            "parody:original".to_string(),
            "parody:various".to_string(),
            "character:Ch".to_string(),
            "female:f1".to_string(),
            "male:catboy".to_string(),
            "male:netorare".to_string(),
            "other:extraneous ads".to_string(),
            "other:webtoon".to_string(),
            "tag:tt".to_string(),
        ]);
        let meta = mapper.to_series_metadata(&b, None, None);
        // parody:original / parody:various 均按规则丢弃（original/various 为原创/合集标记）
        assert_eq!(
            meta.metadata.tags,
            vec![
                "character:Ch".to_string(),
                "f1".to_string(),
                "catboy".to_string(),
                "webtoon".to_string(),
                "tt".to_string(),
            ]
        );
        // 含 webtoon 标签 → reading_direction = Webtoon（不强制右到左）
        assert_eq!(
            meta.metadata.reading_direction,
            Some(ReadingDirection::Webtoon)
        );
    }

    /// Live 复现 search_series 全流程：queries → search_by_title → apply_language_preference。
    #[tokio::test]
    #[ignore]
    async fn dbg_search_live() {
        let client = EHentaiClient::new(reqwest::Client::new(), reqwest::Client::new());
        let queries = EHentaiParser::get_search_queries("愛される資格は過去に落としてきました");
        println!("queries: {:?}", queries);
        let mut raw: Vec<EHentaiBook> = Vec::new();
        for q in &queries {
            let resp = client.search_by_title(q).await.expect("search failed");
            println!("query {:?} -> gmetadata {}", q, resp.gmetadata.len());
            raw.extend(resp.gmetadata);
        }
        println!("raw total: {}", raw.len());
        for b in &raw {
            let langs: Vec<&str> = b
                .tags
                .as_ref()
                .map(|t| {
                    t.iter()
                        .filter(|x| x.starts_with("language:"))
                        .map(|x| x.as_str())
                        .collect()
                })
                .unwrap_or_default();
            println!(
                " gid={} token={} error={:?} title={:?} langs={:?}",
                b.gid, b.token, b.error, b.title, langs
            );
        }
        let mapper = EHentaiMetadataMapper::new(
            crate::config::SeriesMetadataConfig::default(),
            vec![AuthorRole::Writer],
            vec![AuthorRole::Penciller],
            vec!["zh".to_string(), "ja".to_string(), "en".to_string()],
        );
        let processed = mapper.apply_language_preference(&raw);
        let order: Vec<i32> = processed.iter().map(|b| b.gid).collect();
        println!("apply_language_preference order: {:?}", order);
    }

    #[test]
    fn forced_language_from_search_detects() {
        assert_eq!(
            EHentaiMetadataMapper::forced_language_from_search("xxx 中国翻訳 yyy"),
            Some("zh")
        );
        assert_eq!(
            EHentaiMetadataMapper::forced_language_from_search("xxx 中国翻译 yyy"),
            Some("zh")
        );
        assert_eq!(
            EHentaiMetadataMapper::forced_language_from_search("xxx 英訳 yyy"),
            Some("en")
        );
        assert_eq!(
            EHentaiMetadataMapper::forced_language_from_search("xxx 英译 yyy"),
            Some("en")
        );
        assert_eq!(
            EHentaiMetadataMapper::forced_language_from_search("normal title"),
            None
        );
    }

    #[test]
    fn book_matches_forced_language_features() {
        let mapper = EHentaiMetadataMapper::new(
            crate::config::SeriesMetadataConfig::default(),
            vec![],
            vec![],
            vec![],
        );
        // zh：language:chinese / title_jpn 含中国翻訳 / title 含汉化组关键词
        let b = book(1, "t", "Title", None);
        assert!(!mapper.book_matches_forced_language(&b, "zh"));
        let b = EHentaiBook {
            gid: 1,
            token: "t".to_string(),
            title: "Title".to_string(),
            tags: Some(vec!["language:chinese".to_string()]),
            ..Default::default()
        };
        assert!(mapper.book_matches_forced_language(&b, "zh"));
        let b = book_with_jpn(1, "Title", Some("[circle] Title [中国翻訳]"));
        assert!(mapper.book_matches_forced_language(&b, "zh"));
        let b = book_with_jpn(1, "Title [汉化组名]", None);
        assert!(mapper.book_matches_forced_language(&b, "zh"));
        // en：language:english / title_jpn 含英訳 / title 含英文标记
        let e = book(2, "t", "Title [English]", None);
        assert!(mapper.book_matches_forced_language(&e, "en"));
        let e2 = book_with_jpn(3, "Title", Some("[circle] Title [英訳]"));
        assert!(mapper.book_matches_forced_language(&e2, "en"));
        assert!(mapper.book_matches_forced_language(&e2, "en"));
        // 自定义汉化组关键词生效
        let custom = EHentaiMetadataMapper::with_options(
            crate::config::SeriesMetadataConfig::default(),
            vec![],
            vec![],
            vec![],
            "jpn".to_string(),
            vec!["汉化组A".to_string()],
            std::sync::Arc::new(vec![]),
            String::new(),
            None,
        );
        let bz = book_with_jpn(4, "Title [汉化组A]", None);
        assert!(custom.book_matches_forced_language(&bz, "zh"));
        assert!(!custom.book_matches_forced_language(&b, "zh"));
    }

    #[test]
    fn to_series_metadata_title_priority_title_uses_english() {
        // titlePriority="title" → 有 title_jpn 也用英文 title
        let mapper = EHentaiMetadataMapper::with_options(
            crate::config::SeriesMetadataConfig::default(),
            vec![],
            vec![],
            vec![],
            "title".to_string(),
            vec![],
            std::sync::Arc::new(vec![]),
            String::new(),
            None,
        );
        let b = book_with_jpn(1, "English Title", Some("日本語タイトル"));
        let meta = mapper.to_series_metadata(&b, None, None);
        assert_eq!(meta.metadata.titles[0].name, "English Title");
        assert_eq!(meta.metadata.titles[0].r#type, None); // language None → type None
                                                          // 默认 "jpn" → title_jpn 优先 Native
        let mapper2 = EHentaiMetadataMapper::new(
            crate::config::SeriesMetadataConfig::default(),
            vec![],
            vec![],
            vec![],
        );
        let meta2 = mapper2.to_series_metadata(&b, None, None);
        assert_eq!(meta2.metadata.titles[0].name, "日本語タイトル");
        assert_eq!(meta2.metadata.titles[0].r#type, Some(TitleType::Native));
    }

    #[test]
    fn search_result_title_priority_configurable() {
        let mapper = EHentaiMetadataMapper::new(
            crate::config::SeriesMetadataConfig::default(),
            vec![],
            vec![],
            vec![],
        );
        let b = book_with_jpn(1, "English Title", Some("日本語タイトル"));
        let r = mapper.to_series_search_result(&b, "");
        assert_eq!(r.title, "日本語タイトル");
        let mapper_title = EHentaiMetadataMapper::with_options(
            crate::config::SeriesMetadataConfig::default(),
            vec![],
            vec![],
            vec![],
            "title".to_string(),
            vec![],
            std::sync::Arc::new(vec![]),
            String::new(),
            None,
        );
        let r2 = mapper_title.to_series_search_result(&b, "");
        assert_eq!(r2.title, "English Title");
    }

    #[test]
    fn search_result_language_forced_and_tag_fallback() {
        let mapper = EHentaiMetadataMapper::new(
            crate::config::SeriesMetadataConfig::default(),
            vec![],
            vec![],
            vec![],
        );
        // language:japanese 标签 → ja（尊重原始解析；搜索名不强制改写）
        let mut b = book_with_jpn(1, "English Title", Some("日本語タイトル"));
        b.tags = Some(vec!["language:japanese".to_string()]);
        let r = mapper.to_series_search_result(&b, "");
        assert_eq!(r.language.as_deref(), Some("ja"));
        // 搜索名含「中国翻訳」→ 仍尊重结果标签（ja），不强制为 zh
        let r2 = mapper.to_series_search_result(&b, "タイトル 中国翻訳");
        assert_eq!(r2.language.as_deref(), Some("ja"));
        // 搜索名含「英訳」→ 仍尊重结果标签（ja）
        let r3 = mapper.to_series_search_result(&b, "タイトル 英訳");
        assert_eq!(r3.language.as_deref(), Some("ja"));
        // language:chinese 标签 → zh（标签推导）
        let mut b_zh = book_with_jpn(3, "English Title", Some("日本語タイトル"));
        b_zh.tags = Some(vec!["language:chinese".to_string()]);
        let r_zh = mapper.to_series_search_result(&b_zh, "タイトル 英訳");
        assert_eq!(r_zh.language.as_deref(), Some("zh"));
        // 无语言标签 → None（即使搜索名含中国翻訳也不强制）
        let b2 = book_with_jpn(2, "English Title", Some("日本語タイトル"));
        let r4 = mapper.to_series_search_result(&b2, "タイトル 中国翻訳");
        assert_eq!(r4.language, None);
    }

    #[test]
    fn male_only_json_roundtrip() {
        let dir = std::env::temp_dir().join(format!("komf_eh_male_{}", std::process::id()));
        let _ = std::fs::create_dir_all(&dir);
        let path = dir.join("male_only_taglist.json");
        std::fs::write(&path, r#"{"content": ["dilf", "catboy"]}"#).unwrap();
        assert_eq!(
            read_male_only_json(&path),
            Some(vec!["dilf".to_string(), "catboy".to_string()])
        );
        // 空 content → None
        std::fs::write(&path, r#"{"content": []}"#).unwrap();
        assert_eq!(read_male_only_json(&path), None);
        // 缺失文件 → None
        assert_eq!(read_male_only_json(&dir.join("missing.json")), None);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn parse_male_only_html_extracts() {
        let html = r#"
<h2>Fetish Listing</h2>
<ul>
<li><a href="/wiki/dilf" title="dilf">dilf</a>♂ (123)</li>
<li><a href="/wiki/catboy">catboy</a> ‎♂</li>
<li><a href="/wiki/not_male">not_male</a></li>
<li><a href="/wiki/catboy">catboy</a>♂</li>
</ul>"#;
        let tags = parse_male_only_html(html);
        assert_eq!(tags, vec!["dilf".to_string(), "catboy".to_string()]);
        assert_eq!(
            parse_male_only_html("<p>no tags here</p>"),
            Vec::<String>::new()
        );
    }

    #[test]
    fn search_domain_normalization() {
        assert_eq!(ehentai_search_domain("exhentai"), "exhentai.org");
        assert_eq!(ehentai_search_domain("EXHENTAI"), "exhentai.org");
        assert_eq!(ehentai_search_domain("e-hentai"), "e-hentai.org");
        assert_eq!(ehentai_search_domain(""), "e-hentai.org");
        assert_eq!(ehentai_search_domain("e-hentai.org"), "e-hentai.org");
    }

    #[test]
    fn parse_set_cookie_extracts() {
        assert_eq!(
            EHentaiClient::parse_set_cookie("igneous=abc123; path=/; HttpOnly"),
            Some(("igneous".to_string(), "abc123".to_string()))
        );
        assert_eq!(
            EHentaiClient::parse_set_cookie("sk=xyz; path=/"),
            Some(("sk".to_string(), "xyz".to_string()))
        );
        assert_eq!(EHentaiClient::parse_set_cookie("igneous="), None);
        assert_eq!(EHentaiClient::parse_set_cookie(""), None);
    }

    #[tokio::test]
    async fn client_cookies_injected() {
        let client = EHentaiClient::with_options(
            reqwest::Client::new(),
            reqwest::Client::new(),
            "exhentai.org",
            Some("member1"),
            Some("hash2"),
        );
        let header = client.cookie_header().await;
        let cookie = header
            .get(reqwest::header::COOKIE)
            .map(|v| v.to_str().unwrap_or(""))
            .unwrap_or("");
        assert!(cookie.contains("ipb_member_id=member1"));
        assert!(cookie.contains("ipb_pass_hash=hash2"));
        // 无 cookie 时无 Cookie header
        let plain = EHentaiClient::new(reqwest::Client::new(), reqwest::Client::new());
        let header2 = plain.cookie_header().await;
        assert!(header2.get(reqwest::header::COOKIE).is_none());
    }

    #[test]
    fn to_series_metadata_forced_language_overrides() {
        let mapper = EHentaiMetadataMapper::new(
            crate::config::SeriesMetadataConfig::default(),
            vec![AuthorRole::Writer],
            vec![AuthorRole::Penciller],
            vec![],
        );
        // 无语言标签 + forced zh → zh（搜索词含中国翻訳时）
        let b = book(1, "t", "Title", None);
        let meta = mapper.to_series_metadata(&b, None, Some("zh"));
        assert_eq!(meta.metadata.language.as_deref(), Some("zh"));
        // 无 forced → 保持标签推导（此处 None）
        let meta2 = mapper.to_series_metadata(&b, None, None);
        assert_eq!(meta2.metadata.language, None);
        // 有语言标签 + forced en → en
        let mut e = book(2, "t", "Title", None);
        e.tags = Some(vec!["language:japanese".to_string()]);
        let meta3 = mapper.to_series_metadata(&e, None, Some("en"));
        assert_eq!(meta3.metadata.language.as_deref(), Some("en"));
    }

    #[test]
    fn search_result_prefers_japanese_title() {
        let mapper = EHentaiMetadataMapper::new(
            crate::config::SeriesMetadataConfig::default(),
            vec![AuthorRole::Writer],
            vec![AuthorRole::Penciller],
            vec![],
        );
        let mut b = book(4, "t", "English Title", None);
        b.title_jpn = Some("日本語タイトル".to_string());
        let r = mapper.to_series_search_result(&b, "");
        assert_eq!(r.title, "日本語タイトル");
        let mut b2 = book(5, "t2", "English Only", None);
        b2.title_jpn = Some("  ".to_string());
        let r2 = mapper.to_series_search_result(&b2, "");
        assert_eq!(r2.title, "English Only");
    }
}
