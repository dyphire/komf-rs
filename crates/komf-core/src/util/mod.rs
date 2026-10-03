//! 工具模块 —— 对应 `snd.komf.util` 包。

pub mod analyzer;
pub mod book_name_parser;
pub mod chinese;
pub mod download;
pub mod heavy_pool;
pub mod name_similarity;
pub mod natural_comparator;
pub mod string_utils;

pub use analyzer::droppable_floor;
pub use analyzer::{index_analyze_terms, normalize, search_analyze, t2s_str};
pub use book_name_parser::BookNameParser;
pub use chinese::{ChineseConverter, ChineseDirection};
pub use name_similarity::{similarity_score, NameSimilarityMatcher};
pub use natural_comparator::{case_insensitive_nat_sort, SimpleNaturalComparator};
pub use string_utils::{
    bracket_search_term, normalize_search_text, replace_fullwidth_chars, strip_accents,
};
