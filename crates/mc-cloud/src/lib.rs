//! `mc-cloud` —— Multica 云侧出站传输（`docs/62-M9-PLAN.md` §2.1 的「出站传输（唯一）」层）。
//!
//! # 这个 crate 是什么
//!
//! **一条**出站钢管（[`transport::Client`]）+ 三个面的路径/形状位（[`billing`] /
//! [`subscriptions`] / [`webhook`]，各片原地填充）+ cloud-runtime 面的形状位（[`runtime`]）。
//!
//! | 模块 | 写者 | 内容 |
//! | --- | --- | --- |
//! | [`config`] | **M9-0（本片）** | `MULTICA_CLOUD_URL` 的读取口与校验（唯一） |
//! | [`error`] | **M9-0（本片）** | `CloudError` 七变体（不回显 URL / 响应体） |
//! | [`transport`] | **M9-0（本片）** | 客户端 / 请求 / 响应 / 计量 / 错误分类（唯一实现） |
//! | [`billing`] | M9-1 | 8 条 billing 代理的出站路径与响应类型位 |
//! | [`subscriptions`] | M9-2 | 7 条 subscriptions 代理的出站路径与注入体形状位 |
//! | [`webhook`] | M9-6 | stripe 转发的路径与原始体纪律位 |
//! | [`runtime`] | M9-11 | `/api/cloud-runtime/*` 11 条的出站路径与响应类型位 |
//!
//! # 为什么是**独立 crate**（`docs/62` §2.2 判据 1/3）
//!
//! ① 它带 `reqwest` ⇒ 不能落 `mc-core`（那是零 IO 的领域层）；
//! ② 它**不是** daemon 面（只有 HTTP 宿主消费）⇒ 不必比 `mc-http` 更靠下，但三个路由簇
//! **共用一个 base URL** ⇒ 独立 crate 的收益是「一份基址解析 + 一份错误映射」。
//!
//! # 边界（写进各片 `DoD`，`docs/62` §2.7）
//!
//! 1. `mc-http` 的任何 handler **不得**直接 `reqwest`：出站只能经 [`transport::Client`]；
//! 2. `localhost` 之外无硬编码：`MULTICA_CLOUD_URL` 是**唯一**基址来源
//!    （禁止在代码里写 `api.stripe.com` 或云域名）；
//! 3. 承载 URL / 响应体 / `Idempotency-Key` 的类型一律**手写 `Debug`**（判据 ①）。
//!
//! # 上游对照
//!
//! `server/internal/cloudruntime/client.go`（255 行）—— 本 crate 的 [`transport`] 是它的
//! 完整实现（先例：M7-0 完整实现了 `mc-secrets/src/secretbox.rs`）。这是 anchor **唯一**
//! "实现"的部分，因为它是三个路由簇的**唯一使能件**：没有它，M9-1 / M9-2 / M9-6 一片都
//! 无法离线测试（`docs/62` §4.2 的替身接缝就是基址可注入）。
//!
//! 偏离登记见 `docs/32-M3-DAEMON-FACE.md` §9.13。

pub mod billing;
pub mod config;
pub mod error;
pub mod runtime;
pub mod subscriptions;
pub mod transport;
pub mod webhook;

pub use config::{CloudSettings, CLOUD_URL_ENV};
pub use error::CloudError;
pub use transport::{
    infer_op, status_bucket, Client, Config, Request, RequestRecorder, Response, DEFAULT_TIMEOUT,
    MAX_RESPONSE_BODY_SIZE,
};

#[cfg(test)]
mod tests {
    use super::*;

    /// 形状用例（`docs/62` §5 的 anchor 测试四类之一）：禁用态**不 panic、不发请求**。
    #[test]
    fn disabled_client_is_a_valid_value_not_an_error() {
        let settings = CloudSettings::default();
        assert!(!settings.is_configured());
        assert!(!Client::new(Config::default())
            .expect("空配置 = 禁用态")
            .enabled());
        assert!(!Client::disabled().enabled());
    }
}
