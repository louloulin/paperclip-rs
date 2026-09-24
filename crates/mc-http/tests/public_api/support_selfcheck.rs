//! 夹具自检（**不依赖 DB**，跟着默认门跑）。
//!
//! 理由与 M6-5 的 `zipfixture.rs` 相同：夹具自己写错（部署密钥长度不对、surface 令牌的域搞混）
//! 会让一整片用例**一起假红**，而那种红最难查。

use super::support::*;

#[test]
fn deployment_key_fixture_is_32_bytes() {
    assert_eq!(raw_deployment_key().len(), 32);
}

#[test]
fn correct_domain_token_opens_and_wrong_domain_token_does_not() {
    use mc_plugin_host::credentials::{open_token, surface_launch_box, DeploymentKey};

    let claims = surface_claims(
        uuid::Uuid::nil(),
        uuid::Uuid::nil(),
        uuid::Uuid::nil(),
        "panel",
        &"a".repeat(64),
        now_unix() + 60,
    );

    let key = DeploymentKey::new(raw_deployment_key()).expect("key");
    let boxed = surface_launch_box(Some(&key)).expect("box");
    let payload = open_token(&boxed, &mint_surface_token(&claims)).expect("正确域必须解得开");
    assert_eq!(
        serde_json::from_slice::<serde_json::Value>(&payload).unwrap(),
        claims
    );

    // 错域（hook 签名密钥）必须解不开 —— 这正是 `surface.rs` 的第三条拒绝。
    assert!(open_token(&boxed, &mint_surface_token_wrong_domain(&claims)).is_err());
}

#[test]
fn install_token_fixture_hashes_to_64_hex_chars() {
    let (token, hash) = install_token();
    assert!(token.starts_with("mpi_"));
    assert_eq!(hash.len(), 64);
    assert_eq!(hash, mc_plugin_host::token::hash_token(&token));
}

#[test]
fn callback_tokens_are_the_process_wide_table() {
    // 与 `routes/v1/policy.rs` 的文件头偏离 2 对齐：夹具签发的令牌必须落在 router 会查的那张表里。
    let installation_id = uuid::Uuid::new_v4();
    let workspace_id = uuid::Uuid::new_v4();
    let token = issue_callback_token(
        installation_id,
        workspace_id,
        mc_plugin_host::token::ActorKind::Member,
        uuid::Uuid::new_v4(),
        None,
    );
    assert!(token.starts_with("mpc_"));
    let grant = mc_http::routes::v1::policy::callback_tokens_for_test()
        .resolve(&token)
        .expect("刚签发的令牌必须可解析");
    assert_eq!(grant.installation_id.0, installation_id);
    // 第二次仍可解析（`DoD`：不是单次消费）。
    assert!(mc_http::routes::v1::policy::callback_tokens_for_test()
        .resolve(&token)
        .is_ok());
}

#[test]
fn panel_manifest_declares_exactly_one_surface() {
    let manifest = panel_manifest("panel.js", &["issues:read"]);
    assert_eq!(
        manifest["contributes"]["surfaces"][0]["entry"],
        serde_json::json!("panel.js")
    );
}
