//! 「模型看得见哪些 skill」与「模型能调用的名字」。
//!
//! - **上游**：`execenv/skill_visibility.go`（99 行）。
//! - **写者**：M6-9（本 slice）。
//!
//! ## 归一化不是装饰
//!
//! runtime brief 会把这里给出的条目渲染成「模型挑 skill 时读的那张清单」，而模型**唯一**
//! 能调用的名字是落盘时的目录 slug（同一批 skill 的 `writeSkillFiles` 用同一套
//! `sanitizeSkillName` 铺目录）。workspace skill 的 `Name` 是**人看的显示名**
//! （`"PR review"`），照抄给模型就是在给它一个解析不出来的标识符（上游 MUL-5529）。
//!
//! ## slug 必须从**未过滤**的那一批里算
//!
//! `writeSkillFiles` 会把**每一个** skill 都铺到盘上（包括这里被隐藏的那些），每个都占一个
//! slug。先过滤再算 slug 会让后缀整体前移，清单与目录立刻不一致。
//!
//! ## 已知缺口（上游 MUL-5550，本 slice 照抄并登记）
//!
//! [`resolve_skill_slugs`] 看不见文件系统：一个与**用户自装目录**撞名的 skill 实际会写到
//! `<slug>-multica`，而清单里显示的是裸 slug。补这条要把它拿到 `Prepare` 里分配好的 slug，
//! 而这三个渲染器**故意**是纯函数（同输入必须产出逐字节相同的 brief）。
//!
//! ## 与上游的差异（逐条登记在 `docs/32` §9.9）
//!
//! - 上游 `skillDisablesModelInvocation` 用 `yaml.Unmarshal` 解整个 frontmatter 体；本 slice
//!   只做**一个键的窄口径读取**（顶格键 + 标量 `true`/`false`/带引号的 `"true"`），因为
//!   `mc-daemon` 这一侧没有 YAML 解析器（`serde_yaml` 不是它的依赖，M6-0 又冻结了三方依赖）。
//!   窄口径的失败方向是「看不出禁用 ⇒ 仍然列出」，与上游的默认值一致；嵌套/锚点等 YAML
//!   写法本 slice 不认（登记为缺口）。

use crate::execenv::sidecar::{frontmatter_parts, sanitize_skill_name, skill_slug_candidate};
use crate::skill::SkillForEnv;

/// 模型可见的 skill 清单（上游 `modelVisibleSkills`）。
///
/// 入参为空 ⇒ 返回空（上游返回 `nil`）。条目的 `name` 被换成**盘上 slug**。
#[must_use]
pub fn model_visible_skills(skills: &[SkillForEnv]) -> Vec<SkillForEnv> {
    if skills.is_empty() {
        return Vec::new();
    }
    let slugs = resolve_skill_slugs(skills);
    let mut visible = Vec::with_capacity(skills.len());
    for (index, skill) in skills.iter().enumerate() {
        if skill_model_invocation_visible(skill) {
            visible.push(SkillForEnv {
                name: slugs[index].clone(),
                content: skill.content.clone(),
            });
        }
    }
    visible
}

/// 给一批 skill 分配盘上目录 slug（上游 `resolveSkillSlugs`）。
///
/// `sanitizeSkillName` 不是单射（`"A B"` 与 `"A-B"` 都归到 `a-b`），`writeSkillFiles` 靠
/// [`skill_slug_candidate`] 的序列在写盘时拆开冲突。**两边必须用同一个序列**，否则第二个
/// skill 会被「按 `a-b` 列出、写进 `a-b-multica`」，模型因此永远够不到它（只会打到第一个）。
/// 结果只依赖批次与下标顺序 ⇒ 同样的输入逐字节产出同样的 brief。
#[must_use]
pub fn resolve_skill_slugs(skills: &[SkillForEnv]) -> Vec<String> {
    let mut slugs: Vec<String> = Vec::with_capacity(skills.len());
    let mut taken: Vec<String> = Vec::with_capacity(skills.len());
    for skill in skills {
        let base = sanitize_skill_name(&skill.name);
        let mut attempt = 0usize;
        loop {
            let candidate = skill_slug_candidate(&base, attempt);
            if !taken.contains(&candidate) {
                taken.push(candidate.clone());
                slugs.push(candidate);
                break;
            }
            attempt += 1;
        }
    }
    slugs
}

/// 这个 skill 是否允许模型主动调用（上游 `skillModelInvocationVisible`）。
#[must_use]
pub fn skill_model_invocation_visible(skill: &SkillForEnv) -> bool {
    !skill_disables_model_invocation(&skill.content)
}

/// frontmatter 里有没有 `disable-model-invocation: true`（上游 `skillDisablesModelInvocation`）。
///
/// 三种情形都返回 `false`（= 不禁用）：没有 frontmatter、frontmatter 体是空白、键不存在
/// 或不是布尔/`"true"` 字符串。与上游的 `switch` 默认分支一致。
#[must_use]
pub fn skill_disables_model_invocation(content: &str) -> bool {
    let (body, _, ok) = frontmatter_parts(content);
    if !ok || body.trim().is_empty() {
        return false;
    }
    let Some(raw) = top_level_scalar(body, "disable-model-invocation") else {
        return false;
    };
    let value = strip_inline_comment(&raw).trim().to_string();
    let unquoted = unquote(&value);
    let scalar = unquoted.trim();
    scalar.eq_ignore_ascii_case("true") || value.eq_ignore_ascii_case("true")
}

/// 顶格键的标量取值（`None` = 没有这个键，或者它的值是空的 / 是嵌套容器）。
fn top_level_scalar(body: &str, key: &str) -> Option<String> {
    for line in body.lines() {
        if line.starts_with(' ') || line.starts_with('\t') || line.trim().is_empty() {
            continue;
        }
        let Some((candidate, value)) = line.split_once(':') else {
            continue;
        };
        if candidate.trim() != key {
            continue;
        }
        return Some(value.trim().to_string());
    }
    None
}

/// 去掉行尾注释：`#` 之前有空白、且不在引号里才算注释（YAML 的口径）。
fn strip_inline_comment(value: &str) -> String {
    let mut in_single = false;
    let mut in_double = false;
    let mut previous_is_space = false;
    for (index, ch) in value.char_indices() {
        match ch {
            '\'' if !in_double => in_single = !in_single,
            '"' if !in_single => in_double = !in_double,
            '#' if !in_single && !in_double && previous_is_space => {
                return value[..index].trim().to_string();
            }
            _ => {}
        }
        previous_is_space = ch == ' ' || ch == '\t';
    }
    value.trim().to_string()
}

/// 去掉成对的单/双引号（只剥一层，与 YAML 的标量引号同）。
fn unquote(value: &str) -> String {
    let bytes = value.as_bytes();
    if bytes.len() >= 2 {
        let first = bytes[0];
        let last = bytes[bytes.len() - 1];
        if (first == b'"' && last == b'"') || (first == b'\'' && last == b'\'') {
            return value[1..value.len() - 1].to_string();
        }
    }
    value.to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn skill(name: &str, content: &str) -> SkillForEnv {
        SkillForEnv {
            name: name.to_string(),
            content: content.to_string(),
        }
    }

    #[test]
    fn visible_names_are_the_on_disk_slugs() {
        let visible = model_visible_skills(&[
            skill("PR review", "---\nname: PR review\n---\nbody"),
            skill("deploy", "no frontmatter"),
        ]);
        assert_eq!(visible.len(), 2);
        assert_eq!(visible[0].name, "pr-review");
        assert_eq!(visible[1].name, "deploy");
        // 正文原样带过去（brief 渲染要用）。
        assert_eq!(visible[0].content, "---\nname: PR review\n---\nbody");
    }

    #[test]
    fn empty_input_means_empty_output() {
        assert!(model_visible_skills(&[]).is_empty());
    }

    #[test]
    fn disabled_skills_are_hidden_but_still_consume_a_slug() {
        let batch = [
            skill(
                "A B",
                "---\nname: A B\ndisable-model-invocation: true\n---\n",
            ),
            skill("A-B", "---\nname: A-B\n---\n"),
        ];
        // slug 从**未过滤**的那一批算：第一个占 `a-b`，第二个才拿到 `a-b-multica`。
        assert_eq!(resolve_skill_slugs(&batch), vec!["a-b", "a-b-multica"]);

        let visible = model_visible_skills(&batch);
        assert_eq!(visible.len(), 1);
        assert_eq!(visible[0].name, "a-b-multica");
    }

    #[test]
    fn slug_sequence_matches_the_allocator() {
        let batch = [
            skill("same name", ""),
            skill("Same  Name", ""),
            skill("same-name", ""),
        ];
        assert_eq!(
            resolve_skill_slugs(&batch),
            vec!["same-name", "same-name-multica", "same-name-multica-2"]
        );
    }

    #[test]
    fn disable_flag_accepts_the_documented_spellings() {
        for content in [
            "---\ndisable-model-invocation: true\n---\n",
            "---\ndisable-model-invocation: True\n---\n",
            "---\ndisable-model-invocation: \"true\"\n---\n",
            "---\ndisable-model-invocation: 'TRUE'\n---\n",
            "---\nname: x\ndisable-model-invocation: true # hide it\n---\n",
        ] {
            assert!(
                skill_disables_model_invocation(content),
                "should disable: {content:?}"
            );
        }
    }

    #[test]
    fn disable_flag_defaults_to_visible() {
        for content in [
            "no frontmatter at all",
            "---\n---\n",
            "---\n   \n---\n",
            "---\ndisable-model-invocation: false\n---\n",
            "---\ndisable-model-invocation: yes\n---\n",
            "---\ndisable-model-invocation:\n---\n",
            "---\n  disable-model-invocation: true\n---\n", // 缩进 ⇒ 不是顶格键
            "---\nname: disable-model-invocation: true\n---\n",
            "---\ndisable-model-invocation: \"#true\"\n---\n",
        ] {
            assert!(
                !skill_disables_model_invocation(content),
                "should stay visible: {content:?}"
            );
        }
    }

    #[test]
    fn frontmatter_without_a_closing_fence_is_ignored() {
        assert!(!skill_disables_model_invocation(
            "---\ndisable-model-invocation: true\n"
        ));
    }
}
