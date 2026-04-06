use super::*;

impl AccountStore {
    pub fn list(&mut self) -> Vec<AccountView> {
        self.refresh_expired_blocks();
        let mut items = self
            .records
            .values()
            .map(AccountView::from)
            .collect::<Vec<_>>();
        items.sort_by(|left, right| left.id.cmp(&right.id));
        items
    }

    pub fn select_account(
        &mut self,
        policy: RoutingPolicy,
        refresh_skew_seconds: i64,
        excluded_accounts: &HashSet<String>,
    ) -> Result<AccountSelection, SelectionFailure> {
        self.refresh_expired_blocks();

        let candidates = self.build_route_candidates(excluded_accounts);
        let account_id = select_candidate(policy, &candidates, &mut self.round_robin_cursor)
            .ok_or(SelectionFailure::NoEligibleAccount)?;

        let record = self
            .records
            .get_mut(&account_id)
            .ok_or_else(|| SelectionFailure::Internal(anyhow!("selected account disappeared")))?;

        record.in_flight_requests = record.in_flight_requests.saturating_add(1);
        record.last_selected_at = Some(Utc::now());
        let needs_refresh = token_needs_refresh(record, refresh_skew_seconds);
        if needs_refresh {
            record.routing_state = RoutingState::Warming;
            record.refresh_in_flight = true;
        }

        Ok(AccountSelection {
            account_id,
            refresh_token: record.refresh_token.clone(),
            needs_refresh,
        })
    }

    pub fn summarize_selection_failure(
        &mut self,
        excluded_accounts: &HashSet<String>,
    ) -> Option<PoolBlockSummary> {
        self.refresh_expired_blocks();
        let now = Utc::now();
        let routeable = self
            .records
            .values()
            .filter(|record| {
                !record.detached
                    && record.routing_state != RoutingState::AuthInvalid
                    && !excluded_accounts.contains(&record.id)
            })
            .collect::<Vec<_>>();

        if routeable.is_empty() {
            return None;
        }

        let has_eligible = routeable
            .iter()
            .any(|record| !record.refresh_in_flight && !has_active_block_at(record, now));
        if has_eligible {
            return None;
        }

        let has_refresh_in_flight = routeable.iter().any(|record| record.refresh_in_flight);
        if has_refresh_in_flight {
            let retry_after = StdDuration::from_secs(1);
            return Some(PoolBlockSummary {
                blocked_reason: BlockedReason::TemporarilyUnavailable,
                blocked_until: Some(now + duration_from_std(retry_after)),
                retry_after: Some(retry_after),
            });
        }

        let selected = routeable
            .iter()
            .filter(|record| !record.refresh_in_flight)
            .filter_map(|record| {
                record
                    .blocked_reason
                    .map(|reason| (reason, record.blocked_until))
            })
            .min_by(|left, right| compare_block_priority(*left, *right))?;

        let retry_after = selected.1.and_then(|until| {
            let delta = until - now;
            delta.to_std().ok()
        });

        Some(PoolBlockSummary {
            blocked_reason: selected.0,
            blocked_until: selected.1,
            retry_after,
        })
    }

    pub(super) fn build_route_candidates(
        &self,
        excluded_accounts: &HashSet<String>,
    ) -> Vec<RouteCandidate> {
        let mut candidates = self
            .records
            .values()
            .filter(|record| self.is_eligible(record) && !excluded_accounts.contains(&record.id))
            .map(|record| RouteCandidate {
                account_id: record.id.clone(),
                in_flight_requests: record.in_flight_requests,
                last_selected_at: record.last_selected_at,
            })
            .collect::<Vec<_>>();
        candidates.sort_by(|left, right| left.account_id.cmp(&right.account_id));
        candidates
    }

    fn is_eligible(&self, record: &AccountRecord) -> bool {
        !record.detached
            && !record.refresh_in_flight
            && record.routing_state != RoutingState::AuthInvalid
            && record.blocked_until.is_none_or(|until| until <= Utc::now())
    }
}
