use super::*;

/// **一个安装的** Stream 连接（上游 `dingtalkChannel`）。
///
/// 每个安装带自己的机器人（自己的 `AppKey` + 密文 `AppSecret` 在安装配置里）⇒ 它有自己的连接，
/// 与 per-installation 的 slack / telegram 完全同形。`engine.Supervisor` 按活跃安装各建一条
/// （经注册的 [`Factory`]），并由它持有租约 / 重连生命周期。
pub struct DingTalkChannel {
    app_key: String,
    /// 明文 `AppSecret`（**手写脱敏**类型）：开 Stream 连接 + 铸访问令牌（后者归 M7-8）。
    app_secret: AppSecret,
    handler: Option<SharedInboundHandler>,
    opener: Arc<dyn ConnectionOpener>,
    dialer: Arc<dyn WsDialer>,
    /// 本安装的入站队列（**跨重连复用**，见模块文档）。
    dispatcher: Arc<Dispatcher>,
    slots: Arc<DispatchSlotRegistry>,
    knobs: StreamKnobs,
    /// 生命周期停机的判决（见 [`RelinquishGuard`]）。
    relinquish: AtomicBool,
    /// 出站端口（**M7-8** 注入；见 [`DingTalkChannel::with_outbound`]）。
    ///
    /// `None` ⇒ `send` **失败关闭**（构造器不隐式造 HTTP 客户端；工厂会注入）。
    outbound: Option<Arc<dyn outbound::OpenApiTransport>>,
    /// 机器人码（上游 `robotCodeOrAppID`）。`send` 的请求体要它。
    robot_code: String,
}

impl std::fmt::Debug for DingTalkChannel {
    /// 手写脱敏：`app_secret` 是 [`AppSecret`]（`<redacted>`），端口只打印存在性。
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("DingTalkChannel")
            .field("app_key", &self.app_key)
            .field("app_secret", &self.app_secret)
            .field("has_handler", &self.handler.is_some())
            .field("opener", &"<dyn ConnectionOpener>")
            .field("dialer", &"<dyn WsDialer>")
            .field("dispatcher", &self.dispatcher)
            .field("slots", &self.slots)
            .field("knobs", &self.knobs)
            .field("relinquish", &self.relinquish.load(Ordering::SeqCst))
            .field(
                "outbound",
                &self
                    .outbound
                    .as_ref()
                    .map_or("<none>", |_| "<dyn OpenApiTransport>"),
            )
            .field("robot_code", &self.robot_code)
            .finish()
    }
}

impl DingTalkChannel {
    /// 装配一条连接。
    #[must_use]
    pub fn new(
        app_key: impl Into<String>,
        app_secret: AppSecret,
        handler: Option<SharedInboundHandler>,
        opener: Arc<dyn ConnectionOpener>,
        dialer: Arc<dyn WsDialer>,
        dispatcher: Arc<Dispatcher>,
        slots: Arc<DispatchSlotRegistry>,
    ) -> Self {
        let app_key = app_key.into();
        Self {
            robot_code: app_key.clone(),
            app_key,
            app_secret,
            handler,
            opener,
            dialer,
            dispatcher,
            slots,
            knobs: StreamKnobs::default(),
            relinquish: AtomicBool::new(false),
            outbound: None,
        }
    }

    /// 注入出站端口（**M7-8**；工厂在装配时调它）。
    ///
    /// 不注入也可以构造（M7-7 的用例走的就是那条路）—— 那时 `send` **失败关闭**，
    /// 而不是偷偷去造一个 `reqwest` 客户端。生产路径由 [`factory_with_slots`] 注入。
    #[must_use]
    pub fn with_outbound(mut self, transport: Arc<dyn outbound::OpenApiTransport>) -> Self {
        self.outbound = Some(transport);
        self
    }

    /// 换机器人码（上游 `robotCodeOrAppID`）。
    #[must_use]
    pub fn with_robot_code(mut self, robot_code: impl Into<String>) -> Self {
        self.robot_code = robot_code.into();
        self
    }

    /// 换时间旋钮（用例用；上游的 30s / 90s / 10s 是生产默认值）。
    #[must_use]
    pub fn with_knobs(mut self, knobs: StreamKnobs) -> Self {
        self.knobs = knobs;
        self
    }

    /// 本安装的 AppKey（路由键；回调的信封要盖上它）。
    #[must_use]
    pub fn app_key(&self) -> &str {
        &self.app_key
    }

    /// 本安装的入站队列（诊断 / 用例）。
    #[must_use]
    pub fn dispatcher(&self) -> &Arc<Dispatcher> {
        &self.dispatcher
    }

    /// 这一代是否被判为"生命周期停机"（诊断 / 用例）。
    #[must_use]
    pub fn relinquished(&self) -> bool {
        self.relinquish.load(Ordering::SeqCst)
    }

    /// 用例用：把判决置成"生命周期停机"（等价于 `connect` 的 future 被 supervisor 丢弃）。
    #[cfg(test)]
    pub(crate) fn relinquish_for_test(&self) {
        self.relinquish.store(true, Ordering::SeqCst);
    }
}

#[async_trait]
impl Channel for DingTalkChannel {
    fn kind(&self) -> ChannelKind {
        TYPE_DINGTALK
    }

    /// 建连并跑帧循环（上游 `dingtalkChannel.Connect`）。
    ///
    /// 三种收尾**各不相同**（上游逐字）：
    ///
    /// - 网关发 `SYSTEM/disconnect` ⇒ 干净返回（supervisor 退避后重拨，**队列保留**）；
    /// - 停机信号（队列收口 / supervisor 丢弃本 future）⇒ 干净返回，且**判为生命周期停机**
    ///   （`disconnect` 会收口队列）；
    /// - 链路断 / 读超时 / 引导失败 ⇒ `Err`（supervisor 按"这次尝试失败"退避重连）。
    async fn connect(&self) -> ChannelResult<()> {
        if self.handler.is_none() {
            return Err(ChannelError::InvalidConfig {
                kind: TYPE_DINGTALK.as_str().to_string(),
                reason: "inbound handler not configured".to_string(),
            });
        }
        if self.app_secret.is_empty() {
            return Err(ChannelError::InvalidConfig {
                kind: TYPE_DINGTALK.as_str().to_string(),
                reason: "app secret not configured".to_string(),
            });
        }
        // 每一代重新开始判决（同一个 channel 对象可能被反复 connect）。
        self.relinquish.store(false, Ordering::SeqCst);
        let verdict = RelinquishGuard::new(&self.relinquish);

        let connector = Connector::new(
            Arc::clone(&self.opener),
            Arc::clone(&self.dialer),
            self.app_key.clone(),
            self.app_secret.clone(),
            Arc::new(DispatchSink {
                app_key: self.app_key.clone(),
                dispatcher: Arc::clone(&self.dispatcher),
            }),
        )
        .with_knobs(self.knobs);

        // 队列收口 ⇒ 会话也优雅退出（宿主停机时先收队列，帧循环随之收尾）。
        let stop = StopHandle::from_receiver(self.dispatcher.closed_receiver());
        let outcome = connector.run_session(stop).await;
        verdict.defuse();
        match outcome {
            Ok(SessionOutcome::Cancelled | SessionOutcome::DisconnectRequested) => {
                tracing::info!(
                    app_key = self.app_key,
                    outcome = ?outcome,
                    "dingtalk: stream session ended cleanly"
                );
                Ok(())
            }
            Err(error) => {
                tracing::warn!(
                    app_key = self.app_key,
                    code = error.code(),
                    "dingtalk: stream session failed"
                );
                Err(error)
            }
        }
    }

    /// 拆链路（上游 `dingtalkChannel.Disconnect`）。
    ///
    /// 判决见模块文档：**只有**判为"生命周期停机"的那一代才收口队列 —— 传输错误 / 网关要求
    /// 重连之后，队列必须活着（跨重连的会话顺序优先）。收到口后的槽不再复用 ⇒ 下一代会建新队列。
    async fn disconnect(&self) -> ChannelResult<()> {
        if !self.relinquished() {
            return Ok(());
        }
        let drained = self
            .dispatcher
            .drain_and_close(DISCONNECT_DRAIN_BUDGET)
            .await;
        self.slots.release(&self.app_key, &self.dispatcher);
        if drained {
            Ok(())
        } else {
            // 收口预算用尽（上游同样回错误：`dingtalk: dispatcher drain: …`）。supervisor 只
            // 记一条 warn（`disconnect` 不在退避路径上）。
            Err(ChannelError::Shutdown)
        }
    }

    /// 出站：用本安装的机器人往 `out.chat_id` 发一条群消息（上游 `dingtalkChannel.Send`）。
    ///
    /// 上游逐字：`Send` 只给 `out.ChatID`，所以目标是**群**（引用 / 直聊那些形态由
    /// `outbound.rs` / `replier.rs` 的完整目标构造）。
    /// 没注入端口 ⇒ **失败关闭**（见 [`DingTalkChannel::with_outbound`]）。
    async fn send(&self, out: OutboundMessage) -> ChannelResult<SendResult> {
        let Some(transport) = self.outbound.as_ref() else {
            // `send` 不在 supervisor 的退避路径上，用 `Transport` 表达"这条链路不可用"即可。
            return Err(ChannelError::Transport {
                message: "dingtalk: outbound send is not wired for this installation (M7-8)"
                    .to_string(),
            });
        };
        let sender = outbound::Sender::new(
            Arc::clone(transport),
            self.robot_code.clone(),
            self.app_key.clone(),
            self.app_secret.clone(),
        );
        let target = outbound::SendTarget::group(out.chat_id.clone());
        let key = sender
            .send(&target, &out.text)
            .await
            .map_err(outbound::DingTalkApiError::into_channel_error)?;
        Ok(SendResult::single(key))
    }

    /// 上游 `CapText | CapAttachment`。
    ///
    /// `ATTACHMENT` 的**实现**（引用卡片 / 互动卡片 / 媒体出站）归 M7-8；本片照上游声明位图，
    /// 并在 `M7-8` 接上出站之后才真正成立（同 M7-3 → M7-4 的先例）。
    fn capabilities(&self) -> Capability {
        Capability::TEXT.union(Capability::ATTACHMENT)
    }
}
