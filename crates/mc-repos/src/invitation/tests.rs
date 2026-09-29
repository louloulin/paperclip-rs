use chrono::{Duration, Utc};

use mc_core::workspace::WorkspaceRole;

use crate::invitation::*;

#[test]
fn token_is_base64url_and_long_enough() {
    let t = InvitationRepo::generate_token();
    assert!(t.len() >= 43, "token too short: {t}");
    assert!(
        t.chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_'),
        "non base64url char in token: {t}"
    );
    // 两次生成应不同
    let t2 = InvitationRepo::generate_token();
    assert_ne!(t, t2);
}

#[test]
fn row_state_machine() {
    let now = Utc::now();
    let row = InvitationRow {
        id: Uuid::new_v4(),
        workspace_id: Uuid::new_v4(),
        email: "x@y".into(),
        role: "member".into(),
        invited_by_user_id: Uuid::new_v4(),
        token: "t".into(),
        expires_at: now + Duration::days(7),
        accepted_at: None,
        revoked_at: None,
        created_at: now,
    };
    assert!(row.is_active(now));
    assert!(!row.is_expired(now));
    assert!(!row.is_revoked());
    assert!(!row.is_accepted());

    let revoked = InvitationRow {
        revoked_at: Some(now),
        ..row.clone()
    };
    assert!(!revoked.is_active(now));

    let accepted = InvitationRow {
        accepted_at: Some(now),
        ..row.clone()
    };
    assert!(!accepted.is_active(now));

    let expired = InvitationRow {
        expires_at: now - Duration::seconds(1),
        ..row
    };
    assert!(!expired.is_active(now));
    assert!(expired.is_expired(now));
}

#[test]
fn role_round_trip() {
    let row = InvitationRow {
        id: Uuid::new_v4(),
        workspace_id: Uuid::new_v4(),
        email: "x".into(),
        role: "admin".into(),
        invited_by_user_id: Uuid::new_v4(),
        token: "t".into(),
        expires_at: Utc::now(),
        accepted_at: None,
        revoked_at: None,
        created_at: Utc::now(),
    };
    assert_eq!(row.role(), WorkspaceRole::Admin);
}
