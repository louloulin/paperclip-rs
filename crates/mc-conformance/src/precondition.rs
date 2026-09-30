//! **按场景登记的「自然复现」表**（`§297`）——「某条前提在这条 fixture 上，
//! 真池形态本身就是那个替身」的逐条登记。
//!
//! 它从 `requirements.rs` 拆出来不是为了好看，而是门 ⑩：`requirements.rs` 因为这张表
//! 与它的两条判据单测越过 800 行硬上限。**声明**在 [`NATURALLY_REPRODUCED`]，
//! 查询入口是 [`is_naturally_reproduced`]（唯一调用点是 `missing_requirements`）。
//!
//! 拆分的判据与 `requirements.rs` 模块头那几条同款：这张表是**数据 + 一条判据**
//! （同一个前提 id 下别的 fixture 必须仍不可判定），而那张文件是前提 / 凭据 / 请求面
//! 三道闸门 —— 两者混在一处时，「哪道闸放松了」不可读。

/// 一条 fixture 的**前提**在真库形态下**自然复现**的登记（`§297`）。
///
/// # 为什么需要这张表（而不是把 `satisfied_by` 放宽）
///
/// `db_fault_injection` 的 `satisfied_by` 是 `&[]`，理由是「真池没法被要求按需失败」。
/// 这句话对**「瞬时 DB 错误 ⇒ 500」**那条完全成立（本仓真池只会给 404）。
/// 但同一个前提 id 下还有**另一条** fixture：上游注入的 mockDB 返回的是
/// `pgx.ErrNoRows`，期望的是 **404** —— 而「这一行查不到」在真库形态下**自然发生**，
/// 本仓的 handler 会照上游那份 `GetTaskStatus` 的判定顺序回同一个 404。
///
/// 🔴 所以「这条前提真池供不起」是**按场景**成立的，不是按 id 成立的；把整个 id 放宽
/// 会把那条 500 一起放进判定，而它只会变成一条**假 mismatch**。这张表因此按
/// **(分组, 前提)** 登记，而不是动 `REQUIREMENTS`。
///
/// # 纪律（这张表唯一的不安全方向）
///
/// * 每行必须写清「真池里的哪个自然形态顶上了那个替身」—— 空理由不予登记；
/// * **反向自查是硬要求**：同一个前提 id 下**别的** fixture 必须仍不可判定，
///   否则这张表就退化成「按 id 放宽」的同义词（见单测
///   `the_override_does_not_silently_widen_the_requirement`）；
/// * 🔴 **绝不允许**为了压数字往里加行：每加一行都要能指出上游那行 mock 的**语义**
///   在真库里的对应物，而不只是「回放之后变绿了」。
pub const NATURALLY_REPRODUCED: &[(&str, &str, &str)] = &[(
    "TestGetTaskStatus_ErrNoRows_Returns404",
    "db_fault_injection",
    "daemon_test.go:1151 注入的 mockDB 返回 `pgx.ErrNoRows`，期望 404。真池形态下“那一行 \
     不存在”就是同一个自然事件，而 GET /api/daemon/tasks/:id/status 查不到任务时回的正是 \
     同一个 404（routes/daemon 逐字移植上游那份 GetTaskStatus 的判定顺序）⇒ 这条 fixture \
     的断言在真库形态下**成立**。同组的 TestGetTaskStatus_TransientDBError_Returns500 要的是 \
     “池在应答前失败”，真池给不出，仍不可判定",
)];

/// 这个场景的前提是否已在 [`NATURALLY_REPRODUCED`] 里登记（见那张表的纪律段）。
#[must_use]
pub fn is_naturally_reproduced(test: &str, requirement: &str) -> bool {
    NATURALLY_REPRODUCED
        .iter()
        .any(|(t, r, _)| *t == test && *r == requirement)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::requirements::{missing_requirements, requirement};
    use crate::{Fixture, Tier};

    /// 真实的 golden 目录（不现编 fixture）：本片要判的是「那 25 条」，
    /// 现编一条只会证明现编的那条。
    fn golden() -> Vec<Fixture> {
        let dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../contracts/golden");
        crate::load_dir(&dir).expect("contracts/golden loads")
    }

    /// 每行必须指向一条**真实存在**的 fixture，且理由非空 —— 否则这张表会退化成
    /// 「凭一条注释关掉一道判据」。
    #[test]
    fn every_naturally_reproduced_row_names_a_real_fixture_and_states_why() {
        let all = golden();
        for (test, req, why) in NATURALLY_REPRODUCED {
            assert!(why.trim().len() > 40, "{test}/{req}: 理由太短");
            let hits: Vec<&Fixture> = all
                .iter()
                .filter(|fx| fx.source.test == *test && fx.requirement_ids().contains(req))
                .collect();
            assert!(
                !hits.is_empty(),
                "{test}: 语料里没有声明了 {req} 的 fixture ⇒ 这一行应当划掉"
            );
            assert!(
                requirement(req).is_some(),
                "{req} 不在前提表里（登记了一个未知 id）"
            );
        }
    }

    /// 🔴 反向硬要求：同一个前提 id 下**别的** fixture 必须仍不可判定 ——
    /// 否则这张表就是「按 id 放宽 `satisfied_by`」的同义词，而那正是它要取代的做法。
    #[test]
    fn the_override_does_not_silently_widen_the_requirement() {
        let all = golden();
        for (test, req, _) in NATURALLY_REPRODUCED {
            for fx in all.iter().filter(|fx| fx.requirement_ids().contains(req)) {
                if fx.source.test == *test {
                    continue;
                }
                let missing = missing_requirements(fx, Tier::Database).expect("known requirement");
                assert!(
                    missing.contains(req),
                    "{}: {} 已在 {} 那一行被自然复现，本条却跟着进了判定 —— \
                     登记必须逐场景，不许按 id 放宽",
                    fx.id,
                    req,
                    test
                );
            }
        }
    }
}
