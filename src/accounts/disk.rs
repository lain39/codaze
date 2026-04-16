use super::*;
use anyhow::Context;
use tokio::sync::RwLock;

use std::fs::OpenOptions;

#[cfg(unix)]
use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};

#[cfg(windows)]
use std::os::windows::ffi::OsStrExt;

#[cfg(windows)]
use windows_sys::Win32::Storage::FileSystem::ReplaceFileW;

impl AccountStore {
    #[cfg(test)]
    pub fn sync_from_disk_startup(&mut self) -> anyhow::Result<()> {
        self.sync_from_disk_with_mode(SyncMode::Startup)
    }

    #[cfg(test)]
    pub fn sync_from_disk_rescan(&mut self) -> anyhow::Result<()> {
        self.sync_from_disk_with_mode(SyncMode::Rescan)
    }

    #[cfg(test)]
    fn sync_from_disk_with_mode(&mut self, mode: SyncMode) -> anyhow::Result<()> {
        let snapshot = scan_accounts_dir(&self.accounts_dir)?;
        let plan = build_disk_sync_plan(self, snapshot, mode)?;
        execute_disk_sync_fs(&plan)?;
        self.apply_disk_sync_plan(plan);
        Ok(())
    }

    #[cfg(test)]
    pub fn import_account(
        &mut self,
        refresh_token: String,
        label: Option<String>,
        email: Option<String>,
    ) -> anyhow::Result<ImportAccountResult> {
        let plan = self.prepare_import_account(refresh_token, label, email)?;
        if let Some(disk_op) = &plan.disk_op {
            execute_account_disk_op(disk_op)?;
        }
        self.apply_import_account_plan(plan)
    }

    pub(super) fn upsert_disk_record(&mut self, id: String, path: PathBuf, disk: AccountFile) {
        match self.records.get_mut(&id) {
            Some(record) => {
                record.file_path = path;
                record.label = disk.label;
                record.email = disk.email;
                if !record.auth_invalid_tombstone {
                    record.detached = false;
                }
                // Runtime refresh-token state is authoritative once an account is loaded.
                // Rescan only refreshes file-backed metadata; refresh-token rotation is
                // applied by refresh success and persisted back to disk from memory.
            }
            None => {
                self.records.insert(
                    id.clone(),
                    AccountRecord {
                        id,
                        file_path: path,
                        label: disk.label,
                        email: disk.email,
                        refresh_token: disk.refresh_token,
                        access_token: None,
                        account_id: None,
                        plan_type: None,
                        access_token_expires_at: None,
                        last_refresh_at: None,
                        routing_state: RoutingState::Cold,
                        blocked_reason: None,
                        blocked_source: None,
                        blocked_until: None,
                        local_backoff_level: 0,
                        refresh_in_flight: false,
                        in_flight_requests: 0,
                        last_selected_at: None,
                        last_success_at: None,
                        last_error_at: None,
                        last_error: None,
                        auth_invalid_tombstone: false,
                        detached: false,
                    },
                );
            }
        }
    }

    fn find_account_id_by_refresh_token(&self, refresh_token: &str) -> Option<String> {
        self.records
            .values()
            .find(|record| !record.detached && record.refresh_token == refresh_token)
            .map(|record| record.id.clone())
    }

    pub(crate) fn prepare_import_account(
        &self,
        refresh_token: String,
        label: Option<String>,
        email: Option<String>,
    ) -> anyhow::Result<ImportAccountPlan> {
        let refresh_token = normalize_refresh_token(&refresh_token)?;
        let label = normalize_optional_metadata(label);
        let email = normalize_optional_metadata(email);

        if let Some(existing_id) = self.find_account_id_by_refresh_token(&refresh_token) {
            return self.prepare_existing_account_metadata_update(&existing_id, label, email);
        }

        let id = stable_account_id(&refresh_token);
        let path = self
            .accounts_dir
            .join(format!("{id}.{ACCOUNT_FILE_EXTENSION}"));
        let disk = AccountFile {
            refresh_token,
            label,
            email,
        };
        Ok(ImportAccountPlan {
            account_id: id.clone(),
            already_exists: false,
            apply: ImportApply::Insert {
                id,
                path: path.clone(),
                disk: disk.clone(),
            },
            disk_op: Some(AccountDiskOp::Write { path, disk }),
        })
    }

    pub(crate) fn apply_import_account_plan(
        &mut self,
        plan: ImportAccountPlan,
    ) -> anyhow::Result<ImportAccountResult> {
        match plan.apply {
            ImportApply::None => {}
            ImportApply::Insert { id, path, disk } => {
                self.upsert_disk_record(id, path, disk);
            }
            ImportApply::UpdateMetadata {
                account_id,
                label,
                email,
            } => {
                let record = self
                    .records
                    .get_mut(&account_id)
                    .ok_or_else(|| anyhow!("unknown account {account_id}"))?;
                record.label = label;
                record.email = email;
            }
        }

        Ok(ImportAccountResult {
            account: self.view(&plan.account_id)?,
            already_exists: plan.already_exists,
        })
    }

    fn prepare_existing_account_metadata_update(
        &self,
        account_id: &str,
        label: Option<String>,
        email: Option<String>,
    ) -> anyhow::Result<ImportAccountPlan> {
        let record = self
            .records
            .get(account_id)
            .ok_or_else(|| anyhow!("unknown account {account_id}"))?;

        let mut next_label = record.label.clone();
        let mut next_email = record.email.clone();
        let mut changed = false;

        if let Some(label) = label
            && next_label.as_ref() != Some(&label)
        {
            next_label = Some(label);
            changed = true;
        }
        if let Some(email) = email
            && next_email.as_ref() != Some(&email)
        {
            next_email = Some(email);
            changed = true;
        }

        let disk_op = if changed && !record.detached {
            Some(AccountDiskOp::Write {
                path: record.file_path.clone(),
                disk: AccountFile {
                    refresh_token: record.refresh_token.clone(),
                    label: next_label.clone(),
                    email: next_email.clone(),
                },
            })
        } else {
            None
        };

        Ok(ImportAccountPlan {
            account_id: account_id.to_string(),
            already_exists: true,
            apply: if changed {
                ImportApply::UpdateMetadata {
                    account_id: account_id.to_string(),
                    label: next_label,
                    email: next_email,
                }
            } else {
                ImportApply::None
            },
            disk_op,
        })
    }

    pub(crate) fn prepare_remove_account(
        &self,
        id: &str,
    ) -> anyhow::Result<Option<RemoveAccountPlan>> {
        let Some(path) = self.records.get(id).map(|record| record.file_path.clone()) else {
            return Ok(None);
        };
        Ok(Some(RemoveAccountPlan {
            account_id: id.to_string(),
            disk_op: AccountDiskOp::Remove { path },
        }))
    }

    pub(crate) fn apply_remove_account_plan(&mut self, plan: RemoveAccountPlan) {
        if let Some(record) = self.records.get_mut(&plan.account_id) {
            record.detached = true;
        }
        self.maybe_remove_detached(&plan.account_id);
    }

    fn select_canonical_target(
        &self,
        group: &[DiskAccountEntry],
        mode: SyncMode,
    ) -> Option<CanonicalTarget> {
        let refresh_token = group.first()?.disk.refresh_token.clone();

        if mode == SyncMode::Rescan
            && let Some(record) = self.preferred_existing_record(&refresh_token)
        {
            let matched_group_entry = group.iter().find(|entry| entry.id == record.id);
            return Some(CanonicalTarget {
                id: record.id.clone(),
                path: record.file_path.clone(),
                disk: matched_group_entry
                    .map(|entry| entry.disk.clone())
                    .unwrap_or(AccountFile {
                        refresh_token,
                        label: record.label.clone(),
                        email: record.email.clone(),
                    }),
                matched_group_path: matched_group_entry.map(|entry| entry.path.clone()),
            });
        }

        let canonical = group.first()?.clone();
        Some(CanonicalTarget {
            id: canonical.id,
            path: canonical.path.clone(),
            disk: canonical.disk,
            matched_group_path: Some(canonical.path),
        })
    }

    fn preferred_existing_record(&self, refresh_token: &str) -> Option<&AccountRecord> {
        self.records
            .values()
            .filter(|record| !record.detached && record.refresh_token == refresh_token)
            .max_by(|left, right| {
                left.refresh_in_flight
                    .cmp(&right.refresh_in_flight)
                    .then_with(|| left.in_flight_requests.cmp(&right.in_flight_requests))
                    .then_with(|| {
                        left.access_token
                            .is_some()
                            .cmp(&right.access_token.is_some())
                    })
                    .then_with(|| left.last_selected_at.cmp(&right.last_selected_at))
                    .then_with(|| right.id.cmp(&left.id))
            })
    }
}

pub(crate) async fn startup_sync_accounts_from_disk(
    accounts_dir: &Path,
    accounts: &RwLock<AccountStore>,
) -> anyhow::Result<()> {
    sync_accounts_from_disk_async(accounts_dir, accounts, SyncMode::Startup).await
}

pub(crate) async fn rescan_accounts_from_disk(
    accounts_dir: &Path,
    accounts: &RwLock<AccountStore>,
) -> anyhow::Result<()> {
    sync_accounts_from_disk_async(accounts_dir, accounts, SyncMode::Rescan).await
}

async fn sync_accounts_from_disk_async(
    accounts_dir: &Path,
    accounts: &RwLock<AccountStore>,
    mode: SyncMode,
) -> anyhow::Result<()> {
    let accounts_dir = accounts_dir.to_path_buf();
    let snapshot = tokio::task::spawn_blocking({
        let accounts_dir = accounts_dir.clone();
        move || scan_accounts_dir(&accounts_dir)
    })
    .await
    .context("join disk scan task")??;

    let plan = {
        let accounts = accounts.read().await;
        build_disk_sync_plan(&accounts, snapshot, mode)?
    };

    let fs_plan = plan.clone();
    tokio::task::spawn_blocking(move || execute_disk_sync_fs(&fs_plan))
        .await
        .context("join disk sync fs task")??;

    accounts.write().await.apply_disk_sync_plan(plan);
    Ok(())
}

fn scan_accounts_dir(accounts_dir: &Path) -> anyhow::Result<DiskScanSnapshot> {
    ensure_accounts_directories(accounts_dir)?;

    let mut entries = Vec::new();
    let mut failed_file_ids = HashSet::new();
    let mut observed_paths = HashSet::new();
    for entry in fs::read_dir(accounts_dir).context("read accounts dir")? {
        let entry = entry?;
        let path = entry.path();
        if !path.is_file()
            || path.extension().and_then(|ext| ext.to_str()) != Some(ACCOUNT_FILE_EXTENSION)
        {
            continue;
        }
        observed_paths.insert(path.clone());

        let Some(id) = file_stem(&path) else {
            continue;
        };
        let bytes = match fs::read(&path) {
            Ok(bytes) => bytes,
            Err(error) => {
                failed_file_ids.insert(id.clone());
                warn!(account_id = %id, %error, "failed to read account file");
                continue;
            }
        };
        let disk = match serde_json::from_slice::<AccountFile>(&bytes) {
            Ok(disk) => disk,
            Err(error) => {
                failed_file_ids.insert(id.clone());
                warn!(account_id = %id, %error, "failed to parse account file");
                continue;
            }
        };
        let refresh_token = match normalize_refresh_token(&disk.refresh_token) {
            Ok(refresh_token) => refresh_token,
            Err(error) => {
                failed_file_ids.insert(id.clone());
                warn!(account_id = %id, %error, "skipping invalid account file");
                continue;
            }
        };
        let disk = AccountFile {
            refresh_token,
            label: disk.label,
            email: disk.email,
        };
        entries.push(DiskAccountEntry { id, path, disk });
    }

    Ok(DiskScanSnapshot {
        entries,
        failed_file_ids,
        observed_paths,
    })
}

fn build_disk_sync_plan(
    store: &AccountStore,
    snapshot: DiskScanSnapshot,
    mode: SyncMode,
) -> anyhow::Result<DiskSyncPlan> {
    let observed_paths = snapshot.observed_paths.clone();
    let mut entries_by_refresh_token: HashMap<String, Vec<DiskAccountEntry>> = HashMap::new();
    for entry in snapshot.entries {
        entries_by_refresh_token
            .entry(entry.disk.refresh_token.clone())
            .or_default()
            .push(entry);
    }

    let mut writes = Vec::new();
    let mut removals = Vec::new();
    let mut upserts = Vec::new();
    let mut seen_ids = HashSet::new();

    for mut group in entries_by_refresh_token.into_values() {
        group.sort_by(|left, right| left.id.cmp(&right.id));
        let canonical = store
            .select_canonical_target(&group, mode)
            .context("duplicate refresh token group unexpectedly empty")?;
        let merged = merge_duplicate_account_files(&canonical.disk, &group);

        if merged != canonical.disk || !observed_paths.contains(&canonical.path) {
            writes.push((canonical.path.clone(), merged.clone()));
        }

        for duplicate in &group {
            if canonical
                .matched_group_path
                .as_ref()
                .is_some_and(|path| path == &duplicate.path)
            {
                continue;
            }
            removals.push(duplicate.path.clone());
        }

        if group.len() > 1 {
            warn!(
                canonical_id = %canonical.id,
                duplicate_count = group.len() - 1,
                "merged duplicate refresh token account files"
            );
        }

        seen_ids.insert(canonical.id.clone());
        upserts.push(DiskSyncUpsert {
            id: canonical.id,
            path: canonical.path,
            disk: merged,
        });
    }

    Ok(DiskSyncPlan {
        mode,
        writes,
        removals,
        upserts,
        seen_ids,
        failed_file_ids: snapshot.failed_file_ids,
    })
}

fn execute_disk_sync_fs(plan: &DiskSyncPlan) -> anyhow::Result<()> {
    for (path, disk) in &plan.writes {
        write_account_file(path, disk)?;
    }

    for path in &plan.removals {
        if !path.exists() {
            continue;
        }
        fs::remove_file(path).context("remove duplicate refresh token account file")?;
    }

    Ok(())
}

pub(crate) fn execute_account_disk_op(op: &AccountDiskOp) -> anyhow::Result<()> {
    match op {
        AccountDiskOp::Write { path, disk } => write_account_file(path, disk),
        AccountDiskOp::Remove { path } => {
            if !path.exists() {
                return Ok(());
            }
            fs::remove_file(path).context("remove account file")?;
            Ok(())
        }
        AccountDiskOp::MoveToTrash {
            source,
            accounts_dir,
        } => {
            ensure_accounts_directories(accounts_dir)?;
            if !source.exists() {
                return Ok(());
            }
            let target = unique_trash_path(accounts_dir, source);
            fs::rename(source, &target).context("move invalid account file to trash")?;
            set_private_file_permissions(&target)
                .context("tighten trashed account file permissions")?;
            Ok(())
        }
    }
}

impl AccountStore {
    fn apply_disk_sync_plan(&mut self, plan: DiskSyncPlan) {
        for upsert in plan.upserts {
            self.upsert_disk_record(upsert.id, upsert.path, upsert.disk);
        }

        let existing_ids = self.records.keys().cloned().collect::<Vec<_>>();
        for id in existing_ids {
            if plan.seen_ids.contains(&id) {
                continue;
            }
            if plan.mode == SyncMode::Rescan && plan.failed_file_ids.contains(&id) {
                continue;
            }
            let removable = self
                .records
                .get(&id)
                .is_some_and(|record| record.in_flight_requests == 0 && !record.refresh_in_flight);
            if removable {
                self.records.remove(&id);
            } else if let Some(record) = self.records.get_mut(&id) {
                record.detached = true;
            }
        }
    }
}

pub(crate) fn write_account_file(path: &Path, disk: &AccountFile) -> anyhow::Result<()> {
    let parent = path
        .parent()
        .ok_or_else(|| anyhow!("account file has no parent directory"))?;
    ensure_private_dir(parent).context("create account parent dir")?;

    let bytes = serde_json::to_vec_pretty(disk).context("serialize account file")?;
    let temp_path = temp_account_path(path);
    write_private_file(&temp_path, &bytes).context("write temp account file")?;
    replace_account_file(&temp_path, path).context("atomically replace account file")?;
    Ok(())
}

#[cfg(not(windows))]
fn replace_account_file(temp_path: &Path, path: &Path) -> anyhow::Result<()> {
    fs::rename(temp_path, path)?;
    Ok(())
}

#[cfg(windows)]
fn replace_account_file(temp_path: &Path, path: &Path) -> anyhow::Result<()> {
    if !path.exists() {
        fs::rename(temp_path, path)?;
        return Ok(());
    }

    let replaced = encode_wide_null(path);
    let replacement = encode_wide_null(temp_path);
    let success = unsafe {
        ReplaceFileW(
            replaced.as_ptr(),
            replacement.as_ptr(),
            std::ptr::null(),
            0,
            std::ptr::null_mut(),
            std::ptr::null_mut(),
        )
    };
    if success == 0 {
        return Err(std::io::Error::last_os_error().into());
    }

    Ok(())
}

#[cfg(windows)]
fn encode_wide_null(path: &Path) -> Vec<u16> {
    path.as_os_str().encode_wide().chain(Some(0)).collect()
}

pub(super) fn ensure_accounts_directories(accounts_dir: &Path) -> anyhow::Result<()> {
    ensure_private_dir(accounts_dir).context("create accounts dir")?;
    ensure_private_dir(&accounts_dir.join(TRASH_DIR_NAME)).context("create accounts trash dir")?;
    Ok(())
}

fn ensure_private_dir(path: &Path) -> anyhow::Result<()> {
    fs::create_dir_all(path)?;
    set_private_dir_permissions(path)?;
    Ok(())
}

fn write_private_file(path: &Path, bytes: &[u8]) -> anyhow::Result<()> {
    if path.exists() {
        fs::remove_file(path)?;
    }

    let mut file = new_private_file(path)?;
    use std::io::Write as _;
    file.write_all(bytes)?;
    file.sync_all()?;
    Ok(())
}

#[cfg(unix)]
fn new_private_file(path: &Path) -> anyhow::Result<std::fs::File> {
    Ok(OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .open(path)?)
}

#[cfg(not(unix))]
fn new_private_file(path: &Path) -> anyhow::Result<std::fs::File> {
    Ok(OpenOptions::new().write(true).create_new(true).open(path)?)
}

#[cfg(unix)]
fn set_private_file_permissions(path: &Path) -> anyhow::Result<()> {
    fs::set_permissions(path, fs::Permissions::from_mode(0o600))?;
    Ok(())
}

#[cfg(not(unix))]
fn set_private_file_permissions(_path: &Path) -> anyhow::Result<()> {
    Ok(())
}

#[cfg(unix)]
fn set_private_dir_permissions(path: &Path) -> anyhow::Result<()> {
    fs::set_permissions(path, fs::Permissions::from_mode(0o700))?;
    Ok(())
}

#[cfg(not(unix))]
fn set_private_dir_permissions(_path: &Path) -> anyhow::Result<()> {
    Ok(())
}
