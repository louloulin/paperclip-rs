use super::{
    chunk_message, utf16_units, Arc, DeliveryLedger, Duration, Outbound, Sender, TelegramApi,
    EDIT_INTERVAL, MAX_MESSAGE_UNITS,
};

impl Outbound {
    /// 装配（生产节流）。
    #[must_use]
    pub fn new(api: Arc<dyn TelegramApi>, ledger: Arc<DeliveryLedger>) -> Self {
        Self {
            sender: Sender::new(Arc::clone(&api)),
            api,
            ledger,
            edit_interval: EDIT_INTERVAL,
        }
    }

    /// 注入节流间隔（用例把它压到 0，好让"节流"这条判据可测而不睡真觉）。
    #[must_use]
    pub fn with_edit_interval(mut self, interval: Duration) -> Self {
        self.edit_interval = interval;
        self
    }

    /// 发送器（`Channel::send` 走的就是它）。
    #[must_use]
    pub fn sender(&self) -> &Sender {
        &self.sender
    }

    /// 投递账（宿主装配 / 用例断言用）。
    #[must_use]
    pub fn ledger(&self) -> &Arc<DeliveryLedger> {
        &self.ledger
    }

    /// 取这一轮的分片计划（上游 `initializeTerminalReply` 里 `chunkMessage` 那一步）。
    ///
    /// 顺序逐字照上游：**先**按 UTF-16 码元分片，**再**逐片渲染 —— 所以代码围栏不会跨片，
    /// 而"分几片"与"每片长什么样"两件事各有各的用例。
    #[must_use]
    pub fn plan_chunks(text: &str) -> Vec<String> {
        chunk_message(text, MAX_MESSAGE_UNITS)
    }

    /// 流式中途的超上限截断（上游 `pushPartial` 里的 `chunkMessage(text, max)[0]`）。
    ///
    /// 超了就把流式消息**冻在上限处**；完整回复由最终答案分片投递。
    #[must_use]
    pub fn stream_text_cap(snapshot: &str, max_units: usize) -> String {
        if utf16_units(snapshot) > max_units {
            return chunk_message(snapshot, max_units)
                .first()
                .cloned()
                .unwrap_or_default();
        }
        snapshot.to_string()
    }
}
