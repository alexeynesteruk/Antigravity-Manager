//! User Token Database Module
//! UserToken database operations module

#![allow(dead_code)]
// User token storage; some interfaces are reserved for future extension

use crate::modules::credential_store::{self, VaultRef};
use chrono::{FixedOffset, Timelike, Utc};
use rand::RngCore;
use rusqlite::{params, Connection, OptionalExtension, TransactionBehavior};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::path::PathBuf;
use uuid::Uuid;

const DEVICE_KEY_SCOPE: &str = "device_key";

fn key_digest(value: &str) -> String {
    format!("sha256:{:x}", Sha256::digest(value.as_bytes()))
}

fn key_preview(value: &str) -> String {
    if value.chars().count() <= 16 {
        return "<protected key>".into();
    }
    format!(
        "{}…{}",
        value.chars().take(10).collect::<String>(),
        value
            .chars()
            .rev()
            .take(4)
            .collect::<String>()
            .chars()
            .rev()
            .collect::<String>()
    )
}

fn protect_device_key(id: &str, value: &str) -> Result<VaultRef, String> {
    credential_store::store_json(DEVICE_KEY_SCOPE, id, &serde_json::json!({"token":value}))
}

/// User token struct
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct UserToken {
    pub id: String,
    pub token: String,
    pub username: String,
    pub description: Option<String>,
    pub enabled: bool,
    pub expires_type: String, // "day", "week", "month", "never"
    pub expires_at: Option<i64>,
    pub max_ips: i32,                 // 0 = unlimited
    pub curfew_start: Option<String>, // "HH:MM" curfew start time
    pub curfew_end: Option<String>,   // "HH:MM" curfew end time
    pub created_at: i64,
    pub updated_at: i64,
    pub last_used_at: Option<i64>,
    pub total_requests: i64,
    pub total_tokens_used: i64,
}

/// Token IP binding struct
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TokenIpBinding {
    pub id: String,
    pub token_id: String,
    pub ip_address: String,
    pub first_seen_at: i64,
    pub last_seen_at: i64,
    pub request_count: i64,
    pub user_agent: Option<String>,
}

/// Token usage log struct
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TokenUsageLog {
    pub id: String,
    pub token_id: String,
    pub ip_address: String,
    pub model: String,
    pub input_tokens: i32,
    pub output_tokens: i32,
    pub request_time: i64,
    pub status: u16,
}

/// Get the database path
pub fn get_db_path() -> Result<PathBuf, String> {
    let mut path = crate::modules::account::get_data_dir()?;
    path.push("user_tokens.db");
    Ok(path)
}

/// Connect to the database
pub fn connect_db() -> Result<Connection, String> {
    let path = get_db_path()?;
    let conn = Connection::open(&path).map_err(|e| format!("Failed to open database: {}", e))?;
    conn.busy_timeout(std::time::Duration::from_secs(5))
        .map_err(|e| format!("Failed to configure database busy timeout: {}", e))?;
    conn.pragma_update(None, "secure_delete", "ON")
        .map_err(|e| format!("Failed to configure device-key storage: {e}"))?;
    Ok(conn)
}

/// Initialize the database
pub fn init_db() -> Result<(), String> {
    let mut conn = connect_db()?;

    // Create the user_tokens table
    conn.execute(
        "CREATE TABLE IF NOT EXISTS user_tokens (
            id TEXT PRIMARY KEY,
            token TEXT UNIQUE NOT NULL,
            username TEXT NOT NULL,
            description TEXT,
            enabled BOOLEAN NOT NULL DEFAULT 1,
            expires_type TEXT NOT NULL,
            expires_at INTEGER,
            max_ips INTEGER NOT NULL DEFAULT 0,
            created_at INTEGER NOT NULL,
            updated_at INTEGER NOT NULL,
            last_used_at INTEGER,
            total_requests INTEGER NOT NULL DEFAULT 0,
            total_tokens_used INTEGER NOT NULL DEFAULT 0,
            curfew_start TEXT,
            curfew_end TEXT
        )",
        [],
    )
    .map_err(|e| format!("Failed to create user_tokens table: {}", e))?;

    // Try to add new columns (for migrating old databases, ignore errors if they already exist)
    let _ = conn.execute("ALTER TABLE user_tokens ADD COLUMN expires_type TEXT", []);
    let _ = conn.execute("ALTER TABLE user_tokens ADD COLUMN expires_at INTEGER", []);
    let _ = conn.execute(
        "ALTER TABLE user_tokens ADD COLUMN max_ips INTEGER DEFAULT 0",
        [],
    );
    let _ = conn.execute(
        "ALTER TABLE user_tokens ADD COLUMN total_requests INTEGER DEFAULT 0",
        [],
    );
    let _ = conn.execute(
        "ALTER TABLE user_tokens ADD COLUMN total_tokens_used INTEGER DEFAULT 0",
        [],
    );
    let _ = conn.execute(
        "ALTER TABLE user_tokens ADD COLUMN last_used_at INTEGER",
        [],
    );
    let _ = conn.execute("ALTER TABLE user_tokens ADD COLUMN curfew_start TEXT", []);
    let _ = conn.execute("ALTER TABLE user_tokens ADD COLUMN curfew_end TEXT", []);
    let _ = conn.execute(
        "ALTER TABLE user_tokens ADD COLUMN credential_store_ref TEXT",
        [],
    );
    let _ = conn.execute("ALTER TABLE user_tokens ADD COLUMN token_preview TEXT", []);

    // Create the token_ip_bindings table
    conn.execute(
        "CREATE TABLE IF NOT EXISTS token_ip_bindings (
            id TEXT PRIMARY KEY,
            token_id TEXT NOT NULL,
            ip_address TEXT NOT NULL,
            first_seen_at INTEGER NOT NULL,
            last_seen_at INTEGER NOT NULL,
            request_count INTEGER NOT NULL DEFAULT 0,
            user_agent TEXT,
            FOREIGN KEY(token_id) REFERENCES user_tokens(id) ON DELETE CASCADE,
            UNIQUE(token_id, ip_address)
        )",
        [],
    )
    .map_err(|e| format!("Failed to create token_ip_bindings table: {}", e))?;

    // Create the token_usage_logs table
    conn.execute(
        "CREATE TABLE IF NOT EXISTS token_usage_logs (
            id TEXT PRIMARY KEY,
            token_id TEXT NOT NULL,
            ip_address TEXT,
            model TEXT,
            input_tokens INTEGER,
            output_tokens INTEGER,
            request_time INTEGER NOT NULL,
            status INTEGER,
            FOREIGN KEY(token_id) REFERENCES user_tokens(id) ON DELETE CASCADE
        )",
        [],
    )
    .map_err(|e| format!("Failed to create token_usage_logs table: {}", e))?;

    // Create indexes
    let _ = conn.execute(
        "CREATE INDEX IF NOT EXISTS idx_token_usage_logs_token_id ON token_usage_logs(token_id)",
        [],
    );
    let _ = conn.execute("CREATE INDEX IF NOT EXISTS idx_token_usage_logs_request_time ON token_usage_logs(request_time)", []);
    let _ = conn.execute("CREATE INDEX IF NOT EXISTS idx_token_usage_logs_token_time ON token_usage_logs(token_id, request_time DESC)", []);

    // [FIX Issue #1719] Data sanitization: fix NULL fields caused by upgrading from old versions
    // These fields may not have existed in older versions; after ALTER TABLE adds them they default to NULL, causing deserialization to fail
    let _ = conn.execute("UPDATE user_tokens SET expires_type = 'never' WHERE expires_type IS NULL OR expires_type = ''", []);
    let _ = conn.execute(
        "UPDATE user_tokens SET max_ips = 0 WHERE max_ips IS NULL",
        [],
    );
    let _ = conn.execute(
        "UPDATE user_tokens SET total_requests = 0 WHERE total_requests IS NULL",
        [],
    );
    let _ = conn.execute(
        "UPDATE user_tokens SET total_tokens_used = 0 WHERE total_tokens_used IS NULL",
        [],
    );
    let _ = conn.execute(
        "UPDATE user_tokens SET enabled = 1 WHERE enabled IS NULL",
        [],
    );

    migrate_legacy_device_keys(&mut conn)?;
    Ok(())
}

fn migrate_legacy_device_keys(conn: &mut Connection) -> Result<usize, String> {
    let mut published = Vec::new();
    let result = (|| {
        let tx = conn
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .map_err(|e| format!("Failed to start device-key migration: {e}"))?;
        let legacy = {
            let mut query = tx
                .prepare("SELECT id,token FROM user_tokens WHERE credential_store_ref IS NULL")
                .map_err(|e| e.to_string())?;
            let rows = query
                .query_map([], |row| {
                    Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
                })
                .map_err(|e| e.to_string())?;
            rows.collect::<Result<Vec<_>, _>>()
                .map_err(|e| e.to_string())?
        };
        for (id, value) in &legacy {
            if value.starts_with("sha256:") {
                return Err("Protected device key is missing its credential reference".into());
            }
            let reference = protect_device_key(id, value)?;
            published.push(reference.clone());
            tx.execute("UPDATE user_tokens SET token=?1,credential_store_ref=?2,token_preview=?3 WHERE id=?4",
                params![key_digest(value),serde_json::to_string(&reference).map_err(|_| "Invalid device-key reference")?,key_preview(value),id])
                .map_err(|e| format!("Failed to publish protected device key: {e}"))?;
        }
        tx.commit()
            .map_err(|e| format!("Failed to commit device-key migration: {e}"))?;
        Ok(legacy.len())
    })();
    if result.is_err() {
        for reference in published {
            let _ = credential_store::delete(DEVICE_KEY_SCOPE, &reference);
        }
    }
    result
}

/// Explicit administrator action; metadata/list APIs never reveal complete keys.
pub fn reveal_token(id: &str) -> Result<String, String> {
    let conn = connect_db()?;
    let stored: Option<(String, Option<String>)> = conn
        .query_row(
            "SELECT token,credential_store_ref FROM user_tokens WHERE id=?1",
            [id],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .optional()
        .map_err(|e| e.to_string())?;
    let (digest, reference) = stored.ok_or("Device key no longer exists")?;
    let reference: VaultRef = serde_json::from_str(
        &reference.ok_or("Device key must be migrated before it can be revealed")?,
    )
    .map_err(|_| "Invalid device-key credential reference")?;
    let value = credential_store::load_json(DEVICE_KEY_SCOPE, id, &reference)?;
    let key = value
        .get("token")
        .and_then(serde_json::Value::as_str)
        .ok_or("Invalid protected device key")?;
    if key_digest(key) != digest {
        return Err("Device-key credential does not match its database identity".into());
    }
    Ok(key.into())
}

/// Create a new token
pub fn create_token(
    username: String,
    expires_type: String,
    description: Option<String>,
    max_ips: i32,
    curfew_start: Option<String>,
    curfew_end: Option<String>,
    custom_expires_at: Option<i64>, // Custom expiry timestamp (seconds)
) -> Result<UserToken, String> {
    let conn = connect_db()?;
    let id = Uuid::new_v4().to_string();
    let mut random = [0u8; 32];
    rand::rngs::OsRng.fill_bytes(&mut random);
    let token = format!(
        "sk-device-{}",
        random
            .iter()
            .map(|byte| format!("{byte:02x}"))
            .collect::<String>()
    );
    let now = Utc::now().timestamp();

    let expires_at = match expires_type.as_str() {
        "day" => Some(
            Utc::now()
                .checked_add_signed(chrono::Duration::days(1))
                .unwrap()
                .timestamp(),
        ),
        "week" => Some(
            Utc::now()
                .checked_add_signed(chrono::Duration::weeks(1))
                .unwrap()
                .timestamp(),
        ),
        "month" => Some(
            Utc::now()
                .checked_add_signed(chrono::Duration::days(30))
                .unwrap()
                .timestamp(),
        ),
        "custom" => custom_expires_at, // Use the custom timestamp
        _ => None,                     // "never" or other
    };

    let secret_ref = protect_device_key(&id, &token)?;
    let user_token = UserToken {
        id: id.clone(),
        token: token.clone(),
        username: username.clone(),
        description: description.clone(),
        enabled: true,
        expires_type: expires_type.clone(),
        expires_at,
        max_ips,
        curfew_start: curfew_start.clone(),
        curfew_end: curfew_end.clone(),
        created_at: now,
        updated_at: now,
        last_used_at: None,
        total_requests: 0,
        total_tokens_used: 0,
    };

    conn.execute(
        "INSERT INTO user_tokens (
            id, token, username, description, enabled, expires_type, expires_at, max_ips,
            curfew_start, curfew_end,
            created_at, updated_at, total_requests, total_tokens_used, credential_store_ref, token_preview
        ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15, ?16)",
        params![
            user_token.id,
            key_digest(&user_token.token),
            user_token.username,
            user_token.description,
            user_token.enabled,
            user_token.expires_type,
            user_token.expires_at,
            user_token.max_ips,
            user_token.curfew_start,
            user_token.curfew_end,
            user_token.created_at,
            user_token.updated_at,
            user_token.total_requests,
            user_token.total_tokens_used,
            serde_json::to_string(&secret_ref).map_err(|_| "Invalid device-key reference")?,
            key_preview(&user_token.token),
        ],
    )
    .map_err(|e| { let _ = credential_store::delete(DEVICE_KEY_SCOPE,&secret_ref); format!("Failed to insert user token: {e}") })?;

    Ok(user_token)
}

/// List all tokens
pub fn list_tokens() -> Result<Vec<UserToken>, String> {
    let conn = connect_db()?;
    let mut stmt = conn
        .prepare("SELECT * FROM user_tokens ORDER BY created_at DESC")
        .map_err(|e| format!("Failed to prepare query: {}", e))?;

    let token_iter = stmt
        .query_map([], user_token_from_row)
        .map_err(|e| format!("Failed to query tokens: {}", e))?;

    let mut tokens = Vec::new();
    for token in token_iter {
        tokens.push(token.map_err(|e| format!("Failed to parse token row: {}", e))?);
    }

    Ok(tokens)
}

/// Get information for a single token
pub fn get_token_by_id(id: &str) -> Result<Option<UserToken>, String> {
    let conn = connect_db()?;
    let mut stmt = conn
        .prepare("SELECT * FROM user_tokens WHERE id = ?1")
        .map_err(|e| format!("Failed to prepare query: {}", e))?;

    let token = stmt
        .query_row(params![id], user_token_from_row)
        .optional()
        .map_err(|e| format!("Failed to query token: {}", e))?;

    Ok(token)
}

fn user_token_from_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<UserToken> {
    Ok(UserToken {
        id: row.get("id")?,
        token: {
            let raw: String = row.get("token")?;
            let reference: Option<String> = row.get("credential_store_ref").unwrap_or(None);
            if reference.is_some() {
                row.get::<_, Option<String>>("token_preview")
                    .unwrap_or(None)
                    .unwrap_or_else(|| "<protected key>".into())
            } else {
                key_preview(&raw)
            }
        },
        username: row.get("username")?,
        description: row.get("description")?,
        enabled: row.get("enabled")?,
        expires_type: row.get("expires_type")?,
        expires_at: row.get("expires_at")?,
        max_ips: row.get("max_ips")?,
        curfew_start: row.get("curfew_start")?,
        curfew_end: row.get("curfew_end")?,
        created_at: row.get("created_at")?,
        updated_at: row.get("updated_at")?,
        last_used_at: row.get("last_used_at")?,
        total_requests: row.get("total_requests")?,
        total_tokens_used: row.get("total_tokens_used")?,
    })
}

/// Unrestricted lookup for administration and auth-off usage attribution.
/// Network admission must use `admit_token`, which enforces policy and reserves IP capacity.
pub fn get_token_by_value(token: &str) -> Result<Option<UserToken>, String> {
    connect_db()?
        .query_row(
            "SELECT * FROM user_tokens WHERE token = ?1 AND credential_store_ref IS NOT NULL",
            params![key_digest(token)],
            user_token_from_row,
        )
        .optional()
        .map_err(|e| format!("Failed to query token: {}", e))
}

/// Update token status/notes, etc.
pub fn update_token(
    id: &str,
    username: Option<String>,
    description: Option<String>,
    enabled: Option<bool>,
    max_ips: Option<i32>,
    curfew_start: Option<Option<String>>,
    curfew_end: Option<Option<String>>,
) -> Result<(), String> {
    let conn = connect_db()?;
    let now = Utc::now().timestamp();

    let mut query = "UPDATE user_tokens SET updated_at = ?1".to_string();
    let mut params_vec: Vec<Box<dyn rusqlite::ToSql>> = vec![Box::new(now)];
    let mut param_idx = 2;

    if let Some(user) = username {
        query.push_str(&format!(", username = ?{}", param_idx));
        params_vec.push(Box::new(user));
        param_idx += 1;
    }

    if let Some(desc) = description {
        query.push_str(&format!(", description = ?{}", param_idx));
        params_vec.push(Box::new(desc));
        param_idx += 1;
    }

    if let Some(en) = enabled {
        query.push_str(&format!(", enabled = ?{}", param_idx));
        params_vec.push(Box::new(en));
        param_idx += 1;
    }

    if let Some(ips) = max_ips {
        query.push_str(&format!(", max_ips = ?{}", param_idx));
        params_vec.push(Box::new(ips));
        param_idx += 1;
    }

    if let Some(start) = curfew_start {
        query.push_str(&format!(", curfew_start = ?{}", param_idx));
        params_vec.push(Box::new(start));
        param_idx += 1;
    }

    if let Some(end) = curfew_end {
        query.push_str(&format!(", curfew_end = ?{}", param_idx));
        params_vec.push(Box::new(end));
        param_idx += 1;
    }

    query.push_str(&format!(" WHERE id = ?{}", param_idx));
    params_vec.push(Box::new(id.to_string()));

    // Convert Vec<Box<dyn ToSql>> to &[&dyn ToSql]
    let params_refs: Vec<&dyn rusqlite::ToSql> = params_vec.iter().map(|p| p.as_ref()).collect();

    conn.execute(&query, params_refs.as_slice())
        .map_err(|e| format!("Failed to update user token: {}", e))?;

    Ok(())
}

/// Renew a token
pub fn renew_token(id: &str, expires_type: &str) -> Result<(), String> {
    let conn = connect_db()?;
    let now = Utc::now().timestamp();

    let expires_at = match expires_type {
        "day" => Some(
            Utc::now()
                .checked_add_signed(chrono::Duration::days(1))
                .unwrap()
                .timestamp(),
        ),
        "week" => Some(
            Utc::now()
                .checked_add_signed(chrono::Duration::weeks(1))
                .unwrap()
                .timestamp(),
        ),
        "month" => Some(
            Utc::now()
                .checked_add_signed(chrono::Duration::days(30))
                .unwrap()
                .timestamp(),
        ),
        _ => None, // "never" or other
    };

    conn.execute(
        "UPDATE user_tokens SET expires_type = ?1, expires_at = ?2, updated_at = ?3, enabled = 1 WHERE id = ?4",
        params![expires_type, expires_at, now, id],
    ).map_err(|e| format!("Failed to renew token: {}", e))?;

    Ok(())
}

/// Delete a token
pub fn delete_token(id: &str) -> Result<(), String> {
    let mut conn = connect_db()?;
    let tx = conn
        .transaction_with_behavior(TransactionBehavior::Immediate)
        .map_err(|e| e.to_string())?;
    let reference: Option<Option<String>> = tx
        .query_row(
            "SELECT credential_store_ref FROM user_tokens WHERE id=?1",
            [id],
            |row| row.get(0),
        )
        .optional()
        .map_err(|e| e.to_string())?;
    tx.execute("DELETE FROM user_tokens WHERE id = ?1", params![id])
        .map_err(|e| format!("Failed to delete token: {}", e))?;
    tx.commit()
        .map_err(|e| format!("Failed to revoke device key: {e}"))?;
    if let Some(Some(reference)) = reference {
        let reference: VaultRef = serde_json::from_str(&reference).map_err(|_| {
            "Device key revoked; its malformed native-store reference needs manual cleanup"
        })?;
        credential_store::delete(DEVICE_KEY_SCOPE, &reference).map_err(|_| {
            format!(
                "Device key revoked; protected native entry cleanup failed. Cleanup reference: {}",
                serde_json::to_string(&reference).unwrap_or_default()
            )
        })?;
    }
    Ok(())
}

/// Get all IP bindings for a token
pub fn get_token_ips(token_id: &str) -> Result<Vec<TokenIpBinding>, String> {
    let conn = connect_db()?;
    let mut stmt = conn
        .prepare("SELECT * FROM token_ip_bindings WHERE token_id = ?1 ORDER BY last_seen_at DESC")
        .map_err(|e| format!("Failed to prepare query: {}", e))?;

    let iter = stmt
        .query_map(params![token_id], |row| {
            Ok(TokenIpBinding {
                id: row.get("id")?,
                token_id: row.get("token_id")?,
                ip_address: row.get("ip_address")?,
                first_seen_at: row.get("first_seen_at")?,
                last_seen_at: row.get("last_seen_at")?,
                request_count: row.get("request_count")?,
                user_agent: row.get("user_agent")?,
            })
        })
        .map_err(|e| format!("Failed to query token IPs: {}", e))?;

    let mut bindings = Vec::new();
    for b in iter {
        bindings.push(b.map_err(|e| format!("Failed to parse binding row: {}", e))?);
    }

    Ok(bindings)
}

/// Record/update token usage (handles both user_tokens and token_ip_bindings)
pub fn record_token_usage_and_ip(
    token_id: &str,
    ip: &str,
    model: &str,
    input_tokens: i32,
    output_tokens: i32,
    status: u16,
    user_agent: Option<String>,
) -> Result<(), String> {
    let mut conn = connect_db()?;
    let tx = conn
        .transaction()
        .map_err(|e| format!("Failed to create transaction: {}", e))?;
    let now = Utc::now().timestamp();

    // 1. Update the main user_tokens table
    tx.execute(
        "UPDATE user_tokens SET 
            last_used_at = ?1, 
            total_requests = total_requests + 1, 
            total_tokens_used = total_tokens_used + ?2 
        WHERE id = ?3",
        params![now, input_tokens + output_tokens, token_id],
    )
    .map_err(|e| format!("Failed to update user_tokens stats: {}", e))?;

    // 2. Update or insert into the token_ip_bindings table
    let binding_exists: bool = tx.query_row(
        "SELECT EXISTS(SELECT 1 FROM token_ip_bindings WHERE token_id = ?1 AND ip_address = ?2)",
        params![token_id, ip],
        |row| row.get(0),
    ).unwrap_or(false);

    if binding_exists {
        tx.execute(
            "UPDATE token_ip_bindings SET 
                last_seen_at = ?1, 
                request_count = request_count + 1,
                user_agent = COALESCE(?2, user_agent)
            WHERE token_id = ?3 AND ip_address = ?4",
            params![now, user_agent, token_id, ip],
        )
        .map_err(|e| format!("Failed to update ip binding: {}", e))?;
    } else {
        let binding_id = Uuid::new_v4().to_string();
        tx.execute(
            "INSERT INTO token_ip_bindings (
                id, token_id, ip_address, first_seen_at, last_seen_at, request_count, user_agent
            ) VALUES (?1, ?2, ?3, ?4, ?5, 1, ?6)",
            params![binding_id, token_id, ip, now, now, user_agent],
        )
        .map_err(|e| format!("Failed to insert ip binding: {}", e))?;
    }

    // 3. Insert into the token_usage_logs table
    let log_id = Uuid::new_v4().to_string();
    tx.execute(
        "INSERT INTO token_usage_logs (
            id, token_id, ip_address, model, input_tokens, output_tokens, request_time, status
        ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)",
        params![
            log_id,
            token_id,
            ip,
            model,
            input_tokens,
            output_tokens,
            now,
            status
        ],
    )
    .map_err(|e| format!("Failed to insert usage log: {}", e))?;

    tx.commit()
        .map_err(|e| format!("Failed to commit transaction: {}", e))?;

    Ok(())
}

/// The admitted identity comes from the same transaction that checks its policy.
pub enum TokenAdmission {
    Admitted(UserToken),
    Rejected(String),
}

/// Admit a user token and reserve its IP before any downstream work can begin.
///
/// Reservations are durable bindings, including when the downstream request fails
/// or is cancelled. They last until an administrator removes the binding or
/// replaces the token; they are not concurrent-request permits. Admission creates
/// a zero-usage row, while response accounting alone increments request/token totals.
/// Run on a blocking worker when called from async request handling.
pub fn admit_token(token_str: &str, ip: &str) -> Result<TokenAdmission, String> {
    let mut conn = connect_db()?;
    admit_token_on_connection(&mut conn, token_str, ip, Utc::now)
}

fn admit_token_on_connection(
    conn: &mut Connection,
    token_str: &str,
    ip: &str,
    clock: impl FnOnce() -> chrono::DateTime<Utc>,
) -> Result<TokenAdmission, String> {
    // Serialize policy reads and capacity reservation with token updates and with
    // other admissions, across connections/processes rather than a process mutex.
    let tx = conn
        .transaction_with_behavior(TransactionBehavior::Immediate)
        .map_err(|e| format!("Failed to start token admission: {}", e))?;
    let token = tx
        .query_row(
            "SELECT * FROM user_tokens WHERE token = ?1 AND credential_store_ref IS NOT NULL",
            params![key_digest(token_str)],
            user_token_from_row,
        )
        .optional()
        .map_err(|e| format!("Failed to load token admission policy: {}", e))?;
    let Some(token) = token else {
        return Ok(TokenAdmission::Rejected(
            "Invalid token. Please check your API key.".into(),
        ));
    };
    if !token.enabled {
        return Ok(TokenAdmission::Rejected(
            "Your token is disabled. Please contact the administrator.".into(),
        ));
    }

    // Read the clock after acquiring the write transaction, so lock contention
    // cannot authorize a token using an expiry check performed before the wait.
    let now = clock();
    if token.expires_type != "never"
        && token
            .expires_at
            .is_some_and(|expiry| expiry < now.timestamp())
    {
        return Ok(TokenAdmission::Rejected(
            "Your token has expired. Please contact the administrator to renew it.".into(),
        ));
    }
    if let (Some(start), Some(end)) = (&token.curfew_start, &token.curfew_end) {
        if !start.is_empty() && !end.is_empty() {
            let beijing = now.with_timezone(&FixedOffset::east_opt(8 * 3600).unwrap());
            let current = format!("{:02}:{:02}", beijing.hour(), beijing.minute());
            let in_curfew = if start > end {
                current >= *start || current < *end
            } else {
                current >= *start && current < *end
            };
            if in_curfew {
                return Ok(TokenAdmission::Rejected(format!(
                    "Service is not available between {} and {} Beijing Time (Curfew enabled). Current Beijing time: {}",
                    start, end, current,
                )));
            }
        }
    }

    let is_bound: bool = tx.query_row(
        "SELECT EXISTS(SELECT 1 FROM token_ip_bindings WHERE token_id = ?1 AND ip_address = ?2)",
        params![token.id, ip], |row| row.get(0),
    ).map_err(|e| format!("Failed to check token IP binding: {}", e))?;
    if !is_bound {
        if token.max_ips > 0 {
            let count: i64 = tx
                .query_row(
                    "SELECT COUNT(*) FROM token_ip_bindings WHERE token_id = ?1",
                    params![token.id],
                    |row| row.get(0),
                )
                .map_err(|e| format!("Failed to check token IP capacity: {}", e))?;
            if count >= i64::from(token.max_ips) {
                return Ok(TokenAdmission::Rejected(format!(
                    "IP limit reached ({}/{}). Please contact the administrator to increase the limit.",
                    count, token.max_ips,
                )));
            }
        }
        tx.execute(
            "INSERT INTO token_ip_bindings
                (id, token_id, ip_address, first_seen_at, last_seen_at, request_count)
             VALUES (?1, ?2, ?3, ?4, ?4, 0)",
            params![Uuid::new_v4().to_string(), token.id, ip, now.timestamp()],
        )
        .map_err(|e| format!("Failed to reserve token IP capacity: {}", e))?;
    }
    tx.commit()
        .map_err(|e| format!("Failed to commit token admission: {}", e))?;
    Ok(TokenAdmission::Admitted(token))
}

/// Compatibility entry point: validation also durably reserves the admitted IP.
/// Prefer `admit_token` to obtain the identity without a second database lookup.
pub fn validate_token(token_str: &str, ip: &str) -> Result<(bool, Option<String>), String> {
    match admit_token(token_str, ip)? {
        TokenAdmission::Admitted(_) => Ok((true, None)),
        TokenAdmission::Rejected(reason) => Ok((false, Some(reason))),
    }
}

/// Get the username associated with an IP (used by the IP management page)
/// Returns the username of the Token that most recently used this IP
pub fn get_username_for_ip(ip: &str) -> Result<Option<String>, String> {
    let conn = connect_db()?;
    let result: Option<String> = conn
        .query_row(
            "SELECT t.username
         FROM token_ip_bindings b 
         JOIN user_tokens t ON b.token_id = t.id 
         WHERE b.ip_address = ?1 
         ORDER BY b.last_seen_at DESC 
         LIMIT 1",
            params![ip],
            |row| row.get(0),
        )
        .optional()
        .map_err(|e| format!("Failed to query username by ip: {}", e))?;

    Ok(result)
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct UserTokenDetailedSummary {
    pub total_requests: i64,
    pub total_input_tokens: i64,
    pub total_output_tokens: i64,
    pub total_tokens: i64,
    pub success_requests: i64,
    pub failed_requests: i64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct UserTokenTrendPoint {
    pub timestamp_label: String,
    pub input_tokens: i64,
    pub output_tokens: i64,
    pub total_tokens: i64,
    pub requests: i64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct UserTokenModelStat {
    pub model: String,
    pub input_tokens: i64,
    pub output_tokens: i64,
    pub total_tokens: i64,
    pub requests: i64,
    pub percentage: f64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct UserTokenDetailedStats {
    pub token: UserToken,
    pub summary: UserTokenDetailedSummary,
    pub trend: Vec<UserTokenTrendPoint>,
    pub models: Vec<UserTokenModelStat>,
    pub recent_ips: Vec<TokenIpBinding>,
}

/// 查询单个 User Token 的详细统计数据
pub fn get_token_detailed_stats(
    token_id: &str,
    time_range: &str,
) -> Result<UserTokenDetailedStats, String> {
    let token = get_token_by_id(token_id)?
        .ok_or_else(|| format!("User token with id {} not found", token_id))?;

    let conn = connect_db()?;
    let now = Utc::now().timestamp();

    let (start_time, time_format) = match time_range {
        "24h" => (now - 86400, "%m-%d %H:00"),
        "7d" => (now - 7 * 86400, "%m-%d"),
        "30d" => (now - 30 * 86400, "%m-%d"),
        _ => (0, "%Y-%m-%d"),
    };

    // 1. 查询概览指标
    let summary: UserTokenDetailedSummary = conn
        .query_row(
            "SELECT 
                COUNT(*),
                COALESCE(SUM(input_tokens), 0),
                COALESCE(SUM(output_tokens), 0),
                COALESCE(SUM(input_tokens + output_tokens), 0),
                COALESCE(SUM(CASE WHEN status >= 200 AND status < 400 THEN 1 ELSE 0 END), 0),
                COALESCE(SUM(CASE WHEN status >= 400 THEN 1 ELSE 0 END), 0)
            FROM token_usage_logs
            WHERE token_id = ?1 AND request_time >= ?2",
            params![token_id, start_time],
            |row| {
                Ok(UserTokenDetailedSummary {
                    total_requests: row.get(0)?,
                    total_input_tokens: row.get(1)?,
                    total_output_tokens: row.get(2)?,
                    total_tokens: row.get(3)?,
                    success_requests: row.get(4)?,
                    failed_requests: row.get(5)?,
                })
            },
        )
        .map_err(|e| format!("Failed to query token summary: {}", e))?;

    // 2. 查询时间走势
    let mut trend_stmt = conn
        .prepare(
            "SELECT 
                strftime(?1, datetime(request_time, 'unixepoch', 'localtime')) as time_group,
                COALESCE(SUM(input_tokens), 0),
                COALESCE(SUM(output_tokens), 0),
                COALESCE(SUM(input_tokens + output_tokens), 0),
                COUNT(*)
            FROM token_usage_logs
            WHERE token_id = ?2 AND request_time >= ?3
            GROUP BY time_group
            ORDER BY time_group ASC",
        )
        .map_err(|e| format!("Failed to prepare trend query: {}", e))?;

    let trend_rows = trend_stmt
        .query_map(params![time_format, token_id, start_time], |row| {
            Ok(UserTokenTrendPoint {
                timestamp_label: row.get(0)?,
                input_tokens: row.get(1)?,
                output_tokens: row.get(2)?,
                total_tokens: row.get(3)?,
                requests: row.get(4)?,
            })
        })
        .map_err(|e| format!("Failed to execute trend query: {}", e))?;

    let mut trend = Vec::new();
    for row in trend_rows {
        if let Ok(point) = row {
            trend.push(point);
        }
    }

    // 3. 查询模型分布
    let mut model_stmt = conn
        .prepare(
            "SELECT 
                COALESCE(NULLIF(model, ''), 'unknown') as model_name,
                COALESCE(SUM(input_tokens), 0),
                COALESCE(SUM(output_tokens), 0),
                COALESCE(SUM(input_tokens + output_tokens), 0) as total_toks,
                COUNT(*)
            FROM token_usage_logs
            WHERE token_id = ?1 AND request_time >= ?2
            GROUP BY model_name
            ORDER BY total_toks DESC",
        )
        .map_err(|e| format!("Failed to prepare model query: {}", e))?;

    let model_rows = model_stmt
        .query_map(params![token_id, start_time], |row| {
            let model: String = row.get(0)?;
            let input_tokens: i64 = row.get(1)?;
            let output_tokens: i64 = row.get(2)?;
            let total_tokens: i64 = row.get(3)?;
            let requests: i64 = row.get(4)?;
            Ok((model, input_tokens, output_tokens, total_tokens, requests))
        })
        .map_err(|e| format!("Failed to execute model query: {}", e))?;

    let mut models_raw = Vec::new();
    let mut grand_total_tokens: i64 = 0;
    for row in model_rows {
        if let Ok((model, input_tokens, output_tokens, total_tokens, requests)) = row {
            grand_total_tokens += total_tokens;
            models_raw.push((model, input_tokens, output_tokens, total_tokens, requests));
        }
    }

    let models = models_raw
        .into_iter()
        .map(
            |(model, input_tokens, output_tokens, total_tokens, requests)| {
                let percentage = if grand_total_tokens > 0 {
                    (total_tokens as f64 / grand_total_tokens as f64) * 100.0
                } else {
                    0.0
                };
                UserTokenModelStat {
                    model,
                    input_tokens,
                    output_tokens,
                    total_tokens,
                    requests,
                    percentage: (percentage * 10.0).round() / 10.0,
                }
            },
        )
        .collect();

    // 4. 获取前 5 个最近 IP 绑定
    let all_ips = get_token_ips(token_id).unwrap_or_default();
    let recent_ips = all_ips.into_iter().take(5).collect();

    Ok(UserTokenDetailedStats {
        token,
        summary,
        trend,
        models,
        recent_ips,
    })
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct UserTokenRankItem {
    pub id: String,
    pub token: String,
    pub username: String,
    pub description: Option<String>,
    pub enabled: bool,
    pub expires_type: String,
    pub expires_at: Option<i64>,
    pub max_ips: i32,
    pub total_tokens: i64,
    pub input_tokens: i64,
    pub output_tokens: i64,
    pub requests: i64,
    pub percentage: f64,
}

/// 查询用户 Token 的消耗排行榜
pub fn get_user_tokens_ranking(time_range: &str) -> Result<Vec<UserTokenRankItem>, String> {
    let conn = connect_db()?;
    let now = Utc::now().timestamp();

    let start_time = match time_range {
        "minute" => now - 3600,
        "hourly" | "24h" => now - 86400,
        "daily" | "7d" => now - 7 * 86400,
        "weekly" | "30d" => now - 30 * 86400,
        _ => 0,
    };

    let mut stmt = conn
        .prepare(
            "SELECT 
                t.id,
                COALESCE(t.token_preview, '<protected key>'),
                t.username,
                t.description,
                t.enabled,
                t.expires_type,
                t.expires_at,
                t.max_ips,
                t.total_tokens_used,
                t.total_requests,
                COALESCE(SUM(l.input_tokens + l.output_tokens), 0) as period_tokens,
                COALESCE(SUM(l.input_tokens), 0) as period_in,
                COALESCE(SUM(l.output_tokens), 0) as period_out,
                COUNT(l.id) as period_requests
            FROM user_tokens t
            LEFT JOIN token_usage_logs l ON t.id = l.token_id AND l.request_time >= ?1
            GROUP BY t.id
            ORDER BY period_tokens DESC, t.total_tokens_used DESC",
        )
        .map_err(|e| format!("Failed to prepare ranking query: {}", e))?;

    let rows = stmt
        .query_map(params![start_time], |row| {
            let id: String = row.get(0)?;
            let token: String = row.get(1)?;
            let username: String = row.get(2)?;
            let description: Option<String> = row.get(3)?;
            let enabled: bool = row.get(4)?;
            let expires_type: String = row.get(5)?;
            let expires_at: Option<i64> = row.get(6)?;
            let max_ips: i32 = row.get(7)?;
            let legacy_tokens: i64 = row.get(8)?;
            let legacy_requests: i64 = row.get(9)?;
            let period_tokens: i64 = row.get(10)?;
            let period_in: i64 = row.get(11)?;
            let period_out: i64 = row.get(12)?;
            let period_requests: i64 = row.get(13)?;

            // 若选全部历史且日志尚未覆盖历史总额，回退到 legacy 累计值
            let (total_tokens, input_tokens, output_tokens, requests) =
                if start_time == 0 && period_tokens == 0 && legacy_tokens > 0 {
                    (legacy_tokens, legacy_tokens, 0, legacy_requests)
                } else {
                    (period_tokens, period_in, period_out, period_requests)
                };

            Ok(UserTokenRankItem {
                id,
                token,
                username,
                description,
                enabled,
                expires_type,
                expires_at,
                max_ips,
                total_tokens,
                input_tokens,
                output_tokens,
                requests,
                percentage: 0.0,
            })
        })
        .map_err(|e| format!("Failed to execute ranking query: {}", e))?;

    let mut list = Vec::new();
    let mut grand_total: i64 = 0;
    for r in rows {
        if let Ok(item) = r {
            grand_total += item.total_tokens;
            list.push(item);
        }
    }

    for item in &mut list {
        if grand_total > 0 {
            item.percentage =
                ((item.total_tokens as f64 / grand_total as f64) * 1000.0).round() / 10.0;
        }
    }

    Ok(list)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn missing_vault_reference_never_turns_a_database_hash_into_a_bearer_key() {
        let _data = crate::proxy::monitor::prompt_log_tests::TestDataDir::new();
        init_db().unwrap(); let token=admission_fixture(0); let mut conn=connect_db().unwrap();
        let (digest, reference):(String,String)=conn.query_row("SELECT token,credential_store_ref FROM user_tokens WHERE id=?1",[&token.id],|row|Ok((row.get(0)?,row.get(1)?))).unwrap();
        conn.execute("UPDATE user_tokens SET credential_store_ref=NULL WHERE id=?1",[&token.id]).unwrap();
        assert!(migrate_legacy_device_keys(&mut conn).is_err());
        for value in [&digest,&token.token] {
            assert!(matches!(admit_token(value,"192.0.2.1").unwrap(),TokenAdmission::Rejected(_)));
            assert!(get_token_by_value(value).unwrap().is_none());
        }
        credential_store::delete(DEVICE_KEY_SCOPE,&serde_json::from_str(&reference).unwrap()).unwrap();
    }

    #[test]
    fn device_keys_are_protected_revealable_and_independently_revocable() {
        let _data = crate::proxy::monitor::prompt_log_tests::TestDataDir::new();
        init_db().unwrap();
        let first = admission_fixture(0);
        let second = admission_fixture(0);
        let conn = connect_db().unwrap();
        let (stored, reference): (String, String) = conn
            .query_row(
                "SELECT token,credential_store_ref FROM user_tokens WHERE id=?1",
                [&first.id],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .unwrap();
        assert_eq!(stored, key_digest(&first.token));
        assert_ne!(stored, first.token);
        assert_eq!(reveal_token(&first.id).unwrap(), first.token);
        assert!(list_tokens()
            .unwrap()
            .iter()
            .all(|token| token.token != first.token && token.token != second.token));
        assert!(
            !serde_json::to_string(&get_user_tokens_ranking("all").unwrap())
                .unwrap()
                .contains(&first.token)
        );
        assert!(matches!(
            admit_token(&first.token, "192.0.2.1").unwrap(),
            TokenAdmission::Admitted(_)
        ));
        update_token(&first.id, None, None, Some(false), None, None, None).unwrap();
        assert!(matches!(
            admit_token(&first.token, "192.0.2.1").unwrap(),
            TokenAdmission::Rejected(_)
        ));
        assert!(matches!(
            admit_token(&second.token, "192.0.2.1").unwrap(),
            TokenAdmission::Admitted(_)
        ));
        delete_token(&first.id).unwrap();
        let reference: VaultRef = serde_json::from_str(&reference).unwrap();
        assert!(credential_store::load_json(DEVICE_KEY_SCOPE, &first.id, &reference).is_err());
        assert!(reveal_token(&first.id).is_err());
    }

    #[test]
    fn legacy_device_key_migration_preserves_client_values_and_policies() {
        let _data = crate::proxy::monitor::prompt_log_tests::TestDataDir::new();
        init_db().unwrap();
        let mut conn = connect_db().unwrap();
        let original = "sk-legacy-synthetic-credential-for-migration";
        conn.execute("INSERT INTO user_tokens (id,token,username,enabled,expires_type,created_at,updated_at,max_ips)
            VALUES ('legacy-device',?1,'Legacy device',1,'never',1,1,1)",[original]).unwrap();
        conn.execute("INSERT INTO token_ip_bindings (id,token_id,ip_address,first_seen_at,last_seen_at) VALUES
            ('old-binding','legacy-device','192.0.2.7',1,1)",[]).unwrap();
        assert_eq!(migrate_legacy_device_keys(&mut conn).unwrap(), 1);
        assert_eq!(migrate_legacy_device_keys(&mut conn).unwrap(), 0);
        let stored: String = conn
            .query_row(
                "SELECT token FROM user_tokens WHERE id='legacy-device'",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(stored, key_digest(original));
        assert_eq!(reveal_token("legacy-device").unwrap(), original);
        assert!(matches!(
            admit_token(original, "192.0.2.7").unwrap(),
            TokenAdmission::Admitted(_)
        ));
        assert!(matches!(
            admit_token(original, "192.0.2.8").unwrap(),
            TokenAdmission::Rejected(_)
        ));
    }

    #[test]
    fn failed_device_key_migration_keeps_the_existing_client_credential_recoverable() {
        let _data = crate::proxy::monitor::prompt_log_tests::TestDataDir::new();
        init_db().unwrap();
        let mut conn = connect_db().unwrap();
        let original = "sk-synthetic-existing-client-credential";
        conn.execute(
            "INSERT INTO user_tokens (id,token,username,expires_type,created_at,updated_at)
            VALUES ('rollback-device',?1,'Rollback','never',1,1)",
            [original],
        )
        .unwrap();
        conn.execute_batch("CREATE TRIGGER reject_device_migration BEFORE UPDATE ON user_tokens BEGIN SELECT RAISE(ABORT,'fixture write failure'); END;").unwrap();
        assert!(migrate_legacy_device_keys(&mut conn).is_err());
        let stored: String = conn
            .query_row(
                "SELECT token FROM user_tokens WHERE id='rollback-device'",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(stored, original);
        conn.execute_batch("DROP TRIGGER reject_device_migration")
            .unwrap();
        assert!(matches!(
            admit_token(original, "192.0.2.1").unwrap(),
            TokenAdmission::Rejected(_)
        ));
        assert_eq!(migrate_legacy_device_keys(&mut conn).unwrap(), 1);
        assert_eq!(reveal_token("rollback-device").unwrap(), original);
        assert!(matches!(admit_token(original,"192.0.2.1").unwrap(),TokenAdmission::Admitted(_)));
    }

    #[test]
    fn native_cleanup_errors_do_not_undo_device_key_revocation() {
        let _data = crate::proxy::monitor::prompt_log_tests::TestDataDir::new();
        init_db().unwrap();
        let token = admission_fixture(0);
        let conn = connect_db().unwrap();
        let reference: String = conn
            .query_row(
                "SELECT credential_store_ref FROM user_tokens WHERE id=?1",
                [&token.id],
                |row| row.get(0),
            )
            .unwrap();
        conn.execute(
            "UPDATE user_tokens SET credential_store_ref='{}' WHERE id=?1",
            [&token.id],
        )
        .unwrap();
        assert!(delete_token(&token.id).unwrap_err().contains("revoked"));
        assert!(matches!(
            admit_token(&token.token, "192.0.2.1").unwrap(),
            TokenAdmission::Rejected(_)
        ));
        credential_store::delete(DEVICE_KEY_SCOPE, &serde_json::from_str(&reference).unwrap())
            .unwrap();
    }

    fn admission_fixture(max_ips: i32) -> UserToken {
        create_token(
            "admission-fixture".into(),
            "never".into(),
            None,
            max_ips,
            None,
            None,
            None,
        )
        .unwrap()
    }

    #[test]
    fn admission_revokes_disabled_tokens_without_hiding_admin_lookup() {
        let _data = crate::proxy::monitor::prompt_log_tests::TestDataDir::new();
        init_db().unwrap();
        let token = admission_fixture(1);
        assert!(matches!(
            admit_token(&token.token, "192.0.2.1").unwrap(),
            TokenAdmission::Admitted(_)
        ));
        update_token(&token.id, None, None, Some(false), None, None, None).unwrap();
        assert!(
            matches!(admit_token(&token.token, "192.0.2.1").unwrap(), TokenAdmission::Rejected(reason) if reason.contains("disabled"))
        );
        assert!(!get_token_by_value(&token.token).unwrap().unwrap().enabled);
        assert_eq!(get_token_ips(&token.id).unwrap().len(), 1);
        update_token(&token.id, None, None, Some(true), None, None, None).unwrap();
        assert!(
            matches!(admit_token(&token.token, "192.0.2.1").unwrap(), TokenAdmission::Admitted(identity) if identity.id == token.id)
        );
    }

    #[test]
    fn admission_concurrent_new_ips_cannot_overbook_capacity() {
        let _data = crate::proxy::monitor::prompt_log_tests::TestDataDir::new();
        init_db().unwrap();
        let token = admission_fixture(1);
        let barrier = std::sync::Arc::new(std::sync::Barrier::new(9));
        let results = std::thread::scope(|scope| {
            let workers: Vec<_> = (1..=8)
                .map(|index| {
                    let barrier = barrier.clone();
                    let value = token.token.clone();
                    scope.spawn(move || {
                        barrier.wait();
                        admit_token(&value, &format!("192.0.2.{index}"))
                    })
                })
                .collect();
            barrier.wait();
            workers
                .into_iter()
                .map(|worker| worker.join().unwrap().unwrap())
                .collect::<Vec<_>>()
        });
        assert_eq!(
            results
                .iter()
                .filter(|result| matches!(result, TokenAdmission::Admitted(_)))
                .count(),
            1
        );
        assert_eq!(
            results
                .iter()
                .filter(|result| matches!(result, TokenAdmission::Rejected(_)))
                .count(),
            7
        );
        let bindings = get_token_ips(&token.id).unwrap();
        assert_eq!(bindings.len(), 1);
        // No response was accounted: failed/cancelled calls retain their reserved slot.
        assert_eq!(bindings[0].request_count, 0);
        assert_eq!(
            get_token_by_id(&token.id).unwrap().unwrap().total_requests,
            0
        );
        assert!(matches!(
            admit_token(&token.token, &bindings[0].ip_address).unwrap(),
            TokenAdmission::Admitted(_)
        ));
    }

    #[test]
    fn admission_same_ip_is_idempotent_and_usage_is_accounted_separately() {
        let _data = crate::proxy::monitor::prompt_log_tests::TestDataDir::new();
        init_db().unwrap();
        let token = admission_fixture(1);
        let barrier = std::sync::Arc::new(std::sync::Barrier::new(7));
        std::thread::scope(|scope| {
            let workers: Vec<_> = (0..6)
                .map(|_| {
                    let barrier = barrier.clone();
                    let value = token.token.clone();
                    scope.spawn(move || {
                        barrier.wait();
                        assert!(matches!(
                            admit_token(&value, "2001:db8::1").unwrap(),
                            TokenAdmission::Admitted(_)
                        ));
                    })
                })
                .collect();
            barrier.wait();
            for worker in workers {
                worker.join().unwrap();
            }
        });
        let bindings = get_token_ips(&token.id).unwrap();
        assert_eq!(bindings.len(), 1);
        assert_eq!(bindings[0].request_count, 0);
        record_token_usage_and_ip(
            &token.id,
            "2001:db8::1",
            "fixture-model",
            7,
            5,
            502,
            Some("fixture-agent".into()),
        )
        .unwrap();
        let saved = get_token_by_id(&token.id).unwrap().unwrap();
        assert_eq!(saved.total_requests, 1);
        assert_eq!(saved.total_tokens_used, 12);
        let bindings = get_token_ips(&token.id).unwrap();
        assert_eq!(bindings.len(), 1);
        assert_eq!(bindings[0].request_count, 1);
        assert_eq!(bindings[0].user_agent.as_deref(), Some("fixture-agent"));
    }

    #[test]
    fn admission_preserves_existing_bindings_when_limit_is_lowered() {
        let _data = crate::proxy::monitor::prompt_log_tests::TestDataDir::new();
        init_db().unwrap();
        let token = admission_fixture(2);
        for ip in ["192.0.2.1", "192.0.2.2"] {
            assert!(matches!(
                admit_token(&token.token, ip).unwrap(),
                TokenAdmission::Admitted(_)
            ));
        }
        update_token(&token.id, None, None, None, Some(1), None, None).unwrap();
        for ip in ["192.0.2.1", "192.0.2.2"] {
            assert!(matches!(
                admit_token(&token.token, ip).unwrap(),
                TokenAdmission::Admitted(_)
            ));
        }
        assert!(
            matches!(admit_token(&token.token, "192.0.2.3").unwrap(), TokenAdmission::Rejected(reason) if reason.contains("IP limit"))
        );
    }

    #[test]
    fn admission_expiry_uses_transaction_clock_and_preserves_never_policy() {
        use chrono::TimeZone;
        let _data = crate::proxy::monitor::prompt_log_tests::TestDataDir::new();
        init_db().unwrap();
        let token = admission_fixture(1);
        let now = Utc.with_ymd_and_hms(2026, 1, 2, 12, 0, 0).unwrap();
        let mut conn = connect_db().unwrap();
        conn.execute(
            "UPDATE user_tokens SET expires_type = 'day', expires_at = ?1 WHERE id = ?2",
            params![now.timestamp() - 1, token.id],
        )
        .unwrap();
        assert!(
            matches!(admit_token_on_connection(&mut conn, &token.token, "192.0.2.1", || now).unwrap(), TokenAdmission::Rejected(reason) if reason.contains("expired"))
        );
        assert!(get_token_ips(&token.id).unwrap().is_empty());
        conn.execute(
            "UPDATE user_tokens SET expires_at = ?1 WHERE id = ?2",
            params![now.timestamp() + 60, token.id],
        )
        .unwrap();
        assert!(matches!(
            admit_token_on_connection(&mut conn, &token.token, "192.0.2.1", || now).unwrap(),
            TokenAdmission::Admitted(_)
        ));
        conn.execute(
            "UPDATE user_tokens SET expires_type = 'never', expires_at = 1 WHERE id = ?1",
            params![token.id],
        )
        .unwrap();
        assert!(matches!(
            admit_token_on_connection(&mut conn, &token.token, "192.0.2.1", || now).unwrap(),
            TokenAdmission::Admitted(_)
        ));
    }

    #[test]
    fn admission_curfew_uses_beijing_time_with_midnight_and_boundary_rules() {
        use chrono::TimeZone;
        let _data = crate::proxy::monitor::prompt_log_tests::TestDataDir::new();
        init_db().unwrap();
        for (start, end, utc_hour, blocked) in [
            ("09:00", "17:00", 1, true),
            ("09:00", "17:00", 9, false),
            ("23:00", "06:00", 15, true),
            ("23:00", "06:00", 21, true),
            ("23:00", "06:00", 22, false),
            ("09:00", "09:00", 1, false),
        ] {
            let token = create_token(
                "curfew-fixture".into(),
                "never".into(),
                None,
                1,
                Some(start.into()),
                Some(end.into()),
                None,
            )
            .unwrap();
            let now = Utc.with_ymd_and_hms(2026, 1, 2, utc_hour, 0, 0).unwrap();
            let result = admit_token_on_connection(
                &mut connect_db().unwrap(),
                &token.token,
                "192.0.2.1",
                || now,
            )
            .unwrap();
            assert_eq!(
                matches!(result, TokenAdmission::Rejected(_)),
                blocked,
                "curfew {start}..{end}, UTC hour {utc_hour}"
            );
            assert_eq!(
                get_token_ips(&token.id).unwrap().len(),
                usize::from(!blocked)
            );
        }
    }

    #[test]
    fn admission_fails_closed_on_binding_read_or_reservation_errors() {
        let _data = crate::proxy::monitor::prompt_log_tests::TestDataDir::new();
        init_db().unwrap();
        let token = admission_fixture(1);
        let conn = connect_db().unwrap();
        conn.execute_batch("CREATE TRIGGER fail_reservation BEFORE INSERT ON token_ip_bindings BEGIN SELECT RAISE(ABORT, 'fixture reservation failed'); END;").unwrap();
        assert!(admit_token(&token.token, "192.0.2.1").is_err());
        assert!(get_token_ips(&token.id).unwrap().is_empty());
        conn.execute_batch("DROP TRIGGER fail_reservation; DROP TABLE token_ip_bindings;")
            .unwrap();
        assert!(admit_token(&token.token, "192.0.2.1").is_err());
        // An unknown token is rejected without reading or repairing binding state.
        assert!(matches!(
            admit_token("unrecognized", "192.0.2.1").unwrap(),
            TokenAdmission::Rejected(_)
        ));
    }

    #[test]
    fn test_create_and_query_token() {
        let _data = crate::proxy::monitor::prompt_log_tests::TestDataDir::new();
        init_db().unwrap();

        // Use a random username to avoid collisions in existing DB runs during dev
        let username = format!("TestUser_{}", Uuid::new_v4());
        let token_res = create_token(
            username.clone(),
            "day".to_string(),
            Some("Test token".to_string()),
            0,
            None,
            None,
            None,
        );
        assert!(token_res.is_ok());

        let token = token_res.unwrap();
        assert_eq!(token.username, username);
        assert!(token.token.starts_with("sk-"));

        let fetched = get_token_by_id(&token.id);
        assert!(fetched.is_ok());
        assert_eq!(fetched.unwrap().unwrap().username, username);
    }

    #[test]
    fn test_never_expire_token_validation() {
        let _data = crate::proxy::monitor::prompt_log_tests::TestDataDir::new();
        init_db().unwrap();
        let username = format!("NeverExpireUser_{}", Uuid::new_v4());
        let token_res = create_token(
            username.clone(),
            "never".to_string(),
            Some("Never expire test token".to_string()),
            0,
            None,
            None,
            None,
        );
        assert!(token_res.is_ok());
        let token = token_res.unwrap();
        let (valid, reason) =
            validate_token(&token.token, "127.0.0.1").expect("validation must succeed");
        assert!(
            valid,
            "Token with expires_type never must be valid, reason: {:?}",
            reason
        );
    }

    #[test]
    fn test_get_token_detailed_stats() {
        let _data = crate::proxy::monitor::prompt_log_tests::TestDataDir::new();
        init_db().unwrap();
        let username = format!("StatsUser_{}", Uuid::new_v4());
        let token_res = create_token(
            username.clone(),
            "month".to_string(),
            Some("Stats token".to_string()),
            0,
            None,
            None,
            None,
        );
        assert!(token_res.is_ok());
        let token = token_res.unwrap();

        // 记录一条使用用量
        let _ = record_token_usage_and_ip(
            &token.id,
            "127.0.0.1",
            "gemini-2.5-flash",
            100,
            200,
            200,
            Some("test-agent".to_string()),
        );

        let stats_res = get_token_detailed_stats(&token.id, "7d");
        assert!(stats_res.is_ok());
        let stats = stats_res.unwrap();
        assert_eq!(stats.summary.total_requests, 1);
        assert_eq!(stats.summary.total_input_tokens, 100);
        assert_eq!(stats.summary.total_output_tokens, 200);
        assert_eq!(stats.summary.total_tokens, 300);
        assert_eq!(stats.models.len(), 1);
        assert_eq!(stats.models[0].model, "gemini-2.5-flash");
        assert_eq!(stats.recent_ips.len(), 1);
        assert_eq!(stats.recent_ips[0].ip_address, "127.0.0.1");
    }
}
