use pc_feedback::trace::FeedbackTraceService;
use pc_repos::Db;
use uuid::Uuid;
const URL:&str="postgres://paperclip:paperclip@127.0.0.1:5432/paperclip_repos";
#[tokio::test]
async fn validates_ids_without_database_mutation(){let db=Db::connect(URL,2,1).await.unwrap(); let s=FeedbackTraceService::new(db); assert!(s.list_by_issue(Uuid::nil(),10).await.is_err()); assert!(s.list_for_company(Uuid::nil(),10).await.is_err()); assert!(s.get_bundle(Uuid::nil()).await.is_err()); assert!(s.delete(Uuid::nil()).await.is_err());}
