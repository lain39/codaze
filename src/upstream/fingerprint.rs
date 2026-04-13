use crate::accounts::UpstreamAccount;
use crate::config::FingerprintMode;
use http::{HeaderMap, HeaderValue};
use serde_json::{Map, Value, map::Entry};
use uuid::Uuid;

pub(crate) const X_CODEX_INSTALLATION_ID_HEADER: &str = "x-codex-installation-id";

// Stable namespace for deriving a Codex-like installation identifier from a ChatGPT account id.
const INSTALLATION_ID_NAMESPACE: Uuid = Uuid::from_u128(0x6d0a_b975_7f88_4ef4_9466_3f90_47d5_064d);

pub(super) fn installation_id_for_account(
    account: &UpstreamAccount,
    mode: FingerprintMode,
) -> Option<String> {
    if mode != FingerprintMode::Normalize {
        return None;
    }

    account
        .chatgpt_account_id
        .as_deref()
        .map(stable_installation_id)
}

pub(crate) fn stable_installation_id(account_id: &str) -> String {
    Uuid::new_v5(&INSTALLATION_ID_NAMESPACE, account_id.as_bytes()).to_string()
}

pub(super) fn apply_responses_installation_id(
    body: &mut Value,
    installation_id: Option<&str>,
    mode: FingerprintMode,
) {
    if mode != FingerprintMode::Normalize {
        return;
    }
    let Some(installation_id) = installation_id else {
        return;
    };
    let _ = apply_client_metadata_installation_id(body, installation_id);
}

pub(super) fn apply_compact_installation_id_header(
    headers: &mut HeaderMap,
    installation_id: Option<&str>,
    mode: FingerprintMode,
) {
    if mode != FingerprintMode::Normalize {
        return;
    }
    let Some(installation_id) = installation_id else {
        return;
    };
    if let Ok(value) = HeaderValue::from_str(installation_id) {
        headers.insert(X_CODEX_INSTALLATION_ID_HEADER, value);
    }
}

pub(crate) fn apply_client_metadata_installation_id(
    root: &mut Value,
    installation_id: &str,
) -> bool {
    let Some(object) = root.as_object_mut() else {
        return false;
    };

    let metadata = match object.entry("client_metadata".to_string()) {
        Entry::Vacant(entry) => entry.insert(Value::Object(Map::new())),
        Entry::Occupied(mut entry) => {
            if entry.get().is_null() {
                entry.insert(Value::Object(Map::new()));
            } else if !entry.get().is_object() {
                return false;
            }
            entry.into_mut()
        }
    };

    let Some(metadata) = metadata.as_object_mut() else {
        return false;
    };
    metadata.insert(
        X_CODEX_INSTALLATION_ID_HEADER.to_string(),
        Value::String(installation_id.to_string()),
    );
    true
}
