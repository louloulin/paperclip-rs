//! lark **启动期回填**：两件一次性的升级修复（上游
//! `internal/integrations/lark/{union_id_backfill.go,region_backfill.go}`，**201 行**）。
//!
//! - **写者**：M7-14（`docs/60-M7-PLAN.md` §3.3）。
//! - **为什么在安装面这一片**：两条回填都只碰 `lark_installation` 的**列**（`bot_union_id` 与
//!   `region`），没有 wire、没有协议：它们是安装行的**升级修复**，不是运行时回路。
//!
//! # 两条回填各解决什么
//!
//! 1. **[`backfill_bot_union_ids`]**（上游 `BackfillBotUnionIDs`）：迁移 `112` 之前创建的安装
//!    没有 `bot_union_id`。群聊里两个 Bot 同时被装时，Lark 的 `mentions[].id.open_id` 与
//!    `/bot/v3/info` 的 `bot.open_id` 在**两个 WS 视角下结构相反**，唯一一致的是 `union_id`
//!    ⇒ 没有它的行在多 Bot 群里会让**错的** supervisor 处理事件（MUL-2671）。
//!    新安装的设备流收尾**已经**写它 ⇒ 这是一座**一次性**的桥，不是常驻任务。
//! 2. **[`backfill_region_from_legacy_override`]**（上游 `BackfillRegionFromLegacyOverride`）：
//!    迁移 `116` 把所有既有行回填成 `'feishu'`（大陆默认）。而一个**整站**都在用
//!    `MULTICA_LARK_HTTP_BASE_URL` / `…_CALLBACK_BASE_URL` 指向国际 Lark 的自建部署，
//!    它的每一行**其实**都是 Lark ⇒ 一旦 `region` 开始决定主机，这些行会去 `open.feishu.cn`
//!    并在运维清掉 override 的那一刻全坏。判据是**那个 override 主机**：它是**部署级**的，
//!    所以"这个部署上的每一个既有安装都是 Lark"是**确定的**，没有混合态可误判。
//!
//! # 三条共同的纪律（上游注释逐字）
//!
//! - **尽力而为、逐行幂等**：某一行的瞬时 HTTP / 解密 / DB 错误**不**阻断后面的行，
//!   也**不**打断服务器启动；每个结局都带 installation id 记一条日志，运维能在部署后审计覆盖率；
//! - **软失败**：Lark 在通讯录范围受限时回 `code=0` + **空** `union_id` ⇒ 记 warn 走人
//!   （单 Bot 部署里解码器的 `open_id` 过渡回落让安装仍然可用）；
//! - **调用方在启动期的独立协程里发起它**：一次慢的 Lark 往返不该挡住 HTTP listener 启动。
//!   在 `tokio` 里就是 `tokio::spawn(backfill_bot_union_ids(...))` —— 本文件**不**自己 spawn，
//!   好让调用点（宿主）对"启动期做了什么"负全责。
//!
//! # 与上游的形态差异（登记 `docs/32` §30 的 **D3** / **D8**）
//!
//! | # | 差异 | 理由 |
//! | --- | --- | --- |
//! | **D3** | `queries *ChannelStore` 与 `creds CredentialsDecrypter` → [`InstallationService`] + [`LarkInstallationStore`] 的两个端口方法 | 层次铁律：`mc-channel` 不写 SQL（列表与列写都在端口实现里） |
//! | **D8** | `log *slog.Logger` 参数**消失** | 本仓用 `tracing`（全局已初始化）：递一个 logger 进纯逻辑层只会给它一个可选的全局后门 |
//! | **D9** | `MULTICA_LARK_HTTP_BASE_URL` / `…_CALLBACK_BASE_URL` **不落地** | 本仓的 region 解析已经在 `lark_installation.region` 上（M7-10…13 都按列走）；重建那两个部署级 override 会与"每安装 region"抢同一职责 ⇒ 回填的**判据**保留成公开入参（[`is_lark_international_host`] + 两个 `&str`），调用点按自己的装配填 |

use std::sync::Arc;

use super::client::ApiClient;
use super::installation::{Installation, InstallationService, LarkInstallationStore};
use super::params::InstallationCredentials;

/// 一次回填的计数（上游 `attempted / filled / missed / errored` 四个整数）。
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct BackfillStats {
    /// 真的发起过 `GetBotInfo` 的行数（跳过已有 `union_id` 的那些）。
    pub attempted: u64,
    /// 成功写入 `bot_union_id` 的行数。
    pub filled: u64,
    /// Lark 回了空 `union_id` 的行数（软失败）。
    pub missed: u64,
    /// 解密 / 取 Bot 信息 / 落库失败的行数。
    pub errored: u64,
}

/// 回填 `bot_union_id`（上游 `BackfillBotUnionIDs`）。
///
/// `api.is_configured() == false`（替身）⇒ **直接返回**并记一条 info：一个没接线的部署不该
/// 对着一个 stub 打一圈"回填失败"的 warn。
///
/// 单行的 Lark 往返按 10s 截断（上游 `context.WithTimeout(ctx, 10*time.Second)`，
/// 与客户端默认的每请求超时同值）⇒ 一行卡住不会钉住整个回填协程。
///
/// # Errors
///
/// 列表读失败（上游在这里记 warn 走人；本仓把它作为 `Err` 交给调用点，因为"读不到列表"
/// 与"某一行的瞬时错误"是两种不同的结局）。
pub async fn backfill_bot_union_ids(
    store: &Arc<dyn LarkInstallationStore>,
    installs: &InstallationService,
    api: &Arc<dyn ApiClient>,
) -> Result<BackfillStats, String> {
    let mut stats = BackfillStats::default();
    if !api.is_configured() {
        tracing::info!("lark backfill: APIClient not configured; skipping union_id backfill");
        return Ok(stats);
    }
    let rows = store.list_active_missing_union_id().await?;
    for row in rows {
        stats.attempted += 1;
        match fill_one_union_id(store, installs, api, &row).await {
            Ok(Filled::Stamped) => stats.filled += 1,
            Ok(Filled::Absent) => stats.missed += 1,
            Err(()) => stats.errored += 1,
        }
    }
    tracing::info!(
        attempted = stats.attempted,
        filled = stats.filled,
        missed = stats.missed,
        errored = stats.errored,
        "lark backfill: union_id pass complete"
    );
    Ok(stats)
}

/// 一行的结局（`Err(())` = 已记过日志的失败）。
enum Filled {
    /// 写进去了。
    Stamped,
    /// Lark 没给 `union_id`（软失败）。
    Absent,
}

/// 一行：解密 → 取 Bot 信息 → 列写。三条失败路径各记一条带 installation id 的 warn。
async fn fill_one_union_id(
    store: &Arc<dyn LarkInstallationStore>,
    installs: &InstallationService,
    api: &Arc<dyn ApiClient>,
    row: &Installation,
) -> Result<Filled, ()> {
    let Ok(secret) = installs.decrypt_app_secret(row) else {
        tracing::warn!(
            installation_id = %row.id,
            app_id = %row.app_id,
            "lark backfill: decrypt app_secret failed"
        );
        return Err(());
    };
    let mut credentials =
        InstallationCredentials::new(row.app_id.clone(), secret).with_region(row.region);
    if let Some(tenant_key) = row.tenant_key.clone() {
        credentials = credentials.with_tenant_key(tenant_key);
    }
    let Ok(info) = api.get_bot_info(credentials).await else {
        tracing::warn!(
            installation_id = %row.id,
            app_id = %row.app_id,
            "lark backfill: GetBotInfo failed"
        );
        return Err(());
    };
    if !info.has_union_id() {
        tracing::warn!(
            installation_id = %row.id,
            app_id = %row.app_id,
            "lark backfill: union_id absent in Lark response; leaving NULL"
        );
        return Ok(Filled::Absent);
    }
    if store
        .set_bot_union_id(row.id, &info.union_id)
        .await
        .is_err()
    {
        tracing::warn!(
            installation_id = %row.id,
            "lark backfill: persist union_id failed"
        );
        return Err(());
    }
    tracing::info!(
        installation_id = %row.id,
        app_id = %row.app_id,
        "lark backfill: stamped union_id"
    );
    Ok(Filled::Stamped)
}

/// 回填 `region`（上游 `BackfillRegionFromLegacyOverride`）。
///
/// 两个 override 主机**都**不是国际 Lark 主机（含空串 / 大陆主机 / mock / staging）
/// ⇒ 迁移 `116` 的 `'feishu'` 默认对这些行就是对的 ⇒ **一条 `UPDATE` 都不发**。
/// 这条 gating 正是它安全的原因。
///
/// # Errors
///
/// 列写失败（上游记 warn 走人；本仓把它作为 `Err` 交给调用点）。
pub async fn backfill_region_from_legacy_override(
    store: &Arc<dyn LarkInstallationStore>,
    http_override: &str,
    callback_override: &str,
) -> Result<u64, String> {
    if !is_lark_international_host(http_override) && !is_lark_international_host(callback_override)
    {
        return Ok(0);
    }
    let rows = store.relabel_region_to_lark().await?;
    if rows > 0 {
        tracing::info!(
            rows,
            "lark region backfill: relabelled legacy Lark-international installs"
        );
    }
    Ok(rows)
}

/// 一个配置的主机是不是国际 Lark 的开放平台主机（上游 `isLarkInternationalHost`）。
///
/// **解析 URL 并逐字比 host** —— 于是大陆主机、空值、staging / mock URL **永不**触发换标。
/// （不解析就比字符串，会把 `https://open.larksuite.com.evil/` 也算进去。）
#[must_use]
pub fn is_lark_international_host(raw: &str) -> bool {
    let raw = raw.trim();
    if raw.is_empty() {
        return false;
    }
    let Ok(url) = reqwest::Url::parse(raw) else {
        return false;
    };
    url.host_str()
        .is_some_and(|host| host.eq_ignore_ascii_case("open.larksuite.com"))
}

/// 回填的两条入口在同一个协程里按**顺序**跑（上游 `router.go` 的 boot 段同序：
/// `union_id` 需要网络、region 只需要一次列写）。
///
/// **不**自己 spawn：调用点（宿主）负责把它放进启动期的独立协程，于是"启动期做了什么"
/// 有一个唯一可见的地方。
pub async fn run_boot_backfills(
    store: &Arc<dyn LarkInstallationStore>,
    installs: &InstallationService,
    api: &Arc<dyn ApiClient>,
    http_override: &str,
    callback_override: &str,
) -> BackfillStats {
    if let Err(message) =
        backfill_region_from_legacy_override(store, http_override, callback_override).await
    {
        tracing::warn!(%message, "lark region backfill: relabel failed");
    }
    match backfill_bot_union_ids(store, installs, api).await {
        Ok(stats) => stats,
        Err(message) => {
            tracing::warn!(%message, "lark backfill: list installations failed");
            BackfillStats::default()
        }
    }
}

#[cfg(test)]
mod tests;
