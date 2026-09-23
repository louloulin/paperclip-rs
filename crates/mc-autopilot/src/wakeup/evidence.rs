//! 事件证据（`issue_wakeup_receipt`）→ **派发提示词**（`task.handoff_note`）的渲染与合并。
//!
//! - **写者**：M5-6。
//! - **上游**：`service/issue_wakeup_evidence.go`135（136 行）—— `mergeWakeupEvidence`37 /
//!   `renderWakeupEvidence`16 / `canonicalWakeupPayload`9。
//! - **谁调用**：`dispatch`（上游 548-740，本地 `service::plan_dispatch`）在派发前把
//!   `(wakeup, 上一个 task 的 context+handoff_note, pending receipts)` 合成新提示词；
//!   M5-8 的队列写路径照抄落库（`mc-repos` 已有 `replace` 证据的接口，本文件只做渲染）。
//! - **要点**（逐条对上游）：
//!   1. 事实存成**数据**（`{"wakeup_evidence": {…}}` 写进 `task.context`），`handoff_note`
//!      只是渲染结果 ⇒ 「旧派发方只改了 note」时靠**重渲染比对**发现，并把旧文本整段降级为
//!      `legacy`（**绝不解析旧文本**）；
//!   2. `kind != "event"` 的证据一律清空（时间型 wakeup 不携带事件事实）；
//!   3. 预算 `40000` 字节（含 ≤12000 的 instruction），按「先丢 legacy、再从最早的事实开始丢」
//!      的顺序裁剪，任何裁剪都置 `omitted`（渲染时会多一句话提示 model 去读现状）；
//!   4. 单条事实超预算时**保留引用字段**（`event_id`/`occurred_at`/…/`receipt_id`，值 ≤256 字节）
//!      而不是整条丢掉；`coalesced_count > 1` 也置 `omitted`；
//!   5. JSON 键序不稳定（jsonb 会重排）⇒ 渲染前**规范化**（对象键排序、数字保持原样），
//!      否则「重渲染比对」会永远不相等、每次都把有用证据降级成 legacy。

use serde_json::{json, Map, Value};
use uuid::Uuid;

use mc_repos::wakeup::WakeupReceiptRow;

/// 提示词预算（上游 `wakeupNoteLimit`）。
pub const WAKEUP_NOTE_LIMIT: usize = 40000;
/// 旧文本降级时的标题（上游 `wakeupLegacyHeading`）。
pub const WAKEUP_LEGACY_HEADING: &str =
    "Previous wakeup context (historical; follow the current instruction above):\n";
/// 裁剪发生时的提示（上游 `wakeupOmittedEvidence`）。
pub const WAKEUP_OMITTED_EVIDENCE: &str = "Some trigger details were omitted to keep this prompt bounded. Read current issue comments, runs and state before deciding what to do.\n";
/// 单条事实超预算时保留的引用字段（上游 `refs` 列表，顺序即上游顺序）。
const REFERENCE_KEYS: [&str; 13] = [
    "event_id",
    "occurred_at",
    "first_occurred_at",
    "coalesced_count",
    "task_id",
    "source_task_id",
    "comment_id",
    "thread_id",
    "attachment_id",
    "agent_id",
    "actor_type",
    "actor_id",
    "receipt_id",
];
/// 单个引用值的字节上限（上游 `len(value) <= 256`）。
const REFERENCE_VALUE_LIMIT: usize = 256;

/// 存进 `task.context.wakeup_evidence` 的证据包（上游 `wakeupEvidence`）。
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize, Default)]
pub struct WakeupEvidence {
    /// 结构版本；`!= 1` 一律降级为 legacy。
    pub version: i64,
    /// 当时的 instruction（用于「instruction 改了但 note 没重渲染」的识别）。
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub instruction: String,
    /// 事实列表。
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub facts: Vec<WakeupFact>,
    /// 旧派发方留下的自由文本（**不解析**）。
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub legacy: String,
    /// 是否发生过裁剪。
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub omitted: bool,
}

/// 一条事实（上游 `wakeupFact`）。
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct WakeupFact {
    /// 事件名。
    pub event_type: String,
    /// 事件的证据载荷。
    pub payload: Value,
}

/// 渲染结果：`handoff_note` 文本 + 要写进 `task.context` 的 `{"wakeup_evidence": …}` 片段。
#[derive(Debug, Clone)]
pub struct MergedEvidence {
    /// `task.handoff_note`。
    pub note: String,
    /// `task.context["wakeup_evidence"]`。
    pub stored: Value,
}

/// 上游 `mergeWakeupEvidence(w, previous, receipts)` 的等价实现。
///
/// - `wakeup_id` / `instruction` / `kind` 来自 `issue_wakeup`；
/// - `previous_context` / `previous_handoff_note` 来自上一个 task（可能没有 task ⇒ `None`）；
/// - `receipts` 是本次要合并的 pending receipt（调用方已按 `revision` 取好）。
#[must_use]
pub fn merge_wakeup_evidence(
    wakeup_id: Uuid,
    instruction: &str,
    kind: &str,
    previous_context: Option<&Value>,
    previous_handoff_note: Option<&str>,
    receipts: &[WakeupReceiptRow],
) -> MergedEvidence {
    let previous_note = previous_handoff_note.unwrap_or_default();
    let stored = previous_context
        .and_then(|context| context.get("wakeup_evidence"))
        .and_then(|raw| serde_json::from_value::<WakeupEvidence>(raw.clone()).ok())
        .unwrap_or_default();
    let mut evidence = stored;
    // jsonb 会重排键序 ⇒ 渲染前规范化，否则比对恒不相等。
    for fact in &mut evidence.facts {
        fact.payload = canonical_payload(&fact.payload);
    }
    // 「重渲染比对」：旧派发方可能只更新了 handoff_note。
    let previous_instruction = if evidence.instruction.is_empty() {
        instruction
    } else {
        evidence.instruction.as_str()
    };
    if evidence.version != 1
        || render_wakeup_evidence(wakeup_id, previous_instruction, &evidence) != previous_note
    {
        // 用旧文本整段降级，绝不解析。
        evidence = WakeupEvidence {
            version: 1,
            legacy: previous_note.to_string(),
            ..WakeupEvidence::default()
        };
    }
    if kind != "event" {
        evidence = WakeupEvidence {
            version: 1,
            ..WakeupEvidence::default()
        };
    }

    let header = render_header(wakeup_id, instruction);
    let budget = WAKEUP_NOTE_LIMIT
        .saturating_sub(header.len())
        .saturating_sub(WAKEUP_OMITTED_EVIDENCE.len())
        .saturating_sub(WAKEUP_LEGACY_HEADING.len());
    let mut total = evidence.legacy.len();
    for fact in &evidence.facts {
        total += fact_cost(&fact.event_type, &fact.payload);
    }
    let trim = |evidence: &mut WakeupEvidence, total: &mut usize| {
        if *total > budget && !evidence.legacy.is_empty() {
            *total -= evidence.legacy.len();
            evidence.legacy.clear();
            evidence.omitted = true;
        }
        while *total > budget && !evidence.facts.is_empty() {
            let fact = evidence.facts.remove(0);
            *total -= fact_cost(&fact.event_type, &fact.payload);
            evidence.omitted = true;
        }
    };
    trim(&mut evidence, &mut total);
    for receipt in receipts {
        let coalesced = receipt
            .payload
            .get("coalesced_count")
            .and_then(Value::as_i64)
            .unwrap_or(0);
        if coalesced > 1 {
            evidence.omitted = true;
        }
        let mut payload = canonical_payload(&receipt.payload);
        if fact_cost(&receipt.event_type, &payload) > budget {
            // 超预算的事实退化成「引用」——仍然是数据，不丢事件身份。
            let mut refs = Map::new();
            if let Value::Object(fields) = &payload {
                for key in REFERENCE_KEYS {
                    if let Some(value) = fields.get(key) {
                        let rendered = value.to_string();
                        if !rendered.is_empty() && rendered.len() <= REFERENCE_VALUE_LIMIT {
                            refs.insert(key.to_string(), value.clone());
                        }
                    }
                }
            }
            refs.insert("receipt_id".to_string(), json!(receipt.id.to_string()));
            payload = Value::Object(refs);
            evidence.omitted = true;
        }
        total += fact_cost(&receipt.event_type, &payload);
        evidence.facts.push(WakeupFact {
            event_type: receipt.event_type.clone(),
            payload,
        });
        trim(&mut evidence, &mut total);
    }
    evidence.instruction = instruction.to_string();
    let note = render_wakeup_evidence(wakeup_id, instruction, &evidence);
    let stored = serde_json::to_value(&evidence).unwrap_or_else(|_| json!({"version": 1}));
    MergedEvidence { note, stored }
}

/// 上游 `renderWakeupEvidence`。
#[must_use]
pub fn render_wakeup_evidence(
    wakeup_id: Uuid,
    instruction: &str,
    evidence: &WakeupEvidence,
) -> String {
    let mut out = render_header(wakeup_id, instruction);
    if evidence.omitted {
        out.push_str(WAKEUP_OMITTED_EVIDENCE);
    }
    if !evidence.legacy.is_empty() {
        out.push_str(WAKEUP_LEGACY_HEADING);
        out.push_str(&evidence.legacy);
    }
    for fact in &evidence.facts {
        out.push_str(&fact.event_type);
        out.push(' ');
        out.push_str(&fact.payload.to_string());
        out.push('\n');
    }
    out
}

fn render_header(wakeup_id: Uuid, instruction: &str) -> String {
    format!(
        "Wakeup {wakeup_id} triggered. Instruction:\n{instruction}\nTrigger facts (read current state before deciding what to do):\n"
    )
}

/// 上游的 `len(event_type) + len(payload) + 2`（`2` 是 `"%s %s\n"` 的分隔与换行）。
fn fact_cost(event_type: &str, payload: &Value) -> usize {
    event_type.len() + payload.to_string().len() + 2
}

/// 上游 `canonicalWakeupPayload`：解析失败 ⇒ `{}`；成功 ⇒ 规范化重排 `json.Marshal`。
///
/// `json.Marshal` 对 map 是**键有序**的，本地用 `BTreeMap` 中转即可与上游逐字一致
/// （不依赖 `serde_json` 的 `preserve_order` 开关）。
#[must_use]
pub fn canonical_payload(raw: &Value) -> Value {
    fn canonical(value: &Value) -> Value {
        match value {
            Value::Object(map) => {
                let sorted: std::collections::BTreeMap<&String, Value> =
                    map.iter().map(|(k, v)| (k, canonical(v))).collect();
                let mut out = Map::new();
                for (key, value) in sorted {
                    out.insert(key.clone(), value);
                }
                Value::Object(out)
            }
            Value::Array(items) => Value::Array(items.iter().map(canonical).collect()),
            other => other.clone(),
        }
    }
    match raw {
        Value::Null => json!({}),
        other => canonical(other),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use mc_repos::wakeup::WakeupReceiptRow;

    fn receipt(payload: Value) -> WakeupReceiptRow {
        WakeupReceiptRow {
            id: Uuid::now_v7(),
            wakeup_id: Uuid::now_v7(),
            revision: 1,
            event_key: "k".to_string(),
            event_type: "comment.created".to_string(),
            payload,
            coalesce_key: Some("comment.created".to_string()),
            task_id: None,
            processed_at: None,
            created_at: chrono::Utc::now(),
        }
    }

    #[test]
    fn event_wakeup_renders_instruction_and_facts() {
        let id = Uuid::now_v7();
        let merged = merge_wakeup_evidence(
            id,
            "look at the comment",
            "event",
            None,
            None,
            &[receipt(json!({"comment_id": "c1"}))],
        );
        assert!(merged.note.contains(&format!("Wakeup {id} triggered")));
        assert!(merged.note.contains("look at the comment"));
        assert!(merged
            .note
            .contains("comment.created {\"comment_id\":\"c1\"}"));
        assert_eq!(merged.stored["version"], json!(1));
        assert_eq!(merged.stored["instruction"], json!("look at the comment"));
        assert_eq!(
            merged.stored["facts"][0]["event_type"],
            json!("comment.created")
        );
    }

    #[test]
    fn time_wakeup_drops_stored_evidence_and_legacy() {
        // 上游对 `kind != "event"` 只清「`context` 里的既有证据 + legacy」，紧接着的 receipt
        // 循环照旧执行 —— 实践中时间型 wakeup 不会有 pending receipt（`capture_issue_wakeup()`
        // 先按 `kind='event'` 早退），所以这里只验「既有事实与旧文本被丢掉」。
        let merged = merge_wakeup_evidence(
            Uuid::now_v7(),
            "tick",
            "every",
            Some(
                &json!({"wakeup_evidence": {"version": 1, "facts": [{"event_type": "stale.event", "payload": {}}]}}),
            ),
            Some("stale note"),
            &[],
        );
        assert!(!merged.note.contains("stale.event"));
        assert!(!merged.note.contains("stale note"));
        assert_eq!(merged.stored["facts"], Value::Null);
        assert_eq!(merged.stored["legacy"], Value::Null);
        assert_eq!(merged.stored["instruction"], json!("tick"));
    }

    #[test]
    fn legacy_note_is_preserved_when_rendering_no_longer_matches() {
        let merged = merge_wakeup_evidence(
            Uuid::now_v7(),
            "new instruction",
            "event",
            Some(&json!({"wakeup_evidence": {"version": 1, "instruction": "old instruction"}})),
            Some("hand written by an old dispatcher"),
            &[],
        );
        assert!(merged.note.contains(WAKEUP_LEGACY_HEADING));
        assert!(merged.note.contains("hand written by an old dispatcher"));
    }

    #[test]
    fn stable_evidence_is_not_demoted_to_legacy() {
        let id = Uuid::now_v7();
        let first = merge_wakeup_evidence(
            id,
            "keep me",
            "event",
            None,
            None,
            &[receipt(json!({"b": 2, "a": 1}))],
        );
        let context = json!({"wakeup_evidence": first.stored});
        let second = merge_wakeup_evidence(
            id,
            "keep me",
            "event",
            Some(&context),
            Some(&first.note),
            &[],
        );
        assert!(!second.note.contains(WAKEUP_LEGACY_HEADING));
        assert!(second.note.contains("comment.created {\"a\":1,\"b\":2}"));
    }

    #[test]
    fn oversized_fact_degrades_to_references() {
        let big = "x".repeat(WAKEUP_NOTE_LIMIT);
        let merged = merge_wakeup_evidence(
            Uuid::now_v7(),
            "bounded",
            "event",
            None,
            None,
            &[receipt(
                json!({"event_id": "e1", "comment_id": "c1", "body": big}),
            )],
        );
        assert!(merged.stored["omitted"].as_bool().unwrap_or(false));
        let fact = &merged.stored["facts"][0]["payload"];
        assert_eq!(fact["event_id"], json!("e1"));
        assert!(fact.get("body").is_none());
        assert!(fact["receipt_id"].is_string());
    }

    #[test]
    fn coalesced_receipts_mark_evidence_omitted() {
        let merged = merge_wakeup_evidence(
            Uuid::now_v7(),
            "coalesced",
            "event",
            None,
            None,
            &[receipt(json!({"coalesced_count": 3, "comment_id": "c1"}))],
        );
        assert!(merged.stored["omitted"].as_bool().unwrap_or(false));
        assert!(merged.note.contains(WAKEUP_OMITTED_EVIDENCE));
    }

    #[test]
    fn budgets_are_enforced() {
        let receipts: Vec<WakeupReceiptRow> = (0..2000)
            .map(|index| {
                receipt(json!({"comment_id": format!("c{index}"), "pad": "y".repeat(600)}))
            })
            .collect();
        let merged =
            merge_wakeup_evidence(Uuid::now_v7(), "budget", "event", None, None, &receipts);
        assert!(merged.note.len() <= WAKEUP_NOTE_LIMIT);
        assert!(merged.stored["omitted"].as_bool().unwrap_or(false));
    }
}
