//! Security Database Module
//! 安全监控相关的数据库操作

use rusqlite::{params, Connection};
use serde::{Deserialize, Serialize};
use std::path::PathBuf;
#[cfg(test)]
use std::sync::{LazyLock, Mutex, MutexGuard};
use std::time::Duration;

/// IP 访问日志
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct IpAccessLog {
    pub id: String,
    pub client_ip: String,
    pub timestamp: i64,
    pub method: Option<String>,
    pub path: Option<String>,
    pub user_agent: Option<String>,
    pub status: Option<i32>,
    pub duration: Option<i64>,
    pub api_key_hash: Option<String>,
    pub blocked: bool,
    pub block_reason: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub username: Option<String>,
}

/// IP 黑名单条目
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct IpBlacklistEntry {
    pub id: String,
    pub ip_pattern: String,
    pub reason: Option<String>,
    pub created_at: i64,
    pub expires_at: Option<i64>,
    pub created_by: String,
    pub hit_count: i64,
}

/// IP 白名单条目
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct IpWhitelistEntry {
    pub id: String,
    pub ip_pattern: String,
    pub description: Option<String>,
    pub created_at: i64,
}

/// IP 统计概览
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct IpStats {
    pub total_requests: u64,
    pub unique_ips: u64,
    pub blocked_count: u64,
    pub today_requests: u64,
    pub blacklist_count: u64,
    pub whitelist_count: u64,
}

/// IP 访问排行
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct IpRanking {
    pub client_ip: String,
    pub request_count: u64,
    pub last_seen: i64,
    pub is_blocked: bool,
}

/// 获取安全数据库路径
pub fn get_security_db_path() -> Result<PathBuf, String> {
    #[cfg(test)]
    if let Some(path) = test_db_path()
        .lock()
        .map_err(|_| "security test database path lock poisoned".to_string())?
        .clone()
    {
        return Ok(path);
    }

    let data_dir = crate::modules::account::get_data_dir()?;
    Ok(data_dir.join("security.db"))
}

/// 连接数据库
fn connect_db() -> Result<Connection, String> {
    let db_path = get_security_db_path()?;
    let conn = Connection::open(db_path).map_err(|e| e.to_string())?;

    // Configure waiting before changing journal mode, because the latter can contend when
    // independent request threads open their own connections.
    conn.busy_timeout(Duration::from_secs(5))
        .map_err(|e| e.to_string())?;
    conn.pragma_update(None, "journal_mode", "WAL")
        .map_err(|e| e.to_string())?;
    conn.pragma_update(None, "synchronous", "NORMAL")
        .map_err(|e| e.to_string())?;

    Ok(conn)
}

#[cfg(test)]
static TEST_DB_LOCK: LazyLock<Mutex<()>> = LazyLock::new(|| Mutex::new(()));
#[cfg(test)]
static TEST_DB_PATH: LazyLock<Mutex<Option<PathBuf>>> = LazyLock::new(|| Mutex::new(None));

#[cfg(test)]
fn test_db_path() -> &'static Mutex<Option<PathBuf>> {
    &TEST_DB_PATH
}

/// Serializes security database tests and routes their connections to a temporary database.
///
/// The guard deliberately changes neither the process environment nor the user's data directory.
#[cfg(test)]
pub struct TestDbGuard {
    _lock: MutexGuard<'static, ()>,
    directory: PathBuf,
}

#[cfg(test)]
pub fn test_db_guard() -> Result<TestDbGuard, String> {
    let lock = TEST_DB_LOCK
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    let directory =
        std::env::temp_dir().join(format!("api-manager-security-{}", uuid::Uuid::new_v4()));
    std::fs::create_dir_all(&directory).map_err(|e| e.to_string())?;
    let guard = TestDbGuard {
        _lock: lock,
        directory,
    };
    *test_db_path()
        .lock()
        .map_err(|_| "security test database path lock poisoned".to_string())? =
        Some(guard.directory.join("security.db"));
    init_db()?;
    Ok(guard)
}

#[cfg(test)]
impl Drop for TestDbGuard {
    fn drop(&mut self) {
        if let Ok(mut path) = test_db_path().lock() {
            *path = None;
        }
        let _ = std::fs::remove_dir_all(&self.directory);
    }
}

/// 初始化安全数据库
pub fn init_db() -> Result<(), String> {
    let conn = connect_db()?;

    // IP 访问日志表
    conn.execute(
        "CREATE TABLE IF NOT EXISTS ip_access_logs (
            id TEXT PRIMARY KEY,
            client_ip TEXT NOT NULL,
            timestamp INTEGER NOT NULL,
            method TEXT,
            path TEXT,
            user_agent TEXT,
            status INTEGER,
            duration INTEGER,
            api_key_hash TEXT,
            blocked INTEGER DEFAULT 0,
            block_reason TEXT
        )",
        [],
    )
    .map_err(|e| e.to_string())?;

    // IP 黑名单表
    conn.execute(
        "CREATE TABLE IF NOT EXISTS ip_blacklist (
            id TEXT PRIMARY KEY,
            ip_pattern TEXT NOT NULL UNIQUE,
            reason TEXT,
            created_at INTEGER NOT NULL,
            expires_at INTEGER,
            created_by TEXT DEFAULT 'manual',
            hit_count INTEGER DEFAULT 0
        )",
        [],
    )
    .map_err(|e| e.to_string())?;

    // IP 白名单表
    conn.execute(
        "CREATE TABLE IF NOT EXISTS ip_whitelist (
            id TEXT PRIMARY KEY,
            ip_pattern TEXT NOT NULL UNIQUE,
            description TEXT,
            created_at INTEGER NOT NULL
        )",
        [],
    )
    .map_err(|e| e.to_string())?;

    // 创建索引
    conn.execute(
        "CREATE INDEX IF NOT EXISTS idx_ip_access_ip ON ip_access_logs (client_ip)",
        [],
    )
    .map_err(|e| e.to_string())?;

    conn.execute(
        "CREATE INDEX IF NOT EXISTS idx_ip_access_timestamp ON ip_access_logs (timestamp DESC)",
        [],
    )
    .map_err(|e| e.to_string())?;

    conn.execute(
        "CREATE INDEX IF NOT EXISTS idx_ip_access_blocked ON ip_access_logs (blocked)",
        [],
    )
    .map_err(|e| e.to_string())?;

    conn.execute(
        "CREATE INDEX IF NOT EXISTS idx_blacklist_pattern ON ip_blacklist (ip_pattern)",
        [],
    )
    .map_err(|e| e.to_string())?;

    // Migration: Add username column to ip_access_logs
    let _ = conn.execute("ALTER TABLE ip_access_logs ADD COLUMN username TEXT", []);

    Ok(())
}

// ============================================================================
// IP 访问日志操作
// ============================================================================

/// 保存 IP 访问日志
pub fn save_ip_access_log(log: &IpAccessLog) -> Result<(), String> {
    let conn = connect_db()?;

    conn.execute(
        "INSERT INTO ip_access_logs (id, client_ip, timestamp, method, path, user_agent, status, duration, api_key_hash, blocked, block_reason, username)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12)",
        params![
            log.id,
            log.client_ip,
            log.timestamp,
            log.method,
            log.path,
            log.user_agent,
            log.status,
            log.duration,
            log.api_key_hash,
            log.blocked,
            log.block_reason,
            log.username,
        ],
    )
    .map_err(|e| e.to_string())?;

    Ok(())
}

/// 获取 IP 访问日志 (分页)
pub fn get_ip_access_logs(
    limit: usize,
    offset: usize,
    ip_filter: Option<&str>,
    blocked_only: bool,
) -> Result<Vec<IpAccessLog>, String> {
    let conn = connect_db()?;
    let mut stmt = conn
        .prepare(
            "SELECT id, client_ip, timestamp, method, path, user_agent, status, duration, api_key_hash, blocked, block_reason, username
             FROM ip_access_logs
             WHERE (?1 = 0 OR blocked = 1)
               AND (?2 IS NULL OR client_ip LIKE '%' || ?2 || '%')
             ORDER BY timestamp DESC
             LIMIT ?3 OFFSET ?4",
        )
        .map_err(|e| e.to_string())?;

    let logs_iter = stmt
        .query_map(
            params![blocked_only as i64, ip_filter, limit as i64, offset as i64],
            |row| {
                Ok(IpAccessLog {
                    id: row.get(0)?,
                    client_ip: row.get(1)?,
                    timestamp: row.get(2)?,
                    method: row.get(3)?,
                    path: row.get(4)?,
                    user_agent: row.get(5)?,
                    status: row.get(6)?,
                    duration: row.get(7)?,
                    api_key_hash: row.get(8)?,
                    blocked: row.get::<_, i32>(9)? != 0,
                    block_reason: row.get(10)?,
                    username: row.get(11).unwrap_or(None),
                })
            },
        )
        .map_err(|e| e.to_string())?;

    let mut logs = Vec::new();
    for log in logs_iter {
        logs.push(log.map_err(|e| e.to_string())?);
    }
    Ok(logs)
}

/// 获取 IP 统计概览
pub fn get_ip_stats() -> Result<IpStats, String> {
    let conn = connect_db()?;

    let today_start = chrono::Utc::now()
        .date_naive()
        .and_hms_opt(0, 0, 0)
        .unwrap()
        .and_utc()
        .timestamp();

    let (total_requests, unique_ips, blocked_count, today_requests): (u64, u64, u64, u64) = conn
        .query_row(
            "SELECT
                COUNT(*) as total,
                COUNT(DISTINCT client_ip) as unique_ips,
                COALESCE(SUM(CASE WHEN blocked = 1 THEN 1 ELSE 0 END), 0) as blocked,
                COALESCE(SUM(CASE WHEN timestamp >= ?1 THEN 1 ELSE 0 END), 0) as today
             FROM ip_access_logs",
            [today_start],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?)),
        )
        .map_err(|e| e.to_string())?;

    let blacklist_count: u64 = conn
        .query_row("SELECT COUNT(*) FROM ip_blacklist", [], |row| row.get(0))
        .map_err(|e| e.to_string())?;

    let whitelist_count: u64 = conn
        .query_row("SELECT COUNT(*) FROM ip_whitelist", [], |row| row.get(0))
        .map_err(|e| e.to_string())?;

    Ok(IpStats {
        total_requests,
        unique_ips,
        blocked_count,
        today_requests,
        blacklist_count,
        whitelist_count,
    })
}

/// 获取 TOP N IP 访问排行
pub fn get_top_ips(limit: usize, hours: i64) -> Result<Vec<IpRanking>, String> {
    let conn = connect_db()?;

    let since = chrono::Utc::now().timestamp() - (hours * 3600);

    let mut stmt = conn
        .prepare(
            "SELECT client_ip, COUNT(*) as cnt, MAX(timestamp) as last_seen
             FROM ip_access_logs
             WHERE timestamp >= ?1
             GROUP BY client_ip
             ORDER BY cnt DESC
             LIMIT ?2",
        )
        .map_err(|e| e.to_string())?;

    let rankings_iter = stmt
        .query_map([since, limit as i64], |row| {
            Ok(IpRanking {
                client_ip: row.get(0)?,
                request_count: row.get(1)?,
                last_seen: row.get(2)?,
                is_blocked: false, // 稍后填充
            })
        })
        .map_err(|e| e.to_string())?;

    let mut rankings = Vec::new();
    for r in rankings_iter {
        let mut ranking = r.map_err(|e| e.to_string())?;
        // 检查是否在黑名单中
        ranking.is_blocked = is_ip_in_blacklist(&ranking.client_ip)?;
        rankings.push(ranking);
    }

    Ok(rankings)
}

/// 清理旧的 IP 访问日志
pub fn cleanup_old_ip_logs(days: i64) -> Result<usize, String> {
    let conn = connect_db()?;

    let cutoff_timestamp = chrono::Utc::now().timestamp() - (days * 24 * 3600);

    let deleted = conn
        .execute(
            "DELETE FROM ip_access_logs WHERE timestamp < ?1",
            [cutoff_timestamp],
        )
        .map_err(|e| e.to_string())?;

    // VACUUM to reclaim space
    conn.execute("VACUUM", []).map_err(|e| e.to_string())?;

    Ok(deleted)
}

// ============================================================================
// 黑名单操作
// ============================================================================

/// 添加 IP 到黑名单
fn validate_ip_pattern(ip_pattern: &str) -> Result<(), String> {
    let (address, prefix) = match ip_pattern.split_once('/') {
        Some((address, prefix)) => (address, Some(prefix)),
        None => (ip_pattern, None),
    };
    let address: std::net::Ipv4Addr = address
        .parse()
        .map_err(|_| "IP pattern must be an IPv4 address or CIDR".to_string())?;
    if let Some(prefix) = prefix {
        let prefix: u8 = prefix
            .parse()
            .map_err(|_| "CIDR prefix must be between 0 and 32".to_string())?;
        if prefix > 32 {
            return Err("CIDR prefix must be between 0 and 32".to_string());
        }
    }
    let _ = address;
    Ok(())
}

pub fn add_to_blacklist(
    ip_pattern: &str,
    reason: Option<&str>,
    expires_at: Option<i64>,
    created_by: &str,
) -> Result<IpBlacklistEntry, String> {
    validate_ip_pattern(ip_pattern)?;
    let conn = connect_db()?;

    let id = uuid::Uuid::new_v4().to_string();
    let now = chrono::Utc::now().timestamp();

    conn.execute(
        "INSERT INTO ip_blacklist (id, ip_pattern, reason, created_at, expires_at, created_by, hit_count)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, 0)",
        params![id, ip_pattern, reason, now, expires_at, created_by],
    )
    .map_err(|e| e.to_string())?;

    Ok(IpBlacklistEntry {
        id,
        ip_pattern: ip_pattern.to_string(),
        reason: reason.map(|s| s.to_string()),
        created_at: now,
        expires_at,
        created_by: created_by.to_string(),
        hit_count: 0,
    })
}

/// 从黑名单移除
pub fn remove_from_blacklist(id: &str) -> Result<(), String> {
    let conn = connect_db()?;

    conn.execute("DELETE FROM ip_blacklist WHERE id = ?1", [id])
        .map_err(|e| e.to_string())?;

    Ok(())
}

/// 获取黑名单列表
pub fn get_blacklist() -> Result<Vec<IpBlacklistEntry>, String> {
    let conn = connect_db()?;

    let mut stmt = conn
        .prepare(
            "SELECT id, ip_pattern, reason, created_at, expires_at, created_by, hit_count
             FROM ip_blacklist
             ORDER BY created_at DESC",
        )
        .map_err(|e| e.to_string())?;

    let entries_iter = stmt
        .query_map([], |row| {
            Ok(IpBlacklistEntry {
                id: row.get(0)?,
                ip_pattern: row.get(1)?,
                reason: row.get(2)?,
                created_at: row.get(3)?,
                expires_at: row.get(4)?,
                created_by: row.get(5)?,
                hit_count: row.get(6)?,
            })
        })
        .map_err(|e| e.to_string())?;

    let mut entries = Vec::new();
    for e in entries_iter {
        entries.push(e.map_err(|e| e.to_string())?);
    }
    Ok(entries)
}

/// 检查 IP 是否在黑名单中
pub fn is_ip_in_blacklist(ip: &str) -> Result<bool, String> {
    get_blacklist_entry_for_ip(ip).map(|entry| entry.is_some())
}

/// 获取 IP 对应的黑名单条目（如果存在）
pub fn get_blacklist_entry_for_ip(ip: &str) -> Result<Option<IpBlacklistEntry>, String> {
    let conn = connect_db()?;
    let now = chrono::Utc::now().timestamp();

    // 清理过期的黑名单条目
    let _ = conn.execute(
        "DELETE FROM ip_blacklist WHERE expires_at IS NOT NULL AND expires_at <= ?1",
        [now],
    );

    // 精确匹配
    let entry_result = conn.query_row(
        "SELECT id, ip_pattern, reason, created_at, expires_at, created_by, hit_count
         FROM ip_blacklist
         WHERE ip_pattern = ?1 AND (expires_at IS NULL OR expires_at > ?2)",
        params![ip, now],
        |row| {
            Ok(IpBlacklistEntry {
                id: row.get(0)?,
                ip_pattern: row.get(1)?,
                reason: row.get(2)?,
                created_at: row.get(3)?,
                expires_at: row.get(4)?,
                created_by: row.get(5)?,
                hit_count: row.get(6)?,
            })
        },
    );

    if let Ok(mut entry) = entry_result {
        conn.execute(
            "UPDATE ip_blacklist SET hit_count = hit_count + 1 WHERE id = ?1",
            [&entry.id],
        )
        .map_err(|e| e.to_string())?;
        entry.hit_count += 1;
        return Ok(Some(entry));
    }

    // CIDR 匹配
    let entries = get_blacklist()?;
    for mut entry in entries {
        if entry.expires_at.map_or(true, |expires_at| expires_at > now)
            && entry.ip_pattern.contains('/')
            && cidr_match(ip, &entry.ip_pattern)
        {
            conn.execute(
                "UPDATE ip_blacklist SET hit_count = hit_count + 1 WHERE id = ?1",
                [&entry.id],
            )
            .map_err(|e| e.to_string())?;
            entry.hit_count += 1;
            return Ok(Some(entry));
        }
    }

    Ok(None)
}

/// 简单的 CIDR 匹配
fn cidr_match(ip: &str, cidr: &str) -> bool {
    let Some((network, prefix)) = cidr.split_once('/') else {
        return false;
    };
    let Ok(prefix_len) = prefix.parse::<u8>() else {
        return false;
    };
    if prefix_len > 32 {
        return false;
    }
    let (Ok(ip), Ok(network)) = (
        ip.parse::<std::net::Ipv4Addr>(),
        network.parse::<std::net::Ipv4Addr>(),
    ) else {
        return false;
    };
    let mask = if prefix_len == 0 {
        0
    } else {
        !0u32 << (32 - prefix_len)
    };
    (u32::from(ip) & mask) == (u32::from(network) & mask)
}

// ============================================================================
// 白名单操作
// ============================================================================

/// 添加 IP 到白名单
pub fn add_to_whitelist(
    ip_pattern: &str,
    description: Option<&str>,
) -> Result<IpWhitelistEntry, String> {
    validate_ip_pattern(ip_pattern)?;
    let conn = connect_db()?;

    let id = uuid::Uuid::new_v4().to_string();
    let now = chrono::Utc::now().timestamp();

    conn.execute(
        "INSERT INTO ip_whitelist (id, ip_pattern, description, created_at)
         VALUES (?1, ?2, ?3, ?4)",
        params![id, ip_pattern, description, now],
    )
    .map_err(|e| e.to_string())?;

    Ok(IpWhitelistEntry {
        id,
        ip_pattern: ip_pattern.to_string(),
        description: description.map(|s| s.to_string()),
        created_at: now,
    })
}

/// 从白名单移除
pub fn remove_from_whitelist(id: &str) -> Result<(), String> {
    let conn = connect_db()?;

    conn.execute("DELETE FROM ip_whitelist WHERE id = ?1", [id])
        .map_err(|e| e.to_string())?;

    Ok(())
}

/// 获取白名单列表
pub fn get_whitelist() -> Result<Vec<IpWhitelistEntry>, String> {
    let conn = connect_db()?;

    let mut stmt = conn
        .prepare(
            "SELECT id, ip_pattern, description, created_at
             FROM ip_whitelist
             ORDER BY created_at DESC",
        )
        .map_err(|e| e.to_string())?;

    let entries_iter = stmt
        .query_map([], |row| {
            Ok(IpWhitelistEntry {
                id: row.get(0)?,
                ip_pattern: row.get(1)?,
                description: row.get(2)?,
                created_at: row.get(3)?,
            })
        })
        .map_err(|e| e.to_string())?;

    let mut entries = Vec::new();
    for e in entries_iter {
        entries.push(e.map_err(|e| e.to_string())?);
    }
    Ok(entries)
}

/// 检查 IP 是否在白名单中
pub fn is_ip_in_whitelist(ip: &str) -> Result<bool, String> {
    let conn = connect_db()?;

    // 精确匹配
    let count: i64 = conn
        .query_row(
            "SELECT COUNT(*) FROM ip_whitelist WHERE ip_pattern = ?1",
            [ip],
            |row| row.get(0),
        )
        .map_err(|e| e.to_string())?;

    if count > 0 {
        return Ok(true);
    }

    // CIDR 匹配
    let entries = get_whitelist()?;
    for entry in entries {
        if entry.ip_pattern.contains('/') {
            if cidr_match(ip, &entry.ip_pattern) {
                return Ok(true);
            }
        }
    }

    Ok(false)
}

/// 清空所有 IP 访问日志
pub fn clear_ip_access_logs() -> Result<(), String> {
    let conn = connect_db()?;
    conn.execute("DELETE FROM ip_access_logs", [])
        .map_err(|e| e.to_string())?;
    Ok(())
}

/// 获取 IP 访问日志总数
pub fn get_ip_access_logs_count(
    ip_filter: Option<&str>,
    blocked_only: bool,
) -> Result<u64, String> {
    let conn = connect_db()?;
    conn.query_row(
        "SELECT COUNT(*) FROM ip_access_logs
         WHERE (?1 = 0 OR blocked = 1)
           AND (?2 IS NULL OR client_ip LIKE '%' || ?2 || '%')",
        params![blocked_only as i64, ip_filter],
        |row| row.get(0),
    )
    .map_err(|e| e.to_string())
}

#[cfg(test)]
mod tests {
    use super::{cidr_match, validate_ip_pattern};

    #[test]
    fn invalid_cidr_is_rejected_and_never_matches() {
        assert!(validate_ip_pattern("192.0.2.0/33").is_err());
        assert!(!cidr_match("192.0.2.1", "192.0.2.0/33"));
    }
}
