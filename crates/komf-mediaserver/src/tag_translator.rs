//! 标签翻译映射 —— 英文标签 → 中文（数据源 `G:\Tools\komga\tags.json`，
//! 编译时嵌入 tag_translation.json）。当 seriesTitleLanguage 为中文时，
//! 对非 bangumi/ehentai provider 的最终写入 tags 做 display_en → display_zh 翻译。
//!
//! 匹配策略：provider 标签与 `display_en` 或 `key` 小写化后精确匹配才翻译，
//! 未命中保持原文（不猜词、不破坏用户已有中文标签）。
use serde::Deserialize;
use std::collections::HashMap;

const TAG_TRANSLATION_JSON: &str = include_str!("tag_translation.json");

#[derive(Debug, Deserialize)]
struct TagTranslationEntry {
    key: String,
    #[serde(rename = "displayEn")]
    display_en: String,
    #[serde(rename = "displayZh")]
    display_zh: String,
}

/// 标签翻译器：小写 display_en/key → 中文。
#[derive(Debug, Clone, Default)]
pub struct TagTranslator {
    map: HashMap<String, String>,
}

impl TagTranslator {
    /// 加载内置映射。数据缺失/解析失败时回退空映射（保持原样）。
    pub fn builtin() -> Self {
        let entries: Vec<TagTranslationEntry> =
            serde_json::from_str(TAG_TRANSLATION_JSON).unwrap_or_default();
        let mut map = HashMap::with_capacity(entries.len() * 2);
        for e in entries {
            // display_en 与 key 双索引（小写）：provider 标签可能命中其中任一形式。
            map.insert(e.display_en.to_ascii_lowercase(), e.display_zh.clone());
            if e.key.to_ascii_lowercase() != e.display_en.to_ascii_lowercase() {
                map.insert(e.key.to_ascii_lowercase(), e.display_zh);
            }
        }
        Self { map }
    }

    /// 翻译单个标签：精确匹配（小写）命中则替换为中文，否则原样返回。
    pub fn translate(&self, tag: &str) -> String {
        self.map
            .get(&tag.to_ascii_lowercase())
            .cloned()
            .unwrap_or_else(|| tag.to_string())
    }

    /// seriesTitleLanguage 是否属于中文（zh / zh-cn / zh-hans / zh-hk / zh-tw / zh-hant …）。
    /// 与 bangumi `alias_language` 的中文变体口径一致；大小写不敏感。
    pub fn is_chinese_language(lang: Option<&str>) -> bool {
        lang.map(|l| {
            let l = l.trim().to_ascii_lowercase();
            l == "zh" || l == "chinese" || l.starts_with("zh-")
        })
        .unwrap_or(false)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn translator() -> TagTranslator {
        TagTranslator::builtin()
    }

    #[test]
    fn translates_display_en_exact() {
        let tr = translator();
        assert_eq!(tr.translate("Action"), "动作");
        assert_eq!(tr.translate("Adventure"), "冒险");
    }

    #[test]
    fn translates_kebab_key_form() {
        let tr = translator();
        // key 形式（kebab-case）也可命中：mangadex tag 可能用小写键。
        assert_eq!(tr.translate("4-koma"), "四格漫画");
        assert_eq!(tr.translate("achromatic"), "黑白");
    }

    #[test]
    fn case_insensitive() {
        let tr = translator();
        assert_eq!(tr.translate("action"), "动作");
        assert_eq!(tr.translate("ACTION"), "动作");
    }

    #[test]
    fn unmatched_keeps_original() {
        let tr = translator();
        assert_eq!(tr.translate("Score: 8"), "Score: 8");
        assert_eq!(tr.translate("Publisher: Kodansha"), "Publisher: Kodansha");
        assert_eq!(tr.translate("Totally Unknown Tag"), "Totally Unknown Tag");
    }

    #[test]
    fn chinese_language_detection() {
        assert!(TagTranslator::is_chinese_language(Some("zh")));
        assert!(TagTranslator::is_chinese_language(Some("zh-CN")));
        assert!(TagTranslator::is_chinese_language(Some("zh-hans")));
        assert!(TagTranslator::is_chinese_language(Some("ZH-TW")));
        assert!(TagTranslator::is_chinese_language(Some("chinese")));
        assert!(!TagTranslator::is_chinese_language(Some("en")));
        assert!(!TagTranslator::is_chinese_language(Some("ja")));
        assert!(!TagTranslator::is_chinese_language(None));
    }

    #[test]
    fn builtin_has_expected_entries() {
        let tr = translator();
        // 447 条数据中抽样：Genre 类的常见标签。
        for (en, zh) in [("Action", "动作"), ("Romance", "恋爱"), ("Comedy", "喜剧")] {
            assert_eq!(tr.translate(en), zh, "missing translation for {en}");
        }
    }
}
