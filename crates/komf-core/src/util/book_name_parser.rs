//! 书名解析 —— 对应 `BookNameParser.kt`。
use crate::model::BookRange;
use crate::util::string_utils::replace_fullwidth_chars;
use once_cell::sync::Lazy;
use regex::Regex;

static VOLUME_REGEXES: Lazy<Vec<Regex>> = Lazy::new(|| {
    vec![
        Regex::new(r"(?i)(?:^|,?\s)\(?volume\s(?<volumeStart>[0-9]+)(,?\s?[0-9]+,)+(?<volumeEnd>\s?[0-9]+)\)?").unwrap(),
        Regex::new(r"(?i)(?:^|,?\s)\(?([vtT]|vols\.\s|vol\.\s|volume\s)(?<volumeStart>[0-9]+([.x#][0-9]+)?)(?<volumeEnd>-[0-9]+([.x#][0-9]+)?)?\)?").unwrap(),
        Regex::new(r".*第\s*(?<volumeStart>\d+)\s*-?\s*(?<volumeEnd>\d+)?\s*[巻卷册冊集]").unwrap(),
        Regex::new(r".*年(?:[0-9]+月)?(?:[0-9]+日)?(?<volumeStart>\d+)-?(?<volumeEnd>\d+)?号").unwrap(),
    ]
});

static CHAPTER_REGEXES: Lazy<Vec<Regex>> = Lazy::new(|| {
    vec![
        Regex::new(r"(?i)(?:^|\s?)(c|ch\.\s|chapter\s|ep\.\s)(?<start>[0-9]+([.x#][0-9]+)?)(?<end>-[0-9]+([.x#][0-9]+)?)?").unwrap(),
        Regex::new(r".*第\s*(?<start>\d+(?:\.\d+)?)\s*-?\s*(?<end>\d+(?:\.\d+)?)?\s*[話话章节]").unwrap(),
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
