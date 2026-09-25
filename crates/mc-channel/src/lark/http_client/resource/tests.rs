//! 资源响应面的用例（二进制 / `Content-Disposition` / JSON 错误体 / 上限 / 令牌重放）。
//!
//! 替身与装置在父测试模块（[`crate::lark::http_client::tests`]）里。

use crate::lark::client::{ApiClient, ApiError};
use crate::lark::http_client::resource::MAX_MESSAGE_RESOURCE_BYTES;
use crate::lark::http_client::tests::{
    binary_reply, client, credentials, json_reply, recorded, serve, token_reply, Reply,
};
use crate::lark::params::DownloadResourceParams;

use super::{filename_from_content_disposition, ResourceBody};

#[tokio::test]
async fn a_binary_resource_reports_type_name_and_size() {
    let (base, records) = serve(vec![
        token_reply("t1", 7200),
        Reply {
            status: 200,
            content_type: "image/png",
            body: b"PNGDATA".to_vec(),
            ..Reply::default()
        },
    ])
    .await;
    let client = client(&base);
    let resource = client
        .download_message_resource(
            credentials(),
            DownloadResourceParams {
                message_id: "om_1".to_string(),
                file_key: "img_1".to_string(),
                resource_type: "image".to_string(),
            },
        )
        .await
        .expect("download");
    assert_eq!(resource.data, b"PNGDATA");
    assert_eq!(resource.content_type, "image/png");
    assert_eq!(resource.size_bytes, 7);

    let requests = recorded(&records);
    assert_eq!(
        requests[1].target(),
        "/open-apis/im/v1/messages/om_1/resources/img_1?type=image"
    );
    assert_eq!(requests[1].bearer(), Some("t1"));
}

#[tokio::test]
async fn a_content_disposition_filename_is_reported_with_the_main_content_type() {
    let (base, _records) = serve(vec![
        token_reply("t1", 7200),
        Reply {
            disposition: Some("attachment; filename=\"report.pdf\""),
            ..binary_reply("application/octet-stream; charset=binary", b"x".to_vec())
        },
    ])
    .await;
    let client = client(&base);
    let stream = client
        .download_message_resource_stream(
            credentials(),
            DownloadResourceParams {
                message_id: "om_1".to_string(),
                file_key: "file_1".to_string(),
                resource_type: String::new(),
            },
        )
        .await
        .expect("stream");
    // `Content-Type` 的参数被剥掉（主类型才算数）。
    assert_eq!(stream.content_type, "application/octet-stream");
    assert_eq!(stream.filename.as_deref(), Some("report.pdf"));
    // 流式拿到手时还没读过任何字节。
    assert_eq!(stream.body.bytes_read(), 0);
}

#[tokio::test]
async fn a_json_body_on_the_resource_path_is_read_as_a_platform_error() {
    // 平台把一部分资源失败也表达成 JSON ⇒ 读完、判码（不是当成资源字节）。
    let (base, _records) = serve(vec![
        token_reply("t1", 7200),
        json_reply(200, r#"{"code":234001,"msg":"resource not found"}"#),
    ])
    .await;
    let client = client(&base);
    let error = client
        .download_message_resource(
            credentials(),
            DownloadResourceParams {
                message_id: "om_1".to_string(),
                file_key: "img_1".to_string(),
                resource_type: "image".to_string(),
            },
        )
        .await
        .expect_err("platform error");
    assert_eq!(
        error,
        ApiError::Refused {
            op: "download message resource",
            status: None,
            code: 234_001
        }
    );
}

#[tokio::test]
async fn a_resource_declared_over_the_cap_is_rejected() {
    let (base, _records) = serve(vec![
        token_reply("t1", 7200),
        Reply {
            status: 200,
            content_type: "video/mp4",
            body: Vec::new(),
            content_length: Some(MAX_MESSAGE_RESOURCE_BYTES + 1),
            ..Reply::default()
        },
    ])
    .await;
    let client = client(&base);
    let error = client
        .download_message_resource(
            credentials(),
            DownloadResourceParams {
                message_id: "om_1".to_string(),
                file_key: "vid_1".to_string(),
                resource_type: "file".to_string(),
            },
        )
        .await
        .expect_err("over the cap");
    assert_eq!(
        error,
        ApiError::ResourceTooLarge {
            op: "download message resource",
            cap: MAX_MESSAGE_RESOURCE_BYTES
        }
    );
    assert_eq!(MAX_MESSAGE_RESOURCE_BYTES, 100 * 1024 * 1024);
}

#[tokio::test]
async fn a_rejected_token_on_the_resource_path_also_refreshes_once() {
    let (base, records) = serve(vec![
        token_reply("t1", 7200),
        json_reply(400, r#"{"code":99991663}"#),
        token_reply("t2", 7200),
        binary_reply("image/png", b"PNG".to_vec()),
    ])
    .await;
    let client = client(&base);
    let resource = client
        .download_message_resource(
            credentials(),
            DownloadResourceParams {
                message_id: "om_1".to_string(),
                file_key: "img_1".to_string(),
                resource_type: "image".to_string(),
            },
        )
        .await
        .expect("replay");
    assert_eq!(resource.data, b"PNG");
    let requests = recorded(&records);
    assert_eq!(requests.len(), 4);
    assert_eq!(requests[1].bearer(), Some("t1"));
    assert_eq!(requests[3].bearer(), Some("t2"));
}

// =====================================================================
// 端口契约
// =====================================================================

#[tokio::test]
async fn buffered_resource_body_yields_its_bytes_exactly_once() {
    let mut body = ResourceBody::buffered(b"hello".to_vec());
    let first = body.read_chunk("test").await.expect("chunk");
    assert_eq!(first, Some(b"hello".to_vec()));
    // 第二读就是读完（不是空块）。
    assert_eq!(body.read_chunk("test").await.expect("chunk"), None);
    assert_eq!(body.bytes_read(), 5);
}

#[tokio::test]
async fn buffered_resource_body_can_be_read_to_a_cap() {
    let mut body = ResourceBody::buffered(vec![7_u8; 16]);
    let all = body
        .read_all_capped("test", 32)
        .await
        .expect("under the cap");
    assert_eq!(all.len(), 16);

    let mut tiny = ResourceBody::buffered(vec![7_u8; 16]);
    let error = tiny
        .read_all_capped("test", 8)
        .await
        .expect_err("over the cap");
    assert_eq!(error, ApiError::ResourceTooLarge { op: "test", cap: 8 });
}

// =====================================================================
// 响应解码（wire DTO 与信封）
// =====================================================================

#[test]
fn content_disposition_filename_handles_the_common_shapes() {
    assert_eq!(
        filename_from_content_disposition("attachment; filename=\"report.pdf\""),
        Some("report.pdf".to_string())
    );
    assert_eq!(
        filename_from_content_disposition("attachment;filename=report.pdf"),
        Some("report.pdf".to_string())
    );
    assert_eq!(
        filename_from_content_disposition("inline; filename=\"a b c.txt\"; size=12"),
        Some("a b c.txt".to_string())
    );
    // 缺参数 / 空值 / 大小写不同的头名 ⇒ 没有文件名（不是空串）。
    assert_eq!(filename_from_content_disposition(""), None);
    assert_eq!(filename_from_content_disposition("attachment"), None);
    assert_eq!(
        filename_from_content_disposition("attachment; filename=\"\""),
        None
    );
    assert_eq!(
        filename_from_content_disposition("Attachment; Filename=report.pdf"),
        Some("report.pdf".to_string())
    );
}
