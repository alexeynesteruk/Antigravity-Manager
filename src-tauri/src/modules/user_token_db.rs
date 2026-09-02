//! User Token Database Module
//! UserToken database operations module

#![allow(dead_code)]
// User token storage; some interfaces are reserved for future extension

use chrono::{FixedOffset, Timelike, Utc};
use rusqlite::{params, Connection, OptionalExtension};
use serde::{Deserialize, Serialize};
use std::path::PathBuf;
use uuid::Uuid;

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
    Ok(conn)
}

/// Initialize the database
pub fn init_db() -> Result<(), String> {
    let conn = connect_db()?;

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

    Ok(())
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
    let token = format!("sk-{}", Uuid::new_v4().to_string().replace("-", ""));
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
            created_at, updated_at, total_requests, total_tokens_used
        ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14)",
        params![
            user_token.id,
            user_token.token,
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
        ],
    )
    .map_err(|e| format!("Failed to insert user token: {}", e))?;

    Ok(user_token)
}

/// List all tokens
pub fn list_tokens() -> Result<Vec<UserToken>, String> {
    let conn = connect_db()?;
    let mut stmt = conn
        .prepare("SELECT * FROM user_tokens ORDER BY created_at DESC")
        .map_err(|e| format!("Failed to prepare query: {}", e))?;

    let token_iter = stmt
        .query_map([], |row| {
            Ok(UserToken {
                id: row.get("id")?,
                token: row.get("token")?,
                username: row.get("username")?,
                description: row.get("description")?,
                enabled: row.get("enabled").unwrap_or(true), // Defensive default value
                expires_type: row.get("expires_type").unwrap_or("never".to_string()), // Defensive default value
                expires_at: row.get("expires_at").unwrap_or(None),
                max_ips: row.get("max_ips").unwrap_or(0),
                curfew_start: row.get("curfew_start").unwrap_or(None),
                curfew_end: row.get("curfew_end").unwrap_or(None),
                created_at: row.get("created_at")?,
                updated_at: row.get("updated_at")?,
                last_used_at: row.get("last_used_at").unwrap_or(None),
                total_requests: row.get("total_requests").unwrap_or(0),
                total_tokens_used: row.get("total_tokens_used").unwrap_or(0),
            })
        })
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
        .query_row(params![id], |row| {
            Ok(UserToken {
                id: row.get("id")?,
                token: row.get("token")?,
                username: row.get("username")?,
                description: row.get("description")?,
                enabled: row.get("enabled")?,
                expires_type: row.get("expires_type")?,
                expires_at: row.get("expires_at")?,
                max_ips: row.get("max_ips")?,
                curfew_start: row.get("curfew_start").unwrap_or(None),
                curfew_end: row.get("curfew_end").unwrap_or(None),
                created_at: row.get("created_at")?,
                updated_at: row.get("updated_at")?,
                last_used_at: row.get("last_used_at")?,
                total_requests: row.get("total_requests")?,
                total_tokens_used: row.get("total_tokens_used")?,
            })
        })
        .optional()
        .map_err(|e| format!("Failed to query token: {}", e))?;

    Ok(token)
}

/// Get token information based on the Token value
pub fn get_token_by_value(token: &str) -> Result<Option<UserToken>, String> {
    let conn = connect_db()?;
    let mut stmt = conn
        .prepare("SELECT * FROM user_tokens WHERE token = ?1")
        .map_err(|e| format!("Failed to prepare query: {}", e))?;

    let token = stmt
        .query_row(params![token], |row| {
            Ok(UserToken {
                id: row.get("id")?,
                token: row.get("token")?,
                username: row.get("username")?,
                description: row.get("description")?,
                enabled: row.get("enabled")?,
                expires_type: row.get("expires_type")?,
                expires_at: row.get("expires_at")?,
                max_ips: row.get("max_ips")?,
                curfew_start: row.get("curfew_start").unwrap_or(None),
                curfew_end: row.get("curfew_end").unwrap_or(None),
                created_at: row.get("created_at")?,
                updated_at: row.get("updated_at")?,
                last_used_at: row.get("last_used_at")?,
                total_requests: row.get("total_requests")?,
                total_tokens_used: row.get("total_tokens_used")?,
            })
        })
        .optional()
        .map_err(|e| format!("Failed to query token: {}", e))?;

    Ok(token)
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
    let conn = connect_db()?;
    conn.execute("DELETE FROM user_tokens WHERE id = ?1", params![id])
        .map_err(|e| format!("Failed to delete token: {}", e))?;
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

/// Check whether the Token is valid (includes expiry check and IP limit check)
/// Returns: (whether valid, rejection reason)
pub fn validate_token(token_str: &str, ip: &str) -> Result<(bool, Option<String>), String> {
    let token_opt = get_token_by_value(token_str)?;

    if let Some(token) = token_opt {
        // 1. Check the expiry time
        if token.expires_type != "never" {
            if let Some(expires_at) = token.expires_at {
                if expires_at < Utc::now().timestamp() {
                    return Ok((
                        false,
                        Some(
                            "Your token has expired. Please contact the administrator to renew it."
                                .to_string(),
                        ),
                    ));
                }
            }
        }

        // 2. Check the IP limit
        if token.max_ips > 0 {
            let conn = connect_db()?;

            // Check whether the current IP is already bound
            let is_bound: bool = conn.query_row(
                "SELECT EXISTS(SELECT 1 FROM token_ip_bindings WHERE token_id = ?1 AND ip_address = ?2)",
                params![token.id, ip],
                |row| row.get(0)
            ).unwrap_or(false);

            if !is_bound {
                // If not bound, check whether the limit has been reached
                let current_ip_count: i32 = conn
                    .query_row(
                        "SELECT COUNT(*) FROM token_ip_bindings WHERE token_id = ?1",
                        params![token.id],
                        |row| row.get(0),
                    )
                    .unwrap_or(0);

                if current_ip_count >= token.max_ips {
                    return Ok((false, Some(format!("IP limit reached ({}/{}). Please contact the administrator to increase the limit.", current_ip_count, token.max_ips))));
                }
            }
        }

        // 3. Check the curfew time
        // Logic: reject if the current Beijing time falls between start and end
        // Format: HH:MM
        // Uses a fixed UTC+8 (Beijing time), independent of the server's local timezone
        if let (Some(start_str), Some(end_str)) = (&token.curfew_start, &token.curfew_end) {
            if !start_str.is_empty() && !end_str.is_empty() {
                let beijing_offset = FixedOffset::east_opt(8 * 3600).unwrap();
                let now_beijing = Utc::now().with_timezone(&beijing_offset);
                let current_time_str =
                    format!("{:02}:{:02}", now_beijing.hour(), now_beijing.minute());

                // Handling crossing midnight: start > end (e.g. 23:00 to 06:00)
                // Normal: start < end (e.g. 09:00 to 18:00)
                let is_curfew = if start_str > end_str {
                    current_time_str >= *start_str || current_time_str < *end_str
                } else {
                    current_time_str >= *start_str && current_time_str < *end_str
                };

                if is_curfew {
                    return Ok((false, Some(format!("Service is not available between {} and {} Beijing Time (Curfew enabled). Current Beijing time: {}", start_str, end_str, current_time_str))));
                }
            }
        }

        // Everything is fine, the Token is valid
        Ok((true, None))
    } else {
        Ok((
            false,
            Some("Invalid token. Please check your API key.".to_string()),
        ))
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_create_and_query_token() {
        let _ = init_db(); // Ensure DB is initialized

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
        let _ = init_db();
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
}
