//! 字符串工具 —— 对应 `StringUtils.kt` 的相关函数。

/// 用 ASCII 等价字符替换全角字符（对应 Kotlin `replaceFullwidthChars`）。
/// 覆盖整个全角 ASCII 区（U+FF01..=U+FF5E）与全角空格（U+3000）。
pub fn replace_fullwidth_chars(input: &str) -> String {
    input
        .chars()
        .map(|c| match c {
            '！'..='～' => char::from_u32(c as u32 - 0xFEE0).unwrap_or(c),
            '　' => ' ',
            other => other,
        })
        .collect()
}

/// 搜索/匹配统一预处理：全角转半角 + 常见 CJK 标点统一为半角形式 + 折叠空白并 trim。
///
/// 对 query 与 provider 候选标题两侧应用同一管线，使仅全角/标点差异的标题
/// （如 "進撃の巨人：完全版" vs "進撃の巨人:完全版"）可以直接匹配。
pub fn normalize_search_text(input: &str) -> String {
    let mut out = String::with_capacity(input.len());
    let mut last_was_space = true; // 折叠前导空白
    for c in replace_fullwidth_chars(input).chars() {
        match c {
            '、' => out.push(','),
            '。' => out.push('.'),
            '「' | '」' | '『' | '』' | '“' | '”' => out.push('"'),
            '‘' | '’' => out.push('\''),
            '【' => out.push('['),
            '】' => out.push(']'),
            '《' => out.push('<'),
            '》' => out.push('>'),
            '–' | '—' | '〜' => out.push('-'),
            '…' => out.push_str("..."),
            c if c.is_whitespace() => {
                if !last_was_space {
                    out.push(' ');
                    last_was_space = true;
                }
                continue;
            }
            c => out.push(c),
        }
        last_was_space = false;
    }
    out.trim_end().to_string()
}

/// 括号段噪声剔除（Rust 扩展）：同人志式命名中括号段多为噪声
/// （作者/汉化组/版本/画廊号），提取实际搜索词。
///
/// 供除 ehentai 外的 provider 的搜索/匹配/tracker 入口使用
/// （ehentai 在 provider 内部已有更完整的 parse_title/get_search_queries 处理）。
///
/// 规则（仅当搜索词含多个 `[]` 段时生效）：
/// 1. 去除全部 `()` / `[]` 段后仍有文本 → 该文本即实际搜索词
///    （如 `(画集)[chin] 女騎士が転生したら ニートの召使いだった件 全編 [中国翻訳] [DL版] [3542432]`
///    → `女騎士が転生したら ニートの召使いだった件 全編`）；
/// 2. 整名只由多个 `[]` 段组成 → 第一个 `[]` 内容不含 authorSeparator（正则）时取其内容，
///    含 authorSeparator（作者列表段）时取第二个 `[]` 内容。
///
/// 不满足条件（无 `[]` 或仅一个 `[]`）返回 None，调用方沿用原搜索词。
pub fn bracket_search_term(name: &str, author_separator: Option<&str>) -> Option<String> {
    let groups: Vec<&str> = regex::Regex::new(r"\[([^\]]*)\]")
        .ok()?
        .captures_iter(name)
        .filter_map(|c| c.get(1).map(|m| m.as_str()))
        .collect();
    if groups.len() < 2 {
        return None;
    }
    // 1. 段外文本：`()` 与 `[]` 段全部去除后折叠空白
    let without_brackets = regex::Regex::new(r"\([^)]*\)|\[[^\]]*\]")
        .ok()?
        .replace_all(name, " ");
    let outside = without_brackets
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ");
    if !outside.is_empty() {
        return Some(outside);
    }
    // 2. 整名只由 [] 段组成：第一个 [] 是否 authorSeparator（正则）命中
    let first_is_author_list = author_separator
        .and_then(|p| regex::Regex::new(p).ok())
        .map(|re| re.is_match(groups[0]))
        .unwrap_or(false);
    if first_is_author_list {
        Some(groups[1].to_string())
    } else {
        Some(groups[0].to_string())
    }
}

/// 去除拉丁字母的重音符号（对应 Kotlin `stripAccents`，基于 java.text.Normalizer NFD）。
pub fn strip_accents(input: &str) -> String {
    use std::fmt::Write as _;

    let mut out = String::with_capacity(input.len());
    let mut pending_combining = false;

    for c in input.chars() {
        // NFD 分解常见重音字符
        let decomposed = match c {
            'à' | 'á' | 'â' | 'ã' | 'ä' | 'å' => Some('a'),
            'è' | 'é' | 'ê' | 'ë' => Some('e'),
            'ì' | 'í' | 'î' | 'ï' => Some('i'),
            'ò' | 'ó' | 'ô' | 'õ' | 'ö' | 'ø' => Some('o'),
            'ù' | 'ú' | 'û' | 'ü' => Some('u'),
            'ñ' => Some('n'),
            'ç' => Some('c'),
            'ß' => Some('s'),
            'À' | 'Á' | 'Â' | 'Ã' | 'Ä' | 'Å' => Some('A'),
            'È' | 'É' | 'Ê' | 'Ë' => Some('E'),
            'Ì' | 'Í' | 'Î' | 'Ï' => Some('I'),
            'Ò' | 'Ó' | 'Ô' | 'Õ' | 'Ö' | 'Ø' => Some('O'),
            'Ù' | 'Ú' | 'Û' | 'Ü' => Some('U'),
            'Ñ' => Some('N'),
            'Ç' => Some('C'),
            'ẞ' => Some('S'),
            'æ' => Some('a'),
            'Æ' => Some('A'),
            'œ' => Some('o'),
            'Œ' => Some('O'),
            'đ' => Some('d'),
            'Đ' => Some('D'),
            'ł' => Some('l'),
            'Ł' => Some('L'),
            _ => None,
        };

        match decomposed {
            Some(base) => {
                // 丢弃 combining marks
                if pending_combining {
                    pending_combining = false;
                }
                let _ = out.write_char(base);
            }
            None => {
                // U+0300..U+036F 为 combining diacritical marks，直接丢弃
                if !('\u{0300}'..='\u{036f}').contains(&c) {
                    let _ = out.write_char(c);
                }
                pending_combining = false;
            }
        }
    }

    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fullwidth_conversion() {
        assert_eq!(
            replace_fullwidth_chars("ＢＥＲＳＥＲＫ１２３"),
            "BERSERK123"
        );
        assert_eq!(replace_fullwidth_chars("（Ｗｅｂ）"), "(Web)");
        // 全角 ASCII 区（U+FF01..=U+FF5E）全量映射
        assert_eq!(
            replace_fullwidth_chars("！＃％＆＊＋－．／＜＝＞？＠［＼］＾＿｀｛｜｝"),
            "!#%&*+-./<=>?@[\\]^_`{|}"
        );
        assert_eq!(replace_fullwidth_chars("　"), " ");
    }

    #[test]
    fn search_text_normalized() {
        // 全角 + 全角冒号 → 半角
        assert_eq!(
            normalize_search_text("進撃の巨人：完全版"),
            "進撃の巨人:完全版"
        );
        // CJK 标点统一
        assert_eq!(normalize_search_text("【标题】《书名》"), "[标题]<书名>");
        assert_eq!(normalize_search_text("「quote」‘x’"), "\"quote\"'x'");
        assert_eq!(normalize_search_text("a—b–c〜d…e"), "a-b-c-d...e");
        // 空白折叠 + trim
        assert_eq!(normalize_search_text("  a　 b  "), "a b");
        assert_eq!(normalize_search_text("　"), "");
    }

    #[test]
    fn bracket_search_term_prefers_outside_text() {
        // 形如 `(画集)[chin] 标题 [中国翻訳] [DL版] [3542432]` → 段外文本
        assert_eq!(
            bracket_search_term(
                "(画集)[chin] 女騎士が転生したら ニートの召使いだった件 全編 [中国翻訳] [DL版] [3542432]",
                Some("×")
            ),
            Some("女騎士が転生したら ニートの召使いだった件 全編".to_string())
        );
    }

    #[test]
    fn bracket_search_term_only_brackets() {
        // 整名只由 [] 段组成：第一个 [] 不含 authorSeparator → 取第一个 [] 内容（标题段）
        assert_eq!(
            bracket_search_term(
                "[默示录的四骑士][鈴木央][Vol.01-Vol.23][东立][电子版]",
                Some("×")
            ),
            Some("默示录的四骑士".to_string())
        );
        // 第一个 [] 含 authorSeparator（作者列表段）→ 取第二个 [] 内容
        assert_eq!(
            bracket_search_term(
                "[逢沢大介×東西×坂野杏梨][想要成为影之实力者！][未完][角川][电子版]",
                Some("×")
            ),
            Some("想要成为影之实力者！".to_string())
        );
        // 未配置 authorSeparator：第一个 [] 不含分隔符 → 取第一个 [] 内容
        assert_eq!(
            bracket_search_term(
                "[默示录的四骑士][鈴木央][Vol.01-Vol.23][东立][电子版]",
                None
            ),
            Some("默示录的四骑士".to_string())
        );
    }

    #[test]
    fn bracket_search_term_noop_cases() {
        // 无 [] 或仅一个 [] → 不处理
        assert_eq!(
            bracket_search_term("女騎士が転生したら 全編", Some("×")),
            None
        );
        assert_eq!(
            bracket_search_term("[女騎士が転生したら 全編]", Some("×")),
            None
        );
        // authorSeparator 非法正则不 panic：视为不含作者分隔符
        assert_eq!(
            bracket_search_term("[Title] [Author]", Some("([")),
            Some("Title".to_string())
        );
    }

    #[test]
    fn accents_stripped() {
        assert_eq!(strip_accents("Étoile"), "Etoile");
        assert_eq!(strip_accents("café"), "cafe");
    }
}
