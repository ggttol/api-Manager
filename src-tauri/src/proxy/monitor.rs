use serde::{Deserialize, Serialize};
use std::collections::VecDeque;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use tauri::Emitter;
use tokio::sync::{Mutex, Notify, RwLock, Semaphore};

// Admission happens before spawn_blocking, so queued tasks cannot retain unlimited bodies.
static LOG_WRITERS: Semaphore = Semaphore::const_new(4);

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ProxyRequestLog {
    pub id: String,
    pub timestamp: i64,
    pub method: String,
    pub url: String,
    pub status: u16,
    pub duration: u64,                // ms
    pub model: Option<String>,        // 客户端请求的模型名
    pub mapped_model: Option<String>, // 实际路由后使用的模型名
    pub account_email: Option<String>,
    pub client_ip: Option<String>, // 客户端 IP 地址
    pub error: Option<String>,
    pub request_body: Option<String>,
    pub upstream_request_body: Option<String>, // 网关转出给上游(Antigravity)的报文
    pub response_body: Option<String>,
    #[serde(default)]
    pub request_headers: Option<String>,
    #[serde(default)]
    pub upstream_request_headers: Option<String>,
    #[serde(default)]
    pub response_headers: Option<String>,
    pub input_tokens: Option<u32>,
    pub output_tokens: Option<u32>,
    pub cached_tokens: Option<u32>,
    pub protocol: Option<String>, // 协议类型: "openai", "anthropic", "gemini"
    pub username: Option<String>, // User token username
    #[serde(default)]
    pub session_id: Option<String>, // 会话标识 (如 sess-xxx)
}

#[cfg(test)]
pub(crate) mod prompt_log_tests {
    use super::*;

    pub(crate) fn sample_log(id: &str, bytes: usize) -> ProxyRequestLog {
        serde_json::from_value(serde_json::json!({
            "id": id, "timestamp": chrono::Utc::now().timestamp_millis(),
            "method": "POST", "url": "/v1/chat/completions", "status": 500, "duration": 10,
            "request_body": "q".repeat(bytes), "response_body": "错".repeat(bytes),
            "error": "错".repeat(2048)
        }))
        .unwrap()
    }

    // Tests using ABV_DATA_DIR run serially and restore the previous process setting.
    pub(crate) struct TestDataDir {
        _dir: tempfile::TempDir,
        previous: Option<std::ffi::OsString>,
    }
    impl TestDataDir {
        pub(crate) fn new() -> Self {
            let dir = tempfile::tempdir().unwrap();
            let previous = std::env::var_os("ABV_DATA_DIR");
            std::env::set_var("ABV_DATA_DIR", dir.path());
            Self {
                _dir: dir,
                previous,
            }
        }
    }
    impl Drop for TestDataDir {
        fn drop(&mut self) {
            if let Some(previous) = &self.previous {
                std::env::set_var("ABV_DATA_DIR", previous);
            } else {
                std::env::remove_var("ABV_DATA_DIR");
            }
        }
    }

    #[tokio::test]
    async fn prompt_log_memory_summary_and_database_detail() {
        let _dir = TestDataDir::new();
        crate::modules::proxy_db::init_db().unwrap();
        let monitor = ProxyMonitor {
            logs: RwLock::new(VecDeque::new()),
            stats: RwLock::new(ProxyStats::default()),
            max_logs: 2,
            enabled: Arc::new(AtomicBool::new(true)),
            capture_health_logs: Arc::new(AtomicBool::new(false)),
            app_handle: None,
            operation_lock: Arc::new(Mutex::new(())),
            retention_task: Arc::new(RetentionTaskControl {
                stopped: AtomicBool::new(false),
                wake: Notify::new(),
            }),
        };
        let log = sample_log("detail", 4096);
        let response = log.response_body.clone();
        monitor.log_request(log).await;
        let _finished = LOG_WRITERS.acquire_many(4).await.unwrap();
        let logs = monitor.logs.read().await;
        assert!(logs[0].request_body.is_none() && logs[0].response_body.is_none());
        assert_eq!(logs[0].error.as_ref().unwrap().chars().count(), 1024);
        let detail = crate::modules::proxy_db::get_log_detail("detail").unwrap();
        assert_eq!(detail.response_body, response);
        assert_eq!(
            detail.request_body.as_deref(),
            Some("q".repeat(4096).as_str())
        );
        monitor.set_enabled(false);
        monitor.log_request(sample_log("disabled", 4096)).await;
        assert!(crate::modules::proxy_db::get_log_detail("disabled").is_err());
    }
}

impl ProxyRequestLog {
    pub(crate) fn summary(&self) -> Self {
        Self {
            id: self.id.clone(),
            timestamp: self.timestamp,
            method: self.method.clone(),
            url: self.url.clone(),
            status: self.status,
            duration: self.duration,
            model: self.model.clone(),
            mapped_model: self.mapped_model.clone(),
            account_email: self.account_email.clone(),
            client_ip: self.client_ip.clone(),
            error: self
                .error
                .as_ref()
                .map(|error| error.chars().take(1024).collect()),
            request_body: None,
            upstream_request_body: None,
            response_body: None,
            request_headers: None,
            upstream_request_headers: None,
            response_headers: None,
            input_tokens: self.input_tokens,
            output_tokens: self.output_tokens,
            cached_tokens: self.cached_tokens,
            protocol: self.protocol.clone(),
            username: self.username.clone(),
            session_id: self.session_id.clone(),
        }
    }
}

#[derive(Default)]
pub(crate) struct UpstreamCapture {
    body: Option<String>,
    headers: Option<String>,
}

#[derive(Clone, Default)]
pub struct UpstreamRequestBodyHolder(pub std::sync::Arc<std::sync::Mutex<UpstreamCapture>>);

tokio::task_local! {
    pub static CURRENT_UPSTREAM_CAPTURE: UpstreamRequestBodyHolder;
}

impl UpstreamRequestBodyHolder {
    pub fn new() -> Self {
        Self(std::sync::Arc::new(std::sync::Mutex::new(
            UpstreamCapture::default(),
        )))
    }

    pub fn set(&self, body: String) {
        if let Ok(mut lock) = self.0.lock() {
            lock.body = Some(body);
        }
    }

    pub fn set_headers_json(&self, headers: String) {
        if let Ok(mut lock) = self.0.lock() {
            lock.headers = Some(headers);
        }
    }

    pub fn set_value(&self, val: &serde_json::Value) {
        let clean = sanitize_upstream_debug_value(val);
        if let Ok(s) = serde_json::to_string(&clean) {
            self.set(s);
        }
    }

    pub fn take(&self) -> Option<String> {
        self.0.lock().ok().and_then(|mut g| g.body.take())
    }

    pub fn take_headers(&self) -> Option<String> {
        self.0.lock().ok().and_then(|mut g| g.headers.take())
    }
}

fn sanitize_upstream_debug_value(val: &serde_json::Value) -> serde_json::Value {
    match val {
        serde_json::Value::String(s)
            if s.starts_with("data:image/") || s.starts_with("data:audio/") =>
        {
            serde_json::Value::String(format!("[inline data omitted: {} chars]", s.len()))
        }
        serde_json::Value::Array(arr) => {
            serde_json::Value::Array(arr.iter().map(sanitize_upstream_debug_value).collect())
        }
        serde_json::Value::Object(map) => {
            let is_inline = map
                .get("mimeType")
                .and_then(serde_json::Value::as_str)
                .is_some()
                && map
                    .get("data")
                    .and_then(serde_json::Value::as_str)
                    .is_some();
            serde_json::Value::Object(
                map.iter()
                    .map(|(k, v)| {
                        let v = if is_inline && k == "data" {
                            serde_json::Value::String(format!(
                                "[inline data omitted: {} chars]",
                                v.as_str().map(str::len).unwrap_or_default()
                            ))
                        } else {
                            sanitize_upstream_debug_value(v)
                        };
                        (k.clone(), v)
                    })
                    .collect(),
            )
        }
        _ => val.clone(),
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct ProxyStats {
    pub total_requests: u64,
    pub success_count: u64,
    pub error_count: u64,
}

struct RetentionTaskControl {
    stopped: AtomicBool,
    wake: Notify,
}

pub struct ProxyMonitor {
    pub logs: RwLock<VecDeque<ProxyRequestLog>>,
    pub stats: RwLock<ProxyStats>,
    pub max_logs: usize,
    pub enabled: Arc<AtomicBool>,
    pub capture_health_logs: Arc<AtomicBool>,
    app_handle: Option<tauri::AppHandle>,
    /// Serializes persistence and retention with clear so an acknowledged clear
    /// cannot be followed by a write or cleanup that began before it.
    operation_lock: Arc<Mutex<()>>,
    retention_task: Arc<RetentionTaskControl>,
}

impl ProxyMonitor {
    pub fn new(max_logs: usize, app_handle: Option<tauri::AppHandle>) -> Self {
        if let Err(e) = crate::modules::proxy_db::init_db() {
            tracing::error!("Failed to initialize proxy DB: {}", e);
        }

        let operation_lock = Arc::new(Mutex::new(()));
        let retention_task = Arc::new(RetentionTaskControl {
            stopped: AtomicBool::new(false),
            wake: Notify::new(),
        });
        spawn_retention_task(operation_lock.clone(), retention_task.clone());

        Self {
            logs: RwLock::new(VecDeque::with_capacity(max_logs)),
            stats: RwLock::new(ProxyStats::default()),
            max_logs,
            enabled: Arc::new(AtomicBool::new(false)), // Default to disabled
            capture_health_logs: Arc::new(AtomicBool::new(false)), // Default to false
            app_handle,
            operation_lock,
            retention_task,
        }
    }

    pub fn set_enabled(&self, enabled: bool) {
        self.enabled.store(enabled, Ordering::Relaxed);
    }

    pub fn is_enabled(&self) -> bool {
        self.enabled.load(Ordering::Relaxed)
    }

    pub fn set_capture_health_logs(&self, enabled: bool) {
        self.capture_health_logs.store(enabled, Ordering::Relaxed);
    }

    pub fn is_capture_health_logs(&self) -> bool {
        self.capture_health_logs.load(Ordering::Relaxed)
    }

    pub async fn log_request(&self, log: ProxyRequestLog) {
        if let (Some(account), Some(input), Some(output)) =
            (&log.account_email, log.input_tokens, log.output_tokens)
        {
            let model = log.model.clone().unwrap_or_else(|| "unknown".to_string());
            let account = account.clone();
            let cached = log.cached_tokens.unwrap_or(0);
            tokio::task::spawn_blocking(move || {
                if let Err(e) = crate::modules::token_stats::record_usage(
                    &account, &model, input, output, cached,
                ) {
                    tracing::debug!("Failed to record token stats: {}", e);
                }
            });
        }

        if !self.is_enabled() {
            return;
        }
        tracing::info!("[Monitor] Logging request: {} {}", log.method, log.url);
        let _operation = self.operation_lock.lock().await;

        // Persist before updating local state or notifying listeners. This makes
        // the event safe for consumers that immediately refresh authoritative DB
        // data and serializes persistence with clear().
        let Ok(permit) = LOG_WRITERS.try_acquire() else {
            tracing::debug!("Skipping proxy log persistence: writers busy");
            return;
        };
        let mut log_to_save = log.clone();
        let storage_mode = crate::proxy::config::get_payload_storage_mode();
        log_to_save.request_body = crate::proxy::payload_audit::apply_storage_mode_to_body(
            log_to_save.request_body,
            &storage_mode,
        );
        log_to_save.upstream_request_body = crate::proxy::payload_audit::apply_storage_mode_to_body(
            log_to_save.upstream_request_body,
            &storage_mode,
        );
        log_to_save.response_body = crate::proxy::payload_audit::apply_storage_mode_to_body(
            log_to_save.response_body,
            &storage_mode,
        );
        let security_log = log.client_ip.as_ref().map(|ip| {
            crate::modules::security_db::IpAccessLog {
                id: uuid::Uuid::new_v4().to_string(),
                client_ip: ip.clone(),
                timestamp: log.timestamp / 1000,
                method: Some(log.method.clone()),
                path: Some(log.url.clone()),
                user_agent: None,
                status: Some(log.status as i32),
                duration: Some(log.duration as i64),
                api_key_hash: None,
                blocked: false,
                block_reason: None,
                username: log.username.clone(),
            }
        });
        let persisted = tokio::task::spawn_blocking(move || {
            let _permit = permit;
            crate::modules::proxy_db::save_log(log_to_save)?;

            if let Some(security_log) = security_log {
                crate::modules::security_db::save_ip_access_log(&security_log)?;
            }
            Ok::<(), String>(())
        })
        .await;
        match persisted {
            Ok(Ok(())) => {}
            Ok(Err(error)) => {
                tracing::error!("Failed to persist proxy log: {}", error);
                return;
            }
            Err(error) => {
                tracing::error!("Proxy log persistence task failed: {}", error);
                return;
            }
        }

        {
            let mut stats = self.stats.write().await;
            stats.total_requests += 1;
            if log.status >= 200 && log.status < 400 && log.error.is_none() {
                stats.success_count += 1;
            } else {
                stats.error_count += 1;
            }
        }

        let summary = log.summary();
        // Add only the same summary used by the frontend to memory.
        {
            let mut logs = self.logs.write().await;
            if logs.len() >= self.max_logs {
                logs.pop_back();
            }
            logs.push_front(summary.clone());
        }

        if let Some(app) = &self.app_handle {
            let _ = app.emit("proxy://request", &summary);
        }

    }

    pub async fn get_logs(&self, limit: usize) -> Vec<ProxyRequestLog> {
        // Try to get from DB first for true history
        let db_result =
            tokio::task::spawn_blocking(move || crate::modules::proxy_db::get_logs(limit)).await;

        match db_result {
            Ok(Ok(logs)) => logs,
            Ok(Err(e)) => {
                tracing::error!("Failed to get logs from DB: {}", e);
                // Fallback to memory
                let logs = self.logs.read().await;
                logs.iter().take(limit).cloned().collect()
            }
            Err(e) => {
                tracing::error!("Spawn blocking failed for get_logs: {}", e);
                let logs = self.logs.read().await;
                logs.iter().take(limit).cloned().collect()
            }
        }
    }

    pub async fn get_stats(&self) -> ProxyStats {
        let db_result = tokio::task::spawn_blocking(|| crate::modules::proxy_db::get_stats()).await;

        match db_result {
            Ok(Ok(stats)) => stats,
            Ok(Err(e)) => {
                tracing::error!("Failed to get stats from DB: {}", e);
                self.stats.read().await.clone()
            }
            Err(e) => {
                tracing::error!("Spawn blocking failed for get_stats: {}", e);
                self.stats.read().await.clone()
            }
        }
    }

    pub async fn get_logs_filtered(
        &self,
        page: usize,
        page_size: usize,
        search_text: Option<String>,
        level: Option<String>,
    ) -> Result<Vec<ProxyRequestLog>, String> {
        let offset = (page.max(1) - 1) * page_size;
        let errors_only = level.as_deref() == Some("error");
        let search = search_text.unwrap_or_default();

        let res = tokio::task::spawn_blocking(move || {
            crate::modules::proxy_db::get_logs_filtered(
                &search,
                errors_only,
                None,
                page_size,
                offset,
            )
        })
        .await;

        match res {
            Ok(r) => r,
            Err(e) => Err(format!("Spawn blocking failed: {}", e)),
        }
    }

    pub async fn clear(&self) -> Result<(), String> {
        let _operation = self.operation_lock.lock().await;
        tokio::task::spawn_blocking(crate::modules::proxy_db::clear_logs)
            .await
            .map_err(|error| format!("Proxy log clear task failed: {}", error))??;

        let mut logs = self.logs.write().await;
        logs.clear();
        let mut stats = self.stats.write().await;
        *stats = ProxyStats::default();
        Ok(())
    }
}

impl Drop for ProxyMonitor {
    fn drop(&mut self) {
        self.retention_task.stopped.store(true, Ordering::Release);
        self.retention_task.wake.notify_one();
    }
}

fn spawn_retention_task(operation_lock: Arc<Mutex<()>>, retention_task: Arc<RetentionTaskControl>) {
    tokio::spawn(async move {
        let mut interval = tokio::time::interval(std::time::Duration::from_secs(3600));
        interval.tick().await;

        loop {
            if retention_task.stopped.load(Ordering::Acquire) {
                break;
            }

            let _operation = operation_lock.lock().await;
            if retention_task.stopped.load(Ordering::Acquire) {
                break;
            }

            // Reload the persisted policy each run so UI changes take effect
            // without restarting the proxy.
            match tokio::task::spawn_blocking(move || {
                let retention = crate::modules::config::load_app_config()?
                    .proxy
                    .log_retention;
                crate::modules::proxy_db::apply_retention(&retention)
            })
            .await
            {
                Ok(Ok((cleared, deleted))) if cleared > 0 || deleted > 0 => {
                    tracing::info!(
                        "Proxy log retention: cleared {} bodies, deleted {} rows",
                        cleared,
                        deleted
                    );
                }
                Ok(Ok(_)) => {}
                Ok(Err(error)) => {
                    tracing::error!("Failed to apply proxy log retention: {}", error)
                }
                Err(error) => tracing::error!("Proxy log retention task failed: {}", error),
            }
            drop(_operation);

            let shutdown = retention_task.wake.notified();
            tokio::pin!(shutdown);
            if retention_task.stopped.load(Ordering::Acquire) {
                break;
            }
            tokio::select! {
                _ = interval.tick() => {}
                _ = &mut shutdown => break,
            }
        }
    });
}
