//! 名称相似度匹配 —— 对应 `NameSimilarityMatcher.kt`。
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
pub enum NameSimilarityMatcher {
    Exact,
    #[default]
    ClosestMatch,
}

impl NameSimilarityMatcher {
    pub fn matches(&self, name: &str, names_to_match: &[String]) -> bool {
        names_to_match
            .iter()
            .any(|candidate| self.matches_single(name, candidate))
    }

    pub fn matches_single(&self, name: &str, name_to_match: &str) -> bool {
        let name_len = name.chars().count();
        if matches!(self, NameSimilarityMatcher::Exact) || (1..=3).contains(&name_len) {
            return name == name_to_match;
        }

        let distance = levenshtein(&name.to_uppercase(), &name_to_match.to_uppercase());
        let threshold = match name_len {
            4..=6 => 1,
            7..=9 => 2,
            _ => 3,
        };
        distance <= threshold
    }
}

/// 经典两行动态规划 Levenshtein 距离。
pub fn levenshtein(lhs: &str, rhs: &str) -> usize {
    let lhs_chars: Vec<char> = lhs.chars().collect();
    let rhs_chars: Vec<char> = rhs.chars().collect();

    if lhs_chars == rhs_chars {
        return 0;
    }
    if lhs_chars.is_empty() {
        return rhs_chars.len();
    }
    if rhs_chars.is_empty() {
        return lhs_chars.len();
    }

    let lhs_len = lhs_chars.len();
    let rhs_len = rhs_chars.len();

    let mut cost: Vec<usize> = (0..=lhs_len).collect();
    let mut new_cost: Vec<usize> = vec![0; lhs_len + 1];

    for i in 1..=rhs_len {
        new_cost[0] = i;
        for j in 1..=lhs_len {
            let match_cost = if lhs_chars[j - 1] == rhs_chars[i - 1] {
                0
            } else {
                1
            };
            let cost_replace = cost[j - 1] + match_cost;
            let cost_insert = cost[j] + 1;
            let cost_delete = new_cost[j - 1] + 1;
            new_cost[j] = cost_insert.min(cost_delete).min(cost_replace);
        }
        std::mem::swap(&mut cost, &mut new_cost);
    }

    cost[lhs_len]
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn exact_match() {
        let matcher = NameSimilarityMatcher::Exact;
        assert!(matcher.matches_single("One Piece", "One Piece"));
        assert!(!matcher.matches_single("One Piece", "One Piece!"));

        let matcher = NameSimilarityMatcher::ClosestMatch;
        assert!(matcher.matches_single("Berserk", "Berserku"));
    }

    #[test]
    fn short_names_require_exact() {
        let matcher = NameSimilarityMatcher::ClosestMatch;
        assert!(matcher.matches_single("Nar", "Nar"));
        assert!(!matcher.matches_single("Nar", "Nart"));
    }

    #[test]
    fn levenshtein_basics() {
        assert_eq!(levenshtein("kitten", "sitting"), 3);
        assert_eq!(levenshtein("", "abc"), 3);
        assert_eq!(levenshtein("abc", "abc"), 0);
    }
}
