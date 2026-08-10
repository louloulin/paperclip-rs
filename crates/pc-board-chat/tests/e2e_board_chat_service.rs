use std::sync::Arc;
use pc_board_chat::{BoardChatError, BoardChatHookEvent, ChatMessageStatus, ChatRole, NewMessage, NewThread, RecordingBoardChatHook, BoardChatService};
use pc_repos::Db;
use sqlx::PgPool;
use uuid::Uuid;

const URL: &str = "postgres://paperclip:paperclip@127.0.0.1:5432/paperclip_repos";
static LOCK: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());

async fn setup() -> (Db, PgPool) {
    let p = sqlx::postgres::PgPoolOptions::new().max_connections(4).connect(URL).await.unwrap();
    (Db::connect(URL, 4, 1).await.unwrap(), p)
}
async fn company(p: &PgPool) -> Uuid {
    let id = Uuid::new_v4();
    let prefix = format!("BC{}", &id.simple().to_string()[..6]);
    sqlx::query("INSERT INTO companies (id,name,status,issue_prefix,created_at,updated_at) VALUES ($1,$2,'active',$3,now(),now())")
        .bind(id).bind(format!("bc-{id}")).bind(prefix).execute(p).await.unwrap();
    id
}
async fn cleanup(p: &PgPool, cid: Uuid) {
    let _ = sqlx::query("DELETE FROM board_chat_messages WHERE company_id=$1").bind(cid).execute(p).await;
    let _ = sqlx::query("DELETE FROM board_chat_threads WHERE company_id=$1").bind(cid).execute(p).await;
    let _ = sqlx::query("DELETE FROM companies WHERE id=$1").bind(cid).execute(p).await;
}

#[tokio::test(flavor = "current_thread")]
async fn thread_message_lifecycle() {
    let _g = LOCK.lock().await;
    let (db, p) = setup().await;
    let cid = company(&p).await;
    let h = Arc::new(RecordingBoardChatHook::default());
    let s = BoardChatService::with_hooks(db, vec![h.clone()]);
    let title = format!("pc-bc-{}", Uuid::new_v4().simple());
    let thread = s.get_or_create_thread(NewThread { company_id: cid, issue_id: None, title: title.clone(), created_by_user_id: None }).await.unwrap();
    assert_eq!(thread.title, title);
    let threads = s.list_threads(cid, 10).await.unwrap();
    assert!(threads.iter().any(|t| t.id == thread.id));
    let got = s.get_thread(cid, thread.id).await.unwrap().unwrap();
    assert_eq!(got.id, thread.id);
    let msg = s.append_message(NewMessage { thread_id: thread.id, company_id: cid, role: ChatRole::User, author_user_id: None, author_agent_id: None, body: "hello".into(), tool_uses: None, status: None }).await.unwrap();
    assert_eq!(msg.body, "hello");
    let messages = s.list_messages(thread.id, 10).await.unwrap();
    assert!(messages.iter().any(|m| m.id == msg.id));
    let _statused = s.set_message_status(msg.id, ChatMessageStatus::Complete).await.unwrap();
    let _issue_id = s.ensure_board_issue(cid, &format!("issue-{cid}")).await.unwrap();
    let snapshot = h.events_snapshot();
    assert!(snapshot.iter().any(|e| matches!(e, BoardChatHookEvent::ThreadOpened { .. })));
    assert!(snapshot.iter().any(|e| matches!(e, BoardChatHookEvent::MessagePosted { .. })));
    assert!(snapshot.iter().any(|e| matches!(e, BoardChatHookEvent::MessageStatusChanged { .. })));
    assert!(snapshot.iter().any(|e| matches!(e, BoardChatHookEvent::BoardIssueEnsured { .. })));
    cleanup(&p, cid).await;
}

#[tokio::test(flavor = "current_thread")]
async fn validation_paths() {
    let _g = LOCK.lock().await;
    let (db, _p) = setup().await;
    let s = BoardChatService::new(db);
    assert!(s.list_threads(Uuid::nil(), 10).await.is_err());
    assert!(s.list_messages(Uuid::nil(), 10).await.is_err());
    assert!(s.set_message_status(Uuid::nil(), ChatMessageStatus::Complete).await.is_err());
    assert!(s.ensure_board_issue(Uuid::new_v4(), "").await.is_err());
    let _ = ChatMessageStatus::Complete; // suppress
}
