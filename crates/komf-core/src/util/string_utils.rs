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
        assert_eq!(replace_fullwidth_chars("！＃％＆＊＋－．／＜＝＞？＠［＼］＾＿｀｛｜｝"), "!#%&*+-./<=>?@[\\]^_`{|}");
        assert_eq!(replace_fullwidth_chars("　"), " ");
    }

    #[test]
    fn search_text_normalized() {
        // 全角 + 全角冒号 → 半角
        assert_eq!(normalize_search_text("進撃の巨人：完全版"), "進撃の巨人:完全版");
        // CJK 标点统一
        assert_eq!(normalize_search_text("【标题】《书名》"), "[标题]<书名>");
        assert_eq!(normalize_search_text("「quote」‘x’"), "\"quote\"'x'");
        assert_eq!(normalize_search_text("a—b–c〜d…e"), "a-b-c-d...e");
        // 空白折叠 + trim
        assert_eq!(normalize_search_text("  a　 b  "), "a b");
        assert_eq!(normalize_search_text("　"), "");
    }

    #[test]
    fn accents_stripped() {
        assert_eq!(strip_accents("Étoile"), "Etoile");
        assert_eq!(strip_accents("café"), "cafe");
    }
}
