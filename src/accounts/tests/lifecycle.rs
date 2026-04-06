use super::*;

#[test]
fn finish_refresh_success_keeps_memory_state_on_persist_failure() {
    let temp = tempdir().expect("tempdir");
    let mut store = AccountStore::new(temp.path().to_path_buf());
    let view = store
        .import_account(
            "rt_123".to_string(),
            Some("label".to_string()),
            Some("old@example.com".to_string()),
        )
        .expect("import succeeds")
        .account;

    let id = view.id.clone();
    let old_expires_at = Utc::now() + Duration::minutes(10);
    let old_last_refresh_at = Utc::now();
    let blocking_parent = temp.path().join("not-a-dir");
    std::fs::write(&blocking_parent, "file").expect("write blocking parent");
    let record = store.records.get_mut(&id).expect("record exists");
    record.access_token = Some("old_at".to_string());
    record.account_id = Some("acct_old".to_string());
    record.plan_type = Some("plus".to_string());
    record.access_token_expires_at = Some(old_expires_at);
    record.last_refresh_at = Some(old_last_refresh_at);
    record.routing_state = RoutingState::Warming;
    record.refresh_in_flight = true;
    record.file_path = blocking_parent.join("account.json");

    let result = store
        .finish_refresh_success(
            &id,
            RefreshedAccount {
                access_token: "new_at".to_string(),
                refresh_token: Some("rt_rotated".to_string()),
                account_id: Some("acct_new".to_string()),
                plan_type: Some("pro".to_string()),
                email: Some("new@example.com".to_string()),
                access_token_expires_at: Some(Utc::now() + Duration::minutes(30)),
            },
        )
        .expect("refresh should stay usable");
    assert!(
        result
            .persist_warning
            .as_deref()
            .is_some_and(|warning| warning.contains("create account parent dir"))
    );

    let record = store.records.get(&id).expect("record exists");
    assert_eq!(record.refresh_token, "rt_rotated");
    assert_eq!(record.access_token.as_deref(), Some("new_at"));
    assert_eq!(record.account_id.as_deref(), Some("acct_new"));
    assert_eq!(record.plan_type.as_deref(), Some("pro"));
    assert_eq!(record.email.as_deref(), Some("new@example.com"));
    assert_ne!(record.access_token_expires_at, Some(old_expires_at));
    assert_ne!(record.last_refresh_at, Some(old_last_refresh_at));
    assert_eq!(record.routing_state, RoutingState::Ready);
    assert!(!record.refresh_in_flight);
    assert!(record.last_error.is_none());
    assert!(record.last_error_at.is_none());

    let content = std::fs::read_to_string(temp.path().join(format!("{id}.json"))).expect("file");
    let parsed: AccountFile = serde_json::from_str(&content).expect("valid json");
    assert_eq!(parsed.refresh_token, "rt_123");
    assert_eq!(parsed.email.as_deref(), Some("old@example.com"));
}

#[test]
fn auth_invalid_moves_account_file_to_trash() {
    let temp = tempdir().expect("tempdir");
    let mut store = AccountStore::new(temp.path().to_path_buf());
    let view = store
        .import_account(
            "rt_123".to_string(),
            Some("test-label".to_string()),
            Some("user@example.com".to_string()),
        )
        .expect("import succeeds")
        .account;

    let id = view.id;
    let path = temp.path().join(format!("{id}.json"));
    assert!(path.exists());

    store
        .finish_refresh_failure(&id, FailureClass::AuthInvalid, None, "invalid".to_string())
        .expect("failure applied");

    assert!(!path.exists());
    let trash_path = temp.path().join(TRASH_DIR_NAME).join(format!("{id}.json"));
    assert!(trash_path.exists());
    let content = std::fs::read_to_string(&trash_path).expect("trash file");
    let parsed: AccountFile = serde_json::from_str(&content).expect("valid json");
    assert_eq!(parsed.refresh_token, "rt_123");
    assert_eq!(parsed.label.as_deref(), Some("test-label"));
    assert_eq!(parsed.email.as_deref(), Some("user@example.com"));
    assert!(store.view(&id).is_err());
}

#[test]
fn auth_invalid_trash_failure_keeps_account_attached_and_records_persist_error() {
    let temp = tempdir().expect("tempdir");
    let mut store = AccountStore::new(temp.path().to_path_buf());
    let view = store
        .import_account(
            "rt_123".to_string(),
            Some("test-label".to_string()),
            Some("user@example.com".to_string()),
        )
        .expect("import succeeds")
        .account;

    let id = view.id;
    let trash_blocker = temp.path().join("trash");
    std::fs::write(&trash_blocker, "file").expect("write trash blocker");

    store
        .finish_refresh_failure(&id, FailureClass::AuthInvalid, None, "invalid".to_string())
        .expect("failure is absorbed");

    let view = store.view(&id).expect("account remains attached");
    assert_eq!(view.routing_state, RoutingState::AuthInvalid);
    assert_eq!(view.blocked_reason, Some(BlockedReason::AuthInvalid));
    assert!(view.blocked_until.is_none());
    assert!(
        view.last_error
            .as_deref()
            .is_some_and(|value| value.contains("invalid"))
    );
    assert!(
        view.last_error
            .as_deref()
            .is_some_and(|value| value.contains("move invalid account file to trash failed"))
    );
    assert!(temp.path().join(format!("{id}.json")).exists());
    assert!(
        store
            .records
            .get(&id)
            .expect("record exists")
            .auth_invalid_tombstone
    );
}

#[test]
fn auth_invalid_uses_unique_trash_path_when_default_name_is_taken() {
    let temp = tempdir().expect("tempdir");
    let mut store = AccountStore::new(temp.path().to_path_buf());
    let view = store
        .import_account(
            "rt_123".to_string(),
            Some("test-label".to_string()),
            Some("user@example.com".to_string()),
        )
        .expect("import succeeds")
        .account;

    let id = view.id;
    let trash_dir = temp.path().join(TRASH_DIR_NAME);
    std::fs::create_dir_all(&trash_dir).expect("create trash dir");
    let canonical_trash_path = trash_dir.join(format!("{id}.json"));
    std::fs::write(&canonical_trash_path, "{\"refresh_token\":\"existing\"}")
        .expect("seed existing trash file");

    store
        .finish_refresh_failure(&id, FailureClass::AuthInvalid, None, "invalid".to_string())
        .expect("failure applied");

    let trash_files = std::fs::read_dir(&trash_dir)
        .expect("read trash dir")
        .map(|entry| entry.expect("entry").path())
        .collect::<Vec<_>>();
    assert_eq!(trash_files.len(), 2);
    assert!(trash_files.iter().any(|path| path == &canonical_trash_path));
    let moved_path = trash_files
        .iter()
        .find(|path| *path != &canonical_trash_path)
        .expect("moved file path");
    let content = std::fs::read_to_string(moved_path).expect("moved file");
    let parsed: AccountFile = serde_json::from_str(&content).expect("valid json");
    assert_eq!(parsed.refresh_token, "rt_123");
}

#[test]
fn refresh_quota_failure_uses_upstream_retry_after_for_blocked_until() {
    let temp = tempdir().expect("tempdir");
    let mut store = AccountStore::new(temp.path().to_path_buf());
    let view = store
        .import_account("rt_123".to_string(), None, None)
        .expect("import succeeds")
        .account;

    let id = view.id.clone();
    let record = store.records.get_mut(&id).expect("record exists");
    record.refresh_in_flight = true;

    let before = Utc::now();
    store
        .finish_refresh_failure(
            &id,
            FailureClass::QuotaExhausted,
            Some(StdDuration::from_secs(77)),
            "refresh quota exhausted".to_string(),
        )
        .expect("failure applied");

    let view = store.view(&id).expect("account remains");
    assert_eq!(view.routing_state, RoutingState::Cooldown);
    assert_eq!(view.blocked_reason, Some(BlockedReason::QuotaExhausted));
    assert_eq!(view.blocked_source, Some(BlockedSource::UpstreamRetryAfter));
    let blocked_until = view.blocked_until.expect("blocked until");
    assert!(blocked_until >= before + Duration::seconds(76));
    assert!(blocked_until <= before + Duration::seconds(78));
}

#[test]
fn retryable_block_keeps_longer_existing_retry_after_window() {
    let temp = tempdir().expect("tempdir");
    let mut store = AccountStore::new(temp.path().to_path_buf());
    let view = store
        .import_account("rt_123".to_string(), None, None)
        .expect("import succeeds")
        .account;

    let id = view.id.clone();
    let first_before = Utc::now();
    store
        .mark_request_failure(
            &id,
            FailureClass::QuotaExhausted,
            Some(StdDuration::from_secs(600)),
            "quota exhausted".to_string(),
        )
        .expect("first failure applied");

    let first_view = store.view(&id).expect("account remains");
    let first_until = first_view.blocked_until.expect("initial blocked until");
    assert_eq!(
        first_view.blocked_reason,
        Some(BlockedReason::QuotaExhausted)
    );
    assert_eq!(
        first_view.blocked_source,
        Some(BlockedSource::UpstreamRetryAfter)
    );
    assert!(first_until >= first_before + Duration::seconds(599));

    store
        .mark_request_failure(
            &id,
            FailureClass::RateLimited,
            Some(StdDuration::from_secs(1)),
            "rate limited".to_string(),
        )
        .expect("second failure applied");

    let second_view = store.view(&id).expect("account remains");
    assert_eq!(second_view.blocked_until, Some(first_until));
    assert_eq!(
        second_view.blocked_reason,
        Some(BlockedReason::QuotaExhausted)
    );
    assert_eq!(
        second_view.blocked_source,
        Some(BlockedSource::UpstreamRetryAfter)
    );
    assert_eq!(second_view.last_error.as_deref(), Some("rate limited"));
}

#[test]
fn retryable_block_replaces_shorter_existing_window_with_longer_retry_after() {
    let temp = tempdir().expect("tempdir");
    let mut store = AccountStore::new(temp.path().to_path_buf());
    let view = store
        .import_account("rt_123".to_string(), None, None)
        .expect("import succeeds")
        .account;

    let id = view.id.clone();
    store
        .mark_request_failure(
            &id,
            FailureClass::RateLimited,
            Some(StdDuration::from_secs(1)),
            "short retry".to_string(),
        )
        .expect("first failure applied");

    let before = Utc::now();
    store
        .mark_request_failure(
            &id,
            FailureClass::QuotaExhausted,
            Some(StdDuration::from_secs(600)),
            "long retry".to_string(),
        )
        .expect("second failure applied");

    let view = store.view(&id).expect("account remains");
    let blocked_until = view.blocked_until.expect("blocked until");
    assert_eq!(view.blocked_reason, Some(BlockedReason::QuotaExhausted));
    assert_eq!(view.blocked_source, Some(BlockedSource::UpstreamRetryAfter));
    assert!(blocked_until >= before + Duration::seconds(599));
    assert_eq!(view.last_error.as_deref(), Some("long retry"));
}

#[test]
fn local_backoff_retryable_block_does_not_shorten_longer_existing_retry_after_window() {
    let temp = tempdir().expect("tempdir");
    let mut store = AccountStore::new(temp.path().to_path_buf());
    let view = store
        .import_account("rt_123".to_string(), None, None)
        .expect("import succeeds")
        .account;

    let id = view.id.clone();
    store
        .mark_request_failure(
            &id,
            FailureClass::RateLimited,
            Some(StdDuration::from_secs(600)),
            "long retry".to_string(),
        )
        .expect("first failure applied");

    let initial = store.view(&id).expect("account remains");
    let initial_until = initial.blocked_until.expect("blocked until");

    store
        .mark_request_failure(
            &id,
            FailureClass::RateLimited,
            None,
            "local backoff".to_string(),
        )
        .expect("second failure applied");

    let view = store.view(&id).expect("account remains");
    assert_eq!(view.blocked_until, Some(initial_until));
    assert_eq!(view.blocked_reason, Some(BlockedReason::RateLimited));
    assert_eq!(view.blocked_source, Some(BlockedSource::UpstreamRetryAfter));
    assert_eq!(view.last_error.as_deref(), Some("local backoff"));
    assert_eq!(
        store
            .records
            .get(&id)
            .expect("record exists")
            .local_backoff_level,
        1
    );
}

#[test]
fn invalidate_access_token_keeps_account_file_and_returns_to_cold() {
    let temp = tempdir().expect("tempdir");
    let mut store = AccountStore::new(temp.path().to_path_buf());
    let view = store
        .import_account("rt_123".to_string(), None, None)
        .expect("import succeeds")
        .account;

    let id = view.id.clone();
    let record = store.records.get_mut(&id).expect("record exists");
    record.access_token = Some("at_123".to_string());
    record.access_token_expires_at = Some(Utc::now() + Duration::minutes(30));
    record.routing_state = RoutingState::Ready;
    record.blocked_reason = Some(BlockedReason::RateLimited);
    record.blocked_source = Some(BlockedSource::LocalBackoff);
    record.blocked_until = Some(Utc::now() + Duration::minutes(1));

    store.invalidate_access_token(&id);

    let view = store.view(&id).expect("account remains");
    assert_eq!(view.routing_state, RoutingState::Ready);
    assert!(view.access_token_expires_at.is_none());
    assert_eq!(view.blocked_reason, Some(BlockedReason::RateLimited));
    assert!(temp.path().join(format!("{id}.json")).exists());
}

#[test]
fn access_token_rejected_enters_temporary_cooldown_without_trash() {
    let temp = tempdir().expect("tempdir");
    let mut store = AccountStore::new(temp.path().to_path_buf());
    let view = store
        .import_account("rt_123".to_string(), None, None)
        .expect("import succeeds")
        .account;

    let id = view.id.clone();
    let record = store.records.get_mut(&id).expect("record exists");
    record.access_token = Some("at_123".to_string());
    record.access_token_expires_at = Some(Utc::now() + Duration::minutes(30));
    record.in_flight_requests = 1;

    store
        .mark_request_failure(
            &id,
            FailureClass::AccessTokenRejected,
            None,
            "request failed".to_string(),
        )
        .expect("failure applied");

    let view = store.view(&id).expect("account remains");
    assert_eq!(view.routing_state, RoutingState::TemporarilyUnavailable);
    assert_eq!(
        view.blocked_reason,
        Some(BlockedReason::TemporarilyUnavailable)
    );
    assert!(temp.path().join(format!("{id}.json")).exists());
}

#[test]
fn access_token_rejected_does_not_shorten_existing_longer_block() {
    let temp = tempdir().expect("tempdir");
    let mut store = AccountStore::new(temp.path().to_path_buf());
    let view = store
        .import_account("rt_123".to_string(), None, None)
        .expect("import succeeds")
        .account;

    let id = view.id.clone();
    store
        .mark_request_failure(
            &id,
            FailureClass::QuotaExhausted,
            Some(StdDuration::from_secs(600)),
            "quota exhausted".to_string(),
        )
        .expect("first failure applied");

    let initial = store.view(&id).expect("account remains");
    let initial_until = initial.blocked_until.expect("blocked until");

    let record = store.records.get_mut(&id).expect("record exists");
    record.access_token = Some("at_123".to_string());
    record.access_token_expires_at = Some(Utc::now() + Duration::minutes(30));
    record.in_flight_requests = 1;

    store
        .mark_request_failure(
            &id,
            FailureClass::AccessTokenRejected,
            None,
            "request rejected access token".to_string(),
        )
        .expect("second failure applied");

    let view = store.view(&id).expect("account remains");
    assert_eq!(view.routing_state, RoutingState::TemporarilyUnavailable);
    assert_eq!(view.blocked_until, Some(initial_until));
    assert_eq!(view.blocked_reason, Some(BlockedReason::QuotaExhausted));
    assert_eq!(view.blocked_source, Some(BlockedSource::UpstreamRetryAfter));
    assert_eq!(
        view.last_error.as_deref(),
        Some("request rejected access token")
    );
}

#[test]
fn temporary_failure_without_retry_after_does_not_shorten_existing_longer_block() {
    let temp = tempdir().expect("tempdir");
    let mut store = AccountStore::new(temp.path().to_path_buf());
    let view = store
        .import_account("rt_123".to_string(), None, None)
        .expect("import succeeds")
        .account;

    let id = view.id.clone();
    store
        .mark_request_failure(
            &id,
            FailureClass::RateLimited,
            Some(StdDuration::from_secs(600)),
            "rate limited".to_string(),
        )
        .expect("first failure applied");

    let initial = store.view(&id).expect("account remains");
    let initial_until = initial.blocked_until.expect("blocked until");

    store
        .mark_request_failure(
            &id,
            FailureClass::TemporaryFailure,
            None,
            "temporary failure".to_string(),
        )
        .expect("second failure applied");

    let view = store.view(&id).expect("account remains");
    assert_eq!(view.routing_state, RoutingState::TemporarilyUnavailable);
    assert_eq!(view.blocked_until, Some(initial_until));
    assert_eq!(view.blocked_reason, Some(BlockedReason::RateLimited));
    assert_eq!(view.blocked_source, Some(BlockedSource::UpstreamRetryAfter));
    assert_eq!(view.last_error.as_deref(), Some("temporary failure"));
}

#[test]
fn mark_request_failure_auth_invalid_sets_permanent_block() {
    let temp = tempdir().expect("tempdir");
    let mut store = AccountStore::new(temp.path().to_path_buf());
    let view = store
        .import_account("rt_123".to_string(), None, None)
        .expect("import succeeds")
        .account;

    let id = view.id.clone();
    let record = store.records.get_mut(&id).expect("record exists");
    record.access_token = Some("at_123".to_string());
    record.access_token_expires_at = Some(Utc::now() + Duration::minutes(30));
    record.in_flight_requests = 1;

    store
        .mark_request_failure(
            &id,
            FailureClass::AuthInvalid,
            None,
            "auth invalid".to_string(),
        )
        .expect("failure applied");

    let view = store.view(&id).expect("account remains");
    assert_eq!(view.routing_state, RoutingState::AuthInvalid);
    assert_eq!(view.blocked_reason, Some(BlockedReason::AuthInvalid));
    assert_eq!(view.blocked_source, Some(BlockedSource::FixedPolicy));
    assert!(view.blocked_until.is_none());
    assert_eq!(view.last_error.as_deref(), Some("auth invalid"));
}

#[test]
fn request_success_does_not_clear_active_block() {
    let temp = tempdir().expect("tempdir");
    let mut store = AccountStore::new(temp.path().to_path_buf());
    let view = store
        .import_account("rt_123".to_string(), None, None)
        .expect("import succeeds")
        .account;

    let id = view.id.clone();
    let record = store.records.get_mut(&id).expect("record exists");
    record.access_token = Some("at_123".to_string());
    record.routing_state = RoutingState::Cooldown;
    record.blocked_reason = Some(BlockedReason::RateLimited);
    record.blocked_source = Some(BlockedSource::LocalBackoff);
    record.blocked_until = Some(Utc::now() + Duration::minutes(1));
    record.in_flight_requests = 1;
    record.local_backoff_level = 3;
    record.last_error = Some("rate limited".to_string());

    store.mark_request_success(&id);

    let view = store.view(&id).expect("account remains");
    assert_eq!(view.routing_state, RoutingState::Cooldown);
    assert_eq!(view.blocked_reason, Some(BlockedReason::RateLimited));
    assert_eq!(view.in_flight_requests, 0);
    assert_eq!(
        store
            .records
            .get(&id)
            .expect("record exists")
            .local_backoff_level,
        3
    );
    assert_eq!(view.last_error.as_deref(), Some("rate limited"));
}

#[test]
fn refresh_success_does_not_clear_active_block() {
    let temp = tempdir().expect("tempdir");
    let mut store = AccountStore::new(temp.path().to_path_buf());
    let view = store
        .import_account("rt_123".to_string(), None, None)
        .expect("import succeeds")
        .account;

    let id = view.id.clone();
    let record = store.records.get_mut(&id).expect("record exists");
    record.routing_state = RoutingState::Cooldown;
    record.blocked_reason = Some(BlockedReason::RateLimited);
    record.blocked_source = Some(BlockedSource::LocalBackoff);
    record.blocked_until = Some(Utc::now() + Duration::minutes(1));
    record.refresh_in_flight = true;
    record.last_error = Some("rate limited".to_string());

    store
        .finish_refresh_success(
            &id,
            RefreshedAccount {
                access_token: "at_123".to_string(),
                refresh_token: None,
                account_id: Some("acct_123".to_string()),
                plan_type: Some("plus".to_string()),
                email: Some("user@example.com".to_string()),
                access_token_expires_at: Some(Utc::now() + Duration::minutes(30)),
            },
        )
        .expect("refresh succeeds");

    let view = store.view(&id).expect("account remains");
    assert_eq!(view.routing_state, RoutingState::Cooldown);
    assert_eq!(view.blocked_reason, Some(BlockedReason::RateLimited));
    assert_eq!(view.last_error.as_deref(), Some("rate limited"));
}

#[test]
fn refresh_success_preserves_existing_account_metadata_when_new_token_omits_it() {
    let temp = tempdir().expect("tempdir");
    let mut store = AccountStore::new(temp.path().to_path_buf());
    let view = store
        .import_account(
            "rt_123".to_string(),
            Some("label".to_string()),
            Some("old@example.com".to_string()),
        )
        .expect("import succeeds")
        .account;

    let id = view.id.clone();
    let record = store.records.get_mut(&id).expect("record exists");
    record.account_id = Some("acct_old".to_string());
    record.plan_type = Some("plus".to_string());
    record.refresh_in_flight = true;
    record.routing_state = RoutingState::Warming;

    store
        .finish_refresh_success(
            &id,
            RefreshedAccount {
                access_token: "at_123".to_string(),
                refresh_token: None,
                account_id: None,
                plan_type: None,
                email: None,
                access_token_expires_at: Some(Utc::now() + Duration::minutes(30)),
            },
        )
        .expect("refresh succeeds");

    let view = store.view(&id).expect("account remains");
    assert_eq!(view.account_id.as_deref(), Some("acct_old"));
    assert_eq!(view.plan_type.as_deref(), Some("plus"));
    assert_eq!(view.email.as_deref(), Some("old@example.com"));
}

#[test]
fn request_rejected_preserves_existing_block() {
    let temp = tempdir().expect("tempdir");
    let mut store = AccountStore::new(temp.path().to_path_buf());
    let view = store
        .import_account("rt_123".to_string(), None, None)
        .expect("import succeeds")
        .account;

    let id = view.id.clone();
    let record = store.records.get_mut(&id).expect("record exists");
    record.access_token = Some("at_123".to_string());
    record.routing_state = RoutingState::Cooldown;
    record.blocked_reason = Some(BlockedReason::RateLimited);
    record.blocked_source = Some(BlockedSource::LocalBackoff);
    record.blocked_until = Some(Utc::now() + Duration::minutes(1));
    record.in_flight_requests = 1;

    store
        .mark_request_failure(
            &id,
            FailureClass::RequestRejected,
            None,
            "bad request".to_string(),
        )
        .expect("failure applied");

    let view = store.view(&id).expect("account remains");
    assert_eq!(view.routing_state, RoutingState::Cooldown);
    assert_eq!(view.blocked_reason, Some(BlockedReason::RateLimited));
    assert_eq!(view.last_error.as_deref(), Some("bad request"));
}

#[test]
fn wake_account_clears_local_block_without_clearing_last_error() {
    let temp = tempdir().expect("tempdir");
    let mut store = AccountStore::new(temp.path().to_path_buf());
    let view = store
        .import_account("rt_123".to_string(), None, None)
        .expect("import succeeds")
        .account;

    let id = view.id.clone();
    let record = store.records.get_mut(&id).expect("record exists");
    record.access_token = Some("at_123".to_string());
    record.routing_state = RoutingState::Cooldown;
    record.blocked_reason = Some(BlockedReason::RateLimited);
    record.blocked_source = Some(BlockedSource::LocalBackoff);
    record.blocked_until = Some(Utc::now() + Duration::minutes(5));
    record.local_backoff_level = 4;
    record.last_error = Some("rate limited".to_string());

    let result = store.wake_account(&id).expect("wake succeeds");

    assert_eq!(result.disposition, WakeDisposition::Woken);
    assert_eq!(result.account.routing_state, RoutingState::Ready);
    assert!(result.account.blocked_reason.is_none());
    assert!(result.account.blocked_source.is_none());
    assert!(result.account.blocked_until.is_none());
    assert_eq!(result.account.last_error.as_deref(), Some("rate limited"));
    assert_eq!(
        store
            .records
            .get(&id)
            .expect("record exists")
            .local_backoff_level,
        0
    );
}

#[test]
fn wake_account_skips_auth_invalid() {
    let temp = tempdir().expect("tempdir");
    let mut store = AccountStore::new(temp.path().to_path_buf());
    let view = store
        .import_account("rt_123".to_string(), None, None)
        .expect("import succeeds")
        .account;

    let id = view.id.clone();
    let record = store.records.get_mut(&id).expect("record exists");
    record.routing_state = RoutingState::AuthInvalid;
    record.blocked_reason = Some(BlockedReason::AuthInvalid);
    record.blocked_source = Some(BlockedSource::FixedPolicy);
    record.blocked_until = None;
    record.local_backoff_level = 7;

    let result = store.wake_account(&id).expect("wake succeeds");

    assert_eq!(result.disposition, WakeDisposition::SkippedAuthInvalid);
    assert_eq!(result.account.routing_state, RoutingState::AuthInvalid);
    assert_eq!(
        result.account.blocked_reason,
        Some(BlockedReason::AuthInvalid)
    );
    assert_eq!(
        store
            .records
            .get(&id)
            .expect("record exists")
            .local_backoff_level,
        7
    );
}
