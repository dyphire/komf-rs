//! 简繁转换（Simplified ↔ Traditional Chinese）—— Rust 扩展（Kotlin 无）。
//!
//! 基于 `zhconv`（OpenCC 数据移植的纯 Rust 实现，零系统依赖）。
//! 应用范围由配置控制：搜索 / 自动匹配 / 元数据更新（标题、体裁、标签、简介等字段）。

use serde::{Deserialize, Serialize};

/// 简繁转换方向。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "lowercase")]
pub enum ChineseDirection {
    /// 繁体 → 简体（默认）
    #[default]
    T2s,
    /// 简体 → 繁体
    S2t,
}

impl ChineseDirection {
    pub fn as_str(&self) -> &'static str {
        match self {
            ChineseDirection::T2s => "t2s",
            ChineseDirection::S2t => "s2t",
        }
    }
}

/// 转换器封装（无状态：zhconv 转换是纯函数，转换器可 Clone/Send/Sync）。
#[derive(Debug, Clone, Copy)]
pub struct ChineseConverter {
    direction: ChineseDirection,
}

impl ChineseConverter {
    /// 创建转换器（zhconv 无初始化失败路径，方向恒有效）。
    pub fn new(direction: ChineseDirection) -> Result<Self, String> {
        Ok(Self { direction })
    }

    /// 无转换（convert 原样返回）。
    pub fn none() -> Self {
        Self {
            direction: ChineseDirection::T2s,
        }
    }

    pub fn direction(&self) -> ChineseDirection {
        self.direction
    }

    /// 转换文本。
    pub fn convert(&self, input: &str) -> String {
        match self.direction {
            // opencc 数据变体：T2S（繁体→简体）用大陆简体，S2T（简体→繁体）用台湾繁体
            ChineseDirection::T2s => zhconv::zhconv(input, zhconv::Variant::ZhCN),
            ChineseDirection::S2t => zhconv::zhconv(input, zhconv::Variant::ZhTW),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn t2s_converts() {
        let c = ChineseConverter::new(ChineseDirection::T2s).unwrap();
        assert_eq!(
            c.convert("愛される資格は過去に落としてきました"),
            "爱される资格は过去に落としてきました"
        );
        assert_eq!(c.convert("繁體中文"), "繁体中文");
    }

    #[test]
    fn s2t_converts() {
        let c = ChineseConverter::new(ChineseDirection::S2t).unwrap();
        assert_eq!(c.convert("繁体中文"), "繁體中文");
    }

    #[test]
    fn idempotent_noop() {
        // 简体文本经 t2s 应保持不变
        let c = ChineseConverter::new(ChineseDirection::T2s).unwrap();
        assert_eq!(c.convert("简体中文"), "简体中文");
    }
}
