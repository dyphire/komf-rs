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

/// 多信号标题相似度评分（0.0~1.0，越大越相似）：取候选标题中的最高分。
///
/// 信号从强到弱（按顺序短路）：
/// 1. 归一化后完全相等 → 1.0；
/// 2. 前缀（短侧 ≥3 字符且为长侧前缀）→ 0.9；
/// 3. 子串（短侧 ≥4 字符且被长侧包含，非前缀）→ 0.85；
/// 4. 字符 trigram Dice 系数（短侧 ≥3 字符）→ 0.5 + 0.4·dice；
/// 5. 归一化编辑距离 1 - dist/max_len 兜底。
///
/// 仅用于候选召回层的排序（评分，不做匹配判定）；
/// 匹配判定始终由 [`NameSimilarityMatcher`] 负责（尊重 nameMatchingMode 配置）。
pub fn similarity_score(name: &str, names_to_match: &[String]) -> f32 {
    names_to_match
        .iter()
        .map(|t| score_single(name, t))
        .fold(0.0_f32, f32::max)
}

fn score_single(name: &str, title: &str) -> f32 {
    let q = name.to_uppercase();
    let t = title.to_uppercase();
    if q == t {
        return 1.0;
    }
    let ql = q.chars().count();
    let tl = t.chars().count();
    let (short, long) = if ql <= tl {
        (q.as_str(), t.as_str())
    } else {
        (t.as_str(), q.as_str())
    };
    let sl = short.chars().count();
    // 前缀：短侧是长侧的前缀（防误配：短侧至少 3 字符）
    if sl >= 3 && long.starts_with(short) {
        return 0.9;
    }
    // 子串：短侧被长侧包含且不是前缀（至少 4 字符）
    if sl >= 4 && long.contains(short) {
        return 0.85;
    }
    // trigram Dice：对短侧与长侧局部重合的标题给出连续分值
    if sl >= 3 {
        let dice = trigram_dice(short, long);
        if dice > 0.0 {
            return 0.5 + 0.4 * dice;
        }
    }
    // 兜底：归一化编辑距离
    let dist = levenshtein(&q, &t);
    1.0 - dist as f32 / ql.max(tl).max(1) as f32
}

fn trigrams(s: &str) -> Vec<String> {
    let chars: Vec<char> = s.chars().collect();
    chars
        .windows(3)
        .map(|w| w.iter().collect::<String>())
        .collect()
}

fn trigram_dice(lhs: &str, rhs: &str) -> f32 {
    let a = trigrams(lhs);
    let b = trigrams(rhs);
    if a.is_empty() || b.is_empty() {
        return 0.0;
    }
    let inter = a.iter().filter(|x| b.contains(x)).count();
    2.0 * inter as f32 / (a.len() + b.len()) as f32
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

    #[test]
    fn similarity_score_prefers_stronger_signal() {
        // 完全相等 → 1.0
        let s = similarity_score("青春猪头少年", &["青春猪头少年".to_string()]);
        assert!((s - 1.0).abs() < 1e-6);
        // 前缀 → 0.9
        let s = similarity_score("青春猪头少年", &["青春猪头少年系列".to_string()]);
        assert!((s - 0.9).abs() < 1e-6);
        // 长标题前缀同样 0.9
        let s = similarity_score(
            "青春猪头少年",
            &["青春猪头少年不会梦到兔女郎学姐".to_string()],
        );
        assert!((s - 0.9).abs() < 1e-6);
        // 仅 trigram 重合（轮转/局部重叠）："ABCDEF" × "CDEFAB" 交集 {cde,def}=2，
        // Dice = 2*2/(4+4)=0.5 → score 0.7
        let s = similarity_score("ABCDEF", &["CDEFAB".to_string()]);
        assert!((s - 0.7).abs() < 1e-6);
        // 不相似：分数远低于 0.6（排序靠后，但不影响候选返回）
        let s = similarity_score(
            "青春猪头少年",
            &["青春笨蛋少年不做理性小魔女的梦".to_string()],
        );
        assert!(s < 0.6);
    }
}
