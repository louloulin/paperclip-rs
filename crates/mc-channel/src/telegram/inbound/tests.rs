//! `telegram::inbound` 的用例（写者 M7-5）。
//!
//! 上游 `telegram_test.go` 的 `TestInboundFromUpdate*` / `TestInboundGroup*` /
//! `TestInboundFreshAliasIsPlainText` / `TestInboundTelegramCommandSuffixes` /
//! `TestInboundUsesExactStructuredTelegramMentions` 逐条移植 —— 全部是**纯函数**用例，
//! 不需要库、也不需要网络。

use super::*;

/// 造一条 `private` 文本更新（用例的最小装置）。
fn private_update(update_id: i64, message_id: i64, text: &str) -> Update {
    Update {
        update_id,
        message: Some(Box::new(Message {
            message_id,
            from: Some(User {
                id: 111,
                is_bot: false,
                first_name: "Ada".to_string(),
                last_name: "L".to_string(),
                username: "ada".to_string(),
            }),
            chat: Chat {
                id: 555,
                chat_type: "private".to_string(),
            },
            text: text.to_string(),
            ..Message::default()
        })),
    }
}

/// 造一条 `supergroup` 更新（可选被引用消息）。
fn group_update(text: &str, reply_to: Option<Message>) -> Update {
    Update {
        update_id: 1,
        message: Some(Box::new(Message {
            message_id: 2,
            from: Some(User {
                id: 111,
                first_name: "U".to_string(),
                ..User::default()
            }),
            chat: Chat {
                id: -100_200,
                chat_type: "supergroup".to_string(),
            },
            text: text.to_string(),
            reply_to_message: reply_to.map(Box::new),
            ..Message::default()
        })),
    }
}

/// 上游 `TestInboundFromUpdatePrivateText`：p2p 恒为"在跟 bot 说话"，raw 带 bot id 与显示名。
#[test]
fn private_text_is_always_addressed_and_carries_the_raw_envelope() {
    let message = inbound_from_update(&private_update(42, 7, "hello there"), 999, "my_bot")
        .expect("accepted");
    assert_eq!(message.event_id, "42");
    assert_eq!(message.message_id, "555:7");
    assert_eq!(message.source.chat_type, ChatType::P2p);
    assert!(message.addressed_to_bot, "p2p 恒为寻址");
    assert_eq!(message.text, "hello there");
    assert_eq!(message.command_text, "hello there");
    assert_eq!(message.kind, MessageKind::Text);
    assert!(
        message.media_refs.is_empty(),
        "media_refs 是 engine 的输出通道"
    );
    assert!(!message.skip_agent_run);
    let raw: RawEvent = serde_json::from_value(message.raw.clone()).expect("raw");
    assert_eq!(raw.bot_id, "999");
    assert_eq!(raw.event_type, EVENT_TYPE_MESSAGE);
    assert_eq!(raw.sender_name, "Ada L");
}

/// 上游 `TestInboundFromUpdateGroupAddressing`：三种寻址形态 + 未寻址的群聊照样进站。
#[test]
fn group_addressing_follows_the_upstream_three_ways() {
    let plain =
        inbound_from_update(&group_update("plain chatter", None), 999, "my_bot").expect("ingested");
    assert!(
        !plain.addressed_to_bot,
        "未寻址的群聊照样进站，但**不**算在跟 bot 说话（Router 去判）"
    );

    let mentioned = inbound_from_update(&group_update("@my_bot do the thing", None), 999, "my_bot")
        .expect("ingested");
    assert!(mentioned.addressed_to_bot);
    assert_eq!(mentioned.text, "do the thing", "提及词被剥掉");

    let bot_message = Message {
        message_id: 1,
        from: Some(User {
            id: 999,
            is_bot: true,
            ..User::default()
        }),
        ..Message::default()
    };
    let replied = inbound_from_update(&group_update("follow-up", Some(bot_message)), 999, "my_bot")
        .expect("ingested");
    assert!(replied.addressed_to_bot, "回复 bot 自己的消息算寻址");
    assert_eq!(replied.text, "follow-up");
    assert_eq!(replied.command_text, "follow-up");
}

/// 上游 `TestInboundGroupHumanReplyRequiresMentionAndPreservesQuotedContext`：只有"回复 + 提及"
/// 才把被引用的人消息前置进上下文，且 `command_text` 只留发送者自己的指令。
#[test]
fn quoted_human_context_requires_both_a_reply_and_a_mention() {
    let human = Message {
        message_id: 9,
        from: Some(User {
            id: 222,
            first_name: "Ada".to_string(),
            last_name: "Lovelace".to_string(),
            ..User::default()
        }),
        text: "/issue historical command".to_string(),
        ..Message::default()
    };

    let unaddressed = inbound_from_update(
        &group_update("summarize this", Some(human.clone())),
        999,
        "my_bot",
    )
    .expect("ingested");
    assert!(!unaddressed.addressed_to_bot, "只回复、没提及 ⇒ 不寻址");
    assert_eq!(
        unaddressed.text, "summarize this",
        "未寻址的消息**不**做上下文富化"
    );
    assert!(!unaddressed.has_selected_context);

    let addressed = inbound_from_update(
        &group_update("@my_bot summarize this", Some(human)),
        999,
        "my_bot",
    )
    .expect("ingested");
    assert!(addressed.addressed_to_bot);
    assert!(addressed.has_selected_context);
    assert_eq!(
        addressed.command_text, "summarize this",
        "command_text 只留发送者自己的指令"
    );
    for want in [
        "sender=\"Ada Lovelace\"",
        "/issue historical command",
        "summarize this",
    ] {
        assert!(addressed.text.contains(want), "富化后缺少 {want:?}");
    }
    // 被引用消息里的命令**只是历史**：command_text 里不出现它。
    assert!(!addressed.command_text.contains("/issue"));
}

/// 上游 `TestInboundGroupHumanReplyNewCommandPreservesQuotedContext` + `…ChatCommandUsesSameControlNormalization`。
#[test]
fn control_commands_are_stripped_from_the_agent_text_but_kept_in_command_text() {
    let quoted = Message {
        message_id: 9,
        from: Some(User {
            id: 222,
            first_name: "Ada".to_string(),
            last_name: "Lovelace".to_string(),
            ..User::default()
        }),
        text: "the deployment failed after the schema change".to_string(),
        ..Message::default()
    };
    let cleared = inbound_from_update(
        &group_update("@my_bot /clear summarize this", Some(quoted)),
        999,
        "my_bot",
    )
    .expect("ingested");
    assert!(cleared.force_fresh, "/clear 必须先于共享路由请求新会话");
    assert_eq!(cleared.command_text, "/clear summarize this");
    for want in [
        "sender=\"Ada Lovelace\"",
        "the deployment failed after the schema change",
        "summarize this",
    ] {
        assert!(cleared.text.contains(want), "缺少 {want:?}");
    }
    assert!(
        !cleared.text.contains("/clear"),
        "agent 可读正文里不能再有 /clear：{}",
        cleared.text
    );

    let new_chat = inbound_from_update(
        &group_update("@my_bot /new inspect this", None),
        999,
        "my_bot",
    )
    .expect("ingested");
    assert_eq!(new_chat.text, "inspect this");
    assert_eq!(new_chat.command_text, "/new inspect this");
    assert!(
        !new_chat.force_fresh,
        "/new 不带来 /clear 的 force_fresh 语义"
    );
}

/// 上游 `TestInboundGroupHumanReplyUsesCaptionAndHandlesNonText`：被引用消息回落 `caption`，
/// 媒体/空文本回落到占位串。
#[test]
fn quoted_context_falls_back_to_caption_then_to_a_placeholder() {
    let caption_reply = Message {
        message_id: 9,
        from: Some(User {
            id: 222,
            username: "ada".to_string(),
            ..User::default()
        }),
        caption: "diagram caption".to_string(),
        photo: vec![serde_json::json!({ "file_id": "f1" })],
        ..Message::default()
    };
    let empty_reply = Message {
        message_id: 9,
        from: Some(User {
            id: 222,
            username: "ada".to_string(),
            ..User::default()
        }),
        document: Some(serde_json::json!({ "file_name": "notes.txt" })),
        ..Message::default()
    };
    for (reply, wanted) in [
        (caption_reply, "diagram caption"),
        (empty_reply, "[empty or non-text message]"),
    ] {
        let message = inbound_from_update(
            &group_update("@my_bot inspect this", Some(reply)),
            999,
            "my_bot",
        )
        .expect("ingested");
        assert!(message.addressed_to_bot);
        assert_eq!(message.command_text, "inspect this");
        assert!(message.text.contains("sender=\"ada\""), "{}", message.text);
        assert!(message.text.contains(wanted), "{}", message.text);
    }
}

/// 上游 `TestInboundFromUpdateDropsBotsAndChannels`：四种"不得进核心"的更新。
#[test]
fn bots_channel_posts_and_unknown_chat_types_are_dropped() {
    let bot_sender = Update {
        update_id: 1,
        message: Some(Box::new(Message {
            from: Some(User {
                id: 5,
                is_bot: true,
                ..User::default()
            }),
            chat: Chat {
                id: 1,
                chat_type: "private".to_string(),
            },
            text: "x".to_string(),
            ..Message::default()
        })),
    };
    assert!(inbound_from_update(&bot_sender, 999, "b").is_none());

    let channel_post = Update {
        update_id: 1,
        message: Some(Box::new(Message {
            from: Some(User {
                id: 5,
                ..User::default()
            }),
            chat: Chat {
                id: 1,
                chat_type: "channel".to_string(),
            },
            text: "x".to_string(),
            ..Message::default()
        })),
    };
    assert!(inbound_from_update(&channel_post, 999, "b").is_none());

    assert!(inbound_from_update(&Update::default(), 999, "b").is_none());

    // 自己发的消息（同 id 的 bot）也被丢。
    let own = Update {
        update_id: 1,
        message: Some(Box::new(Message {
            from: Some(User {
                id: 999,
                is_bot: true,
                ..User::default()
            }),
            chat: Chat {
                id: 1,
                chat_type: "private".to_string(),
            },
            text: "x".to_string(),
            ..Message::default()
        })),
    };
    assert!(inbound_from_update(&own, 999, "b").is_none());
}

/// 上游 `TestInboundFreshAliasIsPlainText`：`/fresh` **不是**控制指令（只有 `/clear` 与 `/new` 是）。
#[test]
fn the_fresh_alias_is_plain_text() {
    let message = inbound_from_update(
        &private_update(1, 2, "/fresh start over please"),
        999,
        "my_bot",
    )
    .expect("ingested");
    assert!(!message.force_fresh);
    assert_eq!(message.text, "/fresh start over please");
    assert_eq!(message.command_text, message.text);
}

/// 上游 `TestInboundTelegramCommandSuffixes`：`@bot` 后缀只被剥掉，不改命令的语义。
#[test]
fn command_suffixes_are_stripped_without_changing_the_command() {
    let cases = [
        ("/fresh@my_bot continue", "/fresh continue", false),
        ("/clear@my_bot continue", "continue", true),
        ("/issue@my_bot fix login", "/issue fix login", false),
    ];
    for (text, want_text, want_fresh) in cases {
        let message =
            inbound_from_update(&group_update(text, None), 999, "my_bot").expect("ingested");
        assert!(message.addressed_to_bot, "{text}");
        assert_eq!(message.force_fresh, want_fresh, "{text}");
        assert_eq!(message.text, want_text, "{text}");
    }
}

/// 上游 `TestInboundUsesExactStructuredTelegramMentions`：entity 偏移是 **UTF-16** 单位，
/// 且**相似用户名不算提及**。
#[test]
fn structured_mentions_use_utf16_offsets_and_never_match_a_similar_username() {
    let cases: [(String, MessageEntity, bool, &str); 4] = [
        (
            "😀 @my_bot hello".to_string(),
            MessageEntity {
                entity_type: "mention".to_string(),
                offset: 3,
                length: 7,
            },
            true,
            "😀  hello",
        ),
        (
            "@my_bot_extra hello".to_string(),
            MessageEntity {
                entity_type: "mention".to_string(),
                offset: 0,
                length: 13,
            },
            false,
            "@my_bot_extra hello",
        ),
        (
            "/issue@my_bot fix".to_string(),
            MessageEntity {
                entity_type: "bot_command".to_string(),
                offset: 0,
                length: 13,
            },
            true,
            "/issue fix",
        ),
        (
            "/issue@my_bot_extra fix".to_string(),
            MessageEntity {
                entity_type: "bot_command".to_string(),
                offset: 0,
                length: 19,
            },
            false,
            "/issue@my_bot_extra fix",
        ),
    ];
    for (text, entity, addressed, want_text) in cases {
        let update = Update {
            update_id: 1,
            message: Some(Box::new(Message {
                message_id: 2,
                from: Some(User {
                    id: 3,
                    first_name: "U".to_string(),
                    ..User::default()
                }),
                chat: Chat {
                    id: -4,
                    chat_type: "supergroup".to_string(),
                },
                text: text.clone(),
                entities: vec![entity],
                ..Message::default()
            })),
        };
        let message = inbound_from_update(&update, 999, "my_bot").expect("ingested");
        assert_eq!(message.addressed_to_bot, addressed, "{text}");
        assert_eq!(message.text, want_text, "{text}");
    }
}

/// 论坛话题（`is_topic_message`）才带线程 id；p2p / 普通群消息不带。
#[test]
fn topic_messages_carry_a_thread_id_and_others_do_not() {
    let topic = Update {
        update_id: 1,
        message: Some(Box::new(Message {
            message_id: 5,
            from: Some(User {
                id: 7,
                first_name: "U".to_string(),
                ..User::default()
            }),
            chat: Chat {
                id: -100,
                chat_type: "supergroup".to_string(),
            },
            text: "@my_bot hi".to_string(),
            message_thread_id: 77,
            is_topic_message: true,
            ..Message::default()
        })),
    };
    let message = inbound_from_update(&topic, 999, "my_bot").expect("ingested");
    assert_eq!(message.source.thread_id, "77");

    // 带 `message_thread_id` 但**不是**话题消息（普通群的回复链）⇒ 线程 id 留空。
    let mut not_topic = topic.clone();
    if let Some(inner) = not_topic.message.as_mut() {
        inner.is_topic_message = false;
    }
    let message = inbound_from_update(&not_topic, 999, "my_bot").expect("ingested");
    assert_eq!(message.source.thread_id, "");
}

/// 回复上下文（`reply_to`）用复合消息 id，且 `root_id` 跟随线程。
#[test]
fn reply_context_uses_the_composite_message_key() {
    let quoted = Message {
        message_id: 8,
        from: Some(User {
            id: 222,
            first_name: "Ada".to_string(),
            ..User::default()
        }),
        text: "earlier".to_string(),
        ..Message::default()
    };
    let message =
        inbound_from_update(&group_update("plain", Some(quoted)), 999, "my_bot").expect("ingested");
    let reply = message.reply_to.expect("reply ctx");
    assert_eq!(reply.message_id, "-100200:8");
    assert_eq!(reply.root_id, "");
    assert_eq!(parse_message_ref(&reply.message_id), 8);
    assert_eq!(parse_message_ref("8"), 8);
    assert_eq!(parse_message_ref("not-a-ref"), 0);
}

/// 媒体分类：文本优先，其后按 image / audio / video / file / unknown。
#[test]
fn media_classification_prefers_text_then_the_platform_order() {
    let mut message = Message {
        photo: vec![serde_json::json!({})],
        voice: Some(serde_json::json!({})),
        ..Message::default()
    };
    assert_eq!(classify_message(&message), MessageKind::Image);
    message.photo.clear();
    assert_eq!(classify_message(&message), MessageKind::Audio);
    message.voice = None;
    message.video = Some(serde_json::json!({}));
    assert_eq!(classify_message(&message), MessageKind::Video);
    message.video = None;
    message.document = Some(serde_json::json!({}));
    assert_eq!(classify_message(&message), MessageKind::File);
    message.document = None;
    assert_eq!(classify_message(&message), MessageKind::Unknown);
    message.text = "hi".to_string();
    assert_eq!(classify_message(&message), MessageKind::Text, "文本优先");
}

/// 提及扫描器的边界语义（大小写不敏感 + 用户名后缀边界）+ 剥除的一致性。
#[test]
fn mention_scanning_is_case_insensitive_and_boundary_aware() {
    assert!(contains_bot_mention("@MY_BOT hi", "my_bot"));
    assert!(contains_bot_mention("hi @my_bot", "my_bot"));
    assert!(!contains_bot_mention("@my_bot_x hi", "my_bot"));
    assert!(!contains_bot_mention("no mention", "my_bot"));
    // 空用户名由 `mentions_bot` 先挡（`contains_bot_mention` 自己只看 `@` 这个 token）。
    let no_username = Message {
        text: "@my_bot hi".to_string(),
        ..Message::default()
    };
    assert!(!mentions_bot(&no_username, ""));

    assert_eq!(remove_bot_mentions("@my_bot hi", "my_bot"), " hi");
    assert_eq!(
        remove_bot_mentions("a @my_bot b @my_bot", "my_bot"),
        "a  b "
    );
    assert_eq!(
        remove_bot_mentions("@my_bot_x hi", "my_bot"),
        "@my_bot_x hi",
        "相似用户名整段照抄"
    );
    assert_eq!(normalize_text("  @my_bot  hi  ", "my_bot"), "hi");
    assert_eq!(normalize_text("  hi  ", ""), "hi");

    assert!(command_targets_bot("/issue@my_bot", "my_bot"));
    assert!(!command_targets_bot("/issue@other", "my_bot"));
    assert!(!command_targets_bot("/issue", "my_bot"));
}

/// UTF-16 实体切片的越界与非法偏移都返回 `None`（不加 panic）。
#[test]
fn entity_slicing_rejects_out_of_range_offsets() {
    let text = "ab";
    for entity in [
        MessageEntity {
            entity_type: "mention".to_string(),
            offset: -1,
            length: 1,
        },
        MessageEntity {
            entity_type: "mention".to_string(),
            offset: 0,
            length: 0,
        },
        MessageEntity {
            entity_type: "mention".to_string(),
            offset: 1,
            length: 5,
        },
        MessageEntity {
            entity_type: "mention".to_string(),
            offset: 9,
            length: 1,
        },
    ] {
        assert!(message_entity_text(text, &entity).is_none(), "{entity:?}");
    }
    assert_eq!(
        message_entity_text(
            "ab",
            &MessageEntity {
                entity_type: "mention".to_string(),
                offset: 1,
                length: 1,
            }
        ),
        Some("b".to_string())
    );
    // 代理对（😀 = 2 个 UTF-16 单位）不能被切开。
    assert_eq!(
        message_entity_text(
            "😀",
            &MessageEntity {
                entity_type: "mention".to_string(),
                offset: 0,
                length: 2,
            }
        ),
        Some("😀".to_string())
    );
}

/// 显示名：`First Last`，没有真名就回落 `username`。
#[test]
fn sender_display_name_falls_back_to_the_username() {
    let named = User {
        id: 1,
        first_name: "Ada".to_string(),
        last_name: "Lovelace".to_string(),
        username: "ada".to_string(),
        ..User::default()
    };
    assert_eq!(sender_display_name(&named), "Ada Lovelace");
    let only_username = User {
        id: 1,
        first_name: String::new(),
        last_name: String::new(),
        username: "ada".to_string(),
        ..User::default()
    };
    assert_eq!(sender_display_name(&only_username), "ada");
}

/// `decode_raw`：空 `raw` 是基础设施失败（本 adapter 自己写的字段解不开就是 bug）。
#[test]
fn decoding_an_empty_raw_envelope_is_an_infrastructure_failure() {
    let mut message =
        inbound_from_update(&private_update(1, 1, "hi"), 999, "my_bot").expect("ingested");
    assert!(decode_raw(&message).is_ok());
    message.raw = serde_json::Value::Null;
    assert!(decode_raw(&message).is_err());
}
