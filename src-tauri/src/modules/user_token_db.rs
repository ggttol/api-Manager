//! User Token Database Module
//! UserToken 数据库操作模块

#![allow(dead_code)]
// 用户令牌存储，部分接口留作后续扩展

use chrono::{FixedOffset, Local, Timelike, Utc};
use rusqlite::{params, Connection, OptionalExtension, TransactionBehavior};
use serde::{Deserialize, Serialize};
use std::path::PathBuf;
use uuid::Uuid;

/// 用户令牌结构体
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
    pub curfew_start: Option<String>, // "HH:MM" 宵禁开始时间
    pub curfew_end: Option<String>,   // "HH:MM" 宵禁结束时间
    pub created_at: i64,
    pub updated_at: i64,
    pub last_used_at: Option<i64>,
    pub total_requests: i64,
    pub total_tokens_used: i64,
}

/// 令牌 IP 绑定结构体
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

/// 令牌使用日志结构体
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

/// 获取数据库路径
pub fn get_db_path() -> Result<PathBuf, String> {
    let mut path = crate::modules::account::get_data_dir()?;
    path.push("user_tokens.db");
    Ok(path)
}

/// Connect to the token database with declared foreign-key invariants enabled.
pub fn connect_db() -> Result<Connection, String> {
    let path = get_db_path()?;
    let conn = Connection::open(&path).map_err(|e| format!("Failed to open database: {}", e))?;
    conn.execute_batch("PRAGMA foreign_keys = ON; PRAGMA busy_timeout = 5000;")
        .map_err(|e| format!("Failed to configure database connection: {}", e))?;
    Ok(conn)
}

/// 初始化数据库
pub fn init_db() -> Result<(), String> {
    let conn = connect_db()?;

    // 创建 user_tokens 表
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

    // 尝试添加新列 (用于旧数据库迁移，忽略已存在的错误)
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

    // 创建 token_ip_bindings 表
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

    // 创建 token_usage_logs 表
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

    // 创建索引
    let _ = conn.execute(
        "CREATE INDEX IF NOT EXISTS idx_token_usage_logs_token_id ON token_usage_logs(token_id)",
        [],
    );
    let _ = conn.execute("CREATE INDEX IF NOT EXISTS idx_token_usage_logs_request_time ON token_usage_logs(request_time)", []);

    // [FIX Issue #1719] 数据清洗：修复旧版本升级导致的 NULL 字段
    // 这些字段在旧版本中可能不存在，ALTER TABLE 添加后默认为 NULL，导致反序列化失败
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

fn expiry_for_type(
    expires_type: &str,
    custom_expires_at: Option<i64>,
) -> Result<Option<i64>, String> {
    let now = Utc::now();
    match expires_type {
        "day" => Ok(Some(
            now.checked_add_signed(chrono::Duration::days(1))
                .ok_or("Token expiry overflow")?
                .timestamp(),
        )),
        "week" => Ok(Some(
            now.checked_add_signed(chrono::Duration::weeks(1))
                .ok_or("Token expiry overflow")?
                .timestamp(),
        )),
        "month" => Ok(Some(
            now.checked_add_signed(chrono::Duration::days(30))
                .ok_or("Token expiry overflow")?
                .timestamp(),
        )),
        "never" => Ok(None),
        "custom" => match custom_expires_at {
            Some(expires_at) if expires_at > now.timestamp() => Ok(Some(expires_at)),
            Some(_) => Err("Custom token expiry must be in the future".to_string()),
            None => Err("Custom token expiry is required".to_string()),
        },
        _ => Err("Unsupported token expiry type".to_string()),
    }
}

/// 创建新令牌
pub fn create_token(
    username: String,
    expires_type: String,
    description: Option<String>,
    max_ips: i32,
    curfew_start: Option<String>,
    curfew_end: Option<String>,
    custom_expires_at: Option<i64>, // 自定义过期时间戳 (秒)
) -> Result<UserToken, String> {
    let conn = connect_db()?;
    let id = Uuid::new_v4().to_string();
    let token = format!("sk-{}", Uuid::new_v4().to_string().replace("-", ""));
    let now = Utc::now().timestamp();

    let expires_at = expiry_for_type(&expires_type, custom_expires_at)?;

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

/// 列出所有令牌
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
                enabled: row.get("enabled").unwrap_or(true), // 防御性默认值
                expires_type: row.get("expires_type").unwrap_or("never".to_string()), // 防御性默认值
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
/// Count requests authenticated with any user token since server-local midnight.
pub fn get_today_request_count() -> Result<i64, String> {
    let start_of_today = Local::now()
        .date_naive()
        .and_hms_opt(0, 0, 0)
        .and_then(|midnight| midnight.and_local_timezone(Local).earliest())
        .ok_or_else(|| "Failed to determine the start of the local day".to_string())?
        .timestamp();
    let conn = connect_db()?;

    conn.query_row(
        "SELECT COUNT(*) FROM token_usage_logs WHERE request_time >= ?1",
        params![start_of_today],
        |row| row.get(0),
    )
    .map_err(|e| format!("Failed to query today's token requests: {}", e))
}

/// 获取单个令牌信息
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

/// 根据 Token 值获取令牌信息
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

/// 更新令牌状态/备注等
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
    update_token_with_connection(
        &conn,
        id,
        username,
        description,
        enabled,
        max_ips,
        curfew_start,
        curfew_end,
    )
}

fn update_token_with_connection(
    conn: &Connection,
    id: &str,
    username: Option<String>,
    description: Option<String>,
    enabled: Option<bool>,
    max_ips: Option<i32>,
    curfew_start: Option<Option<String>>,
    curfew_end: Option<Option<String>>,
) -> Result<(), String> {
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

    // 将 Vec<Box<dyn ToSql>> 转换为 &[&dyn ToSql]
    let params_refs: Vec<&dyn rusqlite::ToSql> = params_vec.iter().map(|p| p.as_ref()).collect();

    conn.execute(&query, params_refs.as_slice())
        .map_err(|e| format!("Failed to update user token: {}", e))?;

    Ok(())
}

/// Delete a token and all data that depends on it.
///
/// The foreign-key constraints declared by `init_db` cascade bindings and usage
/// logs; using an immediate transaction keeps the parent removal and those
/// cascades indivisible from concurrent admission/accounting work.
pub fn delete_token(id: &str) -> Result<(), String> {
    let mut conn = connect_db()?;
    delete_token_with_connection(&mut conn, id)
}

fn delete_token_with_connection(conn: &mut Connection, id: &str) -> Result<(), String> {
    let tx = conn
        .transaction_with_behavior(TransactionBehavior::Immediate)
        .map_err(|e| format!("Failed to begin token deletion transaction: {}", e))?;
    let deleted = tx
        .execute("DELETE FROM user_tokens WHERE id = ?1", params![id])
        .map_err(|e| format!("Failed to delete user token: {}", e))?;
    if deleted == 0 {
        return Err("User token not found".to_string());
    }
    tx.commit()
        .map_err(|e| format!("Failed to commit token deletion: {}", e))
}

/// Renew a token using an explicit supported duration. Custom expirations must be
/// created with a supplied finite timestamp and cannot be inferred at renewal time.
pub fn renew_token(id: &str, expires_type: &str) -> Result<(), String> {
    let conn = connect_db()?;
    let now = Utc::now().timestamp();
    let expires_at = expiry_for_type(expires_type, None)?;

    conn.execute(
        "UPDATE user_tokens SET expires_type = ?1, expires_at = ?2, updated_at = ?3, enabled = 1 WHERE id = ?4",
        params![expires_type, expires_at, now, id],
    )
    .map_err(|e| format!("Failed to renew token: {}", e))?;

    Ok(())
}

/// 获取令牌的所有 IP 绑定
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

/// 记录/更新令牌使用情况 (同时处理 user_tokens 和 token_ip_bindings)
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

    // 1. 更新 user_tokens 主表
    tx.execute(
        "UPDATE user_tokens SET 
            last_used_at = ?1, 
            total_requests = total_requests + 1, 
            total_tokens_used = total_tokens_used + ?2 
        WHERE id = ?3",
        params![now, input_tokens + output_tokens, token_id],
    )
    .map_err(|e| format!("Failed to update user_tokens stats: {}", e))?;

    // 2. 更新或插入 token_ip_bindings 表
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

    // 3. 插入 token_usage_logs 表
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

/// Check token admission and atomically reserve a limited token's IP slot.
///
/// Reservations deliberately have `request_count = 0`; request/accounting metrics
/// are updated only after the response by `record_token_usage_and_ip`.
pub fn validate_token(token_str: &str, ip: &str) -> Result<(bool, Option<String>), String> {
    let mut conn = connect_db()?;
    validate_token_with_connection(&mut conn, token_str, ip)
}

fn validate_token_with_connection(
    conn: &mut Connection,
    token_str: &str,
    ip: &str,
) -> Result<(bool, Option<String>), String> {
    let tx = conn
        .transaction_with_behavior(TransactionBehavior::Immediate)
        .map_err(|e| format!("Failed to begin token admission transaction: {}", e))?;
    let token = tx
        .query_row(
            "SELECT id, enabled, expires_type, expires_at, max_ips, curfew_start, curfew_end
             FROM user_tokens WHERE token = ?1",
            params![token_str],
            |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, bool>(1)?,
                    row.get::<_, String>(2)?,
                    row.get::<_, Option<i64>>(3)?,
                    row.get::<_, i32>(4)?,
                    row.get::<_, Option<String>>(5)?,
                    row.get::<_, Option<String>>(6)?,
                ))
            },
        )
        .optional()
        .map_err(|e| format!("Failed to query token for admission: {}", e))?;

    let Some((token_id, enabled, expires_type, expires_at, max_ips, curfew_start, curfew_end)) =
        token
    else {
        return Ok((
            false,
            Some("Invalid token. Please check your API key.".to_string()),
        ));
    };

    if !enabled {
        return Ok((
            false,
            Some("This token has been disabled. Please contact the administrator.".to_string()),
        ));
    }

    if expires_type != "never"
        && expires_at.is_some_and(|expires_at| expires_at < Utc::now().timestamp())
    {
        return Ok((
            false,
            Some(
                "Your token has expired. Please contact the administrator to renew it.".to_string(),
            ),
        ));
    }

    if let (Some(start_str), Some(end_str)) = (&curfew_start, &curfew_end) {
        if !start_str.is_empty() && !end_str.is_empty() {
            let beijing_offset = FixedOffset::east_opt(8 * 3600).expect("UTC+8 is valid");
            let now_beijing = Utc::now().with_timezone(&beijing_offset);
            let current_time_str = format!("{:02}:{:02}", now_beijing.hour(), now_beijing.minute());
            let is_curfew = if start_str > end_str {
                current_time_str >= *start_str || current_time_str < *end_str
            } else {
                current_time_str >= *start_str && current_time_str < *end_str
            };
            if is_curfew {
                return Ok((
                    false,
                    Some(format!(
                        "Service is not available between {} and {} Beijing Time (Curfew enabled). Current Beijing time: {}",
                        start_str, end_str, current_time_str
                    )),
                ));
            }
        }
    }

    if max_ips > 0 {
        let is_bound: bool = tx
            .query_row(
                "SELECT EXISTS(SELECT 1 FROM token_ip_bindings WHERE token_id = ?1 AND ip_address = ?2)",
                params![token_id, ip],
                |row| row.get(0),
            )
            .map_err(|e| format!("Failed to query token IP binding: {}", e))?;
        if !is_bound {
            let current_ip_count: i32 = tx
                .query_row(
                    "SELECT COUNT(*) FROM token_ip_bindings WHERE token_id = ?1",
                    params![token_id],
                    |row| row.get(0),
                )
                .map_err(|e| format!("Failed to count token IP bindings: {}", e))?;
            if current_ip_count >= max_ips {
                return Ok((
                    false,
                    Some(format!(
                        "IP limit reached ({}/{}). Please contact the administrator to increase the limit.",
                        current_ip_count, max_ips
                    )),
                ));
            }
            tx.execute(
                "INSERT INTO token_ip_bindings (
                    id, token_id, ip_address, first_seen_at, last_seen_at, request_count, user_agent
                ) VALUES (?1, ?2, ?3, ?4, ?4, 0, NULL)",
                params![
                    Uuid::new_v4().to_string(),
                    token_id,
                    ip,
                    Utc::now().timestamp()
                ],
            )
            .map_err(|e| format!("Failed to reserve token IP binding: {}", e))?;
        }
    }

    tx.commit()
        .map_err(|e| format!("Failed to commit token admission: {}", e))?;
    Ok((true, None))
}

/// 获取 IP 关联的用户名 (用于 IP 管理页面)
/// 返回最近一次使用该 IP 的 Token 所属的用户名
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

    fn test_connection() -> Connection {
        let conn = Connection::open_in_memory().expect("in-memory database");
        conn.execute_batch(
            "PRAGMA foreign_keys = ON;
             CREATE TABLE user_tokens (
                id TEXT PRIMARY KEY, token TEXT UNIQUE NOT NULL, username TEXT NOT NULL,
                description TEXT, enabled BOOLEAN NOT NULL, expires_type TEXT NOT NULL,
                expires_at INTEGER, max_ips INTEGER NOT NULL, curfew_start TEXT, curfew_end TEXT,
                created_at INTEGER NOT NULL, updated_at INTEGER NOT NULL, last_used_at INTEGER,
                total_requests INTEGER NOT NULL DEFAULT 0, total_tokens_used INTEGER NOT NULL DEFAULT 0
             );
             CREATE TABLE token_ip_bindings (
                id TEXT PRIMARY KEY, token_id TEXT NOT NULL, ip_address TEXT NOT NULL,
                first_seen_at INTEGER NOT NULL, last_seen_at INTEGER NOT NULL,
                request_count INTEGER NOT NULL DEFAULT 0, user_agent TEXT,
                FOREIGN KEY(token_id) REFERENCES user_tokens(id) ON DELETE CASCADE,
                UNIQUE(token_id, ip_address)
             );
             CREATE TABLE token_usage_logs (
                id TEXT PRIMARY KEY, token_id TEXT NOT NULL, ip_address TEXT,
                model TEXT, input_tokens INTEGER, output_tokens INTEGER,
                request_time INTEGER NOT NULL, status INTEGER,
                FOREIGN KEY(token_id) REFERENCES user_tokens(id) ON DELETE CASCADE
             );",
        )
        .expect("test schema");
        conn
    }

    fn insert_token(conn: &Connection, id: &str, token: &str, enabled: bool, max_ips: i32) {
        conn.execute(
            "INSERT INTO user_tokens (
                id, token, username, enabled, expires_type, expires_at, max_ips,
                curfew_start, curfew_end, created_at, updated_at, total_requests, total_tokens_used
             ) VALUES (?1, ?2, 'test', ?3, 'never', NULL, ?4, NULL, NULL, 0, 0, 0, 0)",
            params![id, token, enabled, max_ips],
        )
        .expect("insert token");
    }

    #[test]
    fn disabled_token_is_rejected_before_admission() {
        let mut conn = test_connection();
        insert_token(&conn, "disabled", "sk-disabled", false, 1);

        let (valid, reason) =
            validate_token_with_connection(&mut conn, "sk-disabled", "198.51.100.1")
                .expect("admission query");

        assert!(!valid);
        assert!(reason.expect("rejection reason").contains("disabled"));
        let reservations: i64 = conn
            .query_row("SELECT COUNT(*) FROM token_ip_bindings", [], |row| {
                row.get(0)
            })
            .expect("reservation count");
        assert_eq!(reservations, 0);
    }

    #[test]
    fn admission_reserves_limited_ip_without_counting_a_response() {
        let mut conn = test_connection();
        insert_token(&conn, "limited", "sk-limited", true, 1);

        assert!(
            validate_token_with_connection(&mut conn, "sk-limited", "198.51.100.1")
                .expect("first admission")
                .0
        );
        let second = validate_token_with_connection(&mut conn, "sk-limited", "198.51.100.2")
            .expect("second admission");

        assert!(!second.0);
        let (reservations, request_count, total_requests): (i64, i64, i64) = conn
            .query_row(
                "SELECT
                    (SELECT COUNT(*) FROM token_ip_bindings),
                    (SELECT request_count FROM token_ip_bindings WHERE ip_address = '198.51.100.1'),
                    (SELECT total_requests FROM user_tokens WHERE id = 'limited')",
                [],
                |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
            )
            .expect("admission state");
        assert_eq!((reservations, request_count, total_requests), (1, 0, 0));
    }

    #[test]
    fn deleting_token_cascades_bindings_and_usage_logs() {
        let mut conn = test_connection();
        insert_token(&conn, "deleted", "sk-deleted", true, 0);
        conn.execute(
            "INSERT INTO token_ip_bindings (
                id, token_id, ip_address, first_seen_at, last_seen_at, request_count
             ) VALUES ('binding', 'deleted', '198.51.100.1', 0, 0, 0)",
            [],
        )
        .expect("seed binding");
        conn.execute(
            "INSERT INTO token_usage_logs (
                id, token_id, request_time
             ) VALUES ('usage', 'deleted', 0)",
            [],
        )
        .expect("seed usage log");

        delete_token_with_connection(&mut conn, "deleted").expect("delete token");

        let dependents: (i64, i64) = conn
            .query_row(
                "SELECT
                    (SELECT COUNT(*) FROM token_ip_bindings),
                    (SELECT COUNT(*) FROM token_usage_logs)",
                [],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .expect("dependent counts");
        assert_eq!(dependents, (0, 0));
    }

    #[test]
    fn update_distinguishes_curfew_clear_from_omission() {
        let conn = test_connection();
        insert_token(&conn, "curfew", "sk-curfew", true, 0);
        conn.execute(
            "UPDATE user_tokens SET curfew_start = '22:00', curfew_end = '23:00' WHERE id = 'curfew'",
            [],
        )
        .expect("seed curfew");

        update_token_with_connection(&conn, "curfew", None, None, None, None, None, None)
            .expect("omitted curfew is a no-op");
        let retained: Option<String> = conn
            .query_row(
                "SELECT curfew_start FROM user_tokens WHERE id = 'curfew'",
                [],
                |row| row.get(0),
            )
            .expect("retained curfew");
        assert_eq!(retained.as_deref(), Some("22:00"));

        update_token_with_connection(
            &conn,
            "curfew",
            None,
            None,
            None,
            None,
            Some(None),
            Some(None),
        )
        .expect("clear curfew");
        let cleared: (Option<String>, Option<String>) = conn
            .query_row(
                "SELECT curfew_start, curfew_end FROM user_tokens WHERE id = 'curfew'",
                [],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .expect("cleared curfew");
        assert_eq!(cleared, (None, None));
    }

    #[test]
    fn custom_expiry_must_be_explicit_and_future() {
        assert!(expiry_for_type("custom", None).is_err());
        assert!(expiry_for_type("custom", Some(Utc::now().timestamp())).is_err());
        assert!(expiry_for_type("unexpected", None).is_err());
        assert!(expiry_for_type("never", None)
            .expect("explicit never")
            .is_none());
    }
}
