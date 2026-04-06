use super::*;
use crate::accounts::disk::write_account_file;

#[test]
fn import_account_persists_json_file() {
    let temp = tempdir().expect("tempdir");
    let mut store = AccountStore::new(temp.path().to_path_buf());

    let result = store
        .import_account(
            "rt_123".to_string(),
            Some("label".to_string()),
            Some("user@example.com".to_string()),
        )
        .expect("import succeeds");
    let view = result.account;
    assert!(!result.already_exists);

    let path = temp.path().join(format!("{}.json", view.id));
    let content = std::fs::read_to_string(path).expect("file should exist");
    let parsed: AccountFile = serde_json::from_str(&content).expect("valid json");
    assert_eq!(
        parsed,
        AccountFile {
            refresh_token: "rt_123".to_string(),
            label: Some("label".to_string()),
            email: Some("user@example.com".to_string()),
        }
    );
}

#[test]
fn import_account_rejects_blank_refresh_token() {
    let temp = tempdir().expect("tempdir");
    let mut store = AccountStore::new(temp.path().to_path_buf());

    let error = store
        .import_account("   ".to_string(), None, None)
        .expect_err("blank refresh token should fail");

    assert_eq!(error.to_string(), INVALID_REFRESH_TOKEN_MESSAGE);
    assert!(store.list().is_empty());
}

#[test]
fn sync_from_disk_uses_file_stem_as_account_id() {
    let temp = tempdir().expect("tempdir");
    let path = temp.path().join("manual-account.json");
    write_account_file(
        &path,
        &AccountFile {
            refresh_token: "rt_abc".to_string(),
            label: None,
            email: Some("manual@example.com".to_string()),
        },
    )
    .expect("write account file");

    let mut store = AccountStore::new(temp.path().to_path_buf());
    store.sync_from_disk_startup().expect("sync succeeds");

    let view = store.view("manual-account").expect("account loaded");
    assert_eq!(view.email.as_deref(), Some("manual@example.com"));
}

#[test]
fn sync_from_disk_skips_blank_refresh_token_file() {
    let temp = tempdir().expect("tempdir");
    write_account_file(
        &temp.path().join("blank.json"),
        &AccountFile {
            refresh_token: "   ".to_string(),
            label: Some("bad".to_string()),
            email: Some("bad@example.com".to_string()),
        },
    )
    .expect("write invalid account file");

    let mut store = AccountStore::new(temp.path().to_path_buf());
    store.sync_from_disk_startup().expect("sync succeeds");

    assert!(store.list().is_empty());
}

#[test]
fn write_account_file_overwrites_existing_file() {
    let temp = tempdir().expect("tempdir");
    let path = temp.path().join("account.json");

    write_account_file(
        &path,
        &AccountFile {
            refresh_token: "rt_old".to_string(),
            label: Some("old".to_string()),
            email: Some("old@example.com".to_string()),
        },
    )
    .expect("write first version");
    write_account_file(
        &path,
        &AccountFile {
            refresh_token: "rt_new".to_string(),
            label: Some("new".to_string()),
            email: Some("new@example.com".to_string()),
        },
    )
    .expect("overwrite existing file");

    let content = std::fs::read_to_string(path).expect("file should exist");
    let parsed: AccountFile = serde_json::from_str(&content).expect("valid json");
    assert_eq!(
        parsed,
        AccountFile {
            refresh_token: "rt_new".to_string(),
            label: Some("new".to_string()),
            email: Some("new@example.com".to_string()),
        }
    );
}

#[test]
fn rescan_keeps_existing_record_when_file_refresh_token_is_invalid() {
    let temp = tempdir().expect("tempdir");
    let mut store = AccountStore::new(temp.path().to_path_buf());
    let account = store
        .import_account(
            "rt_123".to_string(),
            Some("label".to_string()),
            Some("user@example.com".to_string()),
        )
        .expect("import succeeds")
        .account;

    let id = account.id.clone();
    write_account_file(
        &temp.path().join(format!("{id}.json")),
        &AccountFile {
            refresh_token: "   ".to_string(),
            label: Some("bad".to_string()),
            email: Some("bad@example.com".to_string()),
        },
    )
    .expect("overwrite account file with invalid token");

    store.sync_from_disk_rescan().expect("rescan succeeds");

    let view = store.view(&id).expect("existing record should remain");
    assert_eq!(view.label.as_deref(), Some("label"));
    assert_eq!(view.email.as_deref(), Some("user@example.com"));
}

#[test]
fn import_account_returns_existing_record_for_duplicate_refresh_token() {
    let temp = tempdir().expect("tempdir");
    let mut store = AccountStore::new(temp.path().to_path_buf());

    let first = store
        .import_account(
            "rt_123".to_string(),
            Some("first".to_string()),
            Some("first@example.com".to_string()),
        )
        .expect("first import succeeds");
    let second = store
        .import_account(
            "rt_123".to_string(),
            Some("updated".to_string()),
            Some("updated@example.com".to_string()),
        )
        .expect("second import succeeds");

    assert!(!first.already_exists);
    assert!(second.already_exists);
    assert_eq!(first.account.id, second.account.id);
    assert_eq!(store.records.len(), 1);
    assert_eq!(second.account.label.as_deref(), Some("updated"));
    assert_eq!(second.account.email.as_deref(), Some("updated@example.com"));

    let content = std::fs::read_to_string(temp.path().join(format!("{}.json", first.account.id)))
        .expect("file should exist");
    let parsed: AccountFile = serde_json::from_str(&content).expect("valid json");
    assert_eq!(parsed.label.as_deref(), Some("updated"));
    assert_eq!(parsed.email.as_deref(), Some("updated@example.com"));
}

#[test]
fn import_account_duplicate_refresh_token_only_updates_provided_non_empty_metadata() {
    let temp = tempdir().expect("tempdir");
    let mut store = AccountStore::new(temp.path().to_path_buf());

    let first = store
        .import_account(
            "rt_123".to_string(),
            Some("first".to_string()),
            Some("first@example.com".to_string()),
        )
        .expect("first import succeeds");
    let second = store
        .import_account(
            "rt_123".to_string(),
            Some("   ".to_string()),
            Some("updated@example.com".to_string()),
        )
        .expect("second import succeeds");

    assert!(second.already_exists);
    assert_eq!(first.account.id, second.account.id);
    assert_eq!(second.account.label.as_deref(), Some("first"));
    assert_eq!(second.account.email.as_deref(), Some("updated@example.com"));

    let content = std::fs::read_to_string(temp.path().join(format!("{}.json", first.account.id)))
        .expect("file should exist");
    let parsed: AccountFile = serde_json::from_str(&content).expect("valid json");
    assert_eq!(parsed.label.as_deref(), Some("first"));
    assert_eq!(parsed.email.as_deref(), Some("updated@example.com"));
}

#[test]
fn sync_from_disk_merges_duplicate_refresh_token_files() {
    let temp = tempdir().expect("tempdir");
    write_account_file(
        &temp.path().join("a.json"),
        &AccountFile {
            refresh_token: "rt_dup".to_string(),
            label: Some("canonical".to_string()),
            email: Some("a@example.com".to_string()),
        },
    )
    .expect("write first account file");
    write_account_file(
        &temp.path().join("b.json"),
        &AccountFile {
            refresh_token: "rt_dup".to_string(),
            label: Some("duplicate".to_string()),
            email: Some("b@example.com".to_string()),
        },
    )
    .expect("write second account file");

    let mut store = AccountStore::new(temp.path().to_path_buf());
    store.sync_from_disk_startup().expect("sync succeeds");

    let accounts = store.list();
    assert_eq!(accounts.len(), 1);
    assert_eq!(accounts[0].id, "a");
    assert_eq!(accounts[0].label.as_deref(), Some("canonical"));
    assert_eq!(accounts[0].email.as_deref(), Some("a@example.com"));
    assert!(temp.path().join("a.json").exists());
    assert!(!temp.path().join("b.json").exists());
}

#[test]
fn sync_from_disk_merges_duplicate_metadata_into_canonical_file() {
    let temp = tempdir().expect("tempdir");
    write_account_file(
        &temp.path().join("a.json"),
        &AccountFile {
            refresh_token: "rt_dup".to_string(),
            label: None,
            email: None,
        },
    )
    .expect("write first account file");
    write_account_file(
        &temp.path().join("b.json"),
        &AccountFile {
            refresh_token: "rt_dup".to_string(),
            label: Some("merged-label".to_string()),
            email: Some("merged@example.com".to_string()),
        },
    )
    .expect("write second account file");

    let mut store = AccountStore::new(temp.path().to_path_buf());
    store.sync_from_disk_startup().expect("sync succeeds");

    let content = std::fs::read_to_string(temp.path().join("a.json")).expect("file should exist");
    let parsed: AccountFile = serde_json::from_str(&content).expect("valid json");
    assert_eq!(
        parsed,
        AccountFile {
            refresh_token: "rt_dup".to_string(),
            label: Some("merged-label".to_string()),
            email: Some("merged@example.com".to_string()),
        }
    );
}

#[test]
fn rescan_prefers_existing_in_memory_record_over_smaller_duplicate_filename() {
    let temp = tempdir().expect("tempdir");
    write_account_file(
        &temp.path().join("b.json"),
        &AccountFile {
            refresh_token: "rt_dup".to_string(),
            label: Some("existing".to_string()),
            email: None,
        },
    )
    .expect("write existing account file");

    let mut store = AccountStore::new(temp.path().to_path_buf());
    store
        .sync_from_disk_startup()
        .expect("startup sync succeeds");

    let record = store.records.get_mut("b").expect("record exists");
    record.access_token = Some("at_existing".to_string());
    record.access_token_expires_at = Some(Utc::now() + Duration::minutes(30));
    record.last_selected_at = Some(Utc::now());

    write_account_file(
        &temp.path().join("a.json"),
        &AccountFile {
            refresh_token: "rt_dup".to_string(),
            label: Some("duplicate".to_string()),
            email: Some("dup@example.com".to_string()),
        },
    )
    .expect("write duplicate account file");

    store.sync_from_disk_rescan().expect("rescan succeeds");

    let accounts = store.list();
    assert_eq!(accounts.len(), 1);
    assert_eq!(accounts[0].id, "b");
    assert!(temp.path().join("b.json").exists());
    assert!(!temp.path().join("a.json").exists());
}

#[test]
fn rescan_uses_canonical_file_metadata_as_merge_base() {
    let temp = tempdir().expect("tempdir");
    write_account_file(
        &temp.path().join("b.json"),
        &AccountFile {
            refresh_token: "rt_dup".to_string(),
            label: Some("initial".to_string()),
            email: Some("initial@example.com".to_string()),
        },
    )
    .expect("write existing account file");

    let mut store = AccountStore::new(temp.path().to_path_buf());
    store
        .sync_from_disk_startup()
        .expect("startup sync succeeds");

    let record = store.records.get_mut("b").expect("record exists");
    record.access_token = Some("at_existing".to_string());
    record.access_token_expires_at = Some(Utc::now() + Duration::minutes(30));
    record.last_selected_at = Some(Utc::now());
    record.label = Some("memory-only".to_string());
    record.email = Some("memory@example.com".to_string());

    write_account_file(
        &temp.path().join("b.json"),
        &AccountFile {
            refresh_token: "rt_dup".to_string(),
            label: Some("edited-on-disk".to_string()),
            email: None,
        },
    )
    .expect("rewrite canonical file");
    write_account_file(
        &temp.path().join("a.json"),
        &AccountFile {
            refresh_token: "rt_dup".to_string(),
            label: Some("duplicate-label".to_string()),
            email: Some("duplicate@example.com".to_string()),
        },
    )
    .expect("write duplicate account file");

    store.sync_from_disk_rescan().expect("rescan succeeds");

    let view = store.view("b").expect("canonical record survives");
    assert_eq!(view.label.as_deref(), Some("edited-on-disk"));
    assert_eq!(view.email.as_deref(), Some("duplicate@example.com"));

    let content = std::fs::read_to_string(temp.path().join("b.json")).expect("file should exist");
    let parsed: AccountFile = serde_json::from_str(&content).expect("valid json");
    assert_eq!(parsed.label.as_deref(), Some("edited-on-disk"));
    assert_eq!(parsed.email.as_deref(), Some("duplicate@example.com"));
    assert!(!temp.path().join("a.json").exists());
}

#[test]
fn rescan_keeps_existing_record_when_canonical_file_is_temporarily_invalid() {
    let temp = tempdir().expect("tempdir");
    let mut store = AccountStore::new(temp.path().to_path_buf());
    let view = store
        .import_account(
            "rt_123".to_string(),
            Some("label".to_string()),
            Some("user@example.com".to_string()),
        )
        .expect("import succeeds")
        .account;

    let id = view.id.clone();
    let original_last_selected_at = Utc::now();
    let original_expires_at = Utc::now() + Duration::minutes(30);
    let record = store.records.get_mut(&id).expect("record exists");
    record.access_token = Some("at_123".to_string());
    record.account_id = Some("acct_123".to_string());
    record.plan_type = Some("plus".to_string());
    record.access_token_expires_at = Some(original_expires_at);
    record.last_selected_at = Some(original_last_selected_at);
    record.routing_state = RoutingState::Ready;

    std::fs::write(temp.path().join(format!("{id}.json")), "{not-json").expect("corrupt file");

    store.sync_from_disk_rescan().expect("rescan succeeds");

    let view = store.view(&id).expect("account remains loaded");
    assert_eq!(view.label.as_deref(), Some("label"));
    assert_eq!(view.email.as_deref(), Some("user@example.com"));
    assert_eq!(view.account_id.as_deref(), Some("acct_123"));
    assert_eq!(view.plan_type.as_deref(), Some("plus"));
    assert_eq!(view.access_token_expires_at, Some(original_expires_at));
    assert_eq!(view.last_selected_at, Some(original_last_selected_at));
    assert_eq!(view.routing_state, RoutingState::Ready);
    assert!(!store.records.get(&id).expect("record exists").detached);
}

#[test]
fn rescan_preserves_refresh_target_for_existing_record() {
    let temp = tempdir().expect("tempdir");
    write_account_file(
        &temp.path().join("b.json"),
        &AccountFile {
            refresh_token: "rt_dup".to_string(),
            label: None,
            email: None,
        },
    )
    .expect("write existing account file");

    let mut store = AccountStore::new(temp.path().to_path_buf());
    store
        .sync_from_disk_startup()
        .expect("startup sync succeeds");

    let record = store.records.get_mut("b").expect("record exists");
    record.refresh_in_flight = true;
    record.routing_state = RoutingState::Warming;
    record.in_flight_requests = 1;

    write_account_file(
        &temp.path().join("a.json"),
        &AccountFile {
            refresh_token: "rt_dup".to_string(),
            label: Some("duplicate".to_string()),
            email: None,
        },
    )
    .expect("write duplicate account file");

    store.sync_from_disk_rescan().expect("rescan succeeds");
    store
        .finish_refresh_success(
            "b",
            RefreshedAccount {
                access_token: "at_new".to_string(),
                refresh_token: Some("rt_rotated".to_string()),
                account_id: Some("acct_123".to_string()),
                plan_type: Some("plus".to_string()),
                email: Some("user@example.com".to_string()),
                access_token_expires_at: Some(Utc::now() + Duration::minutes(30)),
            },
        )
        .expect("refresh success");

    let view = store.view("b").expect("canonical record survives");
    assert_eq!(view.account_id.as_deref(), Some("acct_123"));
    assert!(temp.path().join("b.json").exists());
    assert!(!temp.path().join("a.json").exists());

    let content = std::fs::read_to_string(temp.path().join("b.json")).expect("file should exist");
    let parsed: AccountFile = serde_json::from_str(&content).expect("valid json");
    assert_eq!(parsed.refresh_token, "rt_rotated");
}

#[test]
fn rescan_does_not_override_loaded_refresh_token_from_disk_edit() {
    let temp = tempdir().expect("tempdir");
    let mut store = AccountStore::new(temp.path().to_path_buf());
    let view = store
        .import_account(
            "rt_original".to_string(),
            Some("label".to_string()),
            Some("user@example.com".to_string()),
        )
        .expect("import succeeds")
        .account;

    let id = view.id.clone();
    write_account_file(
        &temp.path().join(format!("{id}.json")),
        &AccountFile {
            refresh_token: "rt_edited".to_string(),
            label: Some("edited".to_string()),
            email: Some("edited@example.com".to_string()),
        },
    )
    .expect("rewrite account file");

    store.sync_from_disk_rescan().expect("rescan succeeds");

    let record = store.records.get(&id).expect("record exists");
    assert_eq!(record.refresh_token, "rt_original");

    let view = store.view(&id).expect("account view");
    assert_eq!(view.label.as_deref(), Some("edited"));
    assert_eq!(view.email.as_deref(), Some("edited@example.com"));
}

#[test]
fn rescan_preserves_auth_invalid_tombstone_after_trash_failure() {
    let temp = tempdir().expect("tempdir");
    let mut store = AccountStore::new(temp.path().to_path_buf());
    let view = store
        .import_account(
            "rt_123".to_string(),
            Some("label".to_string()),
            Some("user@example.com".to_string()),
        )
        .expect("import succeeds")
        .account;

    let id = view.id.clone();
    let record = store.records.get_mut(&id).expect("record exists");
    record.routing_state = RoutingState::AuthInvalid;
    record.blocked_reason = Some(BlockedReason::AuthInvalid);
    record.blocked_source = Some(BlockedSource::FixedPolicy);
    record.blocked_until = None;
    record.auth_invalid_tombstone = true;

    store.sync_from_disk_rescan().expect("rescan succeeds");

    let record = store.records.get(&id).expect("record exists");
    assert!(record.auth_invalid_tombstone);
    assert_eq!(record.routing_state, RoutingState::AuthInvalid);
    assert_eq!(record.blocked_reason, Some(BlockedReason::AuthInvalid));
    assert!(record.blocked_until.is_none());

    let view = store.view(&id).expect("account view");
    assert_eq!(view.routing_state, RoutingState::AuthInvalid);
    assert_eq!(view.blocked_reason, Some(BlockedReason::AuthInvalid));
    assert!(view.blocked_until.is_none());
}
