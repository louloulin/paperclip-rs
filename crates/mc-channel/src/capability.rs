//! `Capability` 位图（上游 `server/internal/integrations/channel/capability.go`）。
//!
//! - **写者**：M7-0 建（本 anchor）；**M7-1** 收紧 + 补 `String()` 稳定性用例。
//! - **语义**：位图是 Channel 的**声明**，本包**不含任何降级逻辑** —— 想要"没有富卡片时
//!   退化成纯文本"的调用方自己读位图决定，这样新增一个平台永远不必往核心里加分支。
//!
//! 零值（[`Capability::empty`]）声明"什么都不支持"。

use std::fmt;

/// 能力位（`u64`：上游用 `uint64` 留足高位；8 个已用位见下表）。
///
/// | 位 | 名称 | 含义 |
/// | --- | --- | --- |
/// | 0 | `text` | 能发纯文本（**每个** Channel 至少应声明它） |
/// | 1 | `rich_card` | 能渲染富/互动卡片（Lark 互动卡、Slack Block Kit…） |
/// | 2 | `thread_reply` | 能往线程/话题里回帖 |
/// | 3 | `quote_reply` | 能引用回复某条具体消息 |
/// | 4 | `attachment` | 能收/发媒体附件 |
/// | 5 | `voice` | 能处理语音/音频消息 |
/// | 6 | `typing_indicator` | 能显示"正在输入/思考"指示 |
/// | 7 | `message_edit` | 能改已发出的消息（Lark 卡片 patch、Slack `chat.update`…） |
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Default)]
pub struct Capability(u64);

impl Capability {
    /// 纯文本。
    pub const TEXT: Self = Self(1 << 0);
    /// 富/互动卡片。
    pub const RICH_CARD: Self = Self(1 << 1);
    /// 线程回帖。
    pub const THREAD_REPLY: Self = Self(1 << 2);
    /// 引用回复。
    pub const QUOTE_REPLY: Self = Self(1 << 3);
    /// 媒体附件。
    pub const ATTACHMENT: Self = Self(1 << 4);
    /// 语音/音频。
    pub const VOICE: Self = Self(1 << 5);
    /// 打字/思考指示。
    pub const TYPING_INDICATOR: Self = Self(1 << 6);
    /// 消息编辑（卡片 patch / `chat.update`）。
    pub const MESSAGE_EDIT: Self = Self(1 << 7);

    /// 全部八个已定义的位（各片 `DoD` 的"8 位"就指这个集合）。
    pub const ALL: [Self; 8] = [
        Self::TEXT,
        Self::RICH_CARD,
        Self::THREAD_REPLY,
        Self::QUOTE_REPLY,
        Self::ATTACHMENT,
        Self::VOICE,
        Self::TYPING_INDICATOR,
        Self::MESSAGE_EDIT,
    ];

    /// `String()` 的名字表：**顺序 = 位序（从低位到高位）**，`String()` 按它输出。
    ///
    /// 名字是**稳定对外口径**：改字符串 = 改日志/诊断的可读性，M7-1 有用例钉住。
    const NAMES: [(&'static str, Self); 8] = [
        ("text", Self::TEXT),
        ("rich_card", Self::RICH_CARD),
        ("thread_reply", Self::THREAD_REPLY),
        ("quote_reply", Self::QUOTE_REPLY),
        ("attachment", Self::ATTACHMENT),
        ("voice", Self::VOICE),
        ("typing_indicator", Self::TYPING_INDICATOR),
        ("message_edit", Self::MESSAGE_EDIT),
    ];

    /// 零值：什么都不声明。
    pub const fn empty() -> Self {
        Self(0)
    }

    /// 裸位值（与位图运算/持久化打交道时用）。
    pub const fn bits(self) -> u64 {
        self.0
    }

    /// 从裸位值构造（未知高位**保留**：它们会出现在 `Display` 的十六进制余数里，
    /// 而不是被静默丢掉）。
    pub const fn from_bits(bits: u64) -> Self {
        Self(bits)
    }

    /// 组合（`|` 运算符的具名形态；`BitOr` 也实现了）。
    #[must_use]
    pub const fn union(self, other: Self) -> Self {
        Self(self.0 | other.0)
    }

    /// 判断是否**包含 want 的每一位**（上游 `Has`：是"全都包含"，不是"包含任一"）。
    ///
    /// `has(Capability::empty())` 恒为 `true`（空要求总被满足）。
    pub const fn has(self, want: Self) -> bool {
        self.0 & want.0 == want.0
    }

    /// 是否声明了任意一位。
    pub const fn is_empty(self) -> bool {
        self.0 == 0
    }
}

impl std::ops::BitOr for Capability {
    type Output = Self;

    fn bitor(self, other: Self) -> Self {
        self.union(other)
    }
}

impl std::ops::BitOrAssign for Capability {
    fn bitor_assign(&mut self, other: Self) {
        *self = self.union(other);
    }
}

impl fmt::Display for Capability {
    /// 把置位的位渲染成 `|` 连接的名字表（`text|thread_reply`）；零值渲染 `none`；
    /// **未知高位**以 `0x…` 余数追加，免得被遗忘的名字从日志里静默消失（上游注释的意图）。
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        if self.0 == 0 {
            return formatter.write_str("none");
        }
        let mut parts: Vec<String> = Vec::new();
        let mut remaining = self.0;
        for (name, bit) in Self::NAMES {
            if remaining & bit.0 == bit.0 {
                parts.push(name.to_string());
                remaining &= !bit.0;
            }
        }
        // 未知高位以十六进制余数**追加**（不是替换）：只剩未知位时输出就是 `0x…` 本身。
        if remaining != 0 {
            parts.push(format!("0x{remaining:x}"));
        }
        formatter.write_str(&parts.join("|"))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 八个位互不相同、两两不重叠，且 `ALL` 齐备（"8 位"是本片 `DoD` 的字面要求）。
    #[test]
    fn eight_distinct_bits() {
        assert_eq!(Capability::ALL.len(), 8);
        let mut seen = 0_u64;
        for bit in Capability::ALL {
            assert_eq!(seen & bit.bits(), 0, "位重叠：{bit}");
            seen |= bit.bits();
        }
        assert_eq!(seen, 0xff, "八个已用位必须是低 8 位");
        assert!(Capability::empty().is_empty());
        assert_eq!(Capability::empty().bits(), 0);
    }

    /// `has` 是"全都包含"（不是"任一"），且空要求恒真。
    #[test]
    fn has_is_includes_all() {
        let both = Capability::TEXT | Capability::THREAD_REPLY;
        assert!(both.has(Capability::TEXT));
        assert!(both.has(Capability::THREAD_REPLY));
        assert!(both.has(Capability::TEXT | Capability::THREAD_REPLY));
        assert!(!both.has(Capability::RICH_CARD));
        assert!(!both.has(Capability::TEXT | Capability::RICH_CARD));
        assert!(both.has(Capability::empty()), "空要求总被满足");
        assert!(!Capability::empty().has(Capability::TEXT));
    }

    /// `String()` 的名字表与位序（对外稳定口径）。
    #[test]
    fn display_names_are_stable() {
        assert_eq!(Capability::empty().to_string(), "none");
        assert_eq!(Capability::TEXT.to_string(), "text");
        assert_eq!(Capability::MESSAGE_EDIT.to_string(), "message_edit");
        assert_eq!(
            (Capability::THREAD_REPLY | Capability::TEXT).to_string(),
            "text|thread_reply",
            "低位在前"
        );
        assert_eq!(
            (Capability::TEXT
                | Capability::RICH_CARD
                | Capability::THREAD_REPLY
                | Capability::QUOTE_REPLY
                | Capability::ATTACHMENT
                | Capability::VOICE
                | Capability::TYPING_INDICATOR
                | Capability::MESSAGE_EDIT)
                .to_string(),
            "text|rich_card|thread_reply|quote_reply|attachment|voice|typing_indicator|message_edit"
        );
        // 未知高位不被静默丢掉。
        let unknown = Capability::from_bits(1 << 20);
        assert_eq!(unknown.to_string(), "0x100000");
        assert_eq!(
            Capability::from_bits((1 << 20) | 1).to_string(),
            "text|0x100000"
        );
    }

    /// 位运算与 `From`/`bits` 往返。
    #[test]
    fn bit_arithmetic_round_trips() {
        let mut flags = Capability::empty();
        flags |= Capability::ATTACHMENT;
        flags |= Capability::VOICE;
        assert_eq!(flags.bits(), (1 << 4) | (1 << 5));
        assert_eq!(Capability::from_bits(flags.bits()), flags);
        assert_eq!(
            Capability::from_bits(flags.bits()).to_string(),
            "attachment|voice"
        );
    }
}
