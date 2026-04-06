use super::*;

#[test]
fn summarize_selection_failure_prefers_earliest_retryable_record() {
    let temp = tempdir().expect("tempdir");
    let mut store = AccountStore::new(temp.path().to_path_buf());
    let first = store
        .import_account("rt_1".to_string(), None, None)
        .expect("import")
        .account
        .id;
    let second = store
        .import_account("rt_2".to_string(), None, None)
        .expect("import")
        .account
        .id;

    let now = Utc::now();
    {
        let first_record = store.records.get_mut(&first).expect("first record");
        first_record.blocked_reason = Some(BlockedReason::QuotaExhausted);
        first_record.blocked_source = Some(BlockedSource::UpstreamRetryAfter);
        first_record.blocked_until = Some(now + Duration::minutes(15));
        first_record.routing_state = RoutingState::Cooldown;
    }
    {
        let second_record = store.records.get_mut(&second).expect("second record");
        second_record.blocked_reason = Some(BlockedReason::RateLimited);
        second_record.blocked_source = Some(BlockedSource::UpstreamRetryAfter);
        second_record.blocked_until = Some(now + Duration::minutes(5));
        second_record.routing_state = RoutingState::Cooldown;
    }

    let summary = store
        .summarize_selection_failure(&HashSet::new())
        .expect("summary exists");

    assert_eq!(summary.blocked_reason, BlockedReason::RateLimited);
    let blocked_until = summary.blocked_until.expect("blocked until");
    assert!(blocked_until >= now + Duration::minutes(4));
    assert!(blocked_until <= now + Duration::minutes(6));
    assert!(summary.retry_after.is_some());
}

#[test]
fn summarize_selection_failure_ignores_auth_invalid_accounts() {
    let temp = tempdir().expect("tempdir");
    let mut store = AccountStore::new(temp.path().to_path_buf());
    let id = store
        .import_account("rt_auth".to_string(), None, None)
        .expect("import")
        .account
        .id;

    let record = store.records.get_mut(&id).expect("record exists");
    record.routing_state = RoutingState::AuthInvalid;
    record.blocked_reason = Some(BlockedReason::AuthInvalid);
    record.blocked_source = Some(BlockedSource::FixedPolicy);
    record.blocked_until = None;

    assert!(store.summarize_selection_failure(&HashSet::new()).is_none());
}

#[test]
fn summarize_selection_failure_when_all_routeable_accounts_are_refreshing() {
    let temp = tempdir().expect("tempdir");
    let mut store = AccountStore::new(temp.path().to_path_buf());
    let first = store
        .import_account("rt_1".to_string(), None, None)
        .expect("import")
        .account
        .id;
    let second = store
        .import_account("rt_2".to_string(), None, None)
        .expect("import")
        .account
        .id;

    store
        .records
        .get_mut(&first)
        .expect("first")
        .refresh_in_flight = true;
    store
        .records
        .get_mut(&second)
        .expect("second")
        .refresh_in_flight = true;

    let summary = store
        .summarize_selection_failure(&HashSet::new())
        .expect("summary exists");
    assert_eq!(
        summary.blocked_reason,
        BlockedReason::TemporarilyUnavailable
    );
    assert_eq!(summary.retry_after, Some(StdDuration::from_secs(1)));
    assert!(summary.blocked_until.is_some());
}

#[test]
fn summarize_selection_failure_prefers_refresh_in_flight_over_existing_block() {
    let temp = tempdir().expect("tempdir");
    let mut store = AccountStore::new(temp.path().to_path_buf());
    let blocked = store
        .import_account("rt_blocked".to_string(), None, None)
        .expect("import")
        .account
        .id;
    let refreshing = store
        .import_account("rt_refreshing".to_string(), None, None)
        .expect("import")
        .account
        .id;

    {
        let record = store.records.get_mut(&blocked).expect("blocked");
        record.blocked_reason = Some(BlockedReason::QuotaExhausted);
        record.blocked_source = Some(BlockedSource::UpstreamRetryAfter);
        record.blocked_until = Some(Utc::now() + Duration::minutes(10));
        record.routing_state = RoutingState::Cooldown;
    }
    {
        let record = store.records.get_mut(&refreshing).expect("refreshing");
        record.refresh_in_flight = true;
        record.routing_state = RoutingState::Warming;
    }

    let summary = store
        .summarize_selection_failure(&HashSet::new())
        .expect("summary exists");
    assert_eq!(
        summary.blocked_reason,
        BlockedReason::TemporarilyUnavailable
    );
    assert_eq!(summary.retry_after, Some(StdDuration::from_secs(1)));
    assert!(summary.blocked_until.is_some());
}

#[test]
fn summarize_selection_failure_excludes_failed_accounts_from_consideration() {
    let temp = tempdir().expect("tempdir");
    let mut store = AccountStore::new(temp.path().to_path_buf());
    let blocked = store
        .import_account("rt_blocked".to_string(), None, None)
        .expect("import")
        .account
        .id;
    let refreshing = store
        .import_account("rt_refreshing".to_string(), None, None)
        .expect("import")
        .account
        .id;

    {
        let record = store.records.get_mut(&blocked).expect("blocked");
        record.blocked_reason = Some(BlockedReason::QuotaExhausted);
        record.blocked_source = Some(BlockedSource::UpstreamRetryAfter);
        record.blocked_until = Some(Utc::now() + Duration::minutes(10));
        record.routing_state = RoutingState::Cooldown;
    }
    {
        let record = store.records.get_mut(&refreshing).expect("refreshing");
        record.refresh_in_flight = true;
        record.routing_state = RoutingState::Warming;
    }

    let excluded_accounts = HashSet::from([blocked]);
    let summary = store
        .summarize_selection_failure(&excluded_accounts)
        .expect("summary exists");
    assert_eq!(
        summary.blocked_reason,
        BlockedReason::TemporarilyUnavailable
    );
    assert_eq!(summary.retry_after, Some(StdDuration::from_secs(1)));
    assert!(summary.blocked_until.is_some());
}

#[test]
fn route_candidates_are_sorted_for_stable_round_robin() {
    let temp = tempdir().expect("tempdir");
    let mut store = AccountStore::new(temp.path().to_path_buf());
    for id in ["b-account", "c-account", "a-account"] {
        store.upsert_disk_record(
            id.to_string(),
            temp.path().join(format!("{id}.json")),
            AccountFile {
                refresh_token: format!("rt-{id}"),
                label: None,
                email: None,
            },
        );
    }

    let candidates = store.build_route_candidates(&HashSet::new());
    let ids = candidates
        .into_iter()
        .map(|candidate| candidate.account_id)
        .collect::<Vec<_>>();

    assert_eq!(ids, vec!["a-account", "b-account", "c-account"]);
}
