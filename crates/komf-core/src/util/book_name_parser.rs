//! 书名解析 —— 对应 `BookNameParser.kt`。
use crate::model::BookRange;
use crate::util::string_utils::replace_fullwidth_chars;
use once_cell::sync::Lazy;
use regex::Regex;

static VOLUME_REGEXES: Lazy<Vec<Regex>> = Lazy::new(|| {
    vec![
        Regex::new(r"(?i)(?:^|,?\s)\(?volume\s(?<volumeStart>[0-9]+)(,?\s?[0-9]+,)+(?<volumeEnd>\s?[0-9]+)\)?").unwrap(),
        // vol./vols./volume 前缀后的空格为可选：Komga/kmrs 扫描器普遍产出 "Vol.01"
        // 这类无空格书名（且 bangumi 单行本关联名 "違国日記 (1)" 可解析），
        // 若此处要求空格，associate_book_metadata 卷号匹配会全部失败。
        Regex::new(r"(?i)(?:^|,?\s)\(?([vtT]|vols\.\s?|vol\.\s?|volume\s?)(?<volumeStart>[0-9]+([.x#][0-9]+)?)(?<volumeEnd>-[0-9]+([.x#][0-9]+)?)?\)?").unwrap(),
        Regex::new(r".*第\s*(?<volumeStart>\d+)\s*-?\s*(?<volumeEnd>\d+)?\s*[巻卷册冊集]").unwrap(),
        // 无「第」前缀的 "5巻"/"12卷"/"1-3冊" 结尾形态。放在「第」正则之后：
        // get_volumes 首条命中即返回，走到这里已保证「第N巻」形态被前一条消费，
        // `[^\d第-]` 边界避免匹配 "12巻" 中的部分数字（取最后一组完整数字），
        // 并排除 "-" 使 "1-3冊" 的范围组不被拆散（greedy 回溯下 "-" 会成为合法边界）。
        Regex::new(r"(?:^|.*[^\d第-])(?<volumeStart>\d+(?:\.\d+)?)\s*-?\s*(?<volumeEnd>\d+(?:\.\d+)?)?\s*[巻卷册冊集]\s*$").unwrap(),
        // "巻5"/"巻05"/"巻1-2" 前置形态（巻/卷/册/冊/集 后直接跟结尾数字）。
        // `[^\d]` 边界同理防止 "12巻5" 误解析。
        Regex::new(r"(?:^|.*[^\d])(?:[巻卷册冊集])\s*(?<volumeStart>\d+(?:\.\d+)?)\s*-?\s*(?<volumeEnd>\d+(?:\.\d+)?)?\s*$").unwrap(),
        Regex::new(r".*年(?:[0-9]+月)?(?:[0-9]+日)?(?<volumeStart>\d+)-?(?<volumeEnd>\d+)?号").unwrap(),
    ]
});

static CHAPTER_REGEXES: Lazy<Vec<Regex>> = Lazy::new(|| {
    vec![
        // 前缀空格为可选并补 chap.：Komga/kmrs 扫描器普遍产出 "Chap.001"/"ch.12"
        // 这类无空格（且无 chap 前缀支持）的章节命名。
        Regex::new(r"(?i)(?:^|\s?)(c|ch\.\s?|chap\.\s?|chapter\s?|ep\.\s?)(?<start>[0-9]+([.x#][0-9]+)?)(?<end>-[0-9]+([.x#][0-9]+)?)?").unwrap(),
        Regex::new(r".*第\s*(?<start>\d+(?:\.\d+)?)\s*-?\s*(?<end>\d+(?:\.\d+)?)?\s*[話话章节回]").unwrap(),
        // 无「第」前缀的 "001-100话"/"5話" 结尾形态。放在「第」正则之后：
        // 「第N話」优先；`[^\d第-]` 边界避免 "12話" 拆出部分数字、"1-3话" 拆散范围。
        Regex::new(r"(?:^|.*[^\d第-])(?<start>\d+(?:\.\d+)?)\s*-?\s*(?<end>\d+(?:\.\d+)?)?\s*[話话章节回]\s*$").unwrap(),
    ]
});

static BOOK_NUMBER_REGEXES: Lazy<Vec<Regex>> = Lazy::new(|| {
    vec![
        Regex::new(r"(?i)(?:\s|#|no\.)(?<start>[0-9]+[AB]?([.x#][0-9]+)?)(?<end>-[0-9]+([.x#][0-9]+)?)?(?:\s\(.*\)\s*)*$").unwrap(),
        Regex::new(r"Issue (?<start>[0-9]+[AB]?([.x#][0-9]+)?)(?<end>-[0-9]+([.x#][0-9]+)?)?").unwrap(),
        Regex::new(r"Volume (?<start>[0-9]+[AB]?([.x#][0-9]+)?)(?<end>-[0-9]+([.x#][0-9]+)?)?").unwrap(),
    ]
});

static EXTRA_DATA_REGEX: Lazy<Regex> = Lazy::new(|| Regex::new(r"\[(?<extra>.*?)]").unwrap());

pub struct BookNameParser;

/// 一份书名里独立识别出的卷/章信号（都可能缺失）。
/// 规则：两者并存时这本书按章编号（卷只是分组信息），`chapter` 是有效序号。
#[derive(Debug, Clone, PartialEq)]
pub struct BookVolumeChapter {
    /// 卷号；与章节并存时只是分组信息，不作为该书的序号。
    pub volume: Option<BookRange>,
    /// 章节号；与卷并存时是该书的有效序号。
    pub chapter: Option<BookRange>,
}

impl BookNameParser {
    pub fn get_volumes(name: &str) -> Option<BookRange> {
        // 统一预处理：全角数字/字母/符号转半角（"第５巻" → "第5巻"、"Vol. ３" → "Vol. 3"）
        let name = replace_fullwidth_chars(name);
        for regex in VOLUME_REGEXES.iter() {
            if let Some(captures) = regex.captures(&name) {
                let start_volume = captures
                    .name("volumeStart")
                    .map(|m| m.as_str().replace(['x', '#'], ".").parse::<f64>().ok())
                    .flatten();
                let end_volume = captures
                    .name("volumeEnd")
                    .map(|m| {
                        m.as_str()
                            .replace('-', "")
                            .replace(['x', '#'], ".")
                            .parse::<f64>()
                            .ok()
                    })
                    .flatten();

                return match (start_volume, end_volume) {
                    (Some(start), Some(end)) => Some(BookRange::new(start, end)),
                    (Some(start), None) => Some(BookRange::single(start)),
                    _ => None,
                };
            }
        }
        None
    }

    pub fn get_chapters(name: &str) -> Option<BookRange> {
        Self::get_book_number_from(name, &CHAPTER_REGEXES)
    }

    /// 同时提取卷号与章节号。书名可能两者并存（"Vol.03 ch.12"、"第3卷 第12话"）。
    /// 注意语义：两者并存时这本书按章编号，`chapter` 才是有效序号——旧消费链的
    /// "卷优先"（`get_volumes().or_else(get_chapters)`）会把这类书错认成卷，
    /// 请改用 [`Self::get_primary_number`]。
    pub fn get_volumes_and_chapters(name: &str) -> BookVolumeChapter {
        BookVolumeChapter {
            volume: Self::get_volumes(name),
            chapter: Self::get_chapters(name),
        }
    }

    /// 书的有效序号：卷章并存时取章节（书按章编号，卷只是分组信息），
    /// 纯卷名取卷号，两者都没有时返回 None。
    pub fn get_primary_number(name: &str) -> Option<BookRange> {
        let parsed = Self::get_volumes_and_chapters(name);
        parsed.chapter.or(parsed.volume)
    }

    pub fn get_book_number(name: &str) -> Option<BookRange> {
        Self::get_book_number_from(name, &BOOK_NUMBER_REGEXES)
    }

    fn get_book_number_from(name: &str, regexes: &[Regex]) -> Option<BookRange> {
        // 统一预处理：全角数字/字母转半角（"Chapter １０" → "Chapter 10"）
        let name = replace_fullwidth_chars(name);
        for regex in regexes.iter() {
            // 对应 Kotlin: findAll(name).lastOrNull()
            let last = regex.captures_iter(&name).last();
            if let Some(captures) = last {
                let start = captures
                    .name("start")
                    .map(|m| m.as_str().replace(['x', '#'], ".").parse::<f64>().ok())
                    .flatten();
                let end = captures
                    .name("end")
                    .map(|m| {
                        m.as_str()
                            .replace('-', "")
                            .replace(['x', '#'], ".")
                            .parse::<f64>()
                            .ok()
                    })
                    .flatten();

                return match (start, end) {
                    (Some(start), Some(end)) => Some(BookRange::new(start, end)),
                    (Some(start), None) => Some(BookRange::single(start)),
                    _ => None,
                };
            }
        }
        None
    }

    pub fn get_extra_data(name: &str) -> Vec<String> {
        EXTRA_DATA_REGEX
            .captures_iter(name)
            .filter_map(|c| c.name("extra").map(|m| m.as_str().to_string()))
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 章节前缀空格可选并支持 chap.："Chap.001" / "ch.12" / "ep.3" / "chapter5"。
    #[test]
    fn parses_chapter_prefix_without_space() {
        assert_eq!(
            BookNameParser::get_chapters("Chap.001"),
            Some(BookRange::single(1.0))
        );
        assert_eq!(
            BookNameParser::get_chapters("Some Series ch.12"),
            Some(BookRange::single(12.0))
        );
        assert_eq!(
            BookNameParser::get_chapters("Some Series ep.3"),
            Some(BookRange::single(3.0))
        );
        assert_eq!(
            BookNameParser::get_chapters("Some Series chapter5"),
            Some(BookRange::single(5.0))
        );
    }

    /// 无「第」前缀的章节后缀："001-100话" / "5話" / "1-3章"（含全角数字）。
    #[test]
    fn parses_chapter_suffix_without_dai() {
        for (name, expected) in [
            ("[王牌御史] 001-100话", BookRange::new(1.0, 100.0)),
            ("标题 5話", BookRange::single(5.0)),
            ("５话", BookRange::single(5.0)),
            ("标题 1-3章", BookRange::new(1.0, 3.0)),
        ] {
            assert_eq!(
                BookNameParser::get_chapters(name),
                Some(expected),
                "name: {name}"
            );
        }
        // 「第N話」仍走原有正则，不受新规则影响
        assert_eq!(
            BookNameParser::get_chapters("第10話"),
            Some(BookRange::single(10.0))
        );
        // 边界：不以話/章结尾的章节名不误解析
        assert_eq!(BookNameParser::get_chapters("12話5"), None);
    }

    /// 无「第」前缀的 CJK 卷号：「数字+巻卷册冊集」结尾与「巻卷册冊集+数字」结尾两种形态。
    #[test]
    fn parses_cjk_volume_without_dai() {
        // 数字 + 巻/卷/册/冊/集 结尾（无「第」）
        for (name, expected) in [
            ("ワンピース 5巻", BookRange::single(5.0)),
            ("标题 12卷", BookRange::single(12.0)),
            ("标题 5册", BookRange::single(5.0)),
            ("标题 1-3冊", BookRange::new(1.0, 3.0)),
            ("标题 2集", BookRange::single(2.0)),
            ("5巻", BookRange::single(5.0)),
            ("５巻", BookRange::single(5.0)),
        ] {
            assert_eq!(
                BookNameParser::get_volumes(name),
                Some(expected),
                "name: {name}"
            );
        }
        // 巻/卷/册/冊/集 + 数字 结尾（前置形态）
        for (name, expected) in [
            ("巻5", BookRange::single(5.0)),
            ("标题巻05", BookRange::single(5.0)),
            ("标题 卷1-2", BookRange::new(1.0, 2.0)),
            ("全冊3", BookRange::single(3.0)),
        ] {
            assert_eq!(
                BookNameParser::get_volumes(name),
                Some(expected),
                "name: {name}"
            );
        }
        // 「第N巻」仍走原有正则，不受新规则影响
        assert_eq!(
            BookNameParser::get_volumes("第5巻"),
            Some(BookRange::single(5.0))
        );
        // 边界：「12巻5」不以前置形态误解析（避免吃掉更大数字的一部分）
        assert_eq!(BookNameParser::get_volumes("12巻5"), None);
    }

    /// vol./vols./volume 前缀后的空格为可选（"Vol.01" / "vols.1-2" / "volume2"）：
    /// Komga/kmrs 扫描器普遍产出 "他国日记 Vol.01" 这类无空格书名，旧正则要求
    /// 空格导致 get_volumes 返回 None → associate_book_metadata 卷号匹配全部失败
    /// （bangumi 单行本侧 "違国日記 (1)" 解析正常，两侧不对称）。
    #[test]
    fn parses_volume_prefix_without_space() {
        assert_eq!(
            BookNameParser::get_volumes("他国日记 Vol.01"),
            Some(BookRange::single(1.0))
        );
        assert_eq!(
            BookNameParser::get_volumes("ARMS神臂 愛藏版 Vol.03"),
            Some(BookRange::single(3.0))
        );
        assert_eq!(
            BookNameParser::get_volumes("Vol.01 仰望巨人的少女"),
            Some(BookRange::single(1.0))
        );
        assert_eq!(
            BookNameParser::get_volumes("My Series Vol.1-2"),
            Some(BookRange::new(1.0, 2.0))
        );
        assert_eq!(
            BookNameParser::get_volumes("My Series vols.1-2"),
            Some(BookRange::new(1.0, 2.0))
        );
        assert_eq!(
            BookNameParser::get_volumes("My Series volume2"),
            Some(BookRange::single(2.0))
        );
    }

    #[test]
    fn parses_volumes() {
        assert_eq!(
            BookNameParser::get_volumes("My Series v1"),
            Some(BookRange::single(1.0))
        );
        assert_eq!(
            BookNameParser::get_volumes("My Series Vol. 3"),
            Some(BookRange::single(3.0))
        );
        assert_eq!(
            BookNameParser::get_volumes("My Series Vol. 1-2"),
            Some(BookRange::new(1.0, 2.0))
        );
        assert_eq!(
            BookNameParser::get_volumes("僕のヒーローアカデミア 第5巻"),
            Some(BookRange::single(5.0))
        );
    }

    /// 扩展字符（巻|卷|册|冊）与中间空格
    #[test]
    fn parses_cjk_volume_variants() {
        for name in [
            "第5巻",
            "第5卷",
            "第5册",
            "第5冊",
            "第 5 巻",
            "第 5卷",
            "第5 册",
            "第 1 - 3 巻",
        ] {
            let range = BookNameParser::get_volumes(name);
            let expected = if name.contains("1") && name.contains("3") {
                BookRange::new(1.0, 3.0)
            } else {
                BookRange::single(5.0)
            };
            assert_eq!(range, Some(expected), "name: {name}");
        }
    }

    /// 扩展字符（話|话|章|节）与中间空格
    #[test]
    fn parses_cjk_chapter_variants() {
        for name in [
            "第10話",
            "第10话",
            "第10章",
            "第10节",
            "第 10 章",
            "第 10话",
            "第 1 - 3 章",
        ] {
            let range = BookNameParser::get_chapters(name);
            let expected = if name.contains("1") && name.contains("3") {
                BookRange::new(1.0, 3.0)
            } else {
                BookRange::single(10.0)
            };
            assert_eq!(range, Some(expected), "name: {name}");
        }
    }

    #[test]
    fn parses_chapters() {
        assert_eq!(
            BookNameParser::get_chapters("Some Series c10"),
            Some(BookRange::single(10.0))
        );
        assert_eq!(
            BookNameParser::get_chapters("Some Series Chapter 10-12"),
            Some(BookRange::new(10.0, 12.0))
        );
    }

    #[test]
    fn parses_book_numbers() {
        assert_eq!(
            BookNameParser::get_book_number("Some Series #5"),
            Some(BookRange::single(5.0))
        );
        assert_eq!(
            BookNameParser::get_book_number("Some Series Issue 5"),
            Some(BookRange::single(5.0))
        );
    }

    /// 全角数字统一转半角后提取（卷号/章节号/书号共用预处理）
    #[test]
    fn parses_fullwidth_digits() {
        // 卷号
        assert_eq!(
            BookNameParser::get_volumes("僕のヒーローアカデミア 第５巻"),
            Some(BookRange::single(5.0))
        );
        assert_eq!(
            BookNameParser::get_volumes("My Series Vol. ３"),
            Some(BookRange::single(3.0))
        );
        assert_eq!(
            BookNameParser::get_volumes("My Series Vol. １-２"),
            Some(BookRange::new(1.0, 2.0))
        );
        // 章节号
        assert_eq!(
            BookNameParser::get_chapters("Some Series 第１０話"),
            Some(BookRange::single(10.0))
        );
        assert_eq!(
            BookNameParser::get_chapters("Some Series Chapter １０-１２"),
            Some(BookRange::new(10.0, 12.0))
        );
        // 书号
        assert_eq!(
            BookNameParser::get_book_number("Some Series #５"),
            Some(BookRange::single(5.0))
        );
    }

    #[test]
    fn parses_extra_data() {
        assert_eq!(
            BookNameParser::get_extra_data("Series [Omnibus]"),
            vec!["Omnibus"]
        );
    }
}

/// 卷/章并存的组合提取：与消费链的"卷优先"短路不同，两个信号都要保留。
#[test]
fn extracts_volume_and_chapter_together() {
    let both = BookNameParser::get_volumes_and_chapters("Series Vol.03 ch.12");
    assert_eq!(both.volume, Some(BookRange::single(3.0)));
    assert_eq!(both.chapter, Some(BookRange::single(12.0)));

    let cjk = BookNameParser::get_volumes_and_chapters("第3卷 第12话");
    assert_eq!(cjk.volume, Some(BookRange::single(3.0)));
    assert_eq!(cjk.chapter, Some(BookRange::single(12.0)));

    // 范围取两端 end：读完 1-2 卷、10-12 话 = 卷 2、话 12
    let ranges = BookNameParser::get_volumes_and_chapters("Vol.1-2 ch.10-12");
    assert_eq!(ranges.volume, Some(BookRange::new(1.0, 2.0)));
    assert_eq!(ranges.chapter, Some(BookRange::new(10.0, 12.0)));
}

#[test]
fn combined_extraction_keeps_single_signal_and_absence() {
    let volume_only = BookNameParser::get_volumes_and_chapters("Series Vol.03");
    assert_eq!(volume_only.volume, Some(BookRange::single(3.0)));
    assert_eq!(volume_only.chapter, None);

    let chapter_only = BookNameParser::get_volumes_and_chapters("Series ch.012");
    assert_eq!(chapter_only.volume, None);
    assert_eq!(chapter_only.chapter, Some(BookRange::single(12.0)));

    let neither = BookNameParser::get_volumes_and_chapters("Some Book");
    assert_eq!(neither.volume, None);
    assert_eq!(neither.chapter, None);
}
/// 有效序号：卷章并存时按章编号（卷只是分组），纯卷名取卷号。
#[test]
fn primary_number_is_chapter_when_both_present() {
    assert_eq!(
        BookNameParser::get_primary_number("Series Vol.03 ch.12"),
        Some(BookRange::single(12.0))
    );
    assert_eq!(
        BookNameParser::get_primary_number("第3卷 第12话"),
        Some(BookRange::single(12.0))
    );
    assert_eq!(
        BookNameParser::get_primary_number("Series Vol.03"),
        Some(BookRange::single(3.0))
    );
    assert_eq!(
        BookNameParser::get_primary_number("Series ch.012"),
        Some(BookRange::single(12.0))
    );
    assert_eq!(BookNameParser::get_primary_number("Some Book"), None);
}
