//! Managed account secrets live in the OS credential store. JSON files contain
//! immutable references; losing access to the store is an explicit error.

use base64::{engine::general_purpose::STANDARD, Engine as _};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use std::collections::{HashMap, VecDeque};
use std::path::Path;
use std::sync::{LazyLock, Mutex};

#[cfg(any(target_os = "macos", target_os = "linux"))]
mod unix;
#[cfg(target_os = "windows")]
mod windows;

fn client_identity(service: &str, account: &str) -> Result<String, String> {
    if [service, account].iter().any(|value| {
        value.is_empty()
            || value.len() > 128
            || !value
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || b"-_.".contains(&b))
    }) {
        return Err("Invalid client credential-store identity".into());
    }
    Ok(format!("client/{service}/{account}"))
}

/// External clients require their documented fixed OS-store identity rather
/// than a manager reference. Never mirrors this payload into a home file.
pub(crate) fn store_client_secret(
    service: &str,
    account: &str,
    secret: &[u8],
) -> Result<(), String> {
    let identity = client_identity(service, account)?;
    if secret.is_empty() || secret.len() > 65536 {
        return Err("Invalid client credential payload size".into());
    }
    #[cfg(test)]
    {
        TEST_SECRETS
            .lock()
            .unwrap()
            .insert(identity, secret.to_vec());
    }
    #[cfg(all(not(test), target_os = "windows"))]
    {
        let _ = identity;
        windows::set_named_secret(&format!("{service}:{account}"), account, secret)?;
    }
    #[cfg(all(not(test), any(target_os = "macos", target_os = "linux")))]
    {
        let _ = identity;
        unix::set_named_secret(service, account, secret)?;
    }
    #[cfg(all(
        not(test),
        not(any(target_os = "windows", target_os = "macos", target_os = "linux"))
    ))]
    {
        let _ = identity;
        return Err("Native client credential storage is unsupported on this platform".into());
    }
    if read_client_secret(service, account)? != secret {
        return Err("Client credential-store verification failed".into());
    }
    Ok(())
}

pub(crate) fn read_client_secret(service: &str, account: &str) -> Result<Vec<u8>, String> {
    let identity = client_identity(service, account)?;
    #[cfg(test)]
    {
        TEST_SECRETS
            .lock()
            .unwrap()
            .get(&identity)
            .cloned()
            .ok_or("Client credential-store entry is unavailable".into())
    }
    #[cfg(all(not(test), target_os = "windows"))]
    {
        let _ = identity;
        windows::read_named_secret(&format!("{service}:{account}"))
    }
    #[cfg(all(not(test), any(target_os = "macos", target_os = "linux")))]
    {
        let _ = identity;
        unix::get_named_secret(service, account)
    }
    #[cfg(all(
        not(test),
        not(any(target_os = "windows", target_os = "macos", target_os = "linux"))
    ))]
    {
        let _ = identity;
        Err("Native client credential storage is unsupported on this platform".into())
    }
}

const CHUNK_BYTES: usize = 2000;
// Credential payloads, not prompts or account quota catalogs. Bound malformed
// references before issuing potentially unbounded OS-store requests.
const MAX_PAYLOAD_BYTES: usize = 256 * 1024;
const MAX_PARTS: usize = (MAX_PAYLOAD_BYTES * 4 / 3 + 4).div_ceil(CHUNK_BYTES);
const ACCOUNT_SCOPE: &str = "account";

#[derive(Clone, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct VaultRef {
    version: u32,
    generation: String,
    parts: usize,
    digest: String,
}

type CacheKey = (String, String, VaultRef);
// Immutable generation references make warm reads independent of native-store
// process launch costs. Bound retained plaintext in process memory explicitly.
const CACHE_ENTRIES: usize = 128;
const CACHE_BYTES: usize = 4 * 1024 * 1024;
#[derive(Default)]
struct Cache {
    entries: HashMap<CacheKey, (Value, usize)>,
    order: VecDeque<CacheKey>,
    bytes: usize,
    epoch: u64,
}
impl Cache {
    fn remove(&mut self, key: &CacheKey) {
        if let Some((_, bytes)) = self.entries.remove(key) {
            self.bytes -= bytes;
        }
        self.order.retain(|item| item != key);
    }
    fn insert(&mut self, key: CacheKey, value: Value) {
        let bytes = serde_json::to_vec(&value)
            .map(|value| value.len())
            .unwrap_or(CACHE_BYTES + 1);
        self.remove(&key);
        if bytes > CACHE_BYTES {
            return;
        }
        while self.entries.len() >= CACHE_ENTRIES || self.bytes + bytes > CACHE_BYTES {
            let Some(old) = self.order.front().cloned() else {
                break;
            };
            self.remove(&old);
        }
        self.bytes += bytes;
        self.entries.insert(key.clone(), (value, bytes));
        self.order.push_back(key);
    }
    fn invalidate(&mut self, reference: Option<&VaultRef>) {
        self.epoch = self.epoch.wrapping_add(1);
        let removed: Vec<_> = self
            .entries
            .keys()
            .filter(|key| {
                reference.is_none_or(|reference| key.2.generation == reference.generation)
            })
            .cloned()
            .collect();
        for key in removed {
            self.remove(&key);
        }
    }
}
static CACHE: LazyLock<Mutex<Cache>> = LazyLock::new(|| Mutex::new(Cache::default()));

trait SecretStore {
    fn set(&self, key: &str, value: &[u8]) -> Result<(), String>;
    fn get(&self, key: &str) -> Result<Vec<u8>, String>;
    fn delete(&self, key: &str) -> Result<(), String>;
}

struct NativeStore;

#[cfg(test)]
static TEST_SECRETS: std::sync::LazyLock<
    std::sync::Mutex<std::collections::HashMap<String, Vec<u8>>>,
> = std::sync::LazyLock::new(|| std::sync::Mutex::new(std::collections::HashMap::new()));

impl SecretStore for NativeStore {
    fn set(&self, key: &str, value: &[u8]) -> Result<(), String> {
        #[cfg(test)]
        {
            TEST_SECRETS
                .lock()
                .unwrap()
                .insert(key.into(), value.into());
            Ok(())
        }
        #[cfg(all(not(test), target_os = "windows"))]
        {
            windows::set_secret(key, value)
        }
        #[cfg(all(not(test), any(target_os = "macos", target_os = "linux")))]
        {
            unix::set_secret(key, value)
        }
        #[cfg(all(
            not(test),
            not(any(target_os = "windows", target_os = "macos", target_os = "linux"))
        ))]
        {
            let _ = (key, value);
            Err("OS credential storage is unsupported on this platform".into())
        }
    }
    fn get(&self, key: &str) -> Result<Vec<u8>, String> {
        #[cfg(test)]
        {
            TEST_SECRETS
                .lock()
                .unwrap()
                .get(key)
                .cloned()
                .ok_or_else(|| "Credential-store entry is unavailable".into())
        }
        #[cfg(all(not(test), target_os = "windows"))]
        {
            windows::get_secret(key)
        }
        #[cfg(all(not(test), any(target_os = "macos", target_os = "linux")))]
        {
            unix::get_secret(key)
        }
        #[cfg(all(
            not(test),
            not(any(target_os = "windows", target_os = "macos", target_os = "linux"))
        ))]
        {
            let _ = key;
            Err("OS credential storage is unsupported on this platform".into())
        }
    }
    fn delete(&self, key: &str) -> Result<(), String> {
        #[cfg(test)]
        {
            TEST_SECRETS.lock().unwrap().remove(key);
            Ok(())
        }
        #[cfg(all(not(test), target_os = "windows"))]
        {
            windows::delete_secret(key)
        }
        #[cfg(all(not(test), any(target_os = "macos", target_os = "linux")))]
        {
            unix::delete_secret(key)
        }
        #[cfg(all(
            not(test),
            not(any(target_os = "windows", target_os = "macos", target_os = "linux"))
        ))]
        {
            let _ = key;
            Err("OS credential storage is unsupported on this platform".into())
        }
    }
}

fn validate_reference(scope: &str, reference: &VaultRef) -> Result<(), String> {
    if scope.is_empty()
        || scope.len() > 32
        || !scope
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'_')
        || reference.version != 1
        || uuid::Uuid::parse_str(&reference.generation).is_err()
        || reference.parts == 0
        || reference.parts > MAX_PARTS
        || reference.digest.len() != 64
        || !reference.digest.bytes().all(|b| b.is_ascii_hexdigit())
    {
        return Err("Invalid or unsupported credential-store reference".into());
    }
    Ok(())
}

fn key(scope: &str, reference: &VaultRef, part: usize) -> String {
    format!("v1/{}/{}/{}", scope, reference.generation, part)
}

/// Write and verify an immutable generation before publishing its reference.
pub fn store_json(scope: &str, owner: &str, value: &Value) -> Result<VaultRef, String> {
    match store_with(&NativeStore, scope, owner, value) {
        Ok(reference) => {
            CACHE.lock().unwrap_or_else(|e| e.into_inner()).insert(
                (scope.into(), owner.into(), reference.clone()),
                value.clone(),
            );
            Ok(reference)
        }
        Err(error) => {
            CACHE
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .invalidate(None);
            Err(error)
        }
    }
}

fn store_with(
    store: &impl SecretStore,
    scope: &str,
    owner: &str,
    value: &Value,
) -> Result<VaultRef, String> {
    let payload = serde_json::to_vec(&json!({"scope":scope,"owner":owner,"value":value}))
        .map_err(|_| "Failed to encode credential-store payload")?;
    if payload.len() > MAX_PAYLOAD_BYTES {
        return Err("Credential-store payload is too large".into());
    }
    let encoded = STANDARD.encode(&payload);
    let reference = VaultRef {
        version: 1,
        generation: uuid::Uuid::new_v4().to_string(),
        parts: encoded.len().div_ceil(CHUNK_BYTES),
        digest: format!("{:x}", Sha256::digest(&payload)),
    };
    validate_reference(scope, &reference)?;
    let written: Result<(), String> = (|| {
        for (part, bytes) in encoded.as_bytes().chunks(CHUNK_BYTES).enumerate() {
            store.set(&key(scope, &reference, part), bytes)?;
        }
        let recovered = load_with(store, scope, owner, &reference)?;
        if recovered != *value {
            return Err("Credential-store verification failed".into());
        }
        Ok(())
    })();
    if let Err(error) = written {
        let _ = delete_with(store, scope, &reference);
        return Err(error);
    }
    Ok(reference)
}

pub fn load_json(scope: &str, owner: &str, reference: &VaultRef) -> Result<Value, String> {
    validate_reference(scope, reference)?;
    let key = (scope.into(), owner.into(), reference.clone());
    let epoch = {
        let cache = CACHE.lock().unwrap_or_else(|e| e.into_inner());
        if let Some((value, _)) = cache.entries.get(&key) {
            return Ok(value.clone());
        }
        cache.epoch
    };
    let value = load_with(&NativeStore, scope, owner, reference)?;
    let mut cache = CACHE.lock().unwrap_or_else(|e| e.into_inner());
    // A concurrent deletion must not be undone by this read completing later.
    if cache.epoch == epoch {
        cache.insert(key, value.clone());
    }
    Ok(value)
}

fn load_with(
    store: &impl SecretStore,
    scope: &str,
    owner: &str,
    reference: &VaultRef,
) -> Result<Value, String> {
    validate_reference(scope, reference)?;
    let mut encoded = Vec::new();
    for part in 0..reference.parts {
        let chunk = store.get(&key(scope, reference, part))?;
        if chunk.is_empty() || chunk.len() > CHUNK_BYTES {
            return Err("Invalid credential-store chunk".into());
        }
        encoded.extend_from_slice(&chunk);
    }
    let payload = STANDARD
        .decode(encoded)
        .map_err(|_| "Invalid credential-store encoding")?;
    if payload.len() > MAX_PAYLOAD_BYTES
        || format!("{:x}", Sha256::digest(&payload)) != reference.digest
    {
        return Err("Credential-store payload integrity check failed".into());
    }
    let document: Value =
        serde_json::from_slice(&payload).map_err(|_| "Invalid credential-store payload")?;
    if document.get("scope").and_then(Value::as_str) != Some(scope)
        || document.get("owner").and_then(Value::as_str) != Some(owner)
    {
        return Err("Credential-store identity does not match this document".into());
    }
    document
        .get("value")
        .cloned()
        .ok_or_else(|| "Credential-store payload is incomplete".into())
}

pub fn delete(scope: &str, reference: &VaultRef) -> Result<(), String> {
    CACHE
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .invalidate(Some(reference));
    let result = delete_with(&NativeStore, scope, reference);
    CACHE
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .invalidate(Some(reference));
    result
}

fn delete_with(store: &impl SecretStore, scope: &str, reference: &VaultRef) -> Result<(), String> {
    validate_reference(scope, reference)?;
    for part in 0..reference.parts {
        // Do not multiply an unavailable store's timeout by every chunk.
        store.delete(&key(scope, reference, part))?;
    }
    Ok(())
}

fn account_id(document: &Value) -> Result<&str, String> {
    let id = document
        .get("id")
        .and_then(Value::as_str)
        .ok_or("Missing managed account identity")?;
    crate::utils::db_import_path::validate_account_id(id)?;
    Ok(id)
}

fn reference(document: &Value) -> Result<Option<VaultRef>, String> {
    document
        .get("credential_store")
        .map(|value| {
            serde_json::from_value(value.clone())
                .map_err(|_| "Invalid managed account credential-store reference".into())
        })
        .transpose()
}

const CREDENTIAL_FIELDS: [&str; 8] = [
    "access_token",
    "refresh_token",
    "id_token",
    "expires_in",
    "expiry_timestamp",
    "oauth_client_key",
    "token_type",
    "is_gcp_tos",
];

/// An observed credential version, including public OAuth metadata. Kept only
/// in memory: neither JSON clients nor imports can invent trusted provenance.
#[derive(Clone, Debug)]
pub(crate) struct CredentialRevision {
    owner: String,
    persisted_digest: String,
    hydrated_digest: String,
}

fn credential_values(document: &Value) -> Value {
    let mut values = serde_json::Map::new();
    for field in CREDENTIAL_FIELDS {
        values.insert(
            field.into(),
            document["token"].get(field).cloned().unwrap_or(Value::Null),
        );
    }
    Value::Object(values)
}

fn value_digest(value: &Value) -> Result<String, String> {
    let encoded = serde_json::to_vec(value).map_err(|_| "Failed to encode credential revision")?;
    Ok(format!("{:x}", Sha256::digest(encoded)))
}

fn credential_state(document: &Value) -> Result<Value, String> {
    let protected = reference(document)?;
    let mut values = credential_values(document);
    if protected.is_some() {
        // Vault generations identify the secret values without fetching them.
        for field in ["access_token", "refresh_token", "id_token"] {
            values.as_object_mut().unwrap().remove(field);
        }
    }
    Ok(json!({"reference":protected,"values":values}))
}

fn persisted_credential_digest(document: &Value) -> Result<String, String> {
    value_digest(&json!({
        "state": credential_state(document)?,
        "version": document.get("credential_version"),
    }))
}

pub(crate) fn capture_credential_revision(document: &Value) -> Result<CredentialRevision, String> {
    Ok(CredentialRevision {
        owner: account_id(document)?.to_string(),
        persisted_digest: persisted_credential_digest(document)?,
        hydrated_digest: value_digest(&credential_values(document))?,
    })
}

fn copy_current_credentials(latest: &Value, document: &mut Value) -> Result<(), String> {
    let latest_token = latest
        .get("token")
        .and_then(Value::as_object)
        .ok_or("Missing current managed token metadata")?;
    if let Some(reference) = reference(latest)? {
        validate_reference(ACCOUNT_SCOPE, &reference)?;
        document["credential_store"] =
            serde_json::to_value(reference).map_err(|_| "Failed to encode credential reference")?;
    } else {
        document
            .as_object_mut()
            .ok_or("Invalid account metadata")?
            .remove("credential_store");
    }
    if let Some(version) = latest.get("credential_version") {
        document["credential_version"] = version.clone();
    } else {
        document
            .as_object_mut()
            .unwrap()
            .remove("credential_version");
    }
    let token = document
        .get_mut("token")
        .and_then(Value::as_object_mut)
        .ok_or("Missing managed token metadata")?;
    for field in CREDENTIAL_FIELDS {
        if let Some(value) = latest_token.get(field) {
            token.insert(field.into(), value.clone());
        } else {
            token.remove(field);
        }
    }
    Ok(())
}

/// Legacy plaintext accounts remain readable until the compatible binary saves
/// or explicitly migrates them. A vault-backed account never falls back to JSON.
pub(crate) fn hydrate_account_document(document: &mut Value) -> Result<(), String> {
    let Some(reference) = reference(document)? else {
        return Ok(());
    };
    let id = account_id(document)?;
    let credentials = load_json(ACCOUNT_SCOPE, id, &reference)?;
    let token = document
        .get_mut("token")
        .and_then(Value::as_object_mut)
        .ok_or("Missing managed token metadata")?;
    for field in ["access_token", "refresh_token", "id_token"] {
        let value = credentials
            .get(field)
            .ok_or("Credential-store account payload is incomplete")?;
        if field != "id_token" && !value.is_string() {
            return Err("Invalid credential-store token payload".into());
        }
        if field == "id_token" && !(value.is_null() || value.is_string()) {
            return Err("Invalid credential-store identity token".into());
        }
        token.insert(field.into(), value.clone());
    }
    Ok(())
}

/// Persist a full hydrated account or a metadata-only update of an existing
/// vault-backed document. Secrets are verified in the OS store before the file
/// is replaced; failed replacement leaves the old generation usable.
pub(crate) fn persist_account_document(path: &Path, document: Value) -> Result<(), String> {
    persist_account_document_with_recovery(path, document, None, false)
}

pub(crate) fn persist_account_document_after_verified_recovery(
    path: &Path,
    document: Value,
    expected: Option<&CredentialRevision>,
) -> Result<(), String> {
    persist_account_document_with_recovery(path, document, expected, true)
}

pub(crate) fn persist_typed_account_document(
    path: &Path,
    document: Value,
    expected: Option<&CredentialRevision>,
) -> Result<(), String> {
    persist_account_document_with_recovery(path, document, expected, false)
}

fn read_account_document(path: &Path) -> Result<Value, String> {
    let raw = std::fs::read(path).map_err(|e| format!("Failed to read account metadata: {e}"))?;
    let mut parser = serde_json::Deserializer::from_slice(&raw);
    Value::deserialize(&mut parser).map_err(|_| "Invalid account metadata".into())
}

pub(crate) fn hydrate_current_account_document(
    path: &Path,
    document: &mut Value,
) -> Result<(), String> {
    let id = account_id(document)?.to_string();
    crate::modules::account::with_account_file_write(&id, || {
        let current = read_account_document(path)?;
        if account_id(&current)? != id
            || path.file_stem().and_then(|value| value.to_str()) != Some(id.as_str())
        {
            return Err("Account credential identity changed during loading".into());
        }
        copy_current_credentials(&current, document)?;
        hydrate_account_document(document)
    })
}

fn check_expected_revision(document: &Value, expected: &CredentialRevision) -> Result<(), String> {
    if account_id(document)? != expected.owner
        || persisted_credential_digest(document)? != expected.persisted_digest
    {
        return Err(
            "Account credentials changed concurrently; reload before replacing them".into(),
        );
    }
    Ok(())
}

pub(crate) fn with_credential_revision<T>(
    path: &Path,
    expected: &CredentialRevision,
    apply: impl FnOnce() -> Result<T, String>,
) -> Result<T, String> {
    crate::modules::account::with_account_file_write(&expected.owner, || {
        check_expected_revision(&read_account_document(path)?, expected)?;
        apply()
    })
}

/// Patch only credential-bound fields of the latest account. OAuth callers
/// retain their single-flight lock until this verified publication completes.
pub(crate) fn persist_credential_update(
    path: &Path,
    token: &crate::models::TokenData,
) -> Result<crate::models::TokenData, String> {
    let expected = token
        .credential_revision
        .as_ref()
        .ok_or("Missing observed credential revision for token refresh")?;
    crate::modules::account::with_account_file_write(&expected.owner, || {
        let mut document = read_account_document(path)?;
        check_expected_revision(&document, expected)?;
        if path.file_stem().and_then(|value| value.to_str()) != Some(expected.owner.as_str()) {
            return Err("Account ID does not match its managed filename".into());
        }
        let supplied =
            serde_json::to_value(token).map_err(|_| "Failed to encode refreshed token")?;
        let current = document
            .get_mut("token")
            .and_then(Value::as_object_mut)
            .ok_or("Missing managed token metadata")?;
        for field in CREDENTIAL_FIELDS {
            if let Some(value) = supplied.get(field) {
                current.insert(field.into(), value.clone());
            } else {
                current.remove(field);
            }
        }
        persist_account_document_locked(path, document, Some(expected), false)?;
        let mut committed = read_account_document(path)?;
        hydrate_account_document(&mut committed)?;
        let revision = capture_credential_revision(&committed)?;
        let mut token: crate::models::TokenData =
            serde_json::from_value(committed["token"].clone())
                .map_err(|_| "Invalid committed token metadata")?;
        token.credential_revision = Some(revision);
        Ok(token)
    })
}

fn persist_account_document_with_recovery(
    path: &Path,
    document: Value,
    expected: Option<&CredentialRevision>,
    verified_recovery: bool,
) -> Result<(), String> {
    let id = account_id(&document)?.to_string();
    crate::modules::account::with_account_file_write(&id, || {
        persist_account_document_locked(path, document, expected, verified_recovery)
    })
}

fn preserve_attention_state(latest: &Value, document: &mut Value) {
    let manual = latest.get("validation_blocked").and_then(Value::as_bool) == Some(true)
        && latest
            .get("validation_blocked_reason")
            .and_then(Value::as_str)
            .is_some_and(|reason| reason.starts_with("manual_attention_required:"));
    let incoming_manual = document.get("validation_blocked").and_then(Value::as_bool) == Some(true)
        && document
            .get("validation_blocked_reason")
            .and_then(Value::as_str)
            .is_some_and(|reason| reason.starts_with("manual_attention_required:"));
    let banned = latest.get("banned").and_then(Value::as_bool) == Some(true);
    if manual && !incoming_manual {
        for field in [
            "validation_blocked",
            "validation_blocked_until",
            "validation_blocked_reason",
            "validation_url",
        ] {
            document[field] = latest.get(field).cloned().unwrap_or(Value::Null);
        }
    }
    if banned && document.get("banned").and_then(Value::as_bool) != Some(true) {
        for field in [
            "banned",
            "banned_code",
            "banned_reason",
            "banned_at",
            "ban_appeal_url",
        ] {
            document[field] = latest.get(field).cloned().unwrap_or(Value::Null);
        }
    }
    if manual || banned {
        if latest.get("proxy_disabled").and_then(Value::as_bool) == Some(true)
            && document.get("proxy_disabled").and_then(Value::as_bool) != Some(true)
        {
            for field in [
                "proxy_disabled",
                "proxy_disabled_reason",
                "proxy_disabled_at",
            ] {
                document[field] = latest.get(field).cloned().unwrap_or(Value::Null);
            }
        }
        if latest
            .pointer("/quota/is_forbidden")
            .and_then(Value::as_bool)
            == Some(true)
            && document
                .pointer("/quota/is_forbidden")
                .and_then(Value::as_bool)
                != Some(true)
        {
            if document.get("quota").is_some_and(Value::is_object) {
                document["quota"]["is_forbidden"] = Value::Bool(true);
                document["quota"]["forbidden_reason"] = latest
                    .pointer("/quota/forbidden_reason")
                    .cloned()
                    .unwrap_or(Value::Null);
            } else {
                document["quota"] = latest["quota"].clone();
            }
        }
    }
}

fn persist_account_document_locked(
    path: &Path,
    mut document: Value,
    expected: Option<&CredentialRevision>,
    verified_recovery: bool,
) -> Result<(), String> {
    let id = account_id(&document)?.to_string();
    if path.file_stem().and_then(|v| v.to_str()) != Some(id.as_str()) {
        return Err("Account ID does not match its managed filename".into());
    }
    let latest = match std::fs::read(path) {
        Ok(raw) => {
            let mut parser = serde_json::Deserializer::from_slice(&raw);
            Some(
                Value::deserialize(&mut parser)
                    .map_err(|_| "Failed to parse existing account metadata")?,
            )
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => None,
        Err(error) => return Err(format!("Failed to read existing account metadata: {error}")),
    };
    if let Some(latest) = &latest {
        if account_id(latest)? != id {
            return Err("Existing account identity does not match its managed filename".into());
        }
        if !verified_recovery {
            preserve_attention_state(latest, &mut document);
        }
    }
    if let Some(expected) = expected {
        if expected.owner != id {
            return Err("Credential revision belongs to a different account".into());
        }
        let latest = latest
            .as_ref()
            .ok_or("Account was removed before its update")?;
        if value_digest(&credential_values(&document))? == expected.hydrated_digest {
            // A metadata edit must never replay the snapshot's OAuth values.
            copy_current_credentials(latest, &mut document)?;
        } else if persisted_credential_digest(latest)? != expected.persisted_digest {
            return Err(
                "Account credentials changed concurrently; reload before replacing them".into(),
            );
        }
    } else if let Some(latest) = latest.as_ref() {
        let has_supplied_secrets =
            ["access_token", "refresh_token", "id_token"]
                .iter()
                .any(|field| {
                    document["token"]
                        .get(*field)
                        .and_then(Value::as_str)
                        .is_some_and(|value| !value.is_empty())
                });
        if reference(latest)?.is_some() && has_supplied_secrets {
            // A caller may retain its original Account::new snapshot after
            // creating the file. It has no observed revision: permit metadata
            // edits only when every credential-bound value still equals disk.
            let mut current = latest.clone();
            hydrate_account_document(&mut current)?;
            if credential_values(&current) != credential_values(&document) {
                return Err(
                    "An observed credential revision is required to replace account tokens".into(),
                );
            }
            copy_current_credentials(latest, &mut document)?;
        }
    }
    let supplied_reference = reference(&document)?;
    let Some(token) = document.get("token").and_then(Value::as_object) else {
        // Metadata-only malformed records can still be disabled/cleaned up;
        // there is no OAuth credential here to export to the store.
        if supplied_reference.is_some() {
            return Err("Missing managed token metadata".into());
        }
        let bytes = serde_json::to_vec_pretty(&document)
            .map_err(|_| "Failed to encode account metadata")?;
        return crate::utils::fs::write_private_atomic(path, &bytes)
            .map_err(|e| format!("Failed to save account metadata: {e}"));
    };
    let metadata_only = supplied_reference.is_some()
        && ["access_token", "refresh_token", "id_token"]
            .iter()
            .all(|field| {
                token
                    .get(*field)
                    .is_none_or(|v| v.is_null() || v.as_str() == Some(""))
            });
    if metadata_only {
        // Raw metadata callers may have read this document before a concurrent
        // credential rotation. Adopt the current generation under the writer
        // lock so an old snapshot cannot republish a retired vault reference.
        let latest = latest
            .as_ref()
            .ok_or("Account was removed before its metadata update")?;
        if reference(latest)?.is_none() {
            return Err("Account credentials changed; reload its metadata".into());
        }
        copy_current_credentials(latest, &mut document)?;
        let bytes = serde_json::to_vec_pretty(&document)
            .map_err(|_| "Failed to encode account metadata")?;
        return crate::utils::fs::write_private_atomic(path, &bytes)
            .map_err(|e| format!("Failed to save protected account metadata: {e}"));
    }
    if expected.is_none() {
        if let Some(latest) = &latest {
            if reference(latest)?.is_some()
                || credential_values(latest) != credential_values(&document)
            {
                return Err(
                    "An observed credential revision is required to replace account tokens".into(),
                );
            }
        }
    }
    if supplied_reference.is_some()
        && ["access_token", "refresh_token"].iter().any(|field| {
            token
                .get(*field)
                .and_then(Value::as_str)
                .is_none_or(str::is_empty)
        })
    {
        return Err("Load protected credentials before replacing token fields".into());
    }
    let credentials = json!({
        "access_token":token.get("access_token").and_then(Value::as_str).ok_or("Missing access token")?,
        "refresh_token":token.get("refresh_token").and_then(Value::as_str).ok_or("Missing refresh token")?,
        "id_token":token.get("id_token").cloned().unwrap_or(Value::Null)
    });
    let old_reference = latest.as_ref().map(reference).transpose()?.flatten();
    let reused = old_reference.as_ref().is_some_and(|old| {
        load_json(ACCOUNT_SCOPE, &id, old).is_ok_and(|value| value == credentials)
    });
    let new_reference = if reused {
        old_reference.clone().unwrap()
    } else {
        store_json(ACCOUNT_SCOPE, &id, &credentials)?
    };
    document["credential_store"] = serde_json::to_value(&new_reference)
        .map_err(|_| "Failed to encode credential reference")?;
    document["token"]["access_token"] = json!("");
    document["token"]["refresh_token"] = json!("");
    document["token"]["id_token"] = Value::Null;
    if latest.as_ref().map(credential_state).transpose()?.as_ref()
        != Some(&credential_state(&document)?)
    {
        document["credential_version"] = json!(uuid::Uuid::new_v4().to_string());
    } else if let Some(version) = latest
        .as_ref()
        .and_then(|value| value.get("credential_version"))
    {
        document["credential_version"] = version.clone();
    } else {
        document
            .as_object_mut()
            .unwrap()
            .remove("credential_version");
    }
    let written = serde_json::to_vec_pretty(&document)
        .map_err(|_| "Failed to encode protected account".to_string())
        .and_then(|bytes| {
            crate::utils::fs::write_private_atomic(path, &bytes)
                .map_err(|e| format!("Failed to save protected account: {e}"))
        });
    if let Err(error) = written {
        if !reused {
            let _ = delete(ACCOUNT_SCOPE, &new_reference);
        }
        return Err(error);
    }
    if !reused {
        if let Some(old) = old_reference {
            if let Err(error) = delete(ACCOUNT_SCOPE, &old) {
                tracing::warn!("{error}");
            }
        }
    }
    Ok(())
}

pub(crate) fn account_reference(document: &Value) -> Result<Option<VaultRef>, String> {
    reference(document)
}
pub(crate) fn account_reference_at(path: &Path) -> Result<Option<VaultRef>, String> {
    match std::fs::read(path) {
        Ok(raw) => {
            let mut parser = serde_json::Deserializer::from_slice(&raw);
            reference(&Value::deserialize(&mut parser).map_err(|_| "Invalid account metadata")?)
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(error) => Err(format!("Failed to read credential reference: {error}")),
    }
}
pub(crate) fn delete_account_reference(reference: &VaultRef) -> Result<(), String> {
    delete(ACCOUNT_SCOPE, reference)
}

/// Called only under the shared writer boundary by startup/list scans.
pub(crate) fn migrate_account_file(path: &Path) -> Result<bool, String> {
    let raw = std::fs::read(path)
        .map_err(|e| format!("Failed to read account for credential migration: {e}"))?;
    let mut parser = serde_json::Deserializer::from_slice(&raw);
    let document = Value::deserialize(&mut parser)
        .map_err(|_| "Invalid account JSON during credential migration")?;
    let id = account_id(&document)?;
    if path.file_stem().and_then(|value| value.to_str()) != Some(id) {
        return Err("Account ID does not match its managed filename".into());
    }
    if let Some(reference) = reference(&document)? {
        validate_reference(ACCOUNT_SCOPE, &reference)?;
        return Ok(false);
    }
    if document.get("token").is_none() {
        return Ok(false);
    }
    persist_account_document(path, document)?;
    Ok(true)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::{cell::RefCell, collections::HashMap};

    #[test]
    fn immutable_cache_is_bounded_and_generation_invalidation_is_scoped() {
        let mut cache = Cache::default();
        for index in 0..=CACHE_ENTRIES {
            let reference = VaultRef {
                version: 1,
                generation: format!("generation-{index}"),
                parts: 1,
                digest: "0".repeat(64),
            };
            cache.insert(
                ("account".into(), "fixture".into(), reference),
                json!(index),
            );
        }
        assert_eq!(cache.entries.len(), CACHE_ENTRIES);
        assert!(cache.bytes <= CACHE_BYTES);
        let reference = cache.order.back().unwrap().2.clone();
        let epoch = cache.epoch;
        cache.invalidate(Some(&reference));
        assert_eq!(cache.entries.len(), CACHE_ENTRIES - 1);
        assert_ne!(cache.epoch, epoch);
        let key = ("account".into(), "large-fixture".into(), reference);
        cache.insert(key.clone(), json!("x".repeat(CACHE_BYTES / 2 + 1)));
        let mut other = key.clone();
        other.1 = "another".into();
        cache.insert(other, json!("x".repeat(CACHE_BYTES / 2 + 1)));
        assert!(cache.bytes <= CACHE_BYTES);
        assert!(!cache.entries.contains_key(&key));
    }

    #[test]
    fn stale_metadata_snapshot_preserves_rotated_credentials_and_expiry() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("stale-ref-fixture.json");
        let mut account = json!({
            "id":"stale-ref-fixture",
            "token":{
                "access_token":"old-access", "refresh_token":"old-refresh",
                "id_token":"old-id", "expires_in":100, "expiry_timestamp":1000,
                "oauth_client_key":"old-client", "token_type":"old-type",
                "is_gcp_tos":false, "project_id":"original-project"
            }
        });
        persist_account_document(&path, account.clone()).unwrap();
        let mut stale: Value = serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
        let retired = reference(&stale).unwrap().unwrap();
        let mut observed = stale.clone();
        hydrate_account_document(&mut observed).unwrap();
        let expected = capture_credential_revision(&observed).unwrap();
        account["token"] = json!({
            "access_token":"new-access", "refresh_token":"new-refresh",
            "id_token":"new-id", "expires_in":200, "expiry_timestamp":2000,
            "oauth_client_key":"new-client", "token_type":"Bearer",
            "is_gcp_tos":true, "project_id":"original-project"
        });
        persist_typed_account_document(&path, account, Some(&expected)).unwrap();
        let rotated: Value = serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
        let current = reference(&rotated).unwrap().unwrap();
        assert_ne!(retired, current);
        assert!(load_json(ACCOUNT_SCOPE, "stale-ref-fixture", &retired).is_err());

        stale["proxy_disabled"] = json!(true);
        stale["token"]["project_id"] = json!("edited-project");
        stale["session_id"] = json!("edited-session");
        persist_account_document(&path, stale).unwrap();
        let bytes = std::fs::read(&path).unwrap();
        let mut saved: Value = serde_json::from_slice(&bytes).unwrap();
        assert_eq!(reference(&saved).unwrap().unwrap(), current);
        assert_eq!(saved["token"]["access_token"], "");
        assert_eq!(saved["token"]["refresh_token"], "");
        assert!(saved["token"]["id_token"].is_null());
        assert!(!String::from_utf8(bytes).unwrap().contains("new-refresh"));
        hydrate_account_document(&mut saved).unwrap();
        assert_eq!(saved["token"]["access_token"], "new-access");
        assert_eq!(saved["token"]["refresh_token"], "new-refresh");
        assert_eq!(saved["token"]["id_token"], "new-id");
        assert_eq!(saved["token"]["expires_in"], 200);
        assert_eq!(saved["token"]["expiry_timestamp"], 2000);
        assert_eq!(saved["token"]["oauth_client_key"], "new-client");
        assert_eq!(saved["token"]["token_type"], "Bearer");
        assert_eq!(saved["token"]["is_gcp_tos"], true);
        assert_eq!(saved["token"]["project_id"], "edited-project");
        assert_eq!(saved["session_id"], "edited-session");
        assert_eq!(saved["proxy_disabled"], true);
        delete_account_reference(&current).unwrap();
    }

    #[test]
    fn metadata_hold_can_persist_when_protected_credentials_are_unavailable() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("metadata-fixture.json");
        let document = json!({"id":"metadata-fixture","token":{"access_token":"fixture-a","refresh_token":"fixture-r","id_token":null}});
        persist_account_document(&path, document).unwrap();
        let mut raw: Value = serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
        let reference = reference(&raw).unwrap().unwrap();
        delete_account_reference(&reference).unwrap();
        raw["proxy_disabled"] = json!(true);
        persist_account_document(&path, raw).unwrap();
        let mut saved: Value = serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
        assert_eq!(saved["proxy_disabled"], true);
        assert!(hydrate_account_document(&mut saved).is_err());
        assert_eq!(saved["token"]["refresh_token"], "");
    }

    #[cfg(windows)]
    #[test]
    fn failed_atomic_account_publish_keeps_old_generation_and_discards_new_one() {
        use std::os::windows::fs::OpenOptionsExt;
        let dir = tempfile::tempdir().unwrap();
        let owner = format!("rollback-{}", uuid::Uuid::new_v4());
        let path = dir.path().join(format!("{owner}.json"));
        let mut document = json!({"id":owner,"token":{"access_token":"old-fixture","refresh_token":"refresh-fixture","id_token":null}});
        persist_account_document(&path, document.clone()).unwrap();
        let original = std::fs::read(&path).unwrap();
        let reference = reference(&serde_json::from_slice::<Value>(&original).unwrap())
            .unwrap()
            .unwrap();
        let mut observed: Value = serde_json::from_slice(&original).unwrap();
        hydrate_account_document(&mut observed).unwrap();
        let expected = capture_credential_revision(&observed).unwrap();
        let held = std::fs::OpenOptions::new()
            .read(true)
            .share_mode(3)
            .open(&path)
            .unwrap();
        document["token"]["access_token"] = json!("new-fixture");
        assert!(persist_typed_account_document(&path, document, Some(&expected)).is_err());
        assert_eq!(std::fs::read(&path).unwrap(), original);
        assert_eq!(
            load_json("account", &owner, &reference).unwrap()["access_token"],
            "old-fixture"
        );
        let matching = TEST_SECRETS
            .lock()
            .unwrap()
            .values()
            .filter(|bytes| {
                STANDARD
                    .decode(bytes)
                    .ok()
                    .and_then(|bytes| serde_json::from_slice::<Value>(&bytes).ok())
                    .is_some_and(|value| value["owner"] == owner)
            })
            .count();
        assert_eq!(matching, 1, "unpublished vault generation must be removed");
        drop(held);
        delete_account_reference(&reference).unwrap();
    }
    #[derive(Default)]
    struct MemoryStore {
        entries: RefCell<HashMap<String, Vec<u8>>>,
        fail_after: Option<usize>,
    }
    impl SecretStore for MemoryStore {
        fn set(&self, key: &str, value: &[u8]) -> Result<(), String> {
            if self
                .fail_after
                .is_some_and(|n| self.entries.borrow().len() >= n)
            {
                return Err("fixture store unavailable".into());
            }
            self.entries.borrow_mut().insert(key.into(), value.into());
            Ok(())
        }
        fn get(&self, key: &str) -> Result<Vec<u8>, String> {
            self.entries
                .borrow()
                .get(key)
                .cloned()
                .ok_or("fixture entry missing".into())
        }
        fn delete(&self, key: &str) -> Result<(), String> {
            self.entries.borrow_mut().remove(key);
            Ok(())
        }
    }
    #[test]
    fn chunked_vault_roundtrip_binds_identity_and_detects_corruption() {
        let store = MemoryStore::default();
        let value = json!({"access_token":"synthetic".repeat(1200),"refresh_token":"fixture","id_token":null});
        let reference = store_with(&store, "account", "owner-a", &value).unwrap();
        assert!(reference.parts > 1);
        assert!(store
            .entries
            .borrow()
            .values()
            .all(|value| value.len() <= CHUNK_BYTES));
        assert_eq!(
            load_with(&store, "account", "owner-a", &reference).unwrap(),
            value
        );
        assert!(load_with(&store, "account", "owner-b", &reference).is_err());
        assert!(load_with(&store, "config", "owner-a", &reference).is_err());
        store
            .entries
            .borrow_mut()
            .get_mut(&key("account", &reference, 0))
            .unwrap()[0] = b'!';
        assert!(load_with(&store, "account", "owner-a", &reference).is_err());
        delete_with(&store, "account", &reference).unwrap();
        assert!(store.entries.borrow().is_empty());
    }
    #[test]
    fn partial_native_write_failure_removes_the_unpublished_generation() {
        let store = MemoryStore {
            fail_after: Some(1),
            ..Default::default()
        };
        assert!(store_with(&store, "account", "owner", &json!("fixture".repeat(1000))).is_err());
        assert!(store.entries.borrow().is_empty());
        let mut invalid = VaultRef {
            version: 1,
            generation: uuid::Uuid::new_v4().to_string(),
            parts: MAX_PARTS + 1,
            digest: "0".repeat(64),
        };
        assert!(load_with(&store, "account", "owner", &invalid).is_err());
        invalid.parts = 1;
        invalid.generation = "../../another".into();
        assert!(load_with(&store, "account", "owner", &invalid).is_err());
    }
    #[test]
    fn account_json_redacts_secrets_and_missing_store_fails_closed() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("fixture-account.json");
        let account = json!({"id":"fixture-account","token":{"access_token":"synthetic-access","refresh_token":"synthetic-refresh","id_token":"synthetic-id","expiry_timestamp":123}});
        persist_account_document(&path, account.clone()).unwrap();
        let raw = std::fs::read_to_string(&path).unwrap();
        for secret in ["synthetic-access", "synthetic-refresh", "synthetic-id"] {
            assert!(!raw.contains(secret));
        }
        let mut loaded: Value = serde_json::from_str(&raw).unwrap();
        let original = reference(&loaded).unwrap().unwrap();
        hydrate_account_document(&mut loaded).unwrap();
        assert_eq!(loaded["token"], account["token"]);
        let expected = capture_credential_revision(&loaded).unwrap();
        persist_typed_account_document(&path, account, Some(&expected)).unwrap();
        let unchanged: Value = serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
        assert_eq!(reference(&unchanged).unwrap().unwrap(), original);
        delete_account_reference(&original).unwrap();
        let mut missing = unchanged;
        assert!(hydrate_account_document(&mut missing).is_err());
        assert_eq!(missing["token"]["access_token"], "");
    }
}
