use rusqlite::{params, Connection};
use serde::{Deserialize, Serialize};
use std::path::PathBuf;

/// Aggregated token statistics
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TokenStatsAggregated {
    pub period: String, // e.g., "2024-01-15 14:00" for hourly, "2024-01-15" for daily
    pub total_input_tokens: u64,
    pub total_output_tokens: u64,
    pub total_cached_tokens: u64,
    pub cache_reported_requests: u64,
    pub cache_reported_input_tokens: u64,
    pub cache_reported_tokens: u64,
    #[serde(default)]
    pub total_reasoning_tokens: u64,
    pub total_tokens: u64,
    pub total_reported_requests: u64,
    pub output_reported_requests: u64,
    pub input_reported_requests: u64,
    pub request_count: u64,
}

/// Per-account token statistics
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AccountTokenStats {
    pub account_email: String,
    pub total_input_tokens: u64,
    pub total_output_tokens: u64,
    pub total_cached_tokens: u64,
    pub cache_reported_requests: u64,
    pub cache_reported_input_tokens: u64,
    pub cache_reported_tokens: u64,
    #[serde(default)]
    pub total_reasoning_tokens: u64,
    pub total_tokens: u64,
    pub total_reported_requests: u64,
    pub output_reported_requests: u64,
    pub input_reported_requests: u64,
    pub request_count: u64,
}

/// Summary statistics
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TokenStatsSummary {
    pub total_input_tokens: u64,
    pub total_output_tokens: u64,
    pub total_cached_tokens: u64,
    pub cache_reported_requests: u64,
    pub cache_reported_input_tokens: u64,
    pub cache_reported_tokens: u64,
    #[serde(default)]
    pub total_reasoning_tokens: u64,
    pub total_tokens: u64,
    pub total_reported_requests: u64,
    pub output_reported_requests: u64,
    pub input_reported_requests: u64,
    pub total_requests: u64,
    pub unique_accounts: u64,
}

/// Per-model token statistics
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ModelTokenStats {
    pub model: String,
    pub total_input_tokens: u64,
    pub total_output_tokens: u64,
    pub total_cached_tokens: u64,
    pub cache_reported_requests: u64,
    pub cache_reported_input_tokens: u64,
    pub cache_reported_tokens: u64,
    #[serde(default)]
    pub total_reasoning_tokens: u64,
    pub total_tokens: u64,
    pub total_reported_requests: u64,
    pub output_reported_requests: u64,
    pub input_reported_requests: u64,
    pub request_count: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ModelTrendPoint {
    pub period: String,
    pub model_data: std::collections::HashMap<String, Option<u64>>,
}

/// Account trend data point (for stacked area chart)
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AccountTrendPoint {
    pub period: String,
    pub account_data: std::collections::HashMap<String, Option<u64>>,
}

pub(crate) fn get_db_path() -> Result<PathBuf, String> {
    let data_dir = crate::modules::account::get_data_dir()?;
    Ok(data_dir.join("token_stats.db"))
}

fn connect_db() -> Result<Connection, String> {
    let db_path = get_db_path()?;
    let conn = Connection::open(db_path).map_err(|e| e.to_string())?;

    // Enable WAL mode for better concurrency
    conn.pragma_update(None, "journal_mode", "WAL")
        .map_err(|e| e.to_string())?;
    conn.pragma_update(None, "busy_timeout", 5000)
        .map_err(|e| e.to_string())?;
    conn.pragma_update(None, "synchronous", "NORMAL")
        .map_err(|e| e.to_string())?;

    Ok(conn)
}

fn add_column_if_missing(conn: &Connection, table: &str, column_def: &str) -> Result<(), String> {
    let sql = format!("ALTER TABLE {} ADD COLUMN {}", table, column_def);
    match conn.execute(&sql, []) {
        Ok(_) => Ok(()),
        Err(e) if e.to_string().contains("duplicate column name") => Ok(()),
        Err(e) => Err(e.to_string()),
    }
}

/// Initialize the token stats database
pub fn init_db() -> Result<(), String> {
    let conn = connect_db()?;

    // Create main usage table
    conn.execute(
        "CREATE TABLE IF NOT EXISTS token_usage (
            id INTEGER PRIMARY KEY AUTOINCREMENT,
            timestamp INTEGER NOT NULL,
            account_email TEXT NOT NULL,
            model TEXT NOT NULL,
            input_tokens INTEGER,
            output_tokens INTEGER,
            cached_tokens INTEGER,
            total_tokens INTEGER
        )",
        [],
    )
    .map_err(|e| e.to_string())?;

    // Create indexes for efficient queries
    conn.execute(
        "CREATE INDEX IF NOT EXISTS idx_token_timestamp ON token_usage (timestamp DESC)",
        [],
    )
    .map_err(|e| e.to_string())?;

    conn.execute(
        "CREATE INDEX IF NOT EXISTS idx_token_account ON token_usage (account_email)",
        [],
    )
    .map_err(|e| e.to_string())?;

    let _ = conn.execute(
        "CREATE INDEX IF NOT EXISTS idx_token_account_timestamp ON token_usage (account_email, timestamp DESC)",
        [],
    );

    let _ = conn.execute(
        "CREATE INDEX IF NOT EXISTS idx_token_model ON token_usage (model)",
        [],
    );

    // Create hourly aggregation table for fast queries
    conn.execute(
        "CREATE TABLE IF NOT EXISTS token_stats_hourly (
            hour_bucket TEXT NOT NULL,
            account_email TEXT NOT NULL,
            total_input_tokens INTEGER NOT NULL DEFAULT 0,
            total_output_tokens INTEGER NOT NULL DEFAULT 0,
            total_cached_tokens INTEGER NOT NULL DEFAULT 0,
            total_tokens INTEGER NOT NULL DEFAULT 0,
            request_count INTEGER NOT NULL DEFAULT 0,
            PRIMARY KEY (hour_bucket, account_email)
        )",
        [],
    )
    .map_err(|e| e.to_string())?;

    let _ = conn.execute(
        "CREATE INDEX IF NOT EXISTS idx_hourly_account ON token_stats_hourly (account_email, hour_bucket)",
        [],
    );

    add_column_if_missing(
        &conn,
        "token_usage",
        "cached_tokens INTEGER NOT NULL DEFAULT 0",
    )?;
    add_column_if_missing(&conn, "token_usage", "reasoning_tokens INTEGER")?;
    add_column_if_missing(
        &conn,
        "token_stats_hourly",
        "total_cached_tokens INTEGER NOT NULL DEFAULT 0",
    )?;
    add_column_if_missing(
        &conn,
        "token_stats_hourly",
        "total_reasoning_tokens INTEGER NOT NULL DEFAULT 0",
    )?;

    add_column_if_missing(
        &conn,
        "token_usage",
        "cache_reported INTEGER NOT NULL DEFAULT 0",
    )?;
    // total_tokens 是历史 input + output 派生列；没有原件时不能当作实报总量。
    add_column_if_missing(&conn, "token_usage", "reported_total_tokens INTEGER")?;
    let input_not_null: bool = conn
        .query_row(
            "SELECT \"notnull\" FROM pragma_table_info('token_usage') WHERE name = 'input_tokens'",
            [],
            |row| row.get(0),
        )
        .map_err(|e| e.to_string())?;
    if input_not_null {
        conn.execute_batch(
            "BEGIN;
             ALTER TABLE token_usage RENAME TO token_usage_before_nullable;
             CREATE TABLE token_usage (
                 id INTEGER PRIMARY KEY AUTOINCREMENT,
                 timestamp INTEGER NOT NULL,
                 account_email TEXT NOT NULL,
                 model TEXT NOT NULL,
                 input_tokens INTEGER,
                 output_tokens INTEGER,
                 cached_tokens INTEGER,
                 reasoning_tokens INTEGER,
                 total_tokens INTEGER,
                 reported_total_tokens INTEGER,
                 cache_reported INTEGER NOT NULL DEFAULT 0
             );
             INSERT INTO token_usage
                 (id, timestamp, account_email, model, input_tokens, output_tokens, cached_tokens,
                  reasoning_tokens, total_tokens, reported_total_tokens, cache_reported)
             SELECT id, timestamp, account_email, model, NULLIF(input_tokens, 0), NULLIF(output_tokens, 0),
                    CASE WHEN cache_reported = 0 AND cached_tokens = 0 THEN NULL ELSE cached_tokens END,
                    NULLIF(reasoning_tokens, 0), total_tokens, reported_total_tokens, cache_reported
             FROM token_usage_before_nullable;
             DROP TABLE token_usage_before_nullable;
             CREATE INDEX idx_token_timestamp ON token_usage (timestamp DESC);
             CREATE INDEX idx_token_account ON token_usage (account_email);
             CREATE INDEX idx_token_account_timestamp ON token_usage (account_email, timestamp DESC);
             CREATE INDEX idx_token_model ON token_usage (model);
             COMMIT;"
        ).map_err(|e| e.to_string())?;
    }
    for column in [
        "cache_reported_requests INTEGER NOT NULL DEFAULT 0",
        "cache_reported_input_tokens INTEGER NOT NULL DEFAULT 0",
        "cache_reported_tokens INTEGER NOT NULL DEFAULT 0",
        "reported_total_tokens INTEGER NOT NULL DEFAULT 0",
        "total_reported_requests INTEGER NOT NULL DEFAULT 0",
        "output_reported_requests INTEGER NOT NULL DEFAULT 0",
        "input_reported_requests INTEGER NOT NULL DEFAULT 0",
    ] {
        add_column_if_missing(&conn, "token_stats_hourly", column)?;
    }
    Ok(())
}

/// Record token usage from a request
pub fn record_usage(
    account_email: &str,
    model: &str,
    input_tokens: Option<u32>,
    output_tokens: Option<u32>,
    cached_tokens: Option<u32>,
    reasoning_tokens: Option<u32>,
    reported_total_tokens: Option<u32>,
) -> Result<(), String> {
    let conn = connect_db()?;
    // Raw history and the hourly view must commit together. A failed aggregate
    // update must not leave a request visible only in some statistics views.
    let transaction = conn.unchecked_transaction().map_err(|e| e.to_string())?;
    let timestamp = chrono::Local::now().timestamp();
    let cache_reported = u32::from(input_tokens.zip(cached_tokens).is_some());
    let cache_reported_input = input_tokens.zip(cached_tokens).map(|(input, _)| input);
    let cache_reported_tokens = input_tokens.zip(cached_tokens).map(|(_, cached)| cached);
    let input_reported = u32::from(input_tokens.is_some());
    let output_reported = u32::from(output_tokens.is_some());
    let total_reported = u32::from(reported_total_tokens.is_some());

    // Insert into raw usage table
    transaction.execute(
        "INSERT INTO token_usage (timestamp, account_email, model, input_tokens, output_tokens, cached_tokens, reasoning_tokens, total_tokens, cache_reported, reported_total_tokens)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?8)",
        params![timestamp, account_email, model, input_tokens, output_tokens, cached_tokens, reasoning_tokens, reported_total_tokens, cache_reported],
    ).map_err(|e| e.to_string())?;

    let hour_bucket = chrono::Local::now().format("%Y-%m-%d %H:00").to_string();
    transaction.execute(
        "INSERT INTO token_stats_hourly (hour_bucket, account_email, total_input_tokens, total_output_tokens, total_cached_tokens, total_reasoning_tokens, total_tokens, request_count, cache_reported_requests, cache_reported_input_tokens, cache_reported_tokens, reported_total_tokens, total_reported_requests, output_reported_requests, input_reported_requests)
         VALUES (?1, ?2, COALESCE(?3, 0), COALESCE(?4, 0), COALESCE(?5, 0), COALESCE(?6, 0), COALESCE(?7, 0), 1, ?8, COALESCE(?9, 0), COALESCE(?10, 0), COALESCE(?7, 0), ?11, ?12, ?13)
         ON CONFLICT(hour_bucket, account_email) DO UPDATE SET
            total_input_tokens = total_input_tokens + COALESCE(?3, 0),
            total_output_tokens = total_output_tokens + COALESCE(?4, 0),
            total_cached_tokens = total_cached_tokens + COALESCE(?5, 0),
            total_reasoning_tokens = total_reasoning_tokens + COALESCE(?6, 0),
            total_tokens = total_tokens + COALESCE(?7, 0),
            request_count = request_count + 1,
            cache_reported_requests = cache_reported_requests + ?8,
            cache_reported_input_tokens = cache_reported_input_tokens + COALESCE(?9, 0),
            cache_reported_tokens = cache_reported_tokens + COALESCE(?10, 0),
            reported_total_tokens = reported_total_tokens + COALESCE(?7, 0),
            total_reported_requests = total_reported_requests + ?11,
            output_reported_requests = output_reported_requests + ?12,
            input_reported_requests = input_reported_requests + ?13",
        params![hour_bucket, account_email, input_tokens, output_tokens, cached_tokens, reasoning_tokens, reported_total_tokens, cache_reported, cache_reported_input, cache_reported_tokens, total_reported, output_reported, input_reported],
    ).map_err(|e| e.to_string())?;

    transaction.commit().map_err(|e| e.to_string())
}

/// Attach current weekly usage without loading raw history or persisting derived totals.
pub(crate) fn populate_weekly_usage(accounts: &mut [crate::models::Account]) -> Result<(), String> {
    if !accounts.iter().any(|account| {
        account
            .quota
            .as_ref()
            .is_some_and(|q| q.quota_groups.is_some())
    }) {
        return Ok(());
    }
    let conn = connect_db()?;
    populate_weekly_usage_with_conn(&conn, accounts, chrono::Utc::now().timestamp())
}

fn populate_weekly_usage_with_conn(
    conn: &Connection,
    accounts: &mut [crate::models::Account],
    now: i64,
) -> Result<(), String> {
    let mut stmt = conn
        .prepare(
            "SELECT COALESCE(SUM(input_tokens + output_tokens), 0) FROM token_usage
         WHERE account_email = ?1 AND timestamp >= ?2 AND timestamp < ?3 AND timestamp <= ?4
         AND ((?5 = 0 AND model LIKE 'gemini%')
           OR (?5 = 1 AND (model LIKE 'claude%' OR model LIKE 'gpt%')))",
        )
        .map_err(|e| e.to_string())?;
    for account in accounts {
        let Some(groups) = account
            .quota
            .as_mut()
            .and_then(|quota| quota.quota_groups.as_mut())
        else {
            continue;
        };
        for group in groups {
            let name = group.display_name.to_lowercase();
            for bucket in &mut group.buckets {
                bucket.cycle_tokens = None;
                let Some((start, end)) = bucket.weekly_cycle_bounds(now) else {
                    continue;
                };
                let id = bucket.bucket_id.to_lowercase();
                let third_party =
                    name.contains("claude") || name.contains("gpt") || id.contains("3p");
                if !third_party && !name.contains("gemini") && !id.contains("gemini") {
                    continue;
                }
                bucket.cycle_tokens = Some(
                    stmt.query_row(
                        params![account.email, start, end, now, third_party],
                        |row| row.get(0),
                    )
                    .map_err(|e| e.to_string())?,
                );
            }
        }
    }
    Ok(())
}

/// Get minute-level aggregated stats for a time range
pub fn get_minute_stats(
    minutes: i64,
    step_minutes: i64,
) -> Result<Vec<TokenStatsAggregated>, String> {
    let conn = connect_db()?;
    let step_secs = step_minutes.max(1) * 60;
    let cutoff = if minutes <= 0 {
        0
    } else {
        chrono::Local::now().timestamp() - (minutes * 60)
    };

    let mut stmt = conn
        .prepare(
            "SELECT strftime('%Y-%m-%d %H:%M', datetime(timestamp - (timestamp % ?2), 'unixepoch', 'localtime')) as minute_bucket,
                COALESCE(SUM(input_tokens), 0) as input,
                COALESCE(SUM(output_tokens), 0) as output,
                COALESCE(SUM(cached_tokens), 0) as cached,
                SUM(COALESCE(reasoning_tokens, 0)) as reasoning,
                COALESCE(SUM(reported_total_tokens), 0) as total,
                COUNT(*) as count,
                COALESCE(SUM(cache_reported), 0),
                COALESCE(SUM(CASE WHEN cache_reported = 1 THEN input_tokens ELSE 0 END), 0),
                COALESCE(SUM(CASE WHEN cache_reported = 1 THEN cached_tokens ELSE 0 END), 0),
                COUNT(reported_total_tokens), COUNT(output_tokens), COUNT(input_tokens)
         FROM token_usage
         WHERE timestamp >= ?1
         GROUP BY minute_bucket
         ORDER BY minute_bucket ASC",
        )
        .map_err(|e| e.to_string())?;

    let rows = stmt
        .query_map(params![cutoff, step_secs], |row| {
            Ok(TokenStatsAggregated {
                period: row.get(0)?,
                total_input_tokens: row.get(1)?,
                total_output_tokens: row.get(2)?,
                total_cached_tokens: row.get(3)?,
                cache_reported_requests: row.get(7)?,
                cache_reported_input_tokens: row.get(8)?,
                cache_reported_tokens: row.get(9)?,
                total_reported_requests: row.get(10)?,
                output_reported_requests: row.get(11)?,
                input_reported_requests: row.get(12)?,
                total_reasoning_tokens: row.get(4)?,
                total_tokens: row.get(5)?,
                request_count: row.get(6)?,
            })
        })
        .map_err(|e| e.to_string())?;

    let mut result = Vec::new();
    for row in rows {
        result.push(row.map_err(|e| e.to_string())?);
    }
    Ok(result)
}

/// Get hourly aggregated stats for a time range
pub fn get_hourly_stats(hours: i64) -> Result<Vec<TokenStatsAggregated>, String> {
    let conn = connect_db()?;
    let cutoff_bucket = if hours <= 0 {
        String::new()
    } else {
        let cutoff = chrono::Local::now() - chrono::Duration::hours(hours);
        cutoff.format("%Y-%m-%d %H:00").to_string()
    };

    let mut stmt = conn
        .prepare(
            "SELECT hour_bucket,
                SUM(total_input_tokens) as input,
                SUM(total_output_tokens) as output,
                SUM(total_cached_tokens) as cached,
                SUM(total_reasoning_tokens) as reasoning,
                COALESCE(SUM(reported_total_tokens), 0) as total,
                SUM(request_count) as count,
                SUM(cache_reported_requests),
                SUM(cache_reported_input_tokens),
                SUM(cache_reported_tokens),
                SUM(total_reported_requests),
                SUM(output_reported_requests),
                SUM(input_reported_requests)
         FROM token_stats_hourly
         WHERE hour_bucket >= ?1
         GROUP BY hour_bucket
         ORDER BY hour_bucket ASC",
        )
        .map_err(|e| e.to_string())?;

    let rows = stmt
        .query_map([cutoff_bucket], |row| {
            Ok(TokenStatsAggregated {
                period: row.get(0)?,
                total_input_tokens: row.get(1)?,
                total_output_tokens: row.get(2)?,
                total_cached_tokens: row.get(3)?,
                cache_reported_requests: row.get(7)?,
                cache_reported_input_tokens: row.get(8)?,
                cache_reported_tokens: row.get(9)?,
                total_reported_requests: row.get(10)?,
                output_reported_requests: row.get(11)?,
                input_reported_requests: row.get(12)?,
                total_reasoning_tokens: row.get(4)?,
                total_tokens: row.get(5)?,
                request_count: row.get(6)?,
            })
        })
        .map_err(|e| e.to_string())?;

    let mut result = Vec::new();
    for row in rows {
        result.push(row.map_err(|e| e.to_string())?);
    }
    Ok(result)
}

/// Get daily aggregated stats for a time range
pub fn get_daily_stats(days: i64) -> Result<Vec<TokenStatsAggregated>, String> {
    let conn = connect_db()?;
    let cutoff_bucket = if days <= 0 {
        String::new()
    } else {
        let cutoff = chrono::Local::now() - chrono::Duration::days(days);
        cutoff.format("%Y-%m-%d").to_string()
    };

    let mut stmt = conn
        .prepare(
            "SELECT substr(hour_bucket, 1, 10) as day_bucket,
                SUM(total_input_tokens) as input,
                SUM(total_output_tokens) as output,
                SUM(total_cached_tokens) as cached,
                SUM(total_reasoning_tokens) as reasoning,
                COALESCE(SUM(reported_total_tokens), 0) as total,
                SUM(request_count) as count,
                SUM(cache_reported_requests),
                SUM(cache_reported_input_tokens),
                SUM(cache_reported_tokens),
                SUM(total_reported_requests),
                SUM(output_reported_requests),
                SUM(input_reported_requests)
         FROM token_stats_hourly
         WHERE substr(hour_bucket, 1, 10) >= ?1
         GROUP BY day_bucket
         ORDER BY day_bucket ASC",
        )
        .map_err(|e| e.to_string())?;

    let rows = stmt
        .query_map([cutoff_bucket], |row| {
            Ok(TokenStatsAggregated {
                period: row.get(0)?,
                total_input_tokens: row.get(1)?,
                total_output_tokens: row.get(2)?,
                total_cached_tokens: row.get(3)?,
                cache_reported_requests: row.get(7)?,
                cache_reported_input_tokens: row.get(8)?,
                cache_reported_tokens: row.get(9)?,
                total_reported_requests: row.get(10)?,
                output_reported_requests: row.get(11)?,
                input_reported_requests: row.get(12)?,
                total_reasoning_tokens: row.get(4)?,
                total_tokens: row.get(5)?,
                request_count: row.get(6)?,
            })
        })
        .map_err(|e| e.to_string())?;

    let mut result = Vec::new();
    for row in rows {
        result.push(row.map_err(|e| e.to_string())?);
    }
    Ok(result)
}

/// Get weekly aggregated stats
pub fn get_weekly_stats(weeks: i64) -> Result<Vec<TokenStatsAggregated>, String> {
    let conn = connect_db()?;
    let cutoff_timestamp = if weeks <= 0 {
        0
    } else {
        let cutoff = chrono::Local::now() - chrono::Duration::weeks(weeks);
        cutoff.timestamp()
    };

    let mut stmt = conn
        .prepare(
            "SELECT strftime('%Y-W%W', datetime(timestamp, 'unixepoch', 'localtime')) as week_bucket,
                COALESCE(SUM(input_tokens), 0) as input,
                COALESCE(SUM(output_tokens), 0) as output,
                COALESCE(SUM(cached_tokens), 0) as cached,
                COALESCE(SUM(reasoning_tokens), 0) as reasoning,
                COALESCE(SUM(reported_total_tokens), 0) as total,
                COUNT(*) as count,
                SUM(cache_reported),
                SUM(CASE WHEN cache_reported = 1 THEN input_tokens ELSE 0 END),
                SUM(CASE WHEN cache_reported = 1 THEN cached_tokens ELSE 0 END),
                COUNT(reported_total_tokens),
                COUNT(output_tokens),
                COUNT(input_tokens)
         FROM token_usage
         WHERE timestamp >= ?1
         GROUP BY week_bucket
         ORDER BY week_bucket ASC",
        )
        .map_err(|e| e.to_string())?;

    let rows = stmt
        .query_map([cutoff_timestamp], |row| {
            Ok(TokenStatsAggregated {
                period: row.get(0)?,
                total_input_tokens: row.get(1)?,
                total_output_tokens: row.get(2)?,
                total_cached_tokens: row.get(3)?,
                cache_reported_requests: row.get(7)?,
                cache_reported_input_tokens: row.get(8)?,
                cache_reported_tokens: row.get(9)?,
                total_reported_requests: row.get(10)?,
                output_reported_requests: row.get(11)?,
                input_reported_requests: row.get(12)?,
                total_reasoning_tokens: row.get(4)?,
                total_tokens: row.get(5)?,
                request_count: row.get(6)?,
            })
        })
        .map_err(|e| e.to_string())?;

    let mut result = Vec::new();
    for row in rows {
        result.push(row.map_err(|e| e.to_string())?);
    }
    Ok(result)
}

/// Get per-account statistics for a time range
pub fn get_account_stats(hours: i64) -> Result<Vec<AccountTokenStats>, String> {
    let conn = connect_db()?;
    let cutoff_bucket = if hours <= 0 {
        String::new()
    } else {
        let cutoff = chrono::Local::now() - chrono::Duration::hours(hours);
        cutoff.format("%Y-%m-%d %H:00").to_string()
    };

    let mut stmt = conn
        .prepare(
            "SELECT account_email,
                SUM(total_input_tokens) as input,
                SUM(total_output_tokens) as output,
                SUM(total_cached_tokens) as cached,
                SUM(total_reasoning_tokens) as reasoning,
                COALESCE(SUM(reported_total_tokens), 0) as total,
                SUM(request_count) as count,
                SUM(cache_reported_requests),
                SUM(cache_reported_input_tokens),
                SUM(cache_reported_tokens),
                SUM(total_reported_requests),
                SUM(output_reported_requests),
                SUM(input_reported_requests)
         FROM token_stats_hourly
         WHERE hour_bucket >= ?1
         GROUP BY account_email
         ORDER BY total DESC",
        )
        .map_err(|e| e.to_string())?;

    let rows = stmt
        .query_map([cutoff_bucket], |row| {
            Ok(AccountTokenStats {
                account_email: row.get(0)?,
                total_input_tokens: row.get(1)?,
                total_output_tokens: row.get(2)?,
                total_cached_tokens: row.get(3)?,
                total_reasoning_tokens: row.get(4)?,
                total_tokens: row.get(5)?,
                request_count: row.get(6)?,
                cache_reported_requests: row.get(7)?,
                cache_reported_input_tokens: row.get(8)?,
                cache_reported_tokens: row.get(9)?,
                total_reported_requests: row.get(10)?,
                output_reported_requests: row.get(11)?,
                input_reported_requests: row.get(12)?,
            })
        })
        .map_err(|e| e.to_string())?;

    let mut result = Vec::new();
    for row in rows {
        result.push(row.map_err(|e| e.to_string())?);
    }
    Ok(result)
}

/// Get summary statistics for a time range
pub fn get_summary_stats(hours: i64) -> Result<TokenStatsSummary, String> {
    let conn = connect_db()?;
    let cutoff_bucket = if hours <= 0 {
        String::new()
    } else {
        let cutoff = chrono::Local::now() - chrono::Duration::hours(hours);
        cutoff.format("%Y-%m-%d %H:00").to_string()
    };

    let (
        total_input,
        total_output,
        total_cached,
        total_reasoning,
        total,
        requests,
        cache_requests,
        cache_input,
        cache_tokens,
        total_reported_requests,
        output_reported_requests,
        input_reported_requests,
    ): (u64, u64, u64, u64, u64, u64, u64, u64, u64, u64, u64, u64) = conn
        .query_row(
            "SELECT COALESCE(SUM(total_input_tokens), 0),
                COALESCE(SUM(total_output_tokens), 0),
                COALESCE(SUM(total_cached_tokens), 0),
                COALESCE(SUM(total_reasoning_tokens), 0),
                COALESCE(SUM(reported_total_tokens), 0),
                COALESCE(SUM(request_count), 0),
                COALESCE(SUM(cache_reported_requests), 0),
                COALESCE(SUM(cache_reported_input_tokens), 0),
                COALESCE(SUM(cache_reported_tokens), 0),
                COALESCE(SUM(total_reported_requests), 0),
                COALESCE(SUM(output_reported_requests), 0),
                COALESCE(SUM(input_reported_requests), 0)
         FROM token_stats_hourly
         WHERE hour_bucket >= ?1",
            [&cutoff_bucket],
            |row| {
                Ok((
                    row.get(0)?,
                    row.get(1)?,
                    row.get(2)?,
                    row.get(3)?,
                    row.get(4)?,
                    row.get(5)?,
                    row.get(6)?,
                    row.get(7)?,
                    row.get(8)?,
                    row.get(9)?,
                    row.get(10)?,
                    row.get(11)?,
                ))
            },
        )
        .map_err(|e| e.to_string())?;

    let unique_accounts: u64 = conn
        .query_row(
            "SELECT COUNT(DISTINCT account_email) FROM token_stats_hourly WHERE hour_bucket >= ?1",
            [&cutoff_bucket],
            |row| row.get(0),
        )
        .map_err(|e| e.to_string())?;

    Ok(TokenStatsSummary {
        total_input_tokens: total_input,
        total_output_tokens: total_output,
        total_cached_tokens: total_cached,
        cache_reported_requests: cache_requests,
        cache_reported_input_tokens: cache_input,
        cache_reported_tokens: cache_tokens,
        total_reasoning_tokens: total_reasoning,
        total_tokens: total,
        total_reported_requests,
        output_reported_requests,
        input_reported_requests,
        total_requests: requests,
        unique_accounts,
    })
}

pub fn get_model_stats(hours: i64) -> Result<Vec<ModelTokenStats>, String> {
    let conn = connect_db()?;
    let cutoff = if hours <= 0 {
        0
    } else {
        chrono::Local::now().timestamp() - (hours * 3600)
    };

    let mut stmt = conn
        .prepare(
            "SELECT model,
                COALESCE(SUM(input_tokens), 0) as input,
                COALESCE(SUM(output_tokens), 0) as output,
                COALESCE(SUM(cached_tokens), 0) as cached,
                COALESCE(SUM(reasoning_tokens), 0) as reasoning,
                COALESCE(SUM(reported_total_tokens), 0) as total,
                COUNT(*) as count,
                SUM(cache_reported),
                SUM(CASE WHEN cache_reported = 1 THEN input_tokens ELSE 0 END),
                SUM(CASE WHEN cache_reported = 1 THEN cached_tokens ELSE 0 END),
                COUNT(reported_total_tokens),
                COUNT(output_tokens),
                COUNT(input_tokens)
         FROM token_usage
         WHERE timestamp >= ?1
         GROUP BY model
         ORDER BY total DESC",
        )
        .map_err(|e| e.to_string())?;

    let rows = stmt
        .query_map([cutoff], |row| {
            Ok(ModelTokenStats {
                model: row.get(0)?,
                total_input_tokens: row.get(1)?,
                total_output_tokens: row.get(2)?,
                total_cached_tokens: row.get(3)?,
                cache_reported_requests: row.get(7)?,
                cache_reported_input_tokens: row.get(8)?,
                cache_reported_tokens: row.get(9)?,
                total_reported_requests: row.get(10)?,
                output_reported_requests: row.get(11)?,
                input_reported_requests: row.get(12)?,
                total_reasoning_tokens: row.get(4)?,
                total_tokens: row.get(5)?,
                request_count: row.get(6)?,
            })
        })
        .map_err(|e| e.to_string())?;

    let mut result = Vec::new();
    for row in rows {
        result.push(row.map_err(|e| e.to_string())?);
    }
    Ok(result)
}

pub fn get_model_trend_minute(
    minutes: i64,
    step_minutes: i64,
) -> Result<Vec<ModelTrendPoint>, String> {
    let conn = connect_db()?;
    let step_secs = step_minutes.max(1) * 60;
    let cutoff = if minutes <= 0 {
        0
    } else {
        chrono::Local::now().timestamp() - (minutes * 60)
    };

    let mut stmt = conn
        .prepare(
            "SELECT strftime('%Y-%m-%d %H:%M', datetime(timestamp - (timestamp % ?2), 'unixepoch', 'localtime')) as minute_bucket,
                model,
                SUM(reported_total_tokens) as total
         FROM token_usage
         WHERE timestamp >= ?1
         GROUP BY minute_bucket, model
         ORDER BY minute_bucket ASC",
        )
        .map_err(|e| e.to_string())?;

    let mut trend_map: std::collections::BTreeMap<
        String,
        std::collections::HashMap<String, Option<u64>>,
    > = std::collections::BTreeMap::new();

    let rows = stmt
        .query_map(params![cutoff, step_secs], |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, String>(1)?,
                row.get::<_, Option<u64>>(2)?,
            ))
        })
        .map_err(|e| e.to_string())?;

    for row in rows {
        let (period, model, total) = row.map_err(|e| e.to_string())?;
        trend_map.entry(period).or_default().insert(model, total);
    }

    Ok(trend_map
        .into_iter()
        .map(|(period, model_data)| ModelTrendPoint { period, model_data })
        .collect())
}

pub fn get_model_trend_hourly(hours: i64) -> Result<Vec<ModelTrendPoint>, String> {
    let conn = connect_db()?;
    let cutoff = if hours <= 0 {
        0
    } else {
        chrono::Local::now().timestamp() - (hours * 3600)
    };

    let mut stmt = conn
        .prepare(
            "SELECT strftime('%Y-%m-%d %H:00', datetime(timestamp, 'unixepoch', 'localtime')) as hour_bucket,
                model,
                SUM(reported_total_tokens) as total
         FROM token_usage
         WHERE timestamp >= ?1
         GROUP BY hour_bucket, model
         ORDER BY hour_bucket ASC",
        )
        .map_err(|e| e.to_string())?;

    let mut trend_map: std::collections::BTreeMap<
        String,
        std::collections::HashMap<String, Option<u64>>,
    > = std::collections::BTreeMap::new();

    let rows = stmt
        .query_map([cutoff], |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, String>(1)?,
                row.get::<_, Option<u64>>(2)?,
            ))
        })
        .map_err(|e| e.to_string())?;

    for row in rows {
        let (period, model, total) = row.map_err(|e| e.to_string())?;
        trend_map.entry(period).or_default().insert(model, total);
    }

    Ok(trend_map
        .into_iter()
        .map(|(period, model_data)| ModelTrendPoint { period, model_data })
        .collect())
}

pub fn get_model_trend_daily(days: i64) -> Result<Vec<ModelTrendPoint>, String> {
    let conn = connect_db()?;
    let cutoff = if days <= 0 {
        0
    } else {
        chrono::Local::now().timestamp() - (days * 24 * 3600)
    };

    let mut stmt = conn
        .prepare(
            "SELECT strftime('%Y-%m-%d', datetime(timestamp, 'unixepoch', 'localtime')) as day_bucket,
                model,
                SUM(reported_total_tokens) as total
         FROM token_usage
         WHERE timestamp >= ?1
         GROUP BY day_bucket, model
         ORDER BY day_bucket ASC",
        )
        .map_err(|e| e.to_string())?;

    let mut trend_map: std::collections::BTreeMap<
        String,
        std::collections::HashMap<String, Option<u64>>,
    > = std::collections::BTreeMap::new();

    let rows = stmt
        .query_map([cutoff], |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, String>(1)?,
                row.get::<_, Option<u64>>(2)?,
            ))
        })
        .map_err(|e| e.to_string())?;

    for row in rows {
        let (period, model, total) = row.map_err(|e| e.to_string())?;
        trend_map.entry(period).or_default().insert(model, total);
    }

    Ok(trend_map
        .into_iter()
        .map(|(period, model_data)| ModelTrendPoint { period, model_data })
        .collect())
}

pub fn get_account_trend_minute(
    minutes: i64,
    step_minutes: i64,
) -> Result<Vec<AccountTrendPoint>, String> {
    let conn = connect_db()?;
    let step_secs = step_minutes.max(1) * 60;
    let cutoff = if minutes <= 0 {
        0
    } else {
        chrono::Local::now().timestamp() - (minutes * 60)
    };

    let mut stmt = conn
        .prepare(
            "SELECT strftime('%Y-%m-%d %H:%M', datetime(timestamp - (timestamp % ?2), 'unixepoch', 'localtime')) as minute_bucket,
                account_email,
                SUM(reported_total_tokens) as total
         FROM token_usage
         WHERE timestamp >= ?1
         GROUP BY minute_bucket, account_email
         ORDER BY minute_bucket ASC",
        )
        .map_err(|e| e.to_string())?;

    let mut trend_map: std::collections::BTreeMap<
        String,
        std::collections::HashMap<String, Option<u64>>,
    > = std::collections::BTreeMap::new();

    let rows = stmt
        .query_map(params![cutoff, step_secs], |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, String>(1)?,
                row.get::<_, Option<u64>>(2)?,
            ))
        })
        .map_err(|e| e.to_string())?;

    for row in rows {
        let (period, account, total) = row.map_err(|e| e.to_string())?;
        trend_map.entry(period).or_default().insert(account, total);
    }

    Ok(trend_map
        .into_iter()
        .map(|(period, account_data)| AccountTrendPoint {
            period,
            account_data,
        })
        .collect())
}

pub fn get_account_trend_hourly(hours: i64) -> Result<Vec<AccountTrendPoint>, String> {
    let conn = connect_db()?;
    let cutoff = if hours <= 0 {
        0
    } else {
        chrono::Local::now().timestamp() - (hours * 3600)
    };

    let mut stmt = conn
        .prepare(
            "SELECT strftime('%Y-%m-%d %H:00', datetime(timestamp, 'unixepoch', 'localtime')) as hour_bucket,
                account_email,
                SUM(reported_total_tokens) as total
         FROM token_usage
         WHERE timestamp >= ?1
         GROUP BY hour_bucket, account_email
         ORDER BY hour_bucket ASC",
        )
        .map_err(|e| e.to_string())?;

    let mut trend_map: std::collections::BTreeMap<
        String,
        std::collections::HashMap<String, Option<u64>>,
    > = std::collections::BTreeMap::new();

    let rows = stmt
        .query_map([cutoff], |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, String>(1)?,
                row.get::<_, Option<u64>>(2)?,
            ))
        })
        .map_err(|e| e.to_string())?;

    for row in rows {
        let (period, account, total) = row.map_err(|e| e.to_string())?;
        trend_map.entry(period).or_default().insert(account, total);
    }

    Ok(trend_map
        .into_iter()
        .map(|(period, account_data)| AccountTrendPoint {
            period,
            account_data,
        })
        .collect())
}

pub fn get_account_trend_daily(days: i64) -> Result<Vec<AccountTrendPoint>, String> {
    let conn = connect_db()?;
    let cutoff = if days <= 0 {
        0
    } else {
        chrono::Local::now().timestamp() - (days * 24 * 3600)
    };

    let mut stmt = conn
        .prepare(
            "SELECT strftime('%Y-%m-%d', datetime(timestamp, 'unixepoch', 'localtime')) as day_bucket,
                account_email,
                SUM(reported_total_tokens) as total
         FROM token_usage
         WHERE timestamp >= ?1
         GROUP BY day_bucket, account_email
         ORDER BY day_bucket ASC",
        )
        .map_err(|e| e.to_string())?;

    let mut trend_map: std::collections::BTreeMap<
        String,
        std::collections::HashMap<String, Option<u64>>,
    > = std::collections::BTreeMap::new();

    let rows = stmt
        .query_map([cutoff], |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, String>(1)?,
                row.get::<_, Option<u64>>(2)?,
            ))
        })
        .map_err(|e| e.to_string())?;

    for row in rows {
        let (period, account, total) = row.map_err(|e| e.to_string())?;
        trend_map.entry(period).or_default().insert(account, total);
    }

    Ok(trend_map
        .into_iter()
        .map(|(period, account_data)| AccountTrendPoint {
            period,
            account_data,
        })
        .collect())
}

#[cfg(test)]
mod tests {
    #[test]
    fn aggregate_write_failure_rolls_back_raw_usage() {
        let _data = crate::proxy::monitor::prompt_log_tests::TestDataDir::new();
        init_db().unwrap();
        let conn = connect_db().unwrap();
        conn.execute_batch("CREATE TRIGGER reject_hourly BEFORE INSERT ON token_stats_hourly BEGIN SELECT RAISE(FAIL, 'aggregate unavailable'); END;").unwrap();
        assert!(record_usage(
            "atomic@test",
            "gemini",
            Some(1),
            Some(2),
            None,
            None,
            Some(3)
        )
        .is_err());
        assert_eq!(get_summary_stats(0).unwrap().total_requests, 0);
        let raw: u64 = conn
            .query_row("SELECT COUNT(*) FROM token_usage", [], |row| row.get(0))
            .unwrap();
        assert_eq!(raw, 0);
        conn.execute_batch("DROP TRIGGER reject_hourly;").unwrap();
        record_usage(
            "atomic@test",
            "gemini",
            Some(1),
            Some(2),
            None,
            None,
            Some(3),
        )
        .unwrap();
        assert_eq!(get_summary_stats(0).unwrap().total_requests, 1);
        assert_eq!(get_model_stats(0).unwrap()[0].request_count, 1);
    }

    #[test]
    fn legacy_database_upgrade_keeps_derived_totals_separate_and_new_nulls_readable() {
        let _data = crate::proxy::monitor::prompt_log_tests::TestDataDir::new();
        let conn = connect_db().unwrap();
        conn.execute_batch(
            "CREATE TABLE token_usage (id INTEGER PRIMARY KEY AUTOINCREMENT, timestamp INTEGER NOT NULL,
                 account_email TEXT NOT NULL, model TEXT NOT NULL, input_tokens INTEGER NOT NULL DEFAULT 0,
                 output_tokens INTEGER NOT NULL DEFAULT 0, cached_tokens INTEGER NOT NULL DEFAULT 0,
                 reasoning_tokens INTEGER NOT NULL DEFAULT 0, total_tokens INTEGER NOT NULL DEFAULT 0);
             INSERT INTO token_usage VALUES (1, 1, 'legacy@test', 'gemini', 100, 20, 80, 3, 120);
             INSERT INTO token_usage VALUES (2, 1, 'legacy@test', 'gemini', 0, 0, 0, 0, 0);
             CREATE TABLE token_stats_hourly (hour_bucket TEXT NOT NULL, account_email TEXT NOT NULL,
                 total_input_tokens INTEGER NOT NULL DEFAULT 0, total_output_tokens INTEGER NOT NULL DEFAULT 0,
                 total_cached_tokens INTEGER NOT NULL DEFAULT 0, total_reasoning_tokens INTEGER NOT NULL DEFAULT 0,
                 total_tokens INTEGER NOT NULL DEFAULT 0, request_count INTEGER NOT NULL DEFAULT 0,
                 PRIMARY KEY (hour_bucket, account_email));
             INSERT INTO token_stats_hourly VALUES (strftime('%Y-%m-%d %H:00', 'now', 'localtime'), 'legacy@test', 100, 20, 80, 3, 120, 2);"
        ).unwrap();
        drop(conn);
        init_db().unwrap();
        let conn = connect_db().unwrap();
        let old: (u32, Option<u32>, u32) = conn.query_row("SELECT total_tokens, reported_total_tokens, cache_reported FROM token_usage WHERE id = 1", [], |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?))).unwrap();
        assert_eq!(old, (120, None, 0));
        let unknown: (Option<u32>, Option<u32>, Option<u32>, Option<u32>, Option<u32>) = conn.query_row("SELECT input_tokens, output_tokens, cached_tokens, reasoning_tokens, reported_total_tokens FROM token_usage WHERE id = 2", [], |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?, row.get(4)?))).unwrap();
        assert_eq!(unknown, (None, None, None, None, None));
        drop(conn);
        record_usage(
            "legacy@test",
            "gemini",
            Some(0),
            Some(0),
            Some(0),
            Some(0),
            Some(0),
        )
        .unwrap();
        record_usage(
            "legacy@test",
            "gemini",
            Some(100),
            None,
            Some(80),
            None,
            None,
        )
        .unwrap();
        init_db().unwrap();
        let model = get_model_stats(0).unwrap().remove(0);
        assert_eq!(model.request_count, 4);
        assert_eq!(model.total_tokens, 0);
        assert_eq!(model.total_reported_requests, 1);
        assert_eq!(model.output_reported_requests, 2);
        assert_eq!(model.cache_reported_requests, 2);
        assert_eq!(model.cache_reported_input_tokens, 100);
        assert_eq!(model.cache_reported_tokens, 80);
        let conn = connect_db().unwrap();
        let explicit: (Option<u32>, Option<u32>, Option<u32>, Option<u32>) = conn.query_row("SELECT input_tokens, output_tokens, cached_tokens, reported_total_tokens FROM token_usage WHERE id = 3", [], |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?))).unwrap();
        assert_eq!(explicit, (Some(0), Some(0), Some(0), Some(0)));
        let legacy_hourly: u64 = conn
            .query_row("SELECT total_tokens FROM token_stats_hourly", [], |row| {
                row.get(0)
            })
            .unwrap();
        assert_eq!(legacy_hourly, 120);
        let summary = get_summary_stats(0).unwrap();
        assert_eq!(summary.total_requests, 4);
        assert_eq!(summary.total_tokens, 0);
        assert_eq!(summary.total_reported_requests, 1);
        for trend in [
            get_model_trend_hourly(0).unwrap(),
            get_model_trend_daily(0).unwrap(),
        ] {
            let totals: Vec<_> = trend
                .iter()
                .flat_map(|point| point.model_data.values())
                .copied()
                .collect();
            assert!(totals.contains(&None));
            assert!(totals.contains(&Some(0)));
        }
        for trend in [
            get_account_trend_hourly(0).unwrap(),
            get_account_trend_daily(0).unwrap(),
        ] {
            let totals: Vec<_> = trend
                .iter()
                .flat_map(|point| point.account_data.values())
                .copied()
                .collect();
            assert!(totals.contains(&None));
            assert!(totals.contains(&Some(0)));
        }
    }

    use super::*;

    fn weekly_snapshot(observed: i64, reset: i64, fraction: f64) -> crate::models::QuotaData {
        let iso = |seconds| {
            chrono::DateTime::from_timestamp(seconds, 0)
                .unwrap()
                .to_rfc3339()
        };
        serde_json::from_value(serde_json::json!({
            "models": [], "last_updated": observed,
            "quota_groups": [
                {"display_name": "Gemini Models", "buckets": [
                    {"bucket_id": "gemini-weekly", "window": "weekly", "remaining_fraction": fraction,
                     "reset_time": iso(reset), "observed_at": observed * 1000},
                    {"bucket_id": "gemini-5h", "window": "5h", "remaining_fraction": 1,
                     "reset_time": iso(reset), "observed_at": observed * 1000}
                ]},
                {"display_name": "Claude and GPT models", "buckets": [
                    {"bucket_id": "3p-weekly", "window": "weekly", "remaining_fraction": 0.5,
                     "reset_time": iso(reset + 100), "observed_at": observed * 1000}
                ]}
            ]
        })).unwrap()
    }

    #[test]
    fn weekly_usage_tracks_accounts_groups_and_official_reset_cycles() {
        let now = 1_700_000_000;
        let reset = now + 100;
        let start = reset - 7 * 86400;
        let conn = Connection::open_in_memory().unwrap();
        conn.execute_batch(
            "CREATE TABLE token_usage (timestamp INTEGER, account_email TEXT,
            model TEXT, input_tokens INTEGER, output_tokens INTEGER, cached_tokens INTEGER);",
        )
        .unwrap();
        for (email, model, timestamp, input, output) in [
            ("a", "gemini-pro", start - 1, 900, 0),
            ("a", "gemini-pro", start, 11, 2),
            ("a", "claude-sonnet", start, 900, 0),
            ("a", "claude-sonnet", start + 100, 20, 0),
            ("a", "gpt-oss", now, 30, 0),
            ("a", "custom-alias", now, 900, 0),
            ("b", "gemini-pro", start - 51, 900, 0),
            ("b", "gemini-pro", start - 50, 7, 3),
            ("b", "gemini-pro", now, 900, 0),
            ("a", "gemini-pro", reset, 40, 0),
        ] {
            conn.execute(
                "INSERT INTO token_usage VALUES (?1, ?2, ?3, ?4, ?5, 99)",
                params![timestamp, email, model, input, output],
            )
            .unwrap();
        }
        let mut accounts: Vec<_> = [("a", reset), ("b", reset - 50)]
            .into_iter()
            .map(|(email, account_reset)| {
                let token = crate::models::TokenData::new(
                    String::new(),
                    String::new(),
                    0,
                    None,
                    None,
                    None,
                    false,
                    None,
                );
                let mut account = crate::models::Account::new(email.into(), email.into(), token);
                account.update_quota(weekly_snapshot(now, account_reset, 0.0));
                account
            })
            .collect();
        let buckets = |account: &crate::models::Account| {
            account
                .quota
                .as_ref()
                .unwrap()
                .quota_groups
                .as_ref()
                .unwrap()
                .iter()
                .flat_map(|group| group.buckets.iter().map(|bucket| bucket.cycle_tokens))
                .collect::<Vec<_>>()
        };
        populate_weekly_usage_with_conn(&conn, &mut accounts, now).unwrap();
        assert_eq!(buckets(&accounts[0]), vec![Some(13), None, Some(50)]);
        assert_eq!(buckets(&accounts[1]), vec![Some(910), None, Some(0)]);

        // Legacy persisted boundaries are ignored and omitted on the next serialization.
        let mut legacy = serde_json::to_value(&accounts[0]).unwrap();
        legacy["quota"]["quota_groups"][0]["buckets"][0]["cycle_start"] = now.into();
        accounts[0] = serde_json::from_value(legacy).unwrap();
        assert!(
            serde_json::to_value(&accounts[0]).unwrap()["quota"]["quota_groups"][0]["buckets"][0]
                .get("cycle_start")
                .is_none()
        );
        populate_weekly_usage_with_conn(&conn, &mut accounts, now).unwrap();
        assert_eq!(buckets(&accounts[0]), vec![Some(13), None, Some(50)]);

        // Small and large replenishments, refreshes and reloads keep the official interval.
        for (offset, fraction) in [(1, 0.000001), (2, 1.0), (3, 0.9)] {
            accounts[0].update_quota(weekly_snapshot(now + offset, reset, fraction));
            accounts[0] =
                serde_json::from_value(serde_json::to_value(&accounts[0]).unwrap()).unwrap();
            populate_weekly_usage_with_conn(&conn, &mut accounts, now + offset).unwrap();
            assert_eq!(buckets(&accounts[0]), vec![Some(13), None, Some(50)]);
            assert_eq!(buckets(&accounts[1]), vec![Some(910), None, Some(0)]);
        }
        // An older positive observation cannot move the boundary.
        accounts[0].update_quota(weekly_snapshot(now, reset + 10, 1.0));
        populate_weekly_usage_with_conn(&conn, &mut accounts, now + 3).unwrap();
        assert_eq!(buckets(&accounts[0]), vec![Some(13), None, Some(50)]);

        // A new official reset switches cycles and includes its exact new start.
        accounts[0].update_quota(weekly_snapshot(reset, reset + 7 * 86400, 1.0));
        populate_weekly_usage_with_conn(&conn, &mut accounts, reset).unwrap();
        assert_eq!(buckets(&accounts[0]), vec![Some(40), None, None]);
        let first = &accounts[0]
            .quota
            .as_ref()
            .unwrap()
            .quota_groups
            .as_ref()
            .unwrap()[0]
            .buckets[0];
        assert_eq!(
            first.weekly_cycle_bounds(reset),
            Some((reset, reset + 7 * 86400))
        );

        // Invalid and expired periods are unavailable, never fabricated zero totals.
        accounts[0]
            .quota
            .as_mut()
            .unwrap()
            .quota_groups
            .as_mut()
            .unwrap()[0]
            .buckets[0]
            .reset_time = "invalid".into();
        populate_weekly_usage_with_conn(&conn, &mut accounts, reset + 101).unwrap();
        assert_eq!(buckets(&accounts[0]), vec![None, None, Some(0)]);
        assert_eq!(buckets(&accounts[1]), vec![None, None, None]);
    }

    #[test]
    fn test_record_and_query() {
        let _data = crate::proxy::monitor::prompt_log_tests::TestDataDir::new();
        init_db().unwrap();
        for cached in [None, Some(0), Some(80)] {
            record_usage(
                "cache@test",
                "gemini",
                Some(100),
                Some(2),
                cached,
                None,
                None,
            )
            .unwrap();
        }
        let summary = get_summary_stats(0).unwrap();
        assert_eq!(summary.total_requests, 3);
        assert_eq!(summary.cache_reported_requests, 2);
        assert_eq!(summary.cache_reported_input_tokens, 200);
        assert_eq!(summary.cache_reported_tokens, 80);
        let conn = connect_db().unwrap();
        conn.execute("UPDATE token_usage SET cache_reported = 0", [])
            .unwrap();
        let model = get_model_stats(0).unwrap().remove(0);
        assert_eq!(model.cache_reported_requests, 0);
        assert_eq!(model.cache_reported_input_tokens, 0);
        assert_eq!(model.cache_reported_tokens, 0);
        // Old rows have no recoverable classification; their positive legacy total
        // must not become a known-subset numerator with an unknown denominator.
        assert_eq!(model.total_cached_tokens, 80);
    }
}
