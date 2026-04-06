use crate::classifier::FailureClass;
use crate::config::RoutingPolicy;
use crate::router::{RouteCandidate, select_candidate};
use anyhow::{anyhow, bail};
use chrono::{DateTime, Duration, Utc};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::collections::{HashMap, HashSet};
use std::fmt::Write as _;
use std::fs;
use std::path::{Path, PathBuf};
use std::time::{Duration as StdDuration, SystemTime, UNIX_EPOCH};
use tracing::warn;

mod disk;
mod lifecycle;
mod selection;

pub(crate) use self::disk::{
    execute_account_disk_op, rescan_accounts_from_disk, startup_sync_accounts_from_disk,
};

const TEMPORARY_FAILURE_COOLDOWN_SECONDS: i64 = 60;
const RISK_COOLDOWN_SECONDS: i64 = 1800;
const LOCAL_BACKOFF_BASE_SECONDS: u64 = 1;
const LOCAL_BACKOFF_MAX_SECONDS: u64 = 30 * 60;
const ACCOUNT_FILE_EXTENSION: &str = "json";
const TRASH_DIR_NAME: &str = "trash";
pub(crate) const INVALID_REFRESH_TOKEN_MESSAGE: &str = "refresh_token must not be empty";

#[derive(Debug, Clone, Copy, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum RoutingState {
    Cold,
    Ready,
    Warming,
    Cooldown,
    RiskControlled,
    TemporarilyUnavailable,
    AuthInvalid,
}

#[derive(Debug, Clone, Copy, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum BlockedReason {
    RateLimited,
    QuotaExhausted,
    RiskControlled,
    TemporarilyUnavailable,
    AuthInvalid,
}

#[derive(Debug, Clone, Copy, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum BlockedSource {
    UpstreamRetryAfter,
    LocalBackoff,
    FixedPolicy,
}

#[derive(Debug, Clone)]
pub struct AccountRecord {
    pub id: String,
    pub file_path: PathBuf,
    pub label: Option<String>,
    pub email: Option<String>,
    pub refresh_token: String,
    pub access_token: Option<String>,
    pub account_id: Option<String>,
    pub plan_type: Option<String>,
    pub access_token_expires_at: Option<DateTime<Utc>>,
    pub last_refresh_at: Option<DateTime<Utc>>,
    pub routing_state: RoutingState,
    pub blocked_reason: Option<BlockedReason>,
    pub blocked_source: Option<BlockedSource>,
    pub blocked_until: Option<DateTime<Utc>>,
    pub local_backoff_level: u32,
    pub refresh_in_flight: bool,
    pub in_flight_requests: u32,
    pub last_selected_at: Option<DateTime<Utc>>,
    pub last_success_at: Option<DateTime<Utc>>,
    pub last_error_at: Option<DateTime<Utc>>,
    pub last_error: Option<String>,
    pub auth_invalid_tombstone: bool,
    pub detached: bool,
}

#[derive(Debug, Clone)]
pub struct AccountSelection {
    pub account_id: String,
    pub refresh_token: String,
    pub needs_refresh: bool,
}

#[derive(Debug)]
pub enum SelectionFailure {
    NoEligibleAccount,
    Internal(anyhow::Error),
}

#[derive(Debug, Clone)]
pub struct ImportAccountResult {
    pub account: AccountView,
    pub already_exists: bool,
}

#[derive(Debug, Clone)]
pub(crate) enum AccountDiskOp {
    Write {
        path: PathBuf,
        disk: AccountFile,
    },
    Remove {
        path: PathBuf,
    },
    MoveToTrash {
        source: PathBuf,
        accounts_dir: PathBuf,
    },
}

#[derive(Debug, Clone)]
pub(crate) struct ImportAccountPlan {
    pub(crate) account_id: String,
    pub(crate) already_exists: bool,
    pub(crate) apply: ImportApply,
    pub(crate) disk_op: Option<AccountDiskOp>,
}

#[derive(Debug, Clone)]
pub(crate) enum ImportApply {
    None,
    Insert {
        id: String,
        path: PathBuf,
        disk: AccountFile,
    },
    UpdateMetadata {
        account_id: String,
        label: Option<String>,
        email: Option<String>,
    },
}

#[derive(Debug, Clone)]
pub struct RefreshedAccount {
    pub access_token: String,
    pub refresh_token: Option<String>,
    pub account_id: Option<String>,
    pub plan_type: Option<String>,
    pub email: Option<String>,
    pub access_token_expires_at: Option<DateTime<Utc>>,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct RefreshSuccessResult {
    pub persist_warning: Option<String>,
}

#[derive(Debug, Clone)]
pub(crate) struct RemoveAccountPlan {
    pub(crate) account_id: String,
    pub(crate) disk_op: AccountDiskOp,
}

#[derive(Debug, Clone)]
pub(crate) struct AuthInvalidPlan {
    pub(crate) account_id: String,
    pub(crate) trash_op: Option<AccountDiskOp>,
}

#[derive(Debug, Clone, Serialize)]
pub struct AccountView {
    pub id: String,
    pub label: Option<String>,
    pub email: Option<String>,
    pub routing_state: RoutingState,
    pub blocked_reason: Option<BlockedReason>,
    pub blocked_source: Option<BlockedSource>,
    pub blocked_until: Option<DateTime<Utc>>,
    pub account_id: Option<String>,
    pub plan_type: Option<String>,
    pub refresh_in_flight: bool,
    pub in_flight_requests: u32,
    pub access_token_expires_at: Option<DateTime<Utc>>,
    pub last_refresh_at: Option<DateTime<Utc>>,
    pub last_selected_at: Option<DateTime<Utc>>,
    pub last_success_at: Option<DateTime<Utc>>,
    pub last_error_at: Option<DateTime<Utc>>,
    pub last_error: Option<String>,
}

#[derive(Debug, Clone, Copy, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum WakeDisposition {
    Woken,
    SkippedAuthInvalid,
}

#[derive(Debug, Clone, Serialize)]
pub struct WakeAccountResult {
    pub disposition: WakeDisposition,
    pub account: AccountView,
}

#[derive(Debug, Clone, Serialize)]
pub struct WakeAllResult {
    pub woken: usize,
    pub skipped_auth_invalid: usize,
    pub accounts: Vec<WakeAccountResult>,
}

#[derive(Debug, Clone)]
pub struct UpstreamAccount {
    pub access_token: String,
    pub chatgpt_account_id: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum UpstreamAccountError {
    MissingRecord,
    MissingAccessToken,
}

#[derive(Debug, Clone)]
pub struct PoolBlockSummary {
    pub blocked_reason: BlockedReason,
    #[allow(dead_code)]
    pub blocked_until: Option<DateTime<Utc>>,
    #[allow(dead_code)]
    pub retry_after: Option<StdDuration>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
pub(crate) struct AccountFile {
    refresh_token: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    label: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    email: Option<String>,
}

#[derive(Debug)]
pub struct AccountStore {
    accounts_dir: PathBuf,
    records: HashMap<String, AccountRecord>,
    round_robin_cursor: usize,
}

#[derive(Debug, Clone)]
struct DiskAccountEntry {
    id: String,
    path: PathBuf,
    disk: AccountFile,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum SyncMode {
    Startup,
    Rescan,
}

#[derive(Debug, Clone)]
pub(crate) struct DiskScanSnapshot {
    entries: Vec<DiskAccountEntry>,
    failed_file_ids: HashSet<String>,
    observed_paths: HashSet<PathBuf>,
}

#[derive(Debug, Clone)]
pub(crate) struct DiskSyncUpsert {
    id: String,
    path: PathBuf,
    disk: AccountFile,
}

#[derive(Debug, Clone)]
pub(crate) struct DiskSyncPlan {
    mode: SyncMode,
    writes: Vec<(PathBuf, AccountFile)>,
    removals: Vec<PathBuf>,
    upserts: Vec<DiskSyncUpsert>,
    seen_ids: HashSet<String>,
    failed_file_ids: HashSet<String>,
}

#[derive(Debug, Clone)]
struct CanonicalTarget {
    id: String,
    path: PathBuf,
    disk: AccountFile,
    matched_group_path: Option<PathBuf>,
}

#[derive(Debug, Clone, Copy)]
struct RetryableBlock {
    reason: BlockedReason,
    source: BlockedSource,
    until: DateTime<Utc>,
}

#[derive(Debug, Clone, Copy)]
enum BlockTransition {
    NoChange,
    Clear,
    Permanent {
        reason: BlockedReason,
        source: BlockedSource,
    },
    Timed(RetryableBlock),
}

impl AccountStore {
    pub fn new(accounts_dir: PathBuf) -> Self {
        Self {
            accounts_dir,
            records: HashMap::new(),
            round_robin_cursor: 0,
        }
    }
}

impl From<&AccountRecord> for AccountView {
    fn from(record: &AccountRecord) -> Self {
        Self {
            id: record.id.clone(),
            label: record.label.clone(),
            email: record.email.clone(),
            routing_state: record.routing_state,
            blocked_reason: record.blocked_reason,
            blocked_source: record.blocked_source,
            blocked_until: record.blocked_until,
            account_id: record.account_id.clone(),
            plan_type: record.plan_type.clone(),
            refresh_in_flight: record.refresh_in_flight,
            in_flight_requests: record.in_flight_requests,
            access_token_expires_at: record.access_token_expires_at,
            last_refresh_at: record.last_refresh_at,
            last_selected_at: record.last_selected_at,
            last_success_at: record.last_success_at,
            last_error_at: record.last_error_at,
            last_error: record.last_error.clone(),
        }
    }
}

fn clear_block(record: &mut AccountRecord) {
    apply_block_transition(record, Utc::now(), BlockTransition::Clear);
}

fn apply_auth_invalid_block(record: &mut AccountRecord) {
    apply_block_transition(
        record,
        Utc::now(),
        BlockTransition::Permanent {
            reason: BlockedReason::AuthInvalid,
            source: BlockedSource::FixedPolicy,
        },
    );
}

fn apply_wake(record: &mut AccountRecord) -> WakeDisposition {
    if record.routing_state == RoutingState::AuthInvalid
        || record.blocked_reason == Some(BlockedReason::AuthInvalid)
    {
        return WakeDisposition::SkippedAuthInvalid;
    }

    clear_block(record);
    record.local_backoff_level = 0;
    record.routing_state = if record.refresh_in_flight {
        RoutingState::Warming
    } else if record.access_token.is_some() {
        RoutingState::Ready
    } else {
        RoutingState::Cold
    };
    WakeDisposition::Woken
}

fn compare_block_priority(
    left: (BlockedReason, Option<DateTime<Utc>>),
    right: (BlockedReason, Option<DateTime<Utc>>),
) -> std::cmp::Ordering {
    match (left.1, right.1) {
        (Some(left_until), Some(right_until)) => left_until
            .cmp(&right_until)
            .then_with(|| blocked_reason_rank(left.0).cmp(&blocked_reason_rank(right.0))),
        (Some(_), None) => std::cmp::Ordering::Less,
        (None, Some(_)) => std::cmp::Ordering::Greater,
        (None, None) => blocked_reason_rank(left.0).cmp(&blocked_reason_rank(right.0)),
    }
}

fn blocked_reason_rank(reason: BlockedReason) -> u8 {
    match reason {
        BlockedReason::QuotaExhausted => 0,
        BlockedReason::RateLimited => 1,
        BlockedReason::RiskControlled => 2,
        BlockedReason::TemporarilyUnavailable => 3,
        BlockedReason::AuthInvalid => 4,
    }
}

fn has_active_block_at(record: &AccountRecord, now: DateTime<Utc>) -> bool {
    match record.blocked_until {
        Some(until) => until > now,
        None => record.blocked_reason.is_some(),
    }
}

fn apply_retryable_block(
    record: &mut AccountRecord,
    now: DateTime<Utc>,
    reason: BlockedReason,
    retry_after: Option<StdDuration>,
) {
    let candidate = build_retryable_block(record, now, reason, retry_after);
    apply_block_transition(record, now, BlockTransition::Timed(candidate));
}

fn apply_fixed_block(
    record: &mut AccountRecord,
    now: DateTime<Utc>,
    reason: BlockedReason,
    duration: Duration,
) {
    let candidate = RetryableBlock {
        reason,
        source: BlockedSource::FixedPolicy,
        until: now + duration,
    };
    apply_block_transition(record, now, BlockTransition::Timed(candidate));
}

fn apply_block_transition(
    record: &mut AccountRecord,
    now: DateTime<Utc>,
    transition: BlockTransition,
) {
    // All block-field mutations must flow through this function so that
    // timed/permanent block precedence stays consistent across failure paths.
    match transition {
        BlockTransition::NoChange => {}
        BlockTransition::Clear => {
            record.blocked_reason = None;
            record.blocked_source = None;
            record.blocked_until = None;
        }
        BlockTransition::Permanent { reason, source } => {
            record.blocked_reason = Some(reason);
            record.blocked_source = Some(source);
            record.blocked_until = None;
        }
        BlockTransition::Timed(candidate) => {
            let should_replace_existing = match record.blocked_until {
                Some(existing_until) if existing_until > now => candidate.until >= existing_until,
                Some(_) => true,
                None => !has_active_block_at(record, now),
            };

            if should_replace_existing {
                record.blocked_reason = Some(candidate.reason);
                record.blocked_source = Some(candidate.source);
                record.blocked_until = Some(candidate.until);
            }
        }
    }
}

fn build_retryable_block(
    record: &mut AccountRecord,
    now: DateTime<Utc>,
    reason: BlockedReason,
    retry_after: Option<StdDuration>,
) -> RetryableBlock {
    if let Some(retry_after) = retry_after {
        return RetryableBlock {
            reason,
            source: BlockedSource::UpstreamRetryAfter,
            until: now + duration_from_std(retry_after),
        };
    }

    let cooldown = next_local_backoff(record.local_backoff_level);
    record.local_backoff_level = record.local_backoff_level.saturating_add(1);
    RetryableBlock {
        reason,
        source: BlockedSource::LocalBackoff,
        until: now + duration_from_std(cooldown),
    }
}

fn next_local_backoff(level: u32) -> StdDuration {
    let multiplier = 1u64.checked_shl(level).unwrap_or(u64::MAX);
    let seconds = LOCAL_BACKOFF_BASE_SECONDS
        .saturating_mul(multiplier)
        .min(LOCAL_BACKOFF_MAX_SECONDS);
    StdDuration::from_secs(seconds)
}

fn duration_from_std(duration: StdDuration) -> Duration {
    Duration::from_std(duration).unwrap_or_else(|_| Duration::seconds(i64::MAX))
}

fn account_file_from_record(record: &AccountRecord) -> AccountFile {
    AccountFile {
        refresh_token: record.refresh_token.clone(),
        label: record.label.clone(),
        email: record.email.clone(),
    }
}

fn merge_duplicate_account_files(base: &AccountFile, entries: &[DiskAccountEntry]) -> AccountFile {
    let mut merged = base.clone();

    if merged.label.is_none() {
        merged.label = entries.iter().find_map(|entry| entry.disk.label.clone());
    }
    if merged.email.is_none() {
        merged.email = entries.iter().find_map(|entry| entry.disk.email.clone());
    }

    merged
}

fn unique_trash_path(accounts_dir: &Path, original: &Path) -> PathBuf {
    let file_name = original
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or("account.json");
    let trash_dir = accounts_dir.join(TRASH_DIR_NAME);
    let stem = original
        .file_stem()
        .and_then(|value| value.to_str())
        .unwrap_or("account");

    let mut attempt = 0u64;
    loop {
        let candidate = if attempt == 0 {
            trash_dir.join(file_name)
        } else {
            let timestamp = SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap_or_default()
                .as_nanos();
            trash_dir.join(format!(
                "{stem}-{timestamp}-{attempt}.{ACCOUNT_FILE_EXTENSION}"
            ))
        };
        if !candidate.exists() {
            return candidate;
        }
        attempt = attempt.saturating_add(1);
    }
}

fn temp_account_path(path: &Path) -> PathBuf {
    let stamp = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_nanos();
    let file_name = path
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or("account.json");
    path.with_file_name(format!(".{file_name}.{stamp}.tmp"))
}

fn file_stem(path: &Path) -> Option<String> {
    path.file_stem()
        .and_then(|value| value.to_str())
        .map(ToString::to_string)
}

fn stable_account_id(refresh_token: &str) -> String {
    let digest = Sha256::digest(refresh_token.as_bytes());
    let mut output = String::with_capacity(32);
    for byte in digest.iter().take(16) {
        let _ = write!(&mut output, "{byte:02x}");
    }
    output
}

pub(crate) fn normalize_refresh_token(refresh_token: &str) -> anyhow::Result<String> {
    let trimmed = refresh_token.trim();
    if trimmed.is_empty() {
        bail!(INVALID_REFRESH_TOKEN_MESSAGE);
    }
    Ok(trimmed.to_string())
}

fn normalize_optional_metadata(value: Option<String>) -> Option<String> {
    value.and_then(|value| {
        let trimmed = value.trim();
        (!trimmed.is_empty()).then(|| trimmed.to_string())
    })
}

fn token_needs_refresh(record: &AccountRecord, refresh_skew_seconds: i64) -> bool {
    let Some(expires_at) = record.access_token_expires_at else {
        return true;
    };
    if record.access_token.is_none() {
        return true;
    }
    expires_at <= Utc::now() + Duration::seconds(refresh_skew_seconds)
}

#[cfg(test)]
mod tests;
