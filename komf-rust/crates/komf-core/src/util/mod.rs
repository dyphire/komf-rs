//! 工具模块 —— 对应 `snd.komf.util` 包。

pub mod book_name_parser;
pub mod chinese;
pub mod name_similarity;
pub mod natural_comparator;
pub mod string_utils;

pub use book_name_parser::BookNameParser;
pub use chinese::{ChineseConverter, ChineseDirection};
pub use name_similarity::NameSimilarityMatcher;
pub use natural_comparator::{case_insensitive_nat_sort, SimpleNaturalComparator};
pub use string_utils::{replace_fullwidth_chars, strip_accents};
