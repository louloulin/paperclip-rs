use super::*;

impl Outbound {
    /// 处理一帧 **partial**（上游 `pushPartial`）：首次发占位消息，之后编辑它。
    ///
    /// `lease` 必须是**已经在 `streaming` 阶段取到**的那一把：上游在这一帧里**持有**租约，
    /// 因为"检查完状态再发"这个间隙正是答案进聊天两次的成因（GH #8049）。
    /// `streamed_message_id` 是调用方本地记住的那条消息（0 = 还没有）。
    pub async fn push_partial(
        &self,
        target: &ReplyTarget,
        lease: &DeliveryLease,
        snapshot: &str,
        streamed_message_id: i64,
    ) -> StreamStep {
        // 行里已有的可编辑消息才是权威（可能是另一个副本发的，也可能来自这次自动重试继承的
        // 前一次尝试）⇒ 本地记的那个让位给它。
        let message_id = match lease.message_id() {
            0 => streamed_message_id,
            from_row => from_row,
        };
        let text = Self::stream_text_cap(snapshot, MAX_MESSAGE_UNITS);
        if text.trim().is_empty() && message_id != 0 {
            return StreamStep::Idle {
                reason: "nothing_to_edit",
            };
        }

        if message_id != 0 {
            // 编辑**之前**立刻重新证明租约：取到之后卡一下就可能越过租约，而之后的编辑会覆盖
            // 掉接管者已经发布的最终答案。
            match self.ledger.renew(lease).await {
                Ok(true) => {}
                Ok(false) => {
                    return StreamStep::Idle {
                        reason: "lease_lost",
                    }
                }
                Err(error) => {
                    return StreamStep::Failed {
                        message: error.to_string(),
                    }
                }
            }
            let params =
                EditMessageText::text(target.chat_id, message_id, format_html(&text)).html();
            return match self
                .api
                .edit_message_text(target.bot_token.expose(), &params)
                .await
            {
                Ok(()) => StreamStep::Edited,
                Err(error) if is_not_modified(&error) => StreamStep::Edited,
                Err(error) => {
                    if let Some(wait) = error.retry_after() {
                        return StreamStep::RetryAfter(wait);
                    }
                    StreamStep::Failed {
                        message: format!("stream edit failed ({})", error.method()),
                    }
                }
            };
        }

        // 一个轮次**恰好**一条占位消息，而且在调用**之前**就公开出去 —— 别的进程于是读到
        // "有一条发送在飞"而不是"什么都没发过"。
        match self.ledger.claim_send(lease).await {
            Ok(true) => {}
            Ok(false) => {
                return StreamStep::Idle {
                    reason: "send_already_outstanding",
                }
            }
            Err(error) => {
                return StreamStep::Failed {
                    message: error.to_string(),
                }
            }
        }
        let placeholder = first_non_empty(&format_html(&text), STREAM_PLACEHOLDER);
        let mut params = SendMessage::text(target.chat_id, placeholder);
        params.parse_mode = "HTML".to_string();
        params.message_thread_id = target.thread_id;
        params = params.with_reply_to(target.reply_to);
        match self
            .api
            .send_message(target.bot_token.expose(), &params)
            .await
        {
            Ok(message) => {
                let outcome = classify_send(Ok(()));
                match self
                    .ledger
                    .record_send(lease, true, message.message_id, 0, outcome)
                    .await
                {
                    Ok(_) => StreamStep::PlaceholderCreated {
                        message_id: message.message_id,
                    },
                    Err(error) => StreamStep::Failed {
                        message: error.to_string(),
                    },
                }
            }
            Err(error) => {
                let outcome = classify_send(Err(&error));
                let recorded = self.ledger.record_send(lease, true, 0, 0, outcome).await;
                if let Err(error) = recorded {
                    return StreamStep::Failed {
                        message: error.to_string(),
                    };
                }
                if let Some(wait) = error.retry_after() {
                    return StreamStep::RetryAfter(wait);
                }
                StreamStep::Failed {
                    message: format!("placeholder send failed ({})", error.method()),
                }
            }
        }
    }

    /// 投递**最终答案**（上游 `sendNextTerminalRequest` 的一次请求）：
    /// 有占位消息就先编辑它，否则（或编辑失败到该另发时）补发分片。
    ///
    /// 每一步只做**一片**（上游的 terminal worker 也是这样一片一片走），调用方按
    /// [`Step::RetryAfter`] 的节奏推进 —— 于是既不用睡真觉，也能把"部分投递"钉死。
    pub async fn deliver_answer(
        &self,
        target: &ReplyTarget,
        lease: &DeliveryLease,
        chunks: &[String],
        progress: &mut AnswerProgress,
    ) -> Step {
        if chunks.is_empty() {
            return self.finish(lease, "empty_reply").await;
        }
        if progress.streamed_message_id != 0 && !progress.placeholder_edited && !progress.fresh_send
        {
            return self
                .edit_streamed_reply(target, lease, chunks, progress)
                .await;
        }
        self.send_reply_chunk(target, lease, chunks, progress).await
    }

    /// 收口并回报（`settle` 失败才升级成 [`Step::Failed`]）。
    async fn finish(&self, lease: &DeliveryLease, reason: &str) -> Step {
        match self.ledger.settle(lease, reason).await {
            Ok(_) => Step::done(reason),
            Err(error) => Step::Failed {
                message: error.to_string(),
            },
        }
    }

    /// 把**占位消息**改成最终答案的第一片（上游 `editStreamedReply`）。
    ///
    /// 每个分支都继续操作**同一条**消息：在一条可能已经被编辑过的消息旁边再贴一份答案就是
    /// 重复（GH #8049）。上游那道**阶梯**逐级照搬：not-modified 当成功 → HTML 错就换纯文本
    /// 重试**同一目标** → 目标确实没了才允许新发 → 永久拒绝则停手 → 其余（结果不明）有界重试。
    async fn edit_streamed_reply(
        &self,
        target: &ReplyTarget,
        lease: &DeliveryLease,
        chunks: &[String],
        progress: &mut AnswerProgress,
    ) -> Step {
        if !matches!(self.ledger.renew(lease).await, Ok(true)) {
            return Step::done("lease_lost");
        }
        let chunk = chunks.first().cloned().unwrap_or_default();
        let text = first_non_empty(&format_html(&chunk), STREAM_PLACEHOLDER);
        let params =
            EditMessageText::text(target.chat_id, progress.streamed_message_id, text).html();
        let result = self
            .api
            .edit_message_text(target.bot_token.expose(), &params)
            .await;
        // 429 先于阶梯：Telegram 给的退避是**协议**层面的"现在别问"，不是"这条消息不行"。
        if let Some(wait) = result
            .as_ref()
            .err()
            .and_then(super::super::api::ApiError::retry_after)
        {
            return Step::RetryAfter(wait);
        }
        match classify_edit(&result) {
            EditVerdict::Applied | EditVerdict::NotModified => {}
            EditVerdict::MarkupRefused => {
                // Telegram 拒的是**标记**而不是这条消息：同一个目标，换纯文本再来一次。
                let plain =
                    EditMessageText::text(target.chat_id, progress.streamed_message_id, &chunk);
                let fallback = self
                    .api
                    .edit_message_text(target.bot_token.expose(), &plain)
                    .await;
                if let Some(wait) = fallback
                    .as_ref()
                    .err()
                    .and_then(super::super::api::ApiError::retry_after)
                {
                    return Step::RetryAfter(wait);
                }
                match classify_edit(&fallback) {
                    EditVerdict::Applied | EditVerdict::NotModified => {}
                    _ => return Step::RetryAfter(Duration::ZERO),
                }
            }
            EditVerdict::TargetMissing => {
                // 确认没了 —— **唯一**一种"另发一条新消息"不算重复的情形。
                progress.fresh_send = true;
                progress.chunk_index = 0;
                return Step::RetryAfter(Duration::ZERO);
            }
            EditVerdict::PermanentRejection => {
                // Telegram 会一直拒。停手而不是重发或空转：terminal 队列是**按会话**的，
                // 一条永不收口的回复会挡住同一个 chat 里之后的每一条答案。
                return self.finish(lease, "edit_rejected").await;
            }
            EditVerdict::Ambiguous => {
                // 结果不明：编辑**可能**已经生效。同一个目标上有界重试，然后用同一个理由放弃。
                progress.edit_attempts += 1;
                if progress.edit_attempts >= MAX_AMBIGUOUS_EDIT_ATTEMPTS {
                    return self.finish(lease, "edit_failed").await;
                }
                return Step::RetryAfter(TERMINAL_EDIT_RETRY_DELAY);
            }
        }
        self.record_first_chunk(lease, chunks, progress).await
    }

    /// 第一片已经落进那条占位消息：记账，并按结果回报"收口 / 继续下一片"。
    async fn record_first_chunk(
        &self,
        lease: &DeliveryLease,
        chunks: &[String],
        progress: &mut AnswerProgress,
    ) -> Step {
        progress.placeholder_edited = true;
        progress.chunk_index = 1;
        if let Err(error) = self
            .ledger
            .record_send(
                lease,
                false,
                progress.streamed_message_id,
                i32::try_from(progress.chunk_index).unwrap_or(i32::MAX),
                DeliveryOutcome::Accepted,
            )
            .await
        {
            return Step::Failed {
                message: error.to_string(),
            };
        }
        if progress.chunk_index == chunks.len() {
            return self.finish(lease, "delivered").await;
        }
        Step::RetryAfter(self.edit_interval)
    }

    /// 补发最终答案的**下一片**（上游 `sendReplyChunk`）。
    ///
    /// 发送**之前**先公开、**之后**记录结果 ⇒ 在别的进程里恢复的投递会接着这一片往下走而不是
    /// 重发它；而结果丢掉的那一片会让投递**停下**而不是发两次（`docs/60` §2.3）。
    async fn send_reply_chunk(
        &self,
        target: &ReplyTarget,
        lease: &DeliveryLease,
        chunks: &[String],
        progress: &mut AnswerProgress,
    ) -> Step {
        let Some(chunk) = chunks.get(progress.chunk_index) else {
            return self.finish(lease, "delivered").await;
        };
        if !matches!(self.ledger.claim_send(lease).await, Ok(true)) {
            return Step::done("send_already_outstanding");
        }
        let mut params = SendMessage::text(target.chat_id, format_html(chunk));
        params.parse_mode = "HTML".to_string();
        params.message_thread_id = target.thread_id;
        if progress.chunk_index == 0 {
            params = params.with_reply_to(target.reply_to);
        }
        match self
            .api
            .send_message(target.bot_token.expose(), &params)
            .await
        {
            Ok(message) => {
                progress.chunk_index += 1;
                if let Err(error) = self
                    .ledger
                    .record_send(
                        lease,
                        false,
                        message.message_id,
                        i32::try_from(progress.chunk_index).unwrap_or(i32::MAX),
                        DeliveryOutcome::Accepted,
                    )
                    .await
                {
                    return Step::Failed {
                        message: error.to_string(),
                    };
                }
                if progress.chunk_index == chunks.len() {
                    return self.finish(lease, "delivered").await;
                }
                // 只有第一片引用触发消息（上游逐字）。
                progress.streamed_message_id = progress.streamed_message_id.max(message.message_id);
                Step::RetryAfter(self.edit_interval)
            }
            Err(error) => {
                let outcome = classify_send(Err(&error));
                let _ = self.ledger.record_send(lease, false, 0, 0, outcome).await;
                if outcome == DeliveryOutcome::Unknown {
                    // 结果未知 ⇒ 停下并留证据（重发无法被平台去重）。
                    return self.finish(lease, "send_result_unknown").await;
                }
                if let Some(wait) = error.retry_after() {
                    return Step::RetryAfter(wait);
                }
                Step::RetryAfter(Duration::ZERO)
            }
        }
    }

    /// 投递**失败告知**（上游 `deliverFailureNotice` / `editNoticeOntoPlaceholder`）：
    /// 有占位消息就把它改成告知（**同一个目标**），否则另发一条。
    ///
    /// 与答案的投递**故意同形**：一步最多一次平台调用，回报 `retryAt` 让下一步先重新证明租约。
    /// 在一步里循环（重试一次编辑、把 429 等掉）意味着等待之后的那次调用可能远远越过租约，
    /// 落在一个已经被别的副本接管、甚至已经完成的轮次上（上游注释逐字）。
    pub async fn deliver_failure_notice(
        &self,
        target: &ReplyTarget,
        lease: &DeliveryLease,
        text: &str,
        streamed_message_id: i64,
        progress: &mut AnswerProgress,
    ) -> Step {
        if matches!(self.ledger.inherited_send(lease).await, Ok(true)) {
            // 没人能解释的一条发送：用户可能已经在读关于这一轮的某条消息了。
            return self.finish(lease, "send_result_unknown").await;
        }
        let message_id = match lease.message_id() {
            0 => streamed_message_id,
            from_row => from_row,
        };
        if message_id != 0 && !progress.fresh_send {
            if !matches!(self.ledger.renew(lease).await, Ok(true)) {
                return Step::done("lease_lost");
            }
            let params =
                EditMessageText::text(target.chat_id, message_id, format_html(text)).html();
            let result = self
                .api
                .edit_message_text(target.bot_token.expose(), &params)
                .await;
            if let Some(wait) = result
                .as_ref()
                .err()
                .and_then(super::super::api::ApiError::retry_after)
            {
                return Step::RetryAfter(wait);
            }
            match classify_edit(&result) {
                EditVerdict::Applied | EditVerdict::NotModified => {
                    return self.finish(lease, "failure_notice").await
                }
                EditVerdict::MarkupRefused => {
                    // 告知是纯文本，HTML 被拒只可能是把整段当标记了 ⇒ 换纯文本再来一次。
                    let plain = EditMessageText::text(target.chat_id, message_id, text);
                    let fallback = self
                        .api
                        .edit_message_text(target.bot_token.expose(), &plain)
                        .await;
                    if let Some(wait) = fallback
                        .as_ref()
                        .err()
                        .and_then(super::super::api::ApiError::retry_after)
                    {
                        return Step::RetryAfter(wait);
                    }
                    match classify_edit(&fallback) {
                        EditVerdict::Applied | EditVerdict::NotModified => {
                            return self.finish(lease, "failure_notice").await
                        }
                        _ => return Step::RetryAfter(Duration::ZERO),
                    }
                }
                EditVerdict::TargetMissing => {
                    progress.fresh_send = true;
                    return Step::RetryAfter(Duration::ZERO);
                }
                EditVerdict::PermanentRejection => {
                    // 保留占位消息而不是复制一轮：运行结果在 Multica 里本来就看得到。
                    return self.finish(lease, "edit_rejected").await;
                }
                EditVerdict::Ambiguous => {
                    progress.edit_attempts += 1;
                    if progress.edit_attempts >= MAX_NOTICE_EDIT_ATTEMPTS {
                        return self.finish(lease, "edit_failed").await;
                    }
                    return Step::RetryAfter(TERMINAL_EDIT_RETRY_DELAY);
                }
            }
        }
        if !matches!(self.ledger.claim_send(lease).await, Ok(true)) {
            return Step::done("send_already_outstanding");
        }
        let mut params = SendMessage::text(target.chat_id, text);
        params.message_thread_id = target.thread_id;
        params = params.with_reply_to(target.reply_to);
        match self
            .api
            .send_message(target.bot_token.expose(), &params)
            .await
        {
            Ok(message) => {
                // 告知成为这一轮的可编辑消息（`placeholder = true`，上游同）。
                let outcome = classify_send(Ok(()));
                let _ = self
                    .ledger
                    .record_send(lease, true, message.message_id, 0, outcome)
                    .await;
                self.finish(lease, "failure_notice").await
            }
            Err(error) => {
                let outcome = classify_send(Err(&error));
                let _ = self.ledger.record_send(lease, true, 0, 0, outcome).await;
                if outcome == DeliveryOutcome::Unknown {
                    return self.finish(lease, "send_result_unknown").await;
                }
                if let Some(wait) = error.retry_after() {
                    return Step::RetryAfter(wait);
                }
                Step::Failed {
                    message: format!("failure notice failed ({})", error.method()),
                }
            }
        }
    }
}
