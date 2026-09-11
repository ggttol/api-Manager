use crate::proxy::monitor::ProxyRequestLog;
use rusqlite::{params, Connection};
use std::path::PathBuf;

pub fn get_proxy_db_path() -> Result<PathBuf, String> {
    let data_dir = crate::modules::account::get_data_dir()?;
    Ok(data_dir.join("proxy_logs.db"))
}

fn connect_db() -> Result<Connection, String> {
    let db_path = get_proxy_db_path()?;
    let conn = Connection::open(db_path).map_err(|e| e.to_string())?;

    // Enable WAL mode for better concurrency
    conn.pragma_update(None, "journal_mode", "WAL")
        .map_err(|e| e.to_string())?;

    // Set busy timeout to 5000ms to avoid "database is locked" errors
    conn.pragma_update(None, "busy_timeout", 5000)
        .map_err(|e| e.to_string())?;

    // Synchronous NORMAL is faster and safe enough for WAL
    conn.pragma_update(None, "synchronous", "NORMAL")
        .map_err(|e| e.to_string())?;

    Ok(conn)
}

pub fn init_db() -> Result<(), String> {
    // connect_db will initialize WAL mode and other pragmas
    let conn = connect_db()?;

    conn.execute(
        "CREATE TABLE IF NOT EXISTS request_logs (
            id TEXT PRIMARY KEY,
            timestamp INTEGER,
            method TEXT,
            url TEXT,
            status INTEGER,
            duration INTEGER,
            model TEXT,
            error TEXT
        )",
        [],
    )
    .map_err(|e| e.to_string())?;

    // Try to add new columns (ignore errors if they exist)
    let _ = conn.execute("ALTER TABLE request_logs ADD COLUMN request_body TEXT", []);
    let _ = conn.execute("ALTER TABLE request_logs ADD COLUMN response_body TEXT", []);
    let _ = conn.execute(
        "ALTER TABLE request_logs ADD COLUMN input_tokens INTEGER",
        [],
    );
    let _ = conn.execute(
        "ALTER TABLE request_logs ADD COLUMN output_tokens INTEGER",
        [],
    );
    let _ = conn.execute(
        "ALTER TABLE request_logs ADD COLUMN cached_tokens INTEGER",
        [],
    );
    let _ = conn.execute("ALTER TABLE request_logs ADD COLUMN account_email TEXT", []);
    let _ = conn.execute("ALTER TABLE request_logs ADD COLUMN mapped_model TEXT", []);
    let _ = conn.execute("ALTER TABLE request_logs ADD COLUMN protocol TEXT", []);
    let _ = conn.execute("ALTER TABLE request_logs ADD COLUMN client_ip TEXT", []);
    let _ = conn.execute("ALTER TABLE request_logs ADD COLUMN username TEXT", []);

    conn.execute(
        "CREATE INDEX IF NOT EXISTS idx_timestamp ON request_logs (timestamp DESC)",
        [],
    )
    .map_err(|e| e.to_string())?;

    // Add status index for faster stats queries
    conn.execute(
        "CREATE INDEX IF NOT EXISTS idx_status ON request_logs (status)",
        [],
    )
    .map_err(|e| e.to_string())?;

    Ok(())
}

pub fn save_log(log: &ProxyRequestLog) -> Result<(), String> {
    let conn = connect_db()?;

    conn.execute(
        "INSERT INTO request_logs (id, timestamp, method, url, status, duration, model, error, request_body, response_body, input_tokens, output_tokens, cached_tokens, account_email, mapped_model, protocol, client_ip, username)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15, ?16, ?17, ?18)",
        params![
            log.id,
            log.timestamp,
            log.method,
            log.url,
            log.status,
            log.duration,
            log.model,
            log.error,
            log.request_body,
            log.response_body,
            log.input_tokens,
            log.output_tokens,
            log.cached_tokens,
            log.account_email,
            log.mapped_model,
            log.protocol,
            log.client_ip,
            log.username,
        ],
    ).map_err(|e| e.to_string())?;

    Ok(())
}

/// Get logs summary (without large request_body and response_body fields) with pagination
pub fn get_logs_summary(limit: usize, offset: usize) -> Result<Vec<ProxyRequestLog>, String> {
    let conn = connect_db()?;

    let mut stmt = conn
        .prepare(
            "SELECT id, timestamp, method, url, status, duration, model, error,
                NULL as request_body, NULL as response_body,
                input_tokens, output_tokens, cached_tokens, account_email, mapped_model, protocol, client_ip, username
         FROM request_logs 
         ORDER BY timestamp DESC 
         LIMIT ?1 OFFSET ?2",
        )
        .map_err(|e| e.to_string())?;

    let logs_iter = stmt
        .query_map([limit, offset], |row| {
            Ok(ProxyRequestLog {
                id: row.get(0)?,
                timestamp: row.get(1)?,
                method: row.get(2)?,
                url: row.get(3)?,
                status: row.get(4)?,
                duration: row.get(5)?,
                model: row.get(6)?,
                error: row.get(7)?,
                request_body: None,  // Don't query large fields for list view
                response_body: None, // Don't query large fields for list view
                input_tokens: row.get(10).unwrap_or(None),
                output_tokens: row.get(11).unwrap_or(None),
                cached_tokens: row.get(12).unwrap_or(None),
                account_email: row.get(13).unwrap_or(None),
                mapped_model: row.get(14).unwrap_or(None),
                protocol: row.get(15).unwrap_or(None),
                client_ip: row.get(16).unwrap_or(None),
                username: row.get(17).unwrap_or(None),
            })
        })
        .map_err(|e| e.to_string())?;

    let mut logs = Vec::new();
    for log in logs_iter {
        logs.push(log.map_err(|e| e.to_string())?);
    }
    Ok(logs)
}

/// Get logs (backward compatible, calls get_logs_summary)
pub fn get_logs(limit: usize) -> Result<Vec<ProxyRequestLog>, String> {
    get_logs_summary(limit, 0)
}

pub fn get_stats() -> Result<crate::proxy::monitor::ProxyStats, String> {
    let conn = connect_db()?;

    // Optimized: Use single query instead of three separate queries
    // Use COALESCE to handle NULL values when table is empty (SUM returns NULL for empty set)
    let (total_requests, success_count, error_count): (u64, u64, u64) = conn
        .query_row(
            "SELECT
            COUNT(*) as total,
            COALESCE(SUM(CASE WHEN status >= 200 AND status < 400 AND (error IS NULL OR error = '') THEN 1 ELSE 0 END), 0) as success,
            COALESCE(SUM(CASE WHEN status < 200 OR status >= 400 OR (error IS NOT NULL AND error != '') THEN 1 ELSE 0 END), 0) as error
         FROM request_logs",
            [],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
        )
        .map_err(|e| e.to_string())?;

    Ok(crate::proxy::monitor::ProxyStats {
        total_requests,
        success_count,
        error_count,
    })
}

/// Get single log detail (with request_body and response_body)
pub fn get_log_detail(log_id: &str) -> Result<ProxyRequestLog, String> {
    let conn = connect_db()?;

    let mut stmt = conn
        .prepare(
            "SELECT id, timestamp, method, url, status, duration, model, error,
                request_body, response_body, input_tokens, output_tokens,
                cached_tokens, account_email, mapped_model, protocol, client_ip, username
         FROM request_logs
         WHERE id = ?1",
        )
        .map_err(|e| e.to_string())?;

    stmt.query_row([log_id], |row| {
        Ok(ProxyRequestLog {
            id: row.get(0)?,
            timestamp: row.get(1)?,
            method: row.get(2)?,
            url: row.get(3)?,
            status: row.get(4)?,
            duration: row.get(5)?,
            model: row.get(6)?,
            error: row.get(7)?,
            request_body: row.get(8).unwrap_or(None),
            response_body: row.get(9).unwrap_or(None),
            input_tokens: row.get(10).unwrap_or(None),
            output_tokens: row.get(11).unwrap_or(None),
            cached_tokens: row.get(12).unwrap_or(None),
            account_email: row.get(13).unwrap_or(None),
            mapped_model: row.get(14).unwrap_or(None),
            protocol: row.get(15).unwrap_or(None),
            client_ip: row.get(16).unwrap_or(None),
            username: row.get(17).unwrap_or(None),
        })
    })
    .map_err(|e| e.to_string())
}

/// Cleanup old logs (keep last N days)
pub fn cleanup_old_logs(days: i64) -> Result<usize, String> {
    let conn = connect_db()?;

    // Note: Request log timestamp is stored in milliseconds (chrono::Utc::now().timestamp_millis())
    let cutoff_timestamp_ms = chrono::Utc::now().timestamp_millis() - (days * 24 * 3600 * 1000);

    let deleted = conn
        .execute(
            "DELETE FROM request_logs WHERE timestamp < ?1",
            [cutoff_timestamp_ms],
        )
        .map_err(|e| e.to_string())?;

    // Only execute VACUUM when substantial rows were deleted to avoid saturating disk I/O on startup
    if deleted >= 500 {
        if let Err(e) = conn.execute("VACUUM", []) {
            tracing::warn!("VACUUM failed after log cleanup: {}", e);
        }
    }

    Ok(deleted)
}

/// Limit maximum log count (keep newest N records)
#[allow(dead_code)]
pub fn limit_max_logs(max_count: usize) -> Result<usize, String> {
    let conn = connect_db()?;

    let deleted = conn
        .execute(
            "DELETE FROM request_logs WHERE id NOT IN (
            SELECT id FROM request_logs ORDER BY timestamp DESC LIMIT ?1
        )",
            [max_count],
        )
        .map_err(|e| e.to_string())?;

    // Only execute VACUUM when substantial rows were deleted
    if deleted >= 500 {
        if let Err(e) = conn.execute("VACUUM", []) {
            tracing::warn!("VACUUM failed after limit_max_logs: {}", e);
        }
    }

    Ok(deleted)
}

pub fn clear_logs() -> Result<(), String> {
    let conn = connect_db()?;
    conn.execute("DELETE FROM request_logs", [])
        .map_err(|e| e.to_string())?;
    Ok(())
}

/// Get total count of logs in database
pub fn get_logs_count() -> Result<u64, String> {
    let conn = connect_db()?;

    let count: u64 = conn
        .query_row("SELECT COUNT(*) FROM request_logs", [], |row| row.get(0))
        .map_err(|e| e.to_string())?;

    Ok(count)
}

const LOG_FILTER: &str = "
    (?1 = '' OR url LIKE ?2 OR method LIKE ?2 OR model LIKE ?2
        OR mapped_model LIKE ?2 OR CAST(status AS TEXT) LIKE ?2
        OR account_email LIKE ?2 OR client_ip LIKE ?2 OR protocol LIKE ?2)
    AND (?3 = 0 OR status < 200 OR status >= 400 OR (error IS NOT NULL AND error != ''))
    AND (?4 IS NULL OR account_email = ?4)";

/// Return all logged account identities, independently of pagination and filters.
pub fn get_log_accounts() -> Result<Vec<String>, String> {
    let conn = connect_db()?;
    let mut stmt = conn
        .prepare("SELECT DISTINCT account_email FROM request_logs WHERE account_email IS NOT NULL AND account_email != '' ORDER BY account_email")
        .map_err(|e| e.to_string())?;
    let accounts = stmt
        .query_map([], |row| row.get(0))
        .map_err(|e| e.to_string())?;
    accounts
        .collect::<Result<Vec<_>, _>>()
        .map_err(|e| e.to_string())
}

/// Text search, error status and exact account identity are independent filters.
pub fn get_logs_count_filtered(
    filter: &str,
    errors_only: bool,
    account_email: Option<&str>,
) -> Result<u64, String> {
    query_logs_count_filtered(&connect_db()?, filter, errors_only, account_email)
}

fn query_logs_count_filtered(
    conn: &Connection,
    filter: &str,
    errors_only: bool,
    account_email: Option<&str>,
) -> Result<u64, String> {
    conn.query_row(
        &format!("SELECT COUNT(*) FROM request_logs WHERE {LOG_FILTER}"),
        params![
            filter,
            format!("%{filter}%"),
            errors_only,
            account_email.filter(|email| !email.is_empty())
        ],
        |row| row.get(0),
    )
    .map_err(|e| e.to_string())
}

pub fn get_logs_filtered(
    filter: &str,
    errors_only: bool,
    account_email: Option<&str>,
    limit: usize,
    offset: usize,
) -> Result<Vec<ProxyRequestLog>, String> {
    query_logs_filtered(
        &connect_db()?,
        filter,
        errors_only,
        account_email,
        limit,
        offset,
    )
}

fn query_logs_filtered(
    conn: &Connection,
    filter: &str,
    errors_only: bool,
    account_email: Option<&str>,
    limit: usize,
    offset: usize,
) -> Result<Vec<ProxyRequestLog>, String> {
    let sql = format!(
        "SELECT id, timestamp, method, url, status, duration, model, error,
                input_tokens, output_tokens, cached_tokens, account_email,
                mapped_model, protocol, client_ip, username
         FROM request_logs WHERE {LOG_FILTER}
         ORDER BY timestamp DESC, id DESC LIMIT ?5 OFFSET ?6"
    );
    let mut stmt = conn.prepare(&sql).map_err(|e| e.to_string())?;
    let logs = stmt
        .query_map(
            params![
                filter,
                format!("%{filter}%"),
                errors_only,
                account_email.filter(|email| !email.is_empty()),
                limit,
                offset
            ],
            |row| {
                Ok(ProxyRequestLog {
                    id: row.get(0)?,
                    timestamp: row.get(1)?,
                    method: row.get(2)?,
                    url: row.get(3)?,
                    status: row.get(4)?,
                    duration: row.get(5)?,
                    model: row.get(6)?,
                    error: row.get(7)?,
                    request_body: None,
                    response_body: None,
                    input_tokens: row.get(8)?,
                    output_tokens: row.get(9)?,
                    cached_tokens: row.get(10)?,
                    account_email: row.get(11)?,
                    mapped_model: row.get(12)?,
                    protocol: row.get(13)?,
                    client_ip: row.get(14)?,
                    username: row.get(15)?,
                })
            },
        )
        .map_err(|e| e.to_string())?;
    logs.collect::<Result<Vec<_>, _>>()
        .map_err(|e| e.to_string())
}

/// Get all logs with full details for export
pub fn get_all_logs_for_export() -> Result<Vec<ProxyRequestLog>, String> {
    let conn = connect_db()?;

    let mut stmt = conn
        .prepare(
            "SELECT id, timestamp, method, url, status, duration, model, error,
                request_body, response_body, input_tokens, output_tokens,
                cached_tokens, account_email, mapped_model, protocol, client_ip, username
         FROM request_logs
         ORDER BY timestamp DESC",
        )
        .map_err(|e| e.to_string())?;

    let logs_iter = stmt
        .query_map([], |row| {
            Ok(ProxyRequestLog {
                id: row.get(0)?,
                timestamp: row.get(1)?,
                method: row.get(2)?,
                url: row.get(3)?,
                status: row.get(4)?,
                duration: row.get(5)?,
                model: row.get(6)?,
                error: row.get(7)?,
                request_body: row.get(8).unwrap_or(None),
                response_body: row.get(9).unwrap_or(None),
                input_tokens: row.get(10).unwrap_or(None),
                output_tokens: row.get(11).unwrap_or(None),
                cached_tokens: row.get(12).unwrap_or(None),
                account_email: row.get(13).unwrap_or(None),
                mapped_model: row.get(14).unwrap_or(None),
                protocol: row.get(15).unwrap_or(None),
                client_ip: row.get(16).unwrap_or(None),
                username: row.get(17).unwrap_or(None),
            })
        })
        .map_err(|e| e.to_string())?;

    let mut logs = Vec::new();
    for log in logs_iter {
        logs.push(log.map_err(|e| e.to_string())?);
    }
    Ok(logs)
}

// ... existing code ...

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct IpTokenStats {
    pub client_ip: String,
    pub total_tokens: i64,
    pub input_tokens: i64,
    pub output_tokens: i64,
    pub request_count: i64,
    pub username: Option<String>,
}

/// Get token usage grouped by IP
pub fn get_token_usage_by_ip(limit: usize, hours: i64) -> Result<Vec<IpTokenStats>, String> {
    let conn = connect_db()?;

    // Fix: Database stores timestamp in milliseconds, but we were calculating 'since' in seconds
    // Convert 'hours' to milliseconds
    let since = chrono::Utc::now().timestamp_millis() - (hours * 3600 * 1000);

    // [FIX] 不再从 request_logs 表获取 username，因为该字段可能为空
    // 先获取 IP 统计数据，然后再单独查询每个 IP 的用户名
    let mut stmt = conn
        .prepare(
            "SELECT
            client_ip,
            COALESCE(SUM(input_tokens), 0) + COALESCE(SUM(output_tokens), 0) as total,
            COALESCE(SUM(input_tokens), 0) as input,
            COALESCE(SUM(output_tokens), 0) as output,
            COUNT(*) as cnt
         FROM request_logs
         WHERE timestamp >= ?1 AND client_ip IS NOT NULL AND client_ip != ''
         GROUP BY client_ip
         ORDER BY total DESC
         LIMIT ?2",
        )
        .map_err(|e| e.to_string())?;

    let rows = stmt
        .query_map(params![since, limit], |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, i64>(1)?,
                row.get::<_, i64>(2)?,
                row.get::<_, i64>(3)?,
                row.get::<_, i64>(4)?,
            ))
        })
        .map_err(|e| e.to_string())?;

    let mut stats = Vec::new();
    for row in rows {
        let (client_ip, total_tokens, input_tokens, output_tokens, request_count) =
            row.map_err(|e| e.to_string())?;

        // 从 user_token_db 获取该 IP 关联的用户名
        // 这比从 request_logs 获取更可靠，因为 token_ip_bindings 表在每次 User Token 使用时都会更新
        let username =
            crate::modules::user_token_db::get_username_for_ip(&client_ip).unwrap_or(None);

        stats.push(IpTokenStats {
            client_ip,
            total_tokens,
            input_tokens,
            output_tokens,
            request_count,
            username,
        });
    }

    Ok(stats)
}

#[cfg(test)]
mod filter_tests {
    use super::*;

    fn database() -> Connection {
        let conn = Connection::open_in_memory().unwrap();
        conn.execute_batch(
            "CREATE TABLE request_logs (
                id TEXT PRIMARY KEY, timestamp INTEGER, method TEXT, url TEXT,
                status INTEGER, duration INTEGER, model TEXT, error TEXT,
                input_tokens INTEGER, output_tokens INTEGER, cached_tokens INTEGER,
                account_email TEXT, mapped_model TEXT, protocol TEXT, client_ip TEXT,
                username TEXT
            );
            INSERT INTO request_logs (id, timestamp, method, url, status, duration, model, account_email, client_ip)
            VALUES
                ('a-old', 100, 'POST', '/codex/v1/responses', 500, 1, 'gpt', 'a_100%@example.test', '192.0.2.10'),
                ('a-new', 200, 'POST', '/codex/v1/responses', 429, 1, 'gpt', 'a_100%@example.test', '192.0.2.11'),
                ('a-ok', 300, 'POST', '/codex/v1/responses', 200, 1, 'gpt', 'a_100%@example.test', '192.0.2.12'),
                ('b-error', 400, 'POST', '/codex/v1/responses', 500, 1, 'gpt', 'other-a_100%@example.test', '192.0.2.13'),
                ('a-image', 500, 'POST', '/codex/v1/images/generations', 500, 1, 'gpt-image-2', 'a_100%@example.test', '192.0.2.14');",
        ).unwrap();
        conn
    }

    #[test]
    fn account_search_and_errors_intersect_before_pagination() {
        let conn = database();
        let account = Some("a_100%@example.test");
        assert_eq!(
            query_logs_count_filtered(&conn, "responses", true, account).unwrap(),
            2
        );
        let first = query_logs_filtered(&conn, "responses", true, account, 1, 0).unwrap();
        let second = query_logs_filtered(&conn, "responses", true, account, 1, 1).unwrap();
        assert_eq!(
            first.iter().map(|log| log.id.as_str()).collect::<Vec<_>>(),
            ["a-new"]
        );
        assert_eq!(
            second.iter().map(|log| log.id.as_str()).collect::<Vec<_>>(),
            ["a-old"]
        );
    }

    #[test]
    fn ip_search_count_matches_visible_results() {
        let conn = database();
        assert_eq!(
            query_logs_count_filtered(&conn, "192.0.2.10", false, None).unwrap(),
            1
        );
        let logs = query_logs_filtered(&conn, "192.0.2.10", false, None, 50, 0).unwrap();
        assert_eq!(
            logs.iter().map(|log| log.id.as_str()).collect::<Vec<_>>(),
            ["a-old"]
        );
    }

    #[test]
    fn semantic_stream_failures_remain_visible_under_errors_only() {
        let conn = database();
        conn.execute(
            "INSERT INTO request_logs (id, timestamp, method, url, status, duration, model, error)
             VALUES ('semantic', 600, 'POST', '/codex/v1/responses', 200, 1, 'semantic-model', 'response.failed')",
            [],
        ).unwrap();
        assert_eq!(
            query_logs_count_filtered(&conn, "semantic-model", true, None).unwrap(),
            1
        );
        let logs = query_logs_filtered(&conn, "semantic-model", true, None, 50, 0).unwrap();
        assert_eq!(logs[0].id, "semantic");
        assert_eq!(logs[0].status, 200);
        assert_eq!(logs[0].error.as_deref(), Some("response.failed"));
    }
}
