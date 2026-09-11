use chrono::{DateTime, Local, Utc};
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
    pub total_tokens: u64,
    pub request_count: u64,
}

/// Per-account token statistics
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AccountTokenStats {
    pub account_email: String,
    pub total_input_tokens: u64,
    pub total_output_tokens: u64,
    pub total_cached_tokens: u64,
    pub total_tokens: u64,
    pub request_count: u64,
}

/// Summary statistics
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TokenStatsSummary {
    pub total_input_tokens: u64,
    pub total_output_tokens: u64,
    pub total_cached_tokens: u64,
    pub total_tokens: u64,
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
    pub total_tokens: u64,
    pub request_count: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ModelTrendPoint {
    pub period: String,
    pub model_data: std::collections::HashMap<String, u64>,
}

/// Account trend data point (for stacked area chart)
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AccountTrendPoint {
    pub period: String,
    pub account_data: std::collections::HashMap<String, u64>,
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
            input_tokens INTEGER NOT NULL DEFAULT 0,
            output_tokens INTEGER NOT NULL DEFAULT 0,
            cached_tokens INTEGER NOT NULL DEFAULT 0,
            total_tokens INTEGER NOT NULL DEFAULT 0
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

    add_column_if_missing(
        &conn,
        "token_usage",
        "cached_tokens INTEGER NOT NULL DEFAULT 0",
    )?;
    add_column_if_missing(
        &conn,
        "token_stats_hourly",
        "total_cached_tokens INTEGER NOT NULL DEFAULT 0",
    )?;

    Ok(())
}

/// Record token usage from a request.
///
/// Raw events and their hourly rollup are one logical record: neither is allowed
/// to survive if the other write fails.
pub fn record_usage(
    account_email: &str,
    model: &str,
    input_tokens: u32,
    output_tokens: u32,
    cached_tokens: u32,
) -> Result<(), String> {
    let mut conn = connect_db()?;
    record_usage_at(
        &mut conn,
        account_email,
        model,
        input_tokens,
        output_tokens,
        cached_tokens,
        Local::now(),
    )
}

fn record_usage_at(
    conn: &mut Connection,
    account_email: &str,
    model: &str,
    input_tokens: u32,
    output_tokens: u32,
    cached_tokens: u32,
    recorded_at: chrono::DateTime<Local>,
) -> Result<(), String> {
    let timestamp = recorded_at.timestamp();
    let total_tokens = input_tokens + output_tokens;
    let hour_bucket = recorded_at.format("%Y-%m-%d %H:00").to_string();
    let tx = conn.transaction().map_err(|e| e.to_string())?;

    tx.execute(
        "INSERT INTO token_usage (timestamp, account_email, model, input_tokens, output_tokens, cached_tokens, total_tokens)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
        params![timestamp, account_email, model, input_tokens, output_tokens, cached_tokens, total_tokens],
    )
    .map_err(|e| e.to_string())?;

    tx.execute(
        "INSERT INTO token_stats_hourly (hour_bucket, account_email, total_input_tokens, total_output_tokens, total_cached_tokens, total_tokens, request_count)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, 1)
         ON CONFLICT(hour_bucket, account_email) DO UPDATE SET
            total_input_tokens = total_input_tokens + ?3,
            total_output_tokens = total_output_tokens + ?4,
            total_cached_tokens = total_cached_tokens + ?5,
            total_tokens = total_tokens + ?6,
            request_count = request_count + 1",
        params![hour_bucket, account_email, input_tokens, output_tokens, cached_tokens, total_tokens],
    )
    .map_err(|e| e.to_string())?;

    tx.commit().map_err(|e| e.to_string())
}

/// Statistics windows are aligned to an elapsed-hour boundary, rather than a
/// local wall-clock boundary. A Unix timestamp identifies one real instant, so
/// this remains unambiguous across repeated or skipped local DST hours.
///
/// All statistics views for a selected range use this cutoff. Rollups are
/// hourly, so flooring the elapsed range to an hour prevents a rollup from
/// including usage excluded by a raw-event query.
fn hourly_window_start(hours: i64) -> DateTime<Local> {
    hourly_window_start_at(Local::now(), hours)
}

fn hourly_window_start_at(now: DateTime<Local>, hours: i64) -> DateTime<Local> {
    let timestamp = hourly_window_timestamp(now.timestamp(), hours);

    DateTime::<Utc>::from_timestamp(timestamp, 0)
        .map(|boundary| boundary.with_timezone(&Local))
        // `timestamp` comes from an existing DateTime and is therefore always
        // representable. Retain a real instant rather than panicking if Chrono
        // ever rejects an out-of-range value.
        .unwrap_or(now)
}

fn hourly_window_timestamp(now_timestamp: i64, hours: i64) -> i64 {
    now_timestamp
        .saturating_sub(hours.saturating_mul(3_600))
        .div_euclid(3_600)
        .saturating_mul(3_600)
}

/// Get hourly aggregated stats for a time range
pub fn get_hourly_stats(hours: i64) -> Result<Vec<TokenStatsAggregated>, String> {
    let conn = connect_db()?;
    let cutoff_bucket = hourly_window_start(hours)
        .format("%Y-%m-%d %H:00")
        .to_string();

    let mut stmt = conn
        .prepare(
            "SELECT hour_bucket, 
                SUM(total_input_tokens) as input, 
                SUM(total_output_tokens) as output,
                SUM(total_cached_tokens) as cached,
                SUM(total_tokens) as total,
                SUM(request_count) as count
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
                total_tokens: row.get(4)?,
                request_count: row.get(5)?,
            })
        })
        .map_err(|e| e.to_string())?;

    let mut result = Vec::new();
    for row in rows {
        result.push(row.map_err(|e| e.to_string())?);
    }
    Ok(result)
}

/// Get daily aggregated stats for a time range.
///
/// Daily groups retain the partial first local day when the selected range
/// begins mid-day, so their totals match the corresponding summary and trends.
pub fn get_daily_stats(days: i64) -> Result<Vec<TokenStatsAggregated>, String> {
    let conn = connect_db()?;
    let cutoff_bucket = hourly_window_start(days.saturating_mul(24))
        .format("%Y-%m-%d %H:00")
        .to_string();

    let mut stmt = conn
        .prepare(
            "SELECT substr(hour_bucket, 1, 10) as day_bucket,
                SUM(total_input_tokens) as input,
                SUM(total_output_tokens) as output,
                SUM(total_cached_tokens) as cached,
                SUM(total_tokens) as total,
                SUM(request_count) as count
         FROM token_stats_hourly
         WHERE hour_bucket >= ?1
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
                total_tokens: row.get(4)?,
                request_count: row.get(5)?,
            })
        })
        .map_err(|e| e.to_string())?;

    let mut result = Vec::new();
    for row in rows {
        result.push(row.map_err(|e| e.to_string())?);
    }
    Ok(result)
}

/// Get weekly aggregated stats. The weekly grouping uses the same elapsed-hour
/// cutoff as the dashboard's other views for this range.
pub fn get_weekly_stats(weeks: i64) -> Result<Vec<TokenStatsAggregated>, String> {
    let conn = connect_db()?;
    let cutoff_timestamp = hourly_window_start(weeks.saturating_mul(7 * 24)).timestamp();

    let mut stmt = conn
        .prepare(
            "SELECT strftime('%Y-W%W', datetime(timestamp, 'unixepoch', 'localtime')) as week_bucket,
                SUM(input_tokens) as input, 
                SUM(output_tokens) as output,
                SUM(cached_tokens) as cached,
                SUM(total_tokens) as total,
                COUNT(*) as count
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
                total_tokens: row.get(4)?,
                request_count: row.get(5)?,
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
    let cutoff_bucket = hourly_window_start(hours)
        .format("%Y-%m-%d %H:00")
        .to_string();

    let mut stmt = conn
        .prepare(
            "SELECT account_email,
                SUM(total_input_tokens) as input, 
                SUM(total_output_tokens) as output,
                SUM(total_cached_tokens) as cached,
                SUM(total_tokens) as total,
                SUM(request_count) as count
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
                total_tokens: row.get(4)?,
                request_count: row.get(5)?,
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
    let cutoff_bucket = hourly_window_start(hours)
        .format("%Y-%m-%d %H:00")
        .to_string();

    let (total_input, total_output, total_cached, total, requests): (u64, u64, u64, u64, u64) =
        conn.query_row(
            "SELECT COALESCE(SUM(total_input_tokens), 0),
                COALESCE(SUM(total_output_tokens), 0),
                COALESCE(SUM(total_cached_tokens), 0),
                COALESCE(SUM(total_tokens), 0),
                COALESCE(SUM(request_count), 0)
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
        total_tokens: total,
        total_requests: requests,
        unique_accounts,
    })
}

pub fn get_model_stats(hours: i64) -> Result<Vec<ModelTokenStats>, String> {
    let conn = connect_db()?;
    let cutoff = hourly_window_start(hours).timestamp();

    let mut stmt = conn
        .prepare(
            "SELECT model,
                SUM(input_tokens) as input,
                SUM(output_tokens) as output,
                SUM(cached_tokens) as cached,
                SUM(total_tokens) as total,
                COUNT(*) as count
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
                total_tokens: row.get(4)?,
                request_count: row.get(5)?,
            })
        })
        .map_err(|e| e.to_string())?;

    let mut result = Vec::new();
    for row in rows {
        result.push(row.map_err(|e| e.to_string())?);
    }
    Ok(result)
}

pub fn get_model_trend_hourly(hours: i64) -> Result<Vec<ModelTrendPoint>, String> {
    let conn = connect_db()?;
    let cutoff = hourly_window_start(hours).timestamp();

    let mut stmt = conn
        .prepare(
            "SELECT strftime('%Y-%m-%d %H:00', datetime(timestamp, 'unixepoch', 'localtime')) as hour_bucket,
                model,
                SUM(total_tokens) as total
         FROM token_usage
         WHERE timestamp >= ?1
         GROUP BY hour_bucket, model
         ORDER BY hour_bucket ASC",
        )
        .map_err(|e| e.to_string())?;

    let mut trend_map: std::collections::BTreeMap<String, std::collections::HashMap<String, u64>> =
        std::collections::BTreeMap::new();

    let rows = stmt
        .query_map([cutoff], |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, String>(1)?,
                row.get::<_, u64>(2)?,
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
    let cutoff = hourly_window_start(days.saturating_mul(24)).timestamp();

    let mut stmt = conn
        .prepare(
            "SELECT strftime('%Y-%m-%d', datetime(timestamp, 'unixepoch', 'localtime')) as day_bucket,
                model,
                SUM(total_tokens) as total
         FROM token_usage
         WHERE timestamp >= ?1
         GROUP BY day_bucket, model
         ORDER BY day_bucket ASC",
        )
        .map_err(|e| e.to_string())?;

    let mut trend_map: std::collections::BTreeMap<String, std::collections::HashMap<String, u64>> =
        std::collections::BTreeMap::new();

    let rows = stmt
        .query_map([cutoff], |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, String>(1)?,
                row.get::<_, u64>(2)?,
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

pub fn get_account_trend_hourly(hours: i64) -> Result<Vec<AccountTrendPoint>, String> {
    let conn = connect_db()?;
    let cutoff = hourly_window_start(hours).timestamp();

    let mut stmt = conn
        .prepare(
            "SELECT strftime('%Y-%m-%d %H:00', datetime(timestamp, 'unixepoch', 'localtime')) as hour_bucket,
                account_email,
                SUM(total_tokens) as total
         FROM token_usage
         WHERE timestamp >= ?1
         GROUP BY hour_bucket, account_email
         ORDER BY hour_bucket ASC",
        )
        .map_err(|e| e.to_string())?;

    let mut trend_map: std::collections::BTreeMap<String, std::collections::HashMap<String, u64>> =
        std::collections::BTreeMap::new();

    let rows = stmt
        .query_map([cutoff], |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, String>(1)?,
                row.get::<_, u64>(2)?,
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
    let cutoff = hourly_window_start(days.saturating_mul(24)).timestamp();

    let mut stmt = conn
        .prepare(
            "SELECT strftime('%Y-%m-%d', datetime(timestamp, 'unixepoch', 'localtime')) as day_bucket,
                account_email,
                SUM(total_tokens) as total
         FROM token_usage
         WHERE timestamp >= ?1
         GROUP BY day_bucket, account_email
         ORDER BY day_bucket ASC",
        )
        .map_err(|e| e.to_string())?;

    let mut trend_map: std::collections::BTreeMap<String, std::collections::HashMap<String, u64>> =
        std::collections::BTreeMap::new();

    let rows = stmt
        .query_map([cutoff], |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, String>(1)?,
                row.get::<_, u64>(2)?,
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
    use super::*;

    fn test_connection() -> Connection {
        let conn = Connection::open_in_memory().unwrap();
        conn.execute_batch(
            "CREATE TABLE token_usage (
                id INTEGER PRIMARY KEY AUTOINCREMENT,
                timestamp INTEGER NOT NULL,
                account_email TEXT NOT NULL,
                model TEXT NOT NULL,
                input_tokens INTEGER NOT NULL,
                output_tokens INTEGER NOT NULL,
                cached_tokens INTEGER NOT NULL,
                total_tokens INTEGER NOT NULL
            );
            CREATE TABLE token_stats_hourly (
                hour_bucket TEXT NOT NULL,
                account_email TEXT NOT NULL,
                total_input_tokens INTEGER NOT NULL,
                total_output_tokens INTEGER NOT NULL,
                total_cached_tokens INTEGER NOT NULL,
                total_tokens INTEGER NOT NULL,
                request_count INTEGER NOT NULL,
                PRIMARY KEY (hour_bucket, account_email)
            );",
        )
        .unwrap();
        conn
    }

    #[test]
    fn record_usage_rolls_back_raw_event_when_hourly_rollup_fails() {
        let mut conn = test_connection();
        conn.execute_batch(
            "CREATE TRIGGER abort_hourly_insert BEFORE INSERT ON token_stats_hourly
             BEGIN SELECT RAISE(ABORT, 'hourly failure'); END;",
        )
        .unwrap();

        assert!(
            record_usage_at(&mut conn, "a@example.com", "model", 3, 5, 1, Local::now()).is_err()
        );
        assert_eq!(
            conn.query_row("SELECT COUNT(*) FROM token_usage", [], |row| row
                .get::<_, u64>(0))
                .unwrap(),
            0
        );
        assert_eq!(
            conn.query_row("SELECT COUNT(*) FROM token_stats_hourly", [], |row| row
                .get::<_, u64>(0))
                .unwrap(),
            0
        );

        conn.execute_batch("DROP TRIGGER abort_hourly_insert;")
            .unwrap();
        record_usage_at(&mut conn, "a@example.com", "model", 3, 5, 1, Local::now()).unwrap();
        assert_eq!(
            conn.query_row("SELECT COUNT(*) FROM token_usage", [], |row| row
                .get::<_, u64>(0))
                .unwrap(),
            1
        );
        assert_eq!(
            conn.query_row("SELECT COUNT(*) FROM token_stats_hourly", [], |row| row
                .get::<_, u64>(0))
                .unwrap(),
            1
        );
    }

    #[test]
    fn range_cutoff_is_hour_aligned_and_independent_of_local_dst_boundaries() {
        // This is 01:30 in America/New_York's repeated fall-back hour. The
        // calculation works from the instant, not from an ambiguous local time.
        let now = DateTime::parse_from_rfc3339("2026-11-01T06:30:00Z")
            .unwrap()
            .with_timezone(&Utc);
        let expected = DateTime::parse_from_rfc3339("2026-10-31T06:00:00Z")
            .unwrap()
            .timestamp();

        assert_eq!(hourly_window_timestamp(now.timestamp(), 24), expected);
        assert_eq!(
            hourly_window_start_at(now.with_timezone(&Local), 24).timestamp(),
            expected
        );
    }

    #[test]
    fn daily_rollup_and_raw_views_use_the_same_range_cutoff() {
        let mut conn = test_connection();
        let now = DateTime::parse_from_rfc3339("2026-03-15T15:30:00Z")
            .unwrap()
            .with_timezone(&Local);
        let cutoff = hourly_window_start_at(now, 7 * 24);
        let excluded = cutoff - chrono::Duration::seconds(1);
        let included = cutoff + chrono::Duration::minutes(15);

        record_usage_at(&mut conn, "a@example.com", "old", 2, 0, 0, excluded).unwrap();
        record_usage_at(&mut conn, "a@example.com", "new", 3, 0, 0, cutoff).unwrap();
        record_usage_at(&mut conn, "b@example.com", "new", 5, 0, 0, included).unwrap();

        let raw_total: u64 = conn
            .query_row(
                "SELECT SUM(total_tokens) FROM token_usage WHERE timestamp >= ?1",
                [cutoff.timestamp()],
                |row| row.get(0),
            )
            .unwrap();
        let cutoff_bucket = cutoff.format("%Y-%m-%d %H:00").to_string();
        let rollup_total: u64 = conn
            .query_row(
                "SELECT SUM(total_tokens) FROM token_stats_hourly WHERE hour_bucket >= ?1",
                [&cutoff_bucket],
                |row| row.get(0),
            )
            .unwrap();
        let daily_rollup_total: u64 = conn
            .query_row(
                "SELECT SUM(total_tokens) FROM token_stats_hourly WHERE hour_bucket >= ?1 GROUP BY substr(hour_bucket, 1, 10)",
                [&cutoff_bucket],
                |row| row.get(0),
            )
            .unwrap();

        assert_eq!(raw_total, 8);
        assert_eq!(rollup_total, raw_total);
        assert_eq!(daily_rollup_total, raw_total);
    }
}
