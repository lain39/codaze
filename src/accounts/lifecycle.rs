use super::*;

impl AccountStore {
    pub fn wake_account(&mut self, account_id: &str) -> anyhow::Result<WakeAccountResult> {
        let record = self
            .records
            .get_mut(account_id)
            .ok_or_else(|| anyhow!("unknown account {account_id}"))?;
        let disposition = apply_wake(record);
        let account = AccountView::from(&*record);
        Ok(WakeAccountResult {
            disposition,
            account,
        })
    }

    pub fn wake_all_accounts(&mut self) -> WakeAllResult {
        let mut ids = self.records.keys().cloned().collect::<Vec<_>>();
        ids.sort();

        let mut woken = 0usize;
        let mut skipped_auth_invalid = 0usize;
        let mut accounts = Vec::with_capacity(ids.len());

        for id in ids {
            let Some(record) = self.records.get_mut(&id) else {
                continue;
            };
            let disposition = apply_wake(record);
            match disposition {
                WakeDisposition::Woken => woken += 1,
                WakeDisposition::SkippedAuthInvalid => skipped_auth_invalid += 1,
            }
            accounts.push(WakeAccountResult {
                disposition,
                account: AccountView::from(&*record),
            });
        }

        WakeAllResult {
            woken,
            skipped_auth_invalid,
            accounts,
        }
    }

    #[cfg(test)]
    pub fn finish_refresh_success(
        &mut self,
        account_id: &str,
        refreshed: RefreshedAccount,
    ) -> anyhow::Result<RefreshSuccessResult> {
        let (mut result, persist_op) =
            self.finish_refresh_success_without_persist(account_id, refreshed)?;
        if let Some(persist_op) = persist_op
            && let Err(error) = execute_account_disk_op(&persist_op)
        {
            result.persist_warning = Some(format!("persist account file failed: {error}"));
        }
        Ok(result)
    }

    pub fn finish_refresh_failure(
        &mut self,
        account_id: &str,
        failure: FailureClass,
        retry_after: Option<StdDuration>,
        details: String,
    ) -> anyhow::Result<()> {
        if failure == FailureClass::AuthInvalid {
            let plan = self.prepare_auth_invalid_failure(account_id, details)?;
            if let Some(trash_op) = &plan.trash_op
                && let Err(error) = execute_account_disk_op(trash_op)
            {
                self.note_auth_invalid_trash_failure(&plan.account_id, &error.to_string());
                return Ok(());
            }
            self.finalize_auth_invalid_failure(&plan.account_id)?;
            Ok(())
        } else {
            self.apply_failure(account_id, failure, retry_after, details)
        }
    }

    pub fn mark_request_success(&mut self, account_id: &str) {
        let Some(record) = self.records.get_mut(account_id) else {
            return;
        };

        let now = Utc::now();
        record.in_flight_requests = record.in_flight_requests.saturating_sub(1);
        record.refresh_in_flight = false;
        if record.routing_state != RoutingState::AuthInvalid && !has_active_block_at(record, now) {
            record.routing_state = if record.access_token.is_some() {
                RoutingState::Ready
            } else {
                RoutingState::Cold
            };
            record.local_backoff_level = 0;
            record.last_error = None;
        }
        record.last_success_at = Some(now);
        self.maybe_remove_detached(account_id);
    }

    pub fn mark_request_failure(
        &mut self,
        account_id: &str,
        failure: FailureClass,
        retry_after: Option<StdDuration>,
        details: String,
    ) -> anyhow::Result<()> {
        self.apply_failure(account_id, failure, retry_after, details)
    }

    pub fn invalidate_access_token(&mut self, account_id: &str) {
        let Some(record) = self.records.get_mut(account_id) else {
            return;
        };

        let now = Utc::now();
        record.access_token = None;
        record.access_token_expires_at = None;
        record.refresh_in_flight = false;
        if record.routing_state != RoutingState::AuthInvalid && !has_active_block_at(record, now) {
            record.routing_state = RoutingState::Cold;
        }
        self.maybe_remove_detached(account_id);
    }

    #[cfg(test)]
    pub(crate) fn test_mark_refresh_in_flight(&mut self, account_id: &str) -> anyhow::Result<()> {
        let record = self
            .records
            .get_mut(account_id)
            .ok_or_else(|| anyhow!("unknown account {account_id}"))?;
        record.refresh_in_flight = true;
        record.routing_state = RoutingState::Warming;
        Ok(())
    }

    #[cfg(test)]
    pub(crate) fn test_record_mut(
        &mut self,
        account_id: &str,
    ) -> anyhow::Result<&mut AccountRecord> {
        self.records
            .get_mut(account_id)
            .ok_or_else(|| anyhow!("unknown account {account_id}"))
    }

    pub fn release_selection(&mut self, account_id: &str) {
        if let Some(record) = self.records.get_mut(account_id) {
            let now = Utc::now();
            record.in_flight_requests = record.in_flight_requests.saturating_sub(1);
            record.refresh_in_flight = false;
            if record.routing_state == RoutingState::Warming && !has_active_block_at(record, now) {
                record.routing_state = if record.access_token.is_some() {
                    RoutingState::Ready
                } else {
                    RoutingState::Cold
                };
            }
        }
        self.maybe_remove_detached(account_id);
    }

    pub fn upstream_account(
        &self,
        account_id: &str,
    ) -> Result<UpstreamAccount, UpstreamAccountError> {
        let record = self
            .records
            .get(account_id)
            .ok_or(UpstreamAccountError::MissingRecord)?;
        let access_token = record
            .access_token
            .clone()
            .ok_or(UpstreamAccountError::MissingAccessToken)?;
        Ok(UpstreamAccount {
            access_token,
            chatgpt_account_id: record.account_id.clone(),
        })
    }

    pub fn view(&self, account_id: &str) -> anyhow::Result<AccountView> {
        let record = self
            .records
            .get(account_id)
            .ok_or_else(|| anyhow!("unknown account {account_id}"))?;
        Ok(AccountView::from(record))
    }

    fn apply_failure(
        &mut self,
        account_id: &str,
        failure: FailureClass,
        retry_after: Option<StdDuration>,
        details: String,
    ) -> anyhow::Result<()> {
        let Some(record) = self.records.get_mut(account_id) else {
            return Ok(());
        };

        let now = Utc::now();
        record.in_flight_requests = record.in_flight_requests.saturating_sub(1);
        record.refresh_in_flight = false;
        record.last_error_at = Some(now);
        record.last_error = Some(details);

        // Failure classification decides state and block transition, but does not
        // write blocked_* directly. That stays centralized in accounts/mod.rs.
        match failure {
            FailureClass::AccessTokenRejected => {
                record.access_token = None;
                record.access_token_expires_at = None;
                record.routing_state = RoutingState::TemporarilyUnavailable;
                apply_fixed_block(
                    record,
                    now,
                    BlockedReason::TemporarilyUnavailable,
                    Duration::seconds(TEMPORARY_FAILURE_COOLDOWN_SECONDS),
                );
            }
            FailureClass::AuthInvalid => {
                record.routing_state = RoutingState::AuthInvalid;
                record.access_token = None;
                record.access_token_expires_at = None;
                apply_auth_invalid_block(record);
            }
            FailureClass::RateLimited => {
                apply_retryable_block(record, now, BlockedReason::RateLimited, retry_after);
                record.routing_state = RoutingState::Cooldown;
            }
            FailureClass::QuotaExhausted => {
                apply_retryable_block(record, now, BlockedReason::QuotaExhausted, retry_after);
                record.routing_state = RoutingState::Cooldown;
            }
            FailureClass::RiskControlled => {
                record.routing_state = RoutingState::RiskControlled;
                apply_fixed_block(
                    record,
                    now,
                    BlockedReason::RiskControlled,
                    Duration::seconds(RISK_COOLDOWN_SECONDS),
                );
            }
            FailureClass::TemporaryFailure => {
                record.routing_state = RoutingState::TemporarilyUnavailable;
                if retry_after.is_some() {
                    apply_retryable_block(
                        record,
                        now,
                        BlockedReason::TemporarilyUnavailable,
                        retry_after,
                    );
                } else {
                    apply_fixed_block(
                        record,
                        now,
                        BlockedReason::TemporarilyUnavailable,
                        Duration::seconds(TEMPORARY_FAILURE_COOLDOWN_SECONDS),
                    );
                }
            }
            FailureClass::RequestRejected | FailureClass::InternalFailure => {
                apply_block_transition(record, now, BlockTransition::NoChange);
                if !has_active_block_at(record, now)
                    && record.routing_state != RoutingState::AuthInvalid
                {
                    record.routing_state = if record.access_token.is_some() {
                        RoutingState::Ready
                    } else {
                        RoutingState::Cold
                    };
                }
            }
        }

        self.maybe_remove_detached(account_id);
        Ok(())
    }

    pub(crate) fn finish_refresh_success_without_persist(
        &mut self,
        account_id: &str,
        refreshed: RefreshedAccount,
    ) -> anyhow::Result<(RefreshSuccessResult, Option<AccountDiskOp>)> {
        let record = self
            .records
            .get_mut(account_id)
            .ok_or_else(|| anyhow!("unknown account {account_id}"))?;

        record.access_token = Some(refreshed.access_token);
        if let Some(refresh_token) = refreshed.refresh_token {
            record.refresh_token = refresh_token;
        }
        if let Some(account_id) = refreshed.account_id {
            record.account_id = Some(account_id);
        }
        if let Some(plan_type) = refreshed.plan_type {
            record.plan_type = Some(plan_type);
        }
        if refreshed.email.is_some() {
            record.email = refreshed.email;
        }
        record.access_token_expires_at = refreshed.access_token_expires_at;
        let now = Utc::now();
        record.last_refresh_at = Some(now);
        record.refresh_in_flight = false;
        if !has_active_block_at(record, now) {
            record.routing_state = RoutingState::Ready;
            clear_block(record);
            record.local_backoff_level = 0;
            record.last_error = None;
        }

        let persist_op = (!record.detached).then(|| AccountDiskOp::Write {
            path: record.file_path.clone(),
            disk: account_file_from_record(record),
        });
        self.maybe_remove_detached(account_id);
        Ok((RefreshSuccessResult::default(), persist_op))
    }

    pub(crate) fn prepare_auth_invalid_failure(
        &mut self,
        account_id: &str,
        details: String,
    ) -> anyhow::Result<AuthInvalidPlan> {
        let record = self
            .records
            .get_mut(account_id)
            .ok_or_else(|| anyhow!("unknown account {account_id}"))?;

        let now = Utc::now();
        record.in_flight_requests = record.in_flight_requests.saturating_sub(1);
        record.refresh_in_flight = false;
        record.last_error_at = Some(now);
        record.last_error = Some(details);
        record.access_token = None;
        record.access_token_expires_at = None;
        record.routing_state = RoutingState::AuthInvalid;
        apply_auth_invalid_block(record);
        record.auth_invalid_tombstone = true;

        let trash_op = if record.detached {
            None
        } else {
            Some(AccountDiskOp::MoveToTrash {
                source: record.file_path.clone(),
                accounts_dir: self.accounts_dir.clone(),
            })
        };

        Ok(AuthInvalidPlan {
            account_id: account_id.to_string(),
            trash_op,
        })
    }

    pub(crate) fn finalize_auth_invalid_failure(&mut self, account_id: &str) -> anyhow::Result<()> {
        let record = self
            .records
            .get_mut(account_id)
            .ok_or_else(|| anyhow!("unknown account {account_id}"))?;
        record.routing_state = RoutingState::AuthInvalid;
        record.access_token = None;
        record.access_token_expires_at = None;
        apply_auth_invalid_block(record);
        record.auth_invalid_tombstone = true;
        record.detached = true;
        self.maybe_remove_detached(account_id);
        Ok(())
    }

    pub(crate) fn note_auth_invalid_trash_failure(&mut self, account_id: &str, error: &str) {
        let Some(record) = self.records.get_mut(account_id) else {
            return;
        };

        let suffix = format!("move invalid account file to trash failed: {error}");
        let next_error = match record.last_error.take() {
            Some(current) if current.contains(&suffix) => current,
            Some(current) => format!("{current}; {suffix}"),
            None => suffix,
        };
        record.last_error = Some(next_error);
        record.last_error_at = Some(Utc::now());
    }

    pub(super) fn refresh_expired_blocks(&mut self) {
        let now = Utc::now();
        for record in self.records.values_mut() {
            if let Some(until) = record.blocked_until
                && until <= now
            {
                clear_block(record);
                if record.routing_state != RoutingState::AuthInvalid && !record.refresh_in_flight {
                    record.routing_state = if record.access_token.is_some() {
                        RoutingState::Ready
                    } else {
                        RoutingState::Cold
                    };
                }
            }
            if record.routing_state == RoutingState::Cold && record.access_token.is_some() {
                record.routing_state = RoutingState::Ready;
            }
        }
    }

    pub(super) fn maybe_remove_detached(&mut self, account_id: &str) {
        let remove = self.records.get(account_id).is_some_and(|record| {
            record.detached && record.in_flight_requests == 0 && !record.refresh_in_flight
        });
        if remove {
            self.records.remove(account_id);
        }
    }
}
