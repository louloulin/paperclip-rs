//! `markdown.rs` 的用例（上游那份测试文件，**约 480 行**）。
//!
//! # 与上游的一处**形态**差异（登记 `docs/32` §36 的 D3）
//!
//! 上游的强断言是对着 **goldmark**（真 `CommonMark` 解析器）做的：先把未守的正文渲染一遍、
//! 要求攻击者的主机**真的**作为目标回来，再渲染守过的、要求它一个都不回来。本仓**没有**
//! Markdown 解析器依赖（`docs/60` §3.1：M7 各片不得新增三方依赖），所以换成两条**不需要
//! 解析器**的等价判据：
//!
//! 1. **语料非空**：每条攻击正文在块位置**必须**含有定义形态（`]:`），否则这条用例证明不了
//!    任何事 —— 这是上游"未守的一半不是装饰"那条要求的可测化；
//! 2. **守过之后一条不留**：语料里每个目标都**合格**（带 scheme / `//` / 转义 / 字符引用），
//!    所以守过的正文里**不该再有任何** `]:`。
//!
//! 换来的是一条**更弱**的保证：判据验的是"形态"，不是"某个渲染器真的会解析"。这条差异在
//! §36 的 D3 里逐字登记。

use crate::wecom::replier::MemberLinks;

use super::{
    break_link_adjacency, break_link_reference_definitions, break_member_links,
    container_prefix_before, has_backslash_escape, has_character_reference, has_uri_scheme,
    link_label_end, looks_like_link_destination, parse_container_prefix, skip_continuation_prefix,
    ContainerPrefix, MemberLinkBreaker,
};

// =====================================================================
// 第一段：行内邻接
// =====================================================================

/// 上游 `TestBreakLinkAdjacency`：唯一的一处编辑是 `]` 与 `(` 之间的空格，字符串里别的都不动，
/// 且**绝不**吐出反斜杠（反斜杠是上一版机制吐的东西，也是活租户上验证过不可用的那个）。
#[test]
fn break_link_adjacency_only_inserts_one_space() {
    let cases = [
        ("普通方括号标题", "[Bug] 登录失败", "[Bug] 登录失败"),
        ("只有圆括号", "修复 (见 issue 12)", "修复 (见 issue 12)"),
        ("落单的方括号", "开了一个 [ 没关", "开了一个 [ 没关"),
        ("只有感叹号", "紧急!!!", "紧急!!!"),
        ("成员写的反斜杠原样保留", r"修复路径 C:\", r"修复路径 C:\"),
        (
            "链接",
            "[click here](http://evil.example)",
            "[click here] (http://evil.example)",
        ),
        (
            "图片",
            "![img](http://evil.example/x.png)",
            "![img] (http://evil.example/x.png)",
        ),
        (
            "嵌套方括号",
            "[a[b]](http://evil.example)",
            "[a[b]] (http://evil.example)",
        ),
        ("背靠背", "](](", "] (] ("),
        ("两条链接", "[a](x) and [b](y)", "[a] (x) and [b] (y)"),
        ("反斜杠挡不住这一对", r"x\](u)", r"x\] (u)"),
    ];
    for (name, input, want) in cases {
        let got = break_link_adjacency(input);
        assert_eq!(got, want, "{name}");
        assert!(
            !got.contains("]("),
            "{name}: 还留着相邻的 \"](\" —— 链接仍然能成形：{got:?}"
        );
        if !input.contains('\\') {
            assert!(
                !got.contains('\\'),
                "{name}: 对 {input:?} 吐出了反斜杠 {got:?} —— WeCom 把 \"\\[\" 读成数学定界符"
            );
        }
    }
}

/// 上游 `TestBreakLinkAdjacencyIsIdempotent`：输出里没有 `](`，所以第二遍必须是 no-op。
/// 重要，因为同一段文本会到达不止一个构造点，而一个不停插空格的机制会漂移。
#[test]
fn break_link_adjacency_is_idempotent() {
    for input in ["[a](b)", "](](", "[Bug] 登录失败", "![i](u)"] {
        let once = break_link_adjacency(input);
        let twice = break_link_adjacency(&once);
        assert_eq!(twice, once, "第二遍改动了 {input:?}");
    }
}

// =====================================================================
// 第二段：引用定义
// =====================================================================

/// 语料：会定义一条引用、然后使用它的正文。
///
/// 形状是"一条建立在『`]:` 后面看着像 URL』之上的规则"会做错的那几种：目标藏在 `<` 后面、被推到
/// 下一行、用反斜杠转义或字符引用拼出来、或者定义在引用块 / 列表项里（标签不再在第 0 列）。
///
/// 上游对着 goldmark 逐条验过这些**真的**会解析；本仓的等价值是每条都含定义形态（`]:`），
/// 见下面的断言（以及模块文档的 D3）。
const LINK_REFERENCE_ATTACKS: &[(&str, &str)] = &[
    ("plain", "[重置密码]: https://evil.example\n\n[重置密码]"),
    (
        "冒号后无空格",
        "[重置密码]:https://evil.example\n\n[重置密码]",
    ),
    (
        "缩进三格",
        "   [重置密码]: https://evil.example\n\n[重置密码]",
    ),
    (
        "尖括号目标",
        "[重置密码]: <https://evil.example>\n\n[重置密码]",
    ),
    (
        "目标在下一行",
        "[重置密码]:\nhttps://evil.example\n\n[重置密码]",
    ),
    (
        "带标题",
        "[重置密码]: https://evil.example \"点这里\"\n\n[重置密码]",
    ),
    (
        "标题在下一行",
        "[重置密码]: https://evil.example\n\"点这里\"\n\n[重置密码]",
    ),
    (
        "scheme 相对",
        "[重置密码]: //evil.example/reset\n\n[重置密码]",
    ),
    (
        "转义的 scheme 冒号",
        "[重置密码]: https\\://evil.example\n\n[重置密码]",
    ),
    (
        "转义的斜杠",
        "[重置密码]: \\/\\/evil.example/reset\n\n[重置密码]",
    ),
    (
        "十六进制字符引用",
        "[重置密码]: &#x68;ttps://evil.example\n\n[重置密码]",
    ),
    (
        "十进制字符引用",
        "[重置密码]: &#104;ttps://evil.example\n\n[重置密码]",
    ),
    (
        "引用块里",
        "> [重置密码]: https://evil.example\n\n[重置密码]",
    ),
    (
        "列表项里",
        "- [重置密码]: https://evil.example\n\n[重置密码]",
    ),
    (
        "有序列表项里",
        "1. [重置密码]: https://evil.example\n\n[重置密码]",
    ),
    (
        "标签里有转义的方括号",
        "[重置\\]密码]: https://evil.example\n\n[重置\\]密码]",
    ),
    (
        "标签跨两行",
        "[重置\n密码]: https://evil.example\n\n[重置\n密码]",
    ),
    (
        "大小写折叠的标签",
        "[Reset]: https://evil.example\n\n[reset]",
    ),
    (
        "折叠引用",
        "[重置密码]: https://evil.example\n\n[重置密码][]",
    ),
    (
        "完整引用",
        "[重置密码]: https://evil.example\n\n[点这里][重置密码]",
    ),
    (
        "两条定义",
        "[a]: https://evil.example\n[b]: https://evil.example/2\n\n[a] [b]",
    ),
    (
        "大写 scheme",
        "[重置密码]: HTTPS://EVIL.EXAMPLE\n\n[重置密码]",
    ),
    // 目标单独一行、藏在定义所在的块容器后面。
    (
        "引用块延续",
        "> [重置密码]:\n> https://evil.example\n\n[重置密码]",
    ),
    (
        "引用块延续、标记后无空格",
        ">[重置密码]:\n>https://evil.example\n\n[重置密码]",
    ),
    (
        "引用块延续、标记后制表符",
        ">\t[重置密码]:\n>\thttps://evil.example\n\n[重置密码]",
    ),
    (
        "引用块延续、CRLF",
        "> [重置密码]:\r\n> https://evil.example\r\n\r\n[重置密码]",
    ),
    (
        "引用块延续、嵌两层",
        "> > [重置密码]:\n> > https://evil.example\n\n[重置密码]",
    ),
    (
        "引用块延续、嵌三层",
        "> > > [重置密码]:\n> > > https://evil.example\n\n[重置密码]",
    ),
    (
        "引用块延续、带缩进的标记",
        "  >  >  [重置密码]:\n  >  >  https://evil.example\n\n[重置密码]",
    ),
    (
        "引用块里的列表、靠缩进延续",
        "> - [重置密码]:\n>   https://evil.example\n\n[重置密码]",
    ),
    (
        "引用块里的有序列表",
        "> 1. [重置密码]:\n>    https://evil.example\n\n[重置密码]",
    ),
    (
        "引用块延续、尖括号目标",
        "> [重置密码]:\n> <https://evil.example>\n\n[重置密码]",
    ),
    (
        "引用块延续、scheme 相对",
        "> [重置密码]:\n> //evil.example/x\n\n[重置密码]",
    ),
    (
        "引用块延续、转义 scheme 冒号",
        "> [重置密码]:\n> https\\://evil.example\n\n[重置密码]",
    ),
    (
        "引用块延续、字符引用",
        "> [重置密码]:\n> &#x68;ttps://evil.example\n\n[重置密码]",
    ),
    (
        "引用块惰性延续、丢掉标记",
        "> [重置密码]:\nhttps://evil.example\n\n[重置密码]",
    ),
    (
        "列表靠缩进延续",
        "- [重置密码]:\n  https://evil.example\n\n[重置密码]",
    ),
    // 这样到达的一条定义喂的是同一张引用表，所以值得钉住"花它的每一种方式都被关上"。
    (
        "引用块延续、折叠引用",
        "> [重置密码]:\n> https://evil.example\n\n[重置密码][]",
    ),
    (
        "引用块延续、完整引用",
        "> [重置密码]:\n> https://evil.example\n\n[点这里][重置密码]",
    ),
    (
        "引用块延续、图片引用",
        "> [重置密码]:\n> https://evil.example\n\n![重置密码]",
    ),
    (
        "引用块延续、在引用块里用",
        "> [重置密码]:\n> https://evil.example\n>\n> [重置密码]",
    ),
];

/// 上游 `TestBreakLinkReferenceDefinitionsClosesEveryKnownDefinition`（见模块文档的 D3）。
#[test]
fn every_known_definition_is_closed() {
    for (name, body) in LINK_REFERENCE_ATTACKS {
        assert!(
            body.contains("]:"),
            "{name}: 未守的正文里没有定义形态 —— 这条用例证明不了任何事"
        );
        let guarded = break_member_links(body);
        assert!(
            !guarded.contains("]:"),
            "{name}: 成员定义的一条链接活了下来：\n{body:?} → {guarded:?}"
        );
    }
}

/// 上游 `TestBreakLinkReferenceDefinitionsLeavesProseAlone`：契约的另一半，也是这条规则**不是**
/// "见到 `]:` 就破"的理由。这里每一条都必须**逐字节**原样回来。
#[test]
fn prose_is_left_byte_identical() {
    for input in [
        "[Bug]: 登录失败",
        "[Bug]: 登录失败\n\n[Bug] 还在复现",
        "[WIP]: 明天再看",
        "[文件]: report.pdf",
        "[页面]: /inbox",
        // 目标是「见」，不是那个 URL。
        "[Bug]: 见 https://tracker.example/1",
        // 恰好含 `\` 或 `&` 的相对目标。这两个字符在这里都拼不出转义或字符引用，
        // 所以没有什么要解码、也就没有什么可破。
        "[Owner]: R&D",
        r"[Regex]: \d+",
        r"[Path]: docs\setup",
        "[Query]: foo&bar",
        "[Note]: see A&B for details",
        "[Bug] 登录失败",
        "修复 (见 issue 12)",
        // 不在块位置。
        "见 [文档]: https://wiki.example/x",
        "**[提及你] t**",
        // 空行：没有定义。
        "[重置密码]:\n\nhttps://evil.example",
        // 标签没有内容。
        "[ ]: https://evil.example",
        "[Bug]:",
        "没有方括号的一行",
        // 缩进四格在 CommonMark 里是代码块 —— 上游把这条**刻意**留在"会误伤"那一侧，
        // 这里跟着破（见下一条用例）。
    ] {
        let got = break_member_links(input);
        assert_eq!(got, input, "成员文本被改动了：{input:?}");
    }
}

/// 上游 `TestBreakLinkReferenceDefinitionsInsertsOneSpace`：钉住断点落在哪，以及它是**唯一**
/// 一处编辑 —— 一个空格、夹在 `]` 与 `:` 之间。
#[test]
fn the_break_is_one_space_between_the_bracket_and_the_colon() {
    let cases = [
        (
            "朴素",
            "[重置密码]: https://evil.example",
            "[重置密码] : https://evil.example",
        ),
        ("冒号后无空格", "[a]:https://x", "[a] :https://x"),
        ("目标在下一行", "[a]:\nhttps://x", "[a] :\nhttps://x"),
        ("引用块", "> [a]: https://x", "> [a] : https://x"),
        ("列表项", "- [a]: https://x", "- [a] : https://x"),
        (
            "两条定义",
            "[a]: https://x\n[b]: https://y",
            "[a] : https://x\n[b] : https://y",
        ),
        (
            "混合正文里只有那条定义",
            "看这里\n\n[a]: https://x\n\n[a]",
            "看这里\n\n[a] : https://x\n\n[a]",
        ),
        // 会误伤，刻意留成可见的。两者都只在一行本来就带 URL 的行上多一个空格，
        // 而两者都是"我们读不懂的那个渲染器"的安全一侧：CommonMark 会把前一条叫作缩进代码、
        // 把后一条叫作段落，但**这一个**不是 CommonMark。
        (
            "四个空格缩进在 commonmark 里不是定义",
            "    [a]: https://x",
            "    [a] : https://x",
        ),
        (
            "尾随散文在 commonmark 里不是定义",
            "[a]: https://x 请点击",
            "[a] : https://x 请点击",
        ),
    ];
    for (name, input, want) in cases {
        assert_eq!(
            break_link_reference_definitions(input),
            want,
            "{name}（整条入口）"
        );
        assert_eq!(break_member_links(input), want, "{name}（只过这一段）");
    }
}

// =====================================================================
// 两半必须对同一个容器有同一个答案
// =====================================================================

/// 上游 `scaffoldPrefixes`：深度 `depth` 以内的块脚手架串（去重，含空串）。
fn scaffold_prefixes(depth: usize) -> Vec<String> {
    const PIECES: [&str; 9] = [" ", "   ", "\t", ">", "> ", ">\t", "- ", "* ", "1. "];
    fn build(current: &str, remaining: usize, out: &mut Vec<String>) {
        if !out.iter().any(|seen| seen == current) {
            out.push(current.to_string());
        }
        if remaining == 0 {
            return;
        }
        for piece in PIECES {
            build(&format!("{current}{piece}"), remaining - 1, out);
        }
    }
    let mut out: Vec<String> = Vec::new();
    build("", depth, &mut out);
    out
}

/// 上游 `TestContainerPrefixHalvesAgree`：验的是**错误本身**而不是它的症状，也是在 blockquote
/// 延续那条上线之前唯一能拦住它的用例。
///
/// 破一条定义是一条规则、两个地方决定：[`container_prefix_before`] 说标签前面允许有什么脚手架，
/// [`looks_like_link_destination`] 扫冒号之后的东西。`CommonMark` 把引用块的标记带到它持有的**每一
/// 行**上，所以前半一旦接受一个 `>`，后半就必须在落在下一行的目标上把那个 `>` 预期回来。
#[test]
fn the_two_halves_agree_about_containers() {
    for prefix in scaffold_prefixes(3) {
        let probe = format!("{prefix}[重置密码]");
        let Some(container) = container_prefix_before(&probe, prefix.len()) else {
            continue;
        };
        let quotes = prefix.chars().filter(|c| *c == '>').count();
        assert_eq!(
            container.quotes, quotes,
            "前缀 {prefix:?} 开出 {quotes} 个引用块，container_prefix_before 却报 {}",
            container.quotes
        );
        // 从"惰性延续、一个标记都不带"到"定义自己那个深度"之间的每一层嵌套，都是解析器仍然读成
        // 这条定义的目标的一行。
        for n in 0..=quotes {
            for marker in [">", "> ", ">\t", " > ", "  >"] {
                let rest = format!("\n{}https://evil.example", marker.repeat(n));
                assert!(
                    looks_like_link_destination(&rest, container),
                    "前缀 {prefix:?} 可以托住一条定义，但目标扫描漏掉了 {rest:?} —— \
                     两半对\"什么算块容器\"的看法不一致"
                );
            }
        }
        // 但不能更深。比定义自己那个块更深的一行开的是一个**新**块而不是它的延续，所以跨过那个
        // 标记就是吞掉一个恰好挡路的字符，而不是建模那个容器。
        let deeper = format!("\n{}https://evil.example", "> ".repeat(quotes + 1));
        assert!(
            !looks_like_link_destination(&deeper, container),
            "前缀 {prefix:?} 开出 {quotes} 个引用块，目标扫描却跨过了 {} 个（{deeper:?}）",
            quotes + 1
        );
    }
}

/// 上游 `TestBreakLinkReferenceDefinitionsAcrossBlockScaffolding` 的**可移植那一半**（见模块
/// 文档的 D3）。
///
/// 定义行的脚手架 × 目标行的脚手架全扫一遍，但换掉需要解析器的那半判据，只留三条不需要解析器的
/// 不变式：
///
/// 1. **CRLF 与 LF 同判**：同一份正文换行折叠后，守过的结果折成 LF 必须**逐字节**等于 LF 版本
///    守过的结果 —— 同一个决定、同一个位置；
/// 2. **幂等**：再守一遍不再改动；
/// 3. **语料非空**：命中数有下限，否则这个扫描"什么也没测"就通过了。
#[test]
fn the_sweep_holds_across_block_scaffolding() {
    let prefixes = scaffold_prefixes(2);
    let destinations = [
        "https://evil.example",
        "//evil.example/x",
        "<https://evil.example>",
        "https\\://evil.example",
    ];
    let mut fired = 0_usize;
    let mut swept = 0_usize;
    for definition in &prefixes {
        let probe = format!("{definition}[");
        if container_prefix_before(&probe, definition.len()).is_none() {
            continue;
        }
        for continuation in &prefixes {
            for destination in destinations {
                let body =
                    format!("{definition}[重置密码]:\n{continuation}{destination}\n\n[重置密码]");
                let crlf = body.replace('\n', "\r\n");
                let guarded = break_member_links(&body);
                let guarded_crlf = break_member_links(&crlf).replace("\r\n", "\n");
                assert_eq!(
                    guarded_crlf, guarded,
                    "定义在 {definition:?} 后面、目标在 {continuation:?} 后面时，这一道闸对 CRLF \
                     与 LF 的判决不同：\n{crlf:?}"
                );
                assert_eq!(
                    break_member_links(&guarded),
                    guarded,
                    "第二遍改动了 {body:?}"
                );
                swept += 1;
                if guarded != body {
                    fired += 1;
                }
            }
        }
    }
    assert!(
        swept > 1000,
        "扫描只覆盖了 {swept} 组 —— 它已经不测原来的东西了"
    );
    assert!(
        fired > 200,
        "扫描的 {swept} 组里只有 {fired} 组真的被破了 —— 语料已经空掉"
    );
}

/// 上游 `TestBreakMemberLinksIsIdempotent`：同一段文本会到达不止一个构造点，而长度预算只算一次，
/// 所以一个不停插空格的机制会漂过上限。
#[test]
fn break_member_links_is_idempotent() {
    let mut inputs: Vec<String> = vec![
        "[a](b)".into(),
        "](](".into(),
        "[Bug] 登录失败".into(),
        "![i](u)".into(),
        "[Bug]: 登录失败".into(),
        // 两道闸在同一串上开火：谁也不会造出另一个要找的形态，所以一遍之后必须收敛。
        "[点这里](https://evil.example)\n\n[重置密码]: https://evil.example\n\n[重置密码]".into(),
    ];
    inputs.extend(
        LINK_REFERENCE_ATTACKS
            .iter()
            .map(|(_, body)| (*body).to_owned()),
    );
    for input in inputs {
        let once = break_member_links(&input);
        assert_eq!(break_member_links(&once), once, "第二遍改动了 {input:?}");
    }
}

// =====================================================================
// 叶子：三个判定器本身
// =====================================================================

/// 端口的生产实现必须就是这两段闸（`MemberLinks` 的契约是**一个**方法，见上游的理由）。
#[test]
fn the_port_implementation_is_the_two_halves() {
    let breaker = MemberLinkBreaker;
    for input in [
        "[点这里](https://evil.example)",
        "[重置密码]: https://evil.example\n\n[重置密码]",
        "[Bug] 登录失败",
    ] {
        assert_eq!(breaker.break_links(input), break_member_links(input));
    }
}

/// `has_uri_scheme` 的边界：一个字母开头才算 scheme，否则不是。
#[test]
fn uri_scheme_needs_a_letter_first() {
    assert!(has_uri_scheme("https://x"));
    assert!(has_uri_scheme("javascript:alert(1)"));
    assert!(has_uri_scheme("a+:"));
    assert!(!has_uri_scheme("://x"));
    assert!(!has_uri_scheme("1http://x"));
    assert!(!has_uri_scheme("登录失败"));
    assert!(!has_uri_scheme(""));
}

/// 转义与字符引用都是"机制"而不是"字符"：孤零零的 `\` 或 `&` 什么也不拼。
#[test]
fn escape_machinery_is_read_not_characters() {
    assert!(has_backslash_escape("https\\://x"));
    assert!(has_backslash_escape("\\/\\/x"));
    assert!(!has_backslash_escape(r"\d+"));
    assert!(!has_backslash_escape(r"docs\setup"));
    assert!(!has_backslash_escape(r"trailing\"));

    assert!(has_character_reference("&#x68;ttps"));
    assert!(has_character_reference("&#104;ttps"));
    assert!(!has_character_reference("R&D"));
    assert!(!has_character_reference("foo&bar"));
    assert!(!has_character_reference("&;"));
    assert!(!has_character_reference("&"));
}

/// 标签解析：未转义的 `]` 关闭、未转义的 `[` 取消资格、必须有内容、反斜杠把下一个字符转义掉。
#[test]
fn label_parsing_follows_commonmark() {
    assert_eq!(link_label_end("[a]", 0), Some(2));
    // `\]` 不关标签……但它后面的 `]` 会（`[a\]b]` 里闭合的那个在偏移 5）。
    assert_eq!(link_label_end(r"[a\]b]", 0), Some(5));
    assert_eq!(link_label_end("[a[b]", 0), None);
    assert_eq!(link_label_end("[   ]", 0), None);
    assert_eq!(link_label_end("[没有闭合", 0), None);
    // 标签可以跨行（`[重\n置]` 是 9 个字节，闭合的那个 `]` 在偏移 8）。
    assert_eq!(link_label_end("[重\n置]", 0), Some(8));
    // 上限数的是 rune：一个 999 个汉字的标签仍然合法（按字节数会在第三个字附近判超限）。
    let long = format!("[{}]", "字".repeat(999));
    assert_eq!(link_label_end(&long, 0), Some(long.len() - 1));
    let too_long = format!("[{}]", "字".repeat(1000));
    assert_eq!(link_label_end(&too_long, 0), None);
}

/// `parse_container_prefix` 认得出四种脚手架、认不出别的东西。
#[test]
fn container_prefix_parsing() {
    assert_eq!(
        parse_container_prefix(""),
        Some(ContainerPrefix { quotes: 0 })
    );
    assert_eq!(
        parse_container_prefix("  "),
        Some(ContainerPrefix { quotes: 0 })
    );
    assert_eq!(
        parse_container_prefix("\t"),
        Some(ContainerPrefix { quotes: 0 })
    );
    assert_eq!(
        parse_container_prefix("- "),
        Some(ContainerPrefix { quotes: 0 })
    );
    assert_eq!(
        parse_container_prefix("* "),
        Some(ContainerPrefix { quotes: 0 })
    );
    assert_eq!(
        parse_container_prefix("1. "),
        Some(ContainerPrefix { quotes: 0 })
    );
    assert_eq!(
        parse_container_prefix("> "),
        Some(ContainerPrefix { quotes: 1 })
    );
    assert_eq!(
        parse_container_prefix("> > "),
        Some(ContainerPrefix { quotes: 2 })
    );
    assert_eq!(
        parse_container_prefix("  >  >  "),
        Some(ContainerPrefix { quotes: 2 })
    );
    // 不是脚手架：一个词、一个 `**`、一个没有后继空格的 `-`、一个超过 9 位的序号。
    assert_eq!(parse_container_prefix("见 "), None);
    assert_eq!(parse_container_prefix("**"), None);
    assert_eq!(parse_container_prefix("-"), None);
    assert_eq!(parse_container_prefix("1234567890. "), None);
}

/// `skip_continuation_prefix` 至多吃掉 `prefix.quotes` 个标记，且允许更少（惰性延续）。
#[test]
fn continuation_prefix_stepping() {
    let one = ContainerPrefix { quotes: 1 };
    assert_eq!(skip_continuation_prefix("> https://x", 0, one), 2);
    assert_eq!(skip_continuation_prefix(">\thttps://x", 0, one), 2);
    // 惰性延续：一个标记都不带也照样是这条定义的延续。
    assert_eq!(skip_continuation_prefix("https://x", 0, one), 0);
    // 更深：只吃掉自己那一层，留下多出来的 `>` 自己说话。
    assert_eq!(skip_continuation_prefix("> > https://x", 0, one), 2);
    let none = ContainerPrefix { quotes: 0 };
    assert_eq!(skip_continuation_prefix("  https://x", 0, none), 2);
}

/// 块位置判定：行首之外的 `[` 不是块位置，散文里的 `[` 一个字节就判定。
#[test]
fn block_position_detection() {
    assert_eq!(
        container_prefix_before("[x]", 0),
        Some(ContainerPrefix { quotes: 0 })
    );
    assert_eq!(
        container_prefix_before("> [x]", 2),
        Some(ContainerPrefix { quotes: 1 })
    );
    assert_eq!(
        container_prefix_before("行内 [x]", 7),
        None,
        "散文里的 `[` 不是块位置"
    );
    assert_eq!(container_prefix_before("** [x]", 3), None);
    assert_eq!(
        container_prefix_before("上一行\n[x]", 10),
        Some(ContainerPrefix { quotes: 0 })
    );
}
