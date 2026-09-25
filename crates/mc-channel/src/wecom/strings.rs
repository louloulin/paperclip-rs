//! `WeCom` 的**文案与语言**（上游 `internal/integrations/wecom/strings.go` 170 行 +
//! `language.go` 95 行，两文件合一）。
//!
//! - **写者**：M7-15（`docs/60-M7-PLAN.md` §3.3）。
//! - **本文件只装"气泡自己的文案"**（上游逐字）：本 adapter 说的**其它一切** —— 离线与归档
//!   通知、绑定提示、收件箱卡片的标签 —— 仍然是各自调用点的中文字面量。它们被翻译时再搬进来；
//!   本文件**不**要求它们先搬。
//! - **一个气泡用哪套文案由**目的地**决定，不由安装决定**：1:1 用那个人的 Multica
//!   档案语言，群聊（没有共享档案、也没有成员列表）用**部署自己**的语言。
//! - Slack 的 adapter 硬编码英文、Lark 的硬编码中文，所以本仓**没有**现成的 i18n 机制可加入。
//!   本文件**也刻意不是**一个：一个 locale → 一堆字符串的结构体。没有目录文件、没有消息 id、
//!   没有复数规则。若 `WeCom` 哪天需要一个有真实格式化规则的第三语言，那才是伸手拿框架的时刻。
//!
//! # 为什么是"一个结构体 per locale"而不是 `format!`
//!
//! [`CopyPack`] 的每个字段都是**整句**，没有哪句是拼出来的。流式气泡的每种收尾都带可见文字
//! （`WeCom` 会把它认为空白的收尾帧丢掉，气泡就永远转圈 —— `ws_frame.rs` 的 `hasVisibleChar`）
//! ⇒ 少一个字都是产品缺陷，而不是文案风格。
//!
//! # 与 `language.go` 的差异（登记 `docs/32` §31 的 D2）
//!
//! `language.go` 的另一半是**按目的地查语言**（`localeFor` / `localeForSender` /
//! `localeForUser` + `languageLookup` 端口）：它要 ①`chatTypeSingleInt`（那个常量在
//! `ws_frame.go` = **M7-16** 的写集）、②`channel_user_binding` / `user` 两条读（M7-19/20 的
//! 解析器面）。本片**不**自造第二份"这是不是 1:1"的真值 ⇒ 那三条留给 M7-19/20，
//! 本文件交付它们要用的**全部**纯函数（[`resolve_locale`] / [`deployment_locale`] /
//! [`copy_for`]），接缝就是 [`resolve_locale`] 的入参（一个档案语言字符串）。

use std::fmt;
use std::sync::atomic::{AtomicU8, Ordering};

/// 一个安装的用户被用什么语言回答（上游 `Locale`）。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum Locale {
    /// 中文（`WeCom` 是中文平台 ⇒ 编译期默认）。
    ZhHans,
    /// 英文（给档案语言不是中文的读者）。
    En,
}

impl Locale {
    /// 线上/日志用的取值（与上游两个常量**逐字**）。
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::ZhHans => "zh-Hans",
            Self::En => "en",
        }
    }

    /// 内部原子槽的编码（0 专留给"还没配过"⇒ 见 [`DEPLOYMENT_LOCALE`]）。
    const fn slot(self) -> u8 {
        match self {
            Self::ZhHans => 1,
            Self::En => 2,
        }
    }
}

impl fmt::Display for Locale {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(self.as_str())
    }
}

/// 编译期兜底：中文（`WeCom` 是中文平台）—— 一个什么都不说的部署拿到的就是它。
///
/// **读 [`deployment_locale`]，不要直接读它**：部署可以说话，而群聊的语言是**部署**的属性，
/// 不是"恰好开口的那个人"的属性。
pub const DEFAULT_LOCALE: Locale = Locale::ZhHans;

/// 部署语言的原子槽（0 = 还没配过）。
///
/// 启动时由 `MULTICA_WECOM_DEFAULT_LOCALE` 写一次、每条消息读 ⇒ 必须是原子而不是普通
/// `static mut`：`-race` 会把这次启动写与第一帧入站的读对上。`atomic.Value` 的本地等价物。
static DEPLOYMENT_LOCALE: AtomicU8 = AtomicU8::new(0);

/// 从一个**原始配置字符串**固定部署语言，并返回解析结果（调用方可以打日志）。
///
/// 认不出的值（含空串）**保持当前值不动**：环境变量里的一个笔误不该悄悄换掉一个租户的语言。
///
/// 它**刻意不是** [`resolve_locale`]：那个读的是**用户档案**字段，而 API 已经把它校验成
/// `en` / `zh-Hans` / `ko` / `ja`，所以它能把"不是中文"当成深思熟虑的选择。环境变量没有人校验过：
/// 在那个规则下 `MULTICA_WECOM_DEFAULT_LOCALE=zh_Hant` 或一个多余引号，就会把一个中文租户的
/// 群聊悄悄变成英文。所以这里**精确匹配**，写错的人得到旧语言与一条日志，而不是一个意外。
pub fn set_deployment_locale(raw: &str) -> Locale {
    let resolved = match raw.trim().to_lowercase().as_str() {
        "zh-hans" | "zh" => Some(Locale::ZhHans),
        "en" => Some(Locale::En),
        _ => None,
    };
    match resolved {
        Some(locale) => {
            DEPLOYMENT_LOCALE.store(locale.slot(), Ordering::SeqCst);
            locale
        }
        None => deployment_locale(),
    }
}

/// 当前部署语言；还没配过时是 [`DEFAULT_LOCALE`]（也是每个用例看到的值）。
#[must_use]
pub fn deployment_locale() -> Locale {
    match DEPLOYMENT_LOCALE.load(Ordering::SeqCst) {
        2 => Locale::En,
        _ => DEFAULT_LOCALE,
    }
}

/// 把**用户档案语言**映射到本 adapter 支持的语言（上游 `resolveLocale`，逐字三条）。
///
/// - 空值 ⇒ [`deployment_locale`]（**缺席不是选择**）；
/// - `zh*` ⇒ 中文；
/// - 其余一律英文：一个 `ko` / `ja` 用户是**深思熟虑地**选了"不是中文"，而英文是我们手上
///   给他们的通用语包。
#[must_use]
pub fn resolve_locale(profile_language: &str) -> Locale {
    let value = profile_language.trim().to_lowercase();
    if value.is_empty() {
        return deployment_locale();
    }
    if value.starts_with("zh") {
        Locale::ZhHans
    } else {
        Locale::En
    }
}

/// 一套语言的气泡文案（上游 `copyPack`）。
///
/// 流式回复以"不是答案"收尾的每一种方式都带**可见文字**（见模块文档）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CopyPack {
    /// 这一轮没有需要回复的内容。
    pub stream_no_reply: &'static str,
    /// 没有文字回复，但**有附件**（与上一条分开：上一条说"什么都不来"，而附件随后就到 ——
    /// 一个和下一句矛盾的气泡读起来像 bug，哪怕两半都在正常工作）。
    pub stream_no_reply_with_files: &'static str,
    /// 一次运行都没开始（agent 离线 / 归档，或入队失败）。
    pub stream_not_started: &'static str,
    /// 运行失败，且平台自己没给出原因（**带**原因的那种说原因）。
    pub stream_failed: &'static str,
    /// 用户自己停了这次运行（与失败分开是故意的：邀请重试一个别人刚停掉的东西，
    /// 读起来像机器人没注意到）。
    pub stream_cancelled: &'static str,
}

/// 中文（`zh-Hans`）包：一个中文租户读到的文字。
pub const ZH_HANS: CopyPack = CopyPack {
    stream_no_reply: "（这轮没有需要回复的内容）",
    stream_no_reply_with_files: "（这轮没有文字回复，附件在下面）",
    stream_not_started: "已收到，但这条暂时没能开始处理。",
    stream_failed: "⚠️ 这次没跑通，请稍后再试一次。",
    stream_cancelled: "⏹️ 这次处理已取消。",
};

/// 英文包：给档案语言不是中文的读者。
pub const EN: CopyPack = CopyPack {
    stream_no_reply: "(nothing to reply with this round)",
    stream_no_reply_with_files: "(no text this round — the files follow)",
    stream_not_started: "Got it, but this one couldn't start processing.",
    stream_failed: "⚠️ That run didn't go through. Please try again.",
    stream_cancelled: "⏹️ That run was cancelled.",
};

/// 取一套语言的气泡文案；未知语言退到当前部署语言（**不**报错：一个语言不对的气泡仍然
/// 说了一件有用的事，为语言选择把它丢掉更糟）。
#[must_use]
pub fn copy_for(locale: Locale) -> &'static CopyPack {
    match locale {
        Locale::ZhHans => &ZH_HANS,
        Locale::En => &EN,
    }
}

/// 目的地是不是"一个人"（上游 `localeFor` 的 `chatType != chatTypeSingleInt` 分支）。
///
/// 群聊（`false`）没有共享档案、也没有成员列表 ⇒ 用部署语言。**这是本文件唯一需要的
/// 目的地信息**，而"聊天类型"本身由 M7-16 的帧层给（见模块文档的 D2）。
#[must_use]
pub fn locale_for_destination(is_direct_message: bool, person_locale: Option<&str>) -> Locale {
    // 只有“是 1:1 **且**那个人的档案说了话”才看档案；其余（群聊、或 1:1 但档案缺席）
    // 一律是部署语言 —— 缺席不是选择。
    match (is_direct_message, person_locale) {
        (true, Some(profile_language)) => resolve_locale(profile_language),
        _ => deployment_locale(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use pretty_assertions::assert_eq;

    /// 两个 locale 的取值与上游常量逐字一致，且往返闭合。
    #[test]
    fn locales_match_the_upstream_constants() {
        assert_eq!(Locale::ZhHans.as_str(), "zh-Hans");
        assert_eq!(Locale::En.as_str(), "en");
        assert_eq!(DEFAULT_LOCALE, Locale::ZhHans);
        assert_eq!(format!("{}", Locale::En), "en");
    }

    /// `resolve_locale` 的三条判据、`set_deployment_locale` 的精确匹配、以及
    /// 目的地判据 —— **合成一个用例**。
    ///
    /// 理由：部署语言是一个**进程级**原子槽，三个话题都在写它；拆成三个并发用例，
    /// 任何一个的 `set_deployment_locale` 都会顶掉另一个的假设，结果是假绿/假红
    /// （LUM-1745 的实测教训，见 `docs/32` §27 的下一条）。按顺序走完三个话题是干净解。
    #[test]
    fn deployment_locale_is_process_global_so_these_assertions_live_in_one_case() {
        // ① `resolve_locale` 的三条判据（空 ⇒ 部署语言；`zh*` ⇒ 中文；其余 ⇒ 英文）。
        let _ = set_deployment_locale("en");
        assert_eq!(deployment_locale(), Locale::En);
        assert_eq!(resolve_locale(""), Locale::En, "缺席 = 部署语言，不是选择");
        assert_eq!(resolve_locale("   "), Locale::En);
        for zh in ["zh-Hans", "zh", "ZH-hant", "zh-CN"] {
            assert_eq!(resolve_locale(zh), Locale::ZhHans, "{zh}");
        }
        // 一个选了 ko / ja 的用户是深思熟虑地没选中文 ⇒ 英文。
        for other in ["en", "ko", "ja", "fr", "EN"] {
            assert_eq!(resolve_locale(other), Locale::En, "{other}");
        }

        // ② `set_deployment_locale` 精确匹配：认不出的值**保持旧值**。
        assert_eq!(set_deployment_locale("  ZH-Hans "), Locale::ZhHans);
        assert_eq!(set_deployment_locale("zh"), Locale::ZhHans);
        assert_eq!(set_deployment_locale("en"), Locale::En);
        // `zh_Hant` / 带引号的值都不认（若照 `resolve_locale` 的规则，它们会变成英文 ——
        // 那正是这条用例要防的意外：一个笔误把一个中文租户的群聊换成英文）。
        for typo in ["zh_Hant", "\"en\"", "english", "", "zh-Hant"] {
            assert_eq!(
                set_deployment_locale(typo),
                Locale::En,
                "{typo} 必须保持旧值（en）"
            );
        }
        assert_eq!(set_deployment_locale("zh-Hans"), Locale::ZhHans);

        // ③ 目的地判据：1:1 用那个人的档案语言（缺席 ⇒ 部署语言），群聊一律用部署语言 ——
        //    无论传进来谁的档案。
        assert_eq!(locale_for_destination(true, Some("en")), Locale::En);
        assert_eq!(
            locale_for_destination(true, Some("zh-Hans")),
            Locale::ZhHans
        );
        assert_eq!(locale_for_destination(true, None), Locale::ZhHans);
        assert_eq!(locale_for_destination(false, Some("en")), Locale::ZhHans);
        assert_eq!(locale_for_destination(false, None), Locale::ZhHans);
        let _ = set_deployment_locale("en");
        assert_eq!(locale_for_destination(false, Some("zh-Hans")), Locale::En);
        // 收尾时复原，免得与同 binary 里的其它读方互相影响。
        assert_eq!(set_deployment_locale("zh-Hans"), Locale::ZhHans);
    }

    /// 两套包都齐、都不为空，且**每条都有可见字符**（`WeCom` 会丢掉它认为空白的收尾帧）。
    #[test]
    fn both_packs_are_complete_and_visible() {
        for locale in [Locale::ZhHans, Locale::En] {
            let pack = copy_for(locale);
            for field in [
                pack.stream_no_reply,
                pack.stream_no_reply_with_files,
                pack.stream_not_started,
                pack.stream_failed,
                pack.stream_cancelled,
            ] {
                assert!(!field.is_empty(), "{locale:?} 有空字段");
                assert!(
                    field.chars().any(|c| !c.is_whitespace()),
                    "{locale:?} 的字段只有空白：{field:?}"
                );
                assert!(
                    !field.starts_with(char::is_whitespace)
                        && !field.ends_with(char::is_whitespace),
                    "首尾空白会被 WeCom 判成空帧：{field:?}"
                );
            }
        }
        // 两套**不同**（不是复制粘贴忘了改）。
        assert_ne!(copy_for(Locale::ZhHans), copy_for(Locale::En));
        assert_ne!(
            copy_for(Locale::ZhHans).stream_failed,
            copy_for(Locale::En).stream_failed
        );
    }
}
