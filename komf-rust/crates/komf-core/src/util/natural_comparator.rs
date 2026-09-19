//! 自然排序比较器 —— 对应 `SimpleNaturalComparator.kt`。
use std::cmp::Ordering;

/// 不区分大小写的自然排序比较器。
#[derive(Debug, Clone, Copy, Default)]
pub struct SimpleNaturalComparator;

impl SimpleNaturalComparator {
    pub fn compare(&self, lhs: &str, rhs: &str) -> Ordering {
        natural_compare(lhs, rhs)
    }
}

/// 对应 Kotlin `caseInsensitiveNatSortComparator()`。
pub fn case_insensitive_nat_sort(a: &str, b: &str) -> Ordering {
    natural_compare(a, b)
}

fn natural_compare(lhs: &str, rhs: &str) -> Ordering {
    let lhs: Vec<char> = lhs.to_lowercase().chars().collect();
    let rhs: Vec<char> = rhs.to_lowercase().chars().collect();

    let (mut i, mut j) = (0, 0);
    while i < lhs.len() && j < rhs.len() {
        let l = lhs[i];
        let r = rhs[j];

        if l.is_ascii_digit() && r.is_ascii_digit() {
            // 收集完整数字段
            let mut li = i;
            while li < lhs.len() && lhs[li].is_ascii_digit() {
                li += 1;
            }
            let mut rj = j;
            while rj < rhs.len() && rhs[rj].is_ascii_digit() {
                rj += 1;
            }
            let lnum: String = lhs[i..li].iter().collect();
            let rnum: String = rhs[j..rj].iter().collect();

            // 去掉前导零再比较
            let ltrim = lnum.trim_start_matches('0');
            let rtrim = rnum.trim_start_matches('0');
            let ord = ltrim.len().cmp(&rtrim.len()).then_with(|| ltrim.cmp(rtrim));
            if ord != Ordering::Equal {
                return ord;
            }
            // 数字相等则比较长度（保留前导零顺序的稳定性）
            let ord_len = lnum.len().cmp(&rnum.len());
            if ord_len != Ordering::Equal {
                return ord_len;
            }
            i = li;
            j = rj;
        } else {
            if l != r {
                return l.cmp(&r);
            }
            i += 1;
            j += 1;
        }
    }

    lhs.len().cmp(&rhs.len())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::cmp::Ordering;

    #[test]
    fn natural_order() {
        let mut books = vec!["Book 10", "Book 2", "Book 1"];
        books.sort_by(|a, b| case_insensitive_nat_sort(a, b));
        assert_eq!(books, vec!["Book 1", "Book 2", "Book 10"]);
    }

    #[test]
    fn case_insensitive() {
        assert_eq!(case_insensitive_nat_sort("apple", "Banana"), Ordering::Less);
    }
}
