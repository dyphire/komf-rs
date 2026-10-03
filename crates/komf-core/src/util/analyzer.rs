//! 多语言标题分析链（Lucene `MultiLingualAnalyzer` 语义，移植自 kmrs 的 komga-search）。
//!
//! 用于离线库 FTS 预分词：索引侧写入 `index_analyze` 的空格拼接结果，
//! 查询侧用 `search_analyze` 产出 token 构造 AND 查询。
//!
//! 分析链（两侧对称，差异仅在 CJK unigram 发射）：
//! - t2s（繁→简，zhconv OpenCC 数据）→ standard tokenize（UAX#29 子集）→
//!   CJK width → lowercase → CJK bigram → NGram(3,10, preserveOriginal) → ASCII fold
//!
//! 不变式：`search_analyze` 产出的每个 token 都能在索引侧 `index_analyze` 的
//! 产出中找到（索引侧对每个 CJK 字都发 unigram，保证标题任意子串可命中）。
//!
//! 查询方向说明：komf 的场景是"查询比标题长"（komga 系列名常带卷数/系列装饰），
//! 调用方应配合渐进前缀 AND 回退（先全量 token AND，未命中逐层丢尾部 token）。

use std::sync::OnceLock;
use unicode_normalization::UnicodeNormalization;

/// 繁→简转换（OpenCC 数据，内置 converter 全局复用）。
///
/// 在原始文本上运行（词级规则需要字符上下文），索引/查询两侧映射到同一
/// 规范形式，简繁查询可交叉命中。映射是多对一的（乾/幹 → 干），少数标题会过度合并。
pub fn t2s_str(text: &str) -> String {
    static CONVERTER: OnceLock<&'static zhconv::ZhConverter> = OnceLock::new();
    CONVERTER
        .get_or_init(|| zhconv::converters::get_builtin_converter(zhconv::Variant::ZhCN))
        .convert(text)
}

/// Lucene `StandardTokenizer`（UAX#29 词切分，komf 相关子集）：
/// 汉字/平假名逐字发出，片假名连写保持整体，其余字母/数字按最大 run 聚合
/// （`.`/`:` 在两侧均为字母数字时连接 run 内部，`,`/`;` 仅在数字间连接）。
pub fn standard_tokenize(text: &str) -> Vec<String> {
    #[derive(Clone, Copy, PartialEq, Eq)]
    enum Cls {
        ALetter,
        Numeric,
        Katakana,
        Extend,
    }

    fn classify(c: char) -> Option<Cls> {
        if is_ideographic(c) {
            return None; // 逐字发出，不走 run 机制
        }
        if is_katakana(c) {
            return Some(Cls::Katakana);
        }
        if c.is_alphabetic() {
            return Some(Cls::ALetter);
        }
        if c.is_numeric() {
            return Some(Cls::Numeric);
        }
        if is_extend(c as u32) {
            return Some(Cls::Extend);
        }
        None
    }

    let chars: Vec<char> = text.chars().collect();
    let mut tokens = vec![];
    let mut buf = String::new();
    let mut buf_cls: Option<Cls> = None;

    fn flush(buf: &mut String, tokens: &mut Vec<String>) {
        if !buf.is_empty() {
            tokens.push(std::mem::take(buf));
        }
    }

    let mut i = 0;
    while i < chars.len() {
        let c = chars[i];
        if is_ideographic(c) {
            flush(&mut buf, &mut tokens);
            buf_cls = None;
            tokens.push(c.to_string());
            i += 1;
            continue;
        }
        // `.`/`:` 在字母数字 run 内连接；`,`/`;` 仅在数字间连接（UAX#29 WB6/7/11/12）
        if c == '.' || c == ':' || c == ',' || c == ';' {
            let prev_alnum = buf_cls.is_some();
            let next = chars.get(i + 1);
            let next_alnum = next.is_some_and(|n| n.is_alphanumeric());
            let joins = if c == ',' || c == ';' {
                buf_cls == Some(Cls::Numeric) && next.is_some_and(|n| n.is_numeric())
            } else {
                prev_alnum && next_alnum
            };
            if joins {
                buf.push(c);
            } else {
                flush(&mut buf, &mut tokens);
                buf_cls = None;
            }
            i += 1;
            continue;
        }
        let cls = match classify(c) {
            Some(cls) => cls,
            // 标点与符号作为分隔符
            None => {
                flush(&mut buf, &mut tokens);
                buf_cls = None;
                i += 1;
                continue;
            }
        };
        match (buf_cls, cls) {
            (None, _) => {
                buf.push(c);
                if cls != Cls::Extend {
                    buf_cls = Some(cls);
                }
            }
            (Some(_), Cls::Extend) => buf.push(c),
            (Some(a), b) if a == b => buf.push(c),
            (Some(Cls::ALetter), Cls::Numeric) | (Some(Cls::Numeric), Cls::ALetter) => buf.push(c),
            (Some(_), _) => {
                flush(&mut buf, &mut tokens);
                buf.push(c);
                buf_cls = Some(cls);
            }
        }
        i += 1;
    }
    flush(&mut buf, &mut tokens);
    tokens
}

/// UAX#29 Extend：组合符与 ZWJ
fn is_extend(u: u32) -> bool {
    (0x0300..=0x036F).contains(&u)
        || (0x1AB0..=0x1AFF).contains(&u)
        || (0x1DC0..=0x1DFF).contains(&u)
        || (0x20D0..=0x20FF).contains(&u)
        || (0xFE00..=0xFE0F).contains(&u)
        || (0xFE20..=0xFE2F).contains(&u)
        || u == 0x200D
        || (0xE0100..=0xE01EF).contains(&u)
}

/// 汉字与平假名：逐字发出（UAX#29）
fn is_ideographic(c: char) -> bool {
    let u = c as u32;
    (0x4E00..=0x9FFF).contains(&u)
        || (0x3400..=0x4DBF).contains(&u)
        || (0x20000..=0x2A6DF).contains(&u)
        || (0xF900..=0xFAFF).contains(&u)
        || (0x3040..=0x309F).contains(&u)
}

/// 谚文：不做 NFD（音节/兼容字母/半角谚文都会拆成 conjoining jamo，
/// 同样会切碎 FTS 分词；NFD 对 conjoining jamo 本身是恒等映射，一并归入）。
fn is_hangul(c: char) -> bool {
    let u = c as u32;
    (0xAC00..=0xD7A3).contains(&u) // Hangul Syllables
        || (0x3130..=0x318F).contains(&u) // Hangul Compatibility Jamo
        || (0xFFA0..=0xFFDC).contains(&u) // Halfwidth Hangul
        || (0x1100..=0x11FF).contains(&u) // Hangul Jamo
        || (0xA960..=0xA97F).contains(&u) // Hangul Jamo Extended-A
        || (0xD7B0..=0xD7FF).contains(&u) // Hangul Jamo Extended-B
}

fn is_katakana(c: char) -> bool {
    let u = c as u32;
    (0x30A0..=0x30FF).contains(&u)
        || (0x31F0..=0x31FF).contains(&u)
        || (0xFF61..=0xFF9F).contains(&u)
}

/// Lucene `CJKBigramFilter` 字符范围（汉字/平假名/片假名；不含谚文）
fn is_cjk_char(c: char) -> bool {
    is_ideographic(c) || is_katakana(c)
}

const HALFWIDTH_KATAKANA: [char; 0x3D] = [
    '。', '「', '」', '、', '・', 'ヲ', 'ァ', 'ィ', 'ゥ', 'ェ', 'ォ', 'ャ', 'ュ', 'ョ', 'ッ', 'ー',
    'ア', 'イ', 'ウ', 'エ', 'オ', 'カ', 'キ', 'ク', 'ケ', 'コ', 'サ', 'シ', 'ス', 'セ', 'ソ', 'タ',
    'チ', 'ツ', 'テ', 'ト', 'ナ', 'ニ', 'ヌ', 'ネ', 'ノ', 'ハ', 'ヒ', 'フ', 'ヘ', 'ホ', 'マ', 'ミ',
    'ム', 'メ', 'モ', 'ヤ', 'ユ', 'ヨ', 'ラ', 'リ', 'ル', 'レ', 'ロ', 'ワ', 'ン',
];

fn dakuten(c: char) -> Option<char> {
    Some(match c {
        'ウ' => 'ヴ',
        'カ' => 'ガ',
        'キ' => 'ギ',
        'ク' => 'グ',
        'ケ' => 'ゲ',
        'コ' => 'ゴ',
        'サ' => 'ザ',
        'シ' => 'ジ',
        'ス' => 'ズ',
        'セ' => 'ゼ',
        'ソ' => 'ゾ',
        'タ' => 'ダ',
        'チ' => 'ヂ',
        'ツ' => 'ヅ',
        'テ' => 'デ',
        'ト' => 'ド',
        'ハ' => 'バ',
        'ヒ' => 'ビ',
        'フ' => 'ブ',
        'ヘ' => 'ベ',
        'ホ' => 'ボ',
        _ => return None,
    })
}

fn handakuten(c: char) -> Option<char> {
    Some(match c {
        'ハ' => 'パ',
        'ヒ' => 'ピ',
        'フ' => 'プ',
        'ヘ' => 'ペ',
        'ホ' => 'ポ',
        _ => return None,
    })
}

/// Lucene `CJKWidthFilter`：全角 ASCII（U+FF01-U+FF5E）转 ASCII；
/// 半角片假名（U+FF61-U+FF9F）转全角并做濁点/半濁点结合。
pub fn cjk_width_str(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut iter = text.chars().peekable();
    while let Some(c) = iter.next() {
        let u = c as u32;
        if (0xFF01..=0xFF5E).contains(&u) {
            out.push(char::from_u32(u - 0xFEE0).unwrap());
        } else if (0xFF61..=0xFF9D).contains(&u) {
            let full = HALFWIDTH_KATAKANA[(u - 0xFF61) as usize];
            match iter.peek() {
                Some('ﾞ') if dakuten(full).is_some() => {
                    out.push(dakuten(full).unwrap());
                    iter.next();
                }
                Some('ﾟ') if handakuten(full).is_some() => {
                    out.push(handakuten(full).unwrap());
                    iter.next();
                }
                _ => out.push(full),
            }
        } else if u == 0xFF9E {
            out.push('゛');
        } else if u == 0xFF9F {
            out.push('゜');
        } else {
            out.push(c);
        }
    }
    out
}

pub fn cjk_width(tokens: Vec<String>) -> Vec<String> {
    tokens.into_iter().map(|t| cjk_width_str(&t)).collect()
}

pub fn lowercase(tokens: Vec<String>) -> Vec<String> {
    tokens.into_iter().map(|t| t.to_lowercase()).collect()
}

/// 查询侧 bigram 链：CJK 字符流上滑动 bigram；尾部孤立 CJK 字发 unigram，
/// 非 CJK token 之后的 CJK run 首字也发 unigram（边界扩展）。
///
/// 产出的每个 token 都会成为 AND 查询子句，因此全部必须能在索引侧解析
/// （索引侧 `cjk_bigram_index` 对每个 CJK 字都建 unigram）。
pub fn cjk_bigram(tokens: Vec<String>) -> Vec<String> {
    let mut out = vec![];
    // 待配对 CJK 字；bool 标记该字是否已按左边界规则发出（单字 run 不重复发）
    let mut prev: Option<(String, bool)> = None;
    let mut after_non_cjk = false;
    for token in tokens {
        if token.chars().all(is_cjk_char) {
            for c in token.chars() {
                let c = c.to_string();
                let next = match prev.take() {
                    Some((p, _)) => {
                        out.push(format!("{p}{c}"));
                        (c, false)
                    }
                    None if after_non_cjk => {
                        out.push(c.clone());
                        (c, true)
                    }
                    None => (c, false),
                };
                prev = Some(next);
            }
        } else {
            if let Some((p, emitted)) = prev.take() {
                if !emitted {
                    out.push(p);
                }
            }
            out.push(token);
        }
        after_non_cjk = prev.is_none();
    }
    if let Some((p, emitted)) = prev.take() {
        if !emitted {
            out.push(p);
        }
    }
    out
}

/// 索引侧 bigram 链：同 `cjk_bigram`，但每个 CJK 字 additionally 以 unigram 入索引
/// （Lucene `outputUnigrams` 模式）。查询 token 全部 AND，若 run 中部的字只有
/// bigram 形式，从标题中部截取的子串（"可爱" out of "我的可爱…" → `+可爱 +爱`）
/// 将无法命中——unigram 保证任意子串可查。
///
/// token 携带显式位置：bigram 保持顺序间隔（run 首字 unigram 取边界 unigram 的位置、
/// run 中部 unigram 与其起始的 bigram 同位、尾部 unigram 保留自己的位置），短语对齐不变。
pub fn cjk_bigram_index(tokens: Vec<String>) -> Vec<(String, usize)> {
    let mut out = vec![];
    let mut pos = 0usize;
    // 待配对 CJK 字：(文本, run 首字位置, run 内序号)
    let mut prev: Option<(String, usize, usize)> = None;
    // run 尾字在最后一个 bigram 之后独占一个位置；单字 run 已在起始处发出。
    // 返回下一个空闲位置。
    fn flush_run(
        out: &mut Vec<(String, usize)>,
        prev: &mut Option<(String, usize, usize)>,
        pos: usize,
    ) -> usize {
        match prev.take() {
            Some((p, base, i)) => {
                if i >= 2 {
                    out.push((p, base + i));
                    base + i + 1
                } else {
                    base + 1
                }
            }
            None => pos,
        }
    }
    for token in tokens {
        if token.chars().all(is_cjk_char) {
            for c in token.chars() {
                let c = c.to_string();
                match prev.take() {
                    Some((p, base, i)) => {
                        if i >= 2 {
                            // p 在 run 中部：unigram 与其起始的 bigram 同位
                            out.push((p.clone(), base + i));
                        }
                        out.push((format!("{p}{c}"), base + i));
                        prev = Some((c, base, i + 1));
                    }
                    None => {
                        out.push((c.clone(), pos));
                        prev = Some((c, pos, 1));
                    }
                }
            }
        } else {
            pos = flush_run(&mut out, &mut prev, pos);
            out.push((token, pos));
            pos += 1;
        }
    }
    flush_run(&mut out, &mut prev, pos);
    out
}

/// Lucene `NGramTokenFilter`：每个 token 发出长度 min..=max 的全部子串；
/// `preserve_original` 时整 token 也发出。
pub fn ngram(tokens: Vec<String>, min: usize, max: usize, preserve_original: bool) -> Vec<String> {
    let mut out = vec![];
    for token in tokens {
        let chars: Vec<char> = token.chars().collect();
        let len = chars.len();
        for size in min..=max.min(len) {
            for i in 0..=len - size {
                out.push(chars[i..i + size].iter().collect());
            }
        }
        if preserve_original {
            out.push(token);
        }
    }
    out
}

/// Lucene `ASCIIFoldingFilter`（komf 相关子集）：NFD 分解并剔除组合符，
/// 加上分解无法产出的显式展开（ß→ss 等）。希腊/西里尔按 Lucene 语义过 NFD；
/// **CJK/假名/谚文原样通过**——NFD 会把预组合浊音假名（ギ U+30AE）拆成
/// キ + U+3099 组合符，而 is_extend 码位表不含 U+3099，组合符残留会使
/// FTS5 unicode61 在二次分词时把 bigram 切碎（ギャ → キ|ャ）；
/// 谚文音节（한 U+D55C）同理会被拆成 conjoining jamo（한）。
pub fn ascii_fold_str(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    for c in text.chars() {
        match c {
            'ß' => out.push_str("ss"),
            'ẞ' => out.push_str("SS"),
            'æ' => out.push_str("ae"),
            'Æ' => out.push_str("AE"),
            'œ' => out.push_str("oe"),
            'Œ' => out.push_str("OE"),
            'ø' => out.push('o'),
            'Ø' => out.push('O'),
            'đ' | 'ð' => out.push('d'),
            'Đ' | 'Ð' => out.push('D'),
            'þ' => out.push_str("th"),
            'Þ' => out.push_str("TH"),
            'ł' => out.push('l'),
            'Ł' => out.push('L'),
            'ı' => out.push('i'),
            'ŋ' => out.push('n'),
            'Ŋ' => out.push('N'),
            'ħ' => out.push('h'),
            'Ĥ' => out.push('H'),
            'ƒ' => out.push('f'),
            'ĳ' => out.push_str("ij"),
            'Ĳ' => out.push_str("IJ"),
            'ﬀ' => out.push_str("ff"),
            'ﬁ' => out.push_str("fi"),
            'ﬂ' => out.push_str("fl"),
            'ﬃ' => out.push_str("ffi"),
            'ﬄ' => out.push_str("ffl"),
            'ﬅ' | 'ﬆ' => out.push_str("st"),
            // CJK/假名/谚文不做 NFD：预组合浊音（ギ が 等）会被拆成基字 + U+3099，
            // 组合符残留导致 FTS 二次分词切碎 bigram（见函数文档）；
            // 谚文音节（한 等）会拆成 conjoining jamo（한），同理。
            _ if is_ideographic(c) || is_katakana(c) || is_hangul(c) => out.push(c),
            _ => {
                for d in c.to_string().nfd() {
                    if !is_extend(d as u32) {
                        out.push(d);
                    }
                }
            }
        }
    }
    out
}

pub fn ascii_fold(tokens: Vec<String>) -> Vec<String> {
    tokens.into_iter().map(|t| ascii_fold_str(&t)).collect()
}

/// 查询侧链（对应 Lucene `MultiLingualAnalyzer`）。
pub fn search_analyze(text: &str) -> Vec<String> {
    ascii_fold(cjk_bigram(lowercase(cjk_width(standard_tokenize(
        &t2s_str(text),
    )))))
}

/// 索引侧链（对应 `MultiLingualNGramAnalyzer`，minGram=3、maxGram=10、
/// preserveOriginal，外加 `cjk_bigram_index` 的 unigram 扩展）。
///
/// 每个 token 携带位置。NGram 展开保持 Lucene"每个 gram 一个位置"的语义：
/// `drift` 累积前面 token 多占的位置，使未经该 filter 的 token
/// （长度 < minGram 的 CJK bigram/unigram）保持 `cjk_bigram_index` 赋予的位置。
pub fn index_analyze(text: &str) -> Vec<(String, usize)> {
    let tokens = cjk_bigram_index(lowercase(cjk_width(standard_tokenize(&t2s_str(text)))));
    let mut out = vec![];
    let mut drift = 0usize;
    for (token, pos) in tokens {
        let grams = ngram(vec![token], 3, 10, true);
        for (i, gram) in grams.iter().enumerate() {
            out.push((ascii_fold_str(gram), pos + drift + i));
        }
        // preserve_original 保证每 token 至少一个 gram
        drift += grams.len() - 1;
    }
    out
}

/// 索引侧 token 文本（去位置），空格拼接后写入 FTS 列。
pub fn index_analyze_terms(text: &str) -> Vec<String> {
    index_analyze(text).into_iter().map(|(t, _)| t).collect()
}

/// 渐进前缀回退的最低保留 token 数（对齐 kmrs `cjk_droppable_floor` 的 komf 变体）：
/// 取"最后一个含拉丁字母的 token 之后"与 2 的较大者——只放宽 CJK bigram 链尾部与
/// 数字/量词装饰（"第01话"的 01/话），拉丁整词子句（berserk/jojo）永不放宽：
/// 放宽它会退化为单词查询、冲爆候选上限（kmrs 将数字也视为整词，因其要求 Lucene
/// parity；komf 是召回层，数字装饰应可丢弃）。单 token 查询由调用方按 1 处理。
pub fn droppable_floor(tokens: &[String]) -> usize {
    match tokens
        .iter()
        .rposition(|t| t.chars().any(|c| c.is_ascii_alphabetic()))
    {
        Some(i) => (i + 1).max(2),
        None => 2,
    }
}

/// `MultiLingualAnalyzer.normalize`（前缀/通配查询词用）：t2s → CJK width →
/// lowercase → ASCII fold，不做 bigram。
pub fn normalize(text: &str) -> String {
    ascii_fold_str(&cjk_width_str(&t2s_str(text)).to_lowercase())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn standard_tokenize_latin_and_digits() {
        assert_eq!(standard_tokenize("Berserk"), vec!["Berserk"]);
        assert_eq!(standard_tokenize("v01"), vec!["v01"]);
        assert_eq!(standard_tokenize("9781593070205"), vec!["9781593070205"]);
        assert_eq!(standard_tokenize("hello world"), vec!["hello", "world"]);
        assert_eq!(standard_tokenize("foo.bar"), vec!["foo.bar"]);
        assert_eq!(standard_tokenize("a,b"), vec!["a", "b"]);
        assert_eq!(standard_tokenize("1,5"), vec!["1,5"]);
    }

    #[test]
    fn standard_tokenize_cjk() {
        assert_eq!(standard_tokenize("東京タワー"), vec!["東", "京", "タワー"]);
        assert_eq!(standard_tokenize("ひらがな"), vec!["ひ", "ら", "が", "な"]);
        assert_eq!(standard_tokenize("カタカナ"), vec!["カタカナ"]);
        assert_eq!(
            standard_tokenize("one 東京 two"),
            vec!["one", "東", "京", "two"]
        );
    }

    #[test]
    fn cjk_width_fullwidth_ascii_and_halfwidth_katakana() {
        assert_eq!(cjk_width_str("Ｈｅｌｌｏ"), "Hello");
        assert_eq!(cjk_width_str("１２３"), "123");
        assert_eq!(cjk_width_str("ｶﾀｶﾅ"), "カタカナ");
        assert_eq!(cjk_width_str("ｶﾞｷ"), "ガキ");
        assert_eq!(cjk_width_str("ﾊﾟﾋﾟ"), "パピ");
        assert_eq!(cjk_width_str("ｳﾞ"), "ヴ");
    }

    #[test]
    fn cjk_bigram_sequence() {
        assert_eq!(
            cjk_bigram(standard_tokenize("東京タワー")),
            vec!["東京", "京タ", "タワ", "ワー", "ー"]
        );
        assert_eq!(
            cjk_bigram(standard_tokenize("ひらがな")),
            vec!["ひら", "らが", "がな", "な"]
        );
        // 非 CJK token 冲刷待配对字；其后的 run 首字发边界 unigram
        assert_eq!(
            cjk_bigram(vec!["abc".into(), "東".into(), "京".into(), "def".into()]),
            vec!["abc", "東", "東京", "京", "def"]
        );
    }

    #[test]
    fn cjk_bigram_boundary_unigrams() {
        // 左边界 月 使查询 "3月" 可命中该标题
        assert_eq!(
            search_analyze("3月的狮子"),
            vec!["3", "月", "月的", "的狮", "狮子", "子"]
        );
        assert_eq!(search_analyze("3月"), vec!["3", "月"]);
        // 文本起始处的 run 无左边界 unigram
        assert_eq!(search_analyze("犬夜叉2"), vec!["犬夜", "夜叉", "叉", "2"]);
        // 夹在非 CJK token 间的 run 两侧都发边界 unigram
        assert_eq!(search_analyze("A月的B"), vec!["a", "月", "月的", "的", "b"]);
        // 规则看 token 流：拉丁词后的 run 也获得 unigram
        assert_eq!(
            search_analyze("Batman 東京"),
            vec!["batman", "东", "东京", "京"]
        );
    }

    #[test]
    fn cjk_bigram_index_unigrams_and_positions() {
        // 每字以 unigram 入索引；bigram 保持顺序间隔，run 中部 unigram 与其
        // 起始 bigram 同位，尾部 unigram 保留自己的位置
        assert_eq!(
            cjk_bigram_index(standard_tokenize("東京タワー")),
            vec![
                ("東".to_string(), 0),
                ("東京".to_string(), 1),
                ("京".to_string(), 2),
                ("京タ".to_string(), 2),
                ("タ".to_string(), 3),
                ("タワ".to_string(), 3),
                ("ワ".to_string(), 4),
                ("ワー".to_string(), 4),
                ("ー".to_string(), 5),
            ]
        );
        assert_eq!(
            cjk_bigram_index(vec!["abc".into(), "東".into(), "京".into(), "def".into()]),
            vec![
                ("abc".to_string(), 0),
                ("東".to_string(), 1),
                ("東京".to_string(), 2),
                ("京".to_string(), 3),
                ("def".to_string(), 4),
            ]
        );
        // 单字 run 只发一次
        assert_eq!(
            cjk_bigram_index(vec!["a".into(), "月".into(), "b".into()]),
            vec![
                ("a".to_string(), 0),
                ("月".to_string(), 1),
                ("b".to_string(), 2),
            ]
        );
    }

    #[test]
    fn index_chain_unigrams_and_ngram_positions() {
        // 查询侧 "可爱" → [可爱, 爱]；两侧都必须存在于索引
        let toks = index_analyze("我的可愛對黑岩目高不管用");
        assert!(toks.contains(&("可爱".to_string(), 3)));
        assert!(toks.contains(&("爱".to_string(), 4)));
        assert!(toks.contains(&("我".to_string(), 0)));
        assert!(toks.contains(&("用".to_string(), 12)));
        // 拉丁 token 每个 gram 占一个位置（"berserk" 展开为 15 个 gram +
        // preserveOriginal 在位置 15，CJK token 相应后移）
        let toks = index_analyze("Berserk 東京");
        assert!(toks.contains(&("ber".to_string(), 0)));
        assert!(toks.contains(&("berserk".to_string(), 15)));
        assert!(toks.contains(&("东".to_string(), 16)));
        assert!(toks.contains(&("东京".to_string(), 17)));
        assert!(toks.contains(&("京".to_string(), 18)));
    }

    #[test]
    fn t2s_unifies_simplified_and_traditional() {
        assert_eq!(t2s_str("名偵探柯南"), "名侦探柯南");
        assert_eq!(t2s_str("名侦探柯南"), "名侦探柯南");
        // 词级规则保持多音字组合（zhconv 合并 OpenCC 表后的行为以实测为准）
        assert_eq!(t2s_str("乾燥"), "干燥");
        // 日本新字体同样收拢到简体；假名不动
        assert_eq!(t2s_str("東京タワー"), "东京タワー");
        assert_eq!(t2s_str("3月的狮子"), "3月的狮子");
    }

    #[test]
    fn chains_unify_simplified_and_traditional() {
        assert_eq!(search_analyze("名偵探柯南"), search_analyze("名侦探柯南"));
        assert_eq!(index_analyze("名偵探柯南"), index_analyze("名侦探柯南"));
        assert_eq!(normalize("名偵探"), "名侦探");
    }

    #[test]
    fn ngram_3_to_10_preserve_original() {
        let grams = ngram(vec!["berserk".to_string()], 3, 10, true);
        for expected in [
            "ber", "ers", "rse", "ser", "erk", "bers", "erse", "rser", "serk", "berserk",
        ] {
            assert!(grams.contains(&expected.to_string()), "missing {expected}");
        }
        // 长度 2 token：只剩 original（minGram=3）
        let grams = ngram(vec!["東京".to_string()], 3, 10, true);
        assert_eq!(grams, vec!["東京"]);
    }

    #[test]
    fn ascii_folding() {
        assert_eq!(ascii_fold_str("café"), "cafe");
        assert_eq!(ascii_fold_str("straße"), "strasse");
        assert_eq!(ascii_fold_str("œuvre"), "oeuvre");
        assert_eq!(ascii_fold_str("ﬁle"), "file");
        assert_eq!(ascii_fold_str("naïve"), "naive");
        assert_eq!(ascii_fold_str("Łódź"), "Lodz");
        assert_eq!(ascii_fold_str("東京"), "東京");
    }

    #[test]
    fn ascii_folding_keeps_precomposed_kana() {
        // 回归：NFD 不得拆开预组合浊音假名（ギ→キ+U+3099 会切碎 FTS bigram）。
        // token 级验证走 search_analyze（ascii_fold 是链尾）。
        assert_eq!(search_analyze("ギャル"), vec!["ギャ", "ャル", "ル"]);
        assert_eq!(search_analyze("がわ"), vec!["がわ", "わ"]);
        assert_eq!(
            search_analyze("生意気ギャル")[..4],
            ["生意", "意気", "気ギ", "ギャ"]
        );
    }

    #[test]
    fn ascii_folding_keeps_hangul_syllables() {
        // 回归：NFD 不得拆开谚文音节（한→한 会产生 conjoining jamo，
        // 与 katakana 浊音相同的 FTS 切碎问题）。
        assert_eq!(ascii_fold_str("한글"), "한글");
        assert_eq!(ascii_fold_str("ㄱ"), "ㄱ"); // 兼容字母不拆成 conjoining jamo
        assert_eq!(search_analyze("한글"), vec!["한글"]);
        assert_eq!(index_analyze_terms("한글"), vec!["한글"]);
    }

    #[test]
    fn search_chain() {
        assert_eq!(search_analyze("Ｈｅｌｌｏ"), vec!["hello"]);
        // t2s 先运行：日文标题按简体汉字形式入索引
        assert_eq!(
            search_analyze("東京タワー"),
            vec!["东京", "京タ", "タワ", "ワー", "ー"]
        );
    }

    #[test]
    fn search_chain_frieren() {
        // 核心回归用例：komga 系列名带装饰时应能通过渐进前缀 AND 命中
        // "葬送的芙莉莲系列" 的前 5 个 token 是 "葬送的芙莉莲" 的全部查询 token
        let q_series = search_analyze("葬送的芙莉莲系列");
        assert_eq!(q_series[..5], ["葬送", "送的", "的芙", "芙莉", "莉莲"]);
        // "葬送的芙莉莲 第01话"：空格被分隔，CJK 流连续（bigram 跨空格）
        let q_chapter = search_analyze("葬送的芙莉莲 第01话");
        assert_eq!(q_chapter[..5], ["葬送", "送的", "的芙", "芙莉", "莉莲"]);
        // 索引侧包含查询前缀的全部 token（含尾部单字 unigram）
        let indexed: Vec<String> = index_analyze_terms("葬送的芙莉莲");
        for t in ["葬送", "送的", "的芙", "芙莉", "莉莲", "莲"] {
            assert!(indexed.contains(&t.to_string()), "missing {t}");
        }
    }

    #[test]
    fn droppable_floor_rules() {
        let tokens = |ts: &[&str]| ts.iter().map(|t| t.to_string()).collect::<Vec<_>>();
        // 纯 CJK 链：仅两 token 下限约束
        assert_eq!(droppable_floor(&tokens(&["葬送", "送的", "系列"])), 2);
        // 拉丁整词在首部：其后可放宽，但不低于 2
        assert_eq!(droppable_floor(&tokens(&["berserk", "系", "系列"])), 2);
        // 拉丁整词在尾部：floor 越过它（永不放宽拉丁子句）
        assert_eq!(droppable_floor(&tokens(&["的", "系列", "frieren"])), 3);
        // 数字/量词装饰可放宽（与 kmrs 的差异：komf 召回层语义）
        assert_eq!(droppable_floor(&tokens(&["葬送", "送的", "01", "话"])), 2);
        // 片假名无拉丁字母 → 可放宽
        assert_eq!(droppable_floor(&tokens(&["フリーレン", "系列"])), 2);
    }

    #[test]
    fn normalize_chain() {
        assert_eq!(normalize("Ｈｅｌｌｏ"), "hello");
        // 前缀/通配词不做 bigram
        assert_eq!(normalize("東京"), "东京");
    }
}
