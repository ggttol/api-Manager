use crate::proxy::config::{ProxyEntry, ProxyPoolConfig, ProxySelectionStrategy};
use dashmap::DashMap;
use futures::{stream, StreamExt};
use rquest::Client;
use std::collections::{HashMap, HashSet};
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, OnceLock, RwLock as StdRwLock};
use std::time::Duration;
use tokio::sync::RwLock;

use rquest_util::Emulation;

/// 全局代理池管理器单例
pub static GLOBAL_PROXY_POOL: OnceLock<Arc<ProxyPoolManager>> = OnceLock::new();

/// 获取全局代理池管理器
pub fn get_global_proxy_pool() -> Option<Arc<ProxyPoolManager>> {
    GLOBAL_PROXY_POOL.get().cloned()
}

/// 初始化全局代理池管理器
pub fn init_global_proxy_pool(config: Arc<RwLock<ProxyPoolConfig>>) -> Arc<ProxyPoolManager> {
    if let Some(manager) = GLOBAL_PROXY_POOL.get() {
        // A retry must join the manager's already-published state instead of
        // copying a transient startup snapshot into a second state owner.
        return manager.clone();
    }

    let manager = Arc::new(ProxyPoolManager::new(config));
    match GLOBAL_PROXY_POOL.set(manager.clone()) {
        Ok(()) => manager,
        Err(_) => GLOBAL_PROXY_POOL
            .get()
            .expect("global proxy pool initialized concurrently")
            .clone(),
    }
}

/// 代理配置 (用于构建 reqwest Client)
/// 注意：重命名为 PoolProxyConfig 以避免与 config::ProxyConfig 冲突
#[derive(Debug, Clone)]
pub struct PoolProxyConfig {
    pub proxy: rquest::Proxy,
    pub entry_id: String,
    /// Configuration fingerprint used to prevent stale clients surviving a pool reload.
    pub cache_key: String,
}

/// 代理池管理器
pub struct ProxyPoolManager {
    config: Arc<RwLock<ProxyPoolConfig>>,
    usage_counter: Arc<DashMap<String, usize>>,
    /// Published atomically so routing never observes a partial binding reload.
    account_bindings: Arc<StdRwLock<HashMap<String, String>>>,
    /// Serializes binding admission, publication, and persistence.
    binding_mutation_lock: Arc<tokio::sync::Mutex<()>>,
    health_check_started: AtomicBool,
    round_robin_index: Arc<AtomicUsize>,
}

impl ProxyPoolManager {
    pub fn new(config: Arc<RwLock<ProxyPoolConfig>>) -> Self {
        let account_bindings = config
            .try_read()
            .map(|cfg| cfg.account_bindings.clone())
            .unwrap_or_default();
        if !account_bindings.is_empty() {
            tracing::info!(
                "[ProxyPool] Loaded {} account bindings from config",
                account_bindings.len()
            );
        }
        Self {
            config,
            usage_counter: Arc::new(DashMap::new()),
            account_bindings: Arc::new(StdRwLock::new(account_bindings)),
            binding_mutation_lock: Arc::new(tokio::sync::Mutex::new(())),
            health_check_started: AtomicBool::new(false),
            round_robin_index: Arc::new(AtomicUsize::new(0)),
        }
    }

    /// The canonical pool configuration shared by routing, reloads, and the server.
    pub fn config_state(&self) -> Arc<RwLock<ProxyPoolConfig>> {
        self.config.clone()
    }

    /// Snapshot configuration and dedicated bindings from one publication epoch.
    async fn routing_snapshot(&self) -> (ProxyPoolConfig, HashMap<String, String>) {
        let _mutation = self.binding_mutation_lock.lock().await;
        let config = self.config.read().await.clone();
        let bindings = self
            .account_bindings
            .read()
            .expect("proxy binding lock poisoned")
            .clone();
        (config, bindings)
    }

    /// [NEW] 为指定账号获取“最终生效”的 HttpClient
    /// 逻辑：
    /// 1. 账号显式绑定代理优先 (Account-Proxy Binding)
    /// 2. 如果无绑定，且开启了“自动全局”，取池中第一个节点
    /// 3. 如果以上均无，则检查全局上游代理 (Upstream Proxy) [由调用方 fallback]
    pub async fn get_effective_client(
        &self,
        account_id: Option<&str>,
        timeout_secs: u64,
    ) -> Client {
        let mut builder = Client::builder()
            .emulation(Emulation::Chrome123)
            .timeout(Duration::from_secs(timeout_secs));

        // 尝试获取代理配置
        let proxy_opt = if let Some(acc_id) = account_id {
            self.get_proxy_for_account(acc_id).await.ok().flatten()
        } else {
            // 没有 account_id 的通用请求，如果代理池启用，则默认从中选择节点作为出口
            let (config, bindings) = self.routing_snapshot().await;
            if config.enabled {
                let res = self
                    .select_proxy_from_pool(&config, &bindings)
                    .ok()
                    .flatten();
                if let Some(p) = &res {
                    tracing::info!(
                        "[Proxy] Route: Generic Request -> Proxy {} (Pool)",
                        p.entry_id
                    );
                } else {
                    // [FIX #1583] 明确记录池中无可用代理的情况
                    tracing::warn!("[Proxy] Route: Generic Request -> No available proxy in pool, falling back to upstream or direct");
                }
                res
            } else {
                tracing::debug!("[Proxy] Route: Generic Request -> Proxy pool disabled");
                None
            }
        };

        if let Some(proxy_cfg) = proxy_opt {
            builder = builder.proxy(proxy_cfg.proxy);
            // Already logged more detail in get_proxy_for_account or pool selection
        } else {
            // Fallback 到应用配置的单上游代理
            if let Ok(app_cfg) = crate::modules::config::load_app_config() {
                let up = app_cfg.proxy.upstream_proxy;
                if up.enabled && !up.url.is_empty() {
                    if let Ok(p) = rquest::Proxy::all(&up.url) {
                        tracing::info!(
                            "[Proxy] Route: {:?} -> Upstream: {} (AppConfig)",
                            account_id.unwrap_or("Generic"),
                            redact_proxy_url(&up.url)
                        );
                        builder = builder.proxy(p);
                    }
                } else {
                    tracing::info!(
                        "[Proxy] Route: {:?} -> Direct",
                        account_id.unwrap_or("Generic")
                    );
                }
            }
        }

        builder.build().unwrap_or_else(|_| Client::new())
    }

    /// [NEW] 为指定账号获取“最终生效”的无特征 Standard HttpClient (专门用于纯净场景，如 OAuth 退还)
    pub async fn get_effective_standard_client(
        &self,
        account_id: Option<&str>,
        timeout_secs: u64,
    ) -> Client {
        let mut builder = Client::builder()
            // 无 Emulation 设置，走纯正的基础 TLS 指纹
            .timeout(Duration::from_secs(timeout_secs));

        // 尝试获取代理配置
        let proxy_opt = if let Some(acc_id) = account_id {
            self.get_proxy_for_account(acc_id).await.ok().flatten()
        } else {
            // 没有 account_id 的通用请求，如果代理池启用，则默认从中选择节点作为出口
            let (config, bindings) = self.routing_snapshot().await;
            if config.enabled {
                let res = self
                    .select_proxy_from_pool(&config, &bindings)
                    .ok()
                    .flatten();
                if let Some(p) = &res {
                    tracing::info!(
                        "[Proxy] Route: Generic Request (Standard Client) -> Proxy {} (Pool)",
                        p.entry_id
                    );
                } else {
                    tracing::warn!("[Proxy] Route: Generic Request (Standard Client) -> No available proxy in pool, falling back to upstream or direct");
                }
                res
            } else {
                tracing::debug!(
                    "[Proxy] Route: Generic Request (Standard Client) -> Proxy pool disabled"
                );
                None
            }
        };

        if let Some(proxy_cfg) = proxy_opt {
            builder = builder.proxy(proxy_cfg.proxy);
        } else {
            // Fallback 到应用配置的单上游代理
            if let Ok(app_cfg) = crate::modules::config::load_app_config() {
                let up = app_cfg.proxy.upstream_proxy;
                if up.enabled && !up.url.is_empty() {
                    if let Ok(p) = rquest::Proxy::all(&up.url) {
                        tracing::info!(
                            "[Proxy] Route: {:?} (Standard Client) -> Upstream: {} (AppConfig)",
                            account_id.unwrap_or("Generic"),
                            redact_proxy_url(&up.url)
                        );
                        builder = builder.proxy(p);
                    }
                } else {
                    tracing::info!(
                        "[Proxy] Route: {:?} (Standard Client) -> Direct",
                        account_id.unwrap_or("Generic")
                    );
                }
            }
        }

        builder.build().unwrap_or_else(|_| Client::new())
    }

    /// 为账号获取代理
    pub async fn get_proxy_for_account(
        &self,
        account_id: &str,
    ) -> Result<Option<PoolProxyConfig>, String> {
        let (config, bindings) = self.routing_snapshot().await;

        if !config.enabled || config.proxies.is_empty() {
            return Ok(None);
        }

        // 1. 优先使用账号绑定 (专属 IP)
        if let Some(proxy) = self.get_bound_proxy(account_id, &config, &bindings)? {
            tracing::info!(
                "[Proxy] Route: Account {} -> Proxy {} (Bound)",
                account_id,
                proxy.entry_id
            );
            return Ok(Some(proxy));
        }

        // 2. 否则从池中策略选择 (公用池)
        let res = self.select_proxy_from_pool(&config, &bindings)?;
        if let Some(p) = &res {
            tracing::info!(
                "[Proxy] Route: Account {} -> Proxy {} (Pool)",
                account_id,
                p.entry_id
            );
        }
        Ok(res)
    }

    /// 获取账号绑定的代理
    fn get_bound_proxy(
        &self,
        account_id: &str,
        config: &ProxyPoolConfig,
        bindings: &HashMap<String, String>,
    ) -> Result<Option<PoolProxyConfig>, String> {
        let proxy_id = bindings.get(account_id);
        if let Some(proxy_id) = proxy_id {
            if let Some(entry) = config
                .proxies
                .iter()
                .find(|proxy| proxy.id.as_str() == proxy_id.as_str())
            {
                if entry.enabled {
                    if config.auto_failover && !entry.is_healthy {
                        return Ok(None);
                    }
                    return Ok(Some(self.build_proxy_config(entry)?));
                }
            }
        }
        Ok(None)
    }

    /// 从代理池中选择代理
    fn select_proxy_from_pool(
        &self,
        config: &ProxyPoolConfig,
        bindings: &HashMap<String, String>,
    ) -> Result<Option<PoolProxyConfig>, String> {
        let bound_ids: HashSet<String> = bindings.values().cloned().collect();
        let healthy_proxies: Vec<_> = config
            .proxies
            .iter()
            .filter(|p| {
                p.enabled && (!config.auto_failover || p.is_healthy) && !bound_ids.contains(&p.id)
            })
            .collect();
        if healthy_proxies.is_empty() {
            return Ok(None);
        }
        let selected = match config.strategy {
            ProxySelectionStrategy::RoundRobin => self.select_round_robin(&healthy_proxies),
            ProxySelectionStrategy::Random => self.select_random(&healthy_proxies),
            ProxySelectionStrategy::Priority => self.select_by_priority(&healthy_proxies),
            ProxySelectionStrategy::LeastConnections => {
                self.select_least_connections(&healthy_proxies)
            }
            ProxySelectionStrategy::WeightedRoundRobin => self.select_weighted(&healthy_proxies),
        };
        if let Some(entry) = selected {
            *self.usage_counter.entry(entry.id.clone()).or_insert(0) += 1;
            Ok(Some(self.build_proxy_config(entry)?))
        } else {
            Ok(None)
        }
    }
    fn select_round_robin<'a>(&self, proxies: &[&'a ProxyEntry]) -> Option<&'a ProxyEntry> {
        if proxies.is_empty() {
            return None;
        }
        let index = self.round_robin_index.fetch_add(1, Ordering::Relaxed);
        Some(proxies[index % proxies.len()])
    }

    fn select_random<'a>(&self, proxies: &[&'a ProxyEntry]) -> Option<&'a ProxyEntry> {
        if proxies.is_empty() {
            return None;
        }
        use rand::seq::SliceRandom;
        let mut rng = rand::thread_rng();
        proxies.choose(&mut rng).copied()
    }

    fn select_by_priority<'a>(&self, proxies: &[&'a ProxyEntry]) -> Option<&'a ProxyEntry> {
        // priority 越小越优先
        proxies.iter().min_by_key(|p| p.priority).copied()
    }

    fn select_least_connections<'a>(&self, proxies: &[&'a ProxyEntry]) -> Option<&'a ProxyEntry> {
        proxies
            .iter()
            .min_by_key(|p| self.usage_counter.get(&p.id).map(|v| *v).unwrap_or(0))
            .copied()
    }

    fn select_weighted<'a>(&self, proxies: &[&'a ProxyEntry]) -> Option<&'a ProxyEntry> {
        // 简单实现: 类似优先级的加权, 这里暂用 Priority 代替
        self.select_by_priority(proxies)
    }

    /// 构建 rquest::Proxy 配置
    fn build_proxy_config(&self, entry: &ProxyEntry) -> Result<PoolProxyConfig, String> {
        let raw_url = crate::proxy::config::normalize_proxy_url(&entry.url);
        let (clean_url, parsed_auth) = match url::Url::parse(&raw_url) {
            Ok(mut url) => {
                let user = (!url.username().is_empty())
                    .then(|| decode_url_userinfo(url.username()))
                    .flatten();
                let password = url.password().and_then(decode_url_userinfo);
                let _ = url.set_username("");
                let _ = url.set_password(None);
                let auth = match (user, password) {
                    (Some(user), Some(password)) => Some((user, password)),
                    _ => None,
                };
                (url.to_string(), auth)
            }
            Err(_) => (raw_url.clone(), None),
        };
        let mut proxy = rquest::Proxy::all(&clean_url)
            .or_else(|_| rquest::Proxy::all(&raw_url))
            .map_err(|e| format!("Invalid proxy URL: {}", e))?;
        if let Some(auth) = &entry.auth {
            if !auth.username.is_empty() {
                proxy = proxy.basic_auth(&auth.username, &auth.password);
            }
        } else if let Some((user, password)) = parsed_auth {
            proxy = proxy.basic_auth(&user, &password);
        }
        Ok(PoolProxyConfig {
            proxy,
            entry_id: entry.id.clone(),
            cache_key: proxy_cache_key(entry),
        })
    }

    /// 绑定账号到代理
    pub async fn bind_account_to_proxy(
        &self,
        account_id: String,
        proxy_id: String,
    ) -> Result<(), String> {
        let _mutation = self.binding_mutation_lock.lock().await;
        let config = self.config.read().await;
        let entry = config
            .proxies
            .iter()
            .find(|proxy| proxy.id == proxy_id)
            .ok_or_else(|| format!("Proxy {} not found", proxy_id))?;

        let snapshot = {
            let mut bindings = self
                .account_bindings
                .write()
                .expect("proxy binding lock poisoned");
            if bindings.get(&account_id).map(String::as_str) != Some(proxy_id.as_str()) {
                if let Some(max) = entry.max_accounts.filter(|max| *max > 0) {
                    let count = bindings
                        .values()
                        .filter(|bound_proxy_id| *bound_proxy_id == &proxy_id)
                        .count();
                    if count >= max {
                        return Err(format!("Proxy {} has reached max accounts limit", proxy_id));
                    }
                }
            }
            bindings.insert(account_id.clone(), proxy_id.clone());
            bindings.clone()
        };
        drop(config);
        self.persist_bindings(snapshot).await;
        tracing::info!(
            "[ProxyPool] Bound account {} to proxy {}",
            account_id,
            proxy_id
        );
        Ok(())
    }

    /// 解绑账号代理
    pub async fn unbind_account_proxy(&self, account_id: String) {
        let _mutation = self.binding_mutation_lock.lock().await;
        let snapshot = {
            let mut bindings = self
                .account_bindings
                .write()
                .expect("proxy binding lock poisoned");
            bindings.remove(&account_id);
            bindings.clone()
        };
        self.persist_bindings(snapshot).await;
        tracing::info!("[ProxyPool] Unbound account {}", account_id);
    }

    /// 获取账号当前绑定的代理ID
    pub fn get_account_binding(&self, account_id: &str) -> Option<String> {
        self.account_bindings
            .read()
            .expect("proxy binding lock poisoned")
            .get(account_id)
            .cloned()
    }

    /// 获取所有绑定关系的完整快照
    pub fn get_all_bindings_snapshot(&self) -> HashMap<String, String> {
        self.account_bindings
            .read()
            .expect("proxy binding lock poisoned")
            .clone()
    }

    /// Publish the full configuration and binding snapshot for a restarted server.
    pub async fn replace_config(&self, config: ProxyPoolConfig) {
        let _mutation = self.binding_mutation_lock.lock().await;
        *self.config.write().await = config.clone();
        *self
            .account_bindings
            .write()
            .expect("proxy binding lock poisoned") = config.account_bindings;
    }

    /// Publish a complete binding snapshot after a hot reload.
    pub async fn sync_bindings_from_config(&self) {
        let _mutation = self.binding_mutation_lock.lock().await;
        let snapshot = self.config.read().await.account_bindings.clone();
        *self
            .account_bindings
            .write()
            .expect("proxy binding lock poisoned") = snapshot;
    }

    /// Persist the complete binding snapshot while binding_mutation_lock is held.
    async fn persist_bindings(&self, bindings: HashMap<String, String>) {
        let pool_config = {
            let mut config = self.config.write().await;
            config.account_bindings = bindings;
            config.clone()
        };
        if let Err(error) = crate::modules::config::update_app_config(|app_config| {
            app_config.proxy.proxy_pool = pool_config;
            Ok(())
        }) {
            tracing::error!("[ProxyPool] Failed to persist bindings: {}", error);
        }
    }

    /// 批量检查代理健康状态
    pub async fn health_check(&self) -> Result<(), String> {
        let proxies_to_check: Vec<ProxyEntry> = {
            let config = self.config.read().await;
            config
                .proxies
                .iter()
                .filter(|proxy| proxy.enabled)
                .cloned()
                .collect()
        };

        let concurrency_limit = 20usize;
        let results = stream::iter(proxies_to_check)
            .map(|proxy| async move {
                let fingerprint = proxy_health_check_key(&proxy);
                let (is_healthy, latency) = self.check_proxy_health(&proxy).await;

                let latency_msg = if let Some(ms) = latency {
                    format!("{}ms", ms)
                } else {
                    "-".to_string()
                };

                tracing::info!(
                    "Proxy {} ({}) health check: {} (Latency: {})",
                    proxy.name,
                    redact_proxy_url(&proxy.url),
                    if is_healthy { "✓ OK" } else { "✗ FAILED" },
                    latency_msg
                );

                (proxy.id, fingerprint, is_healthy, latency)
            })
            .buffer_unordered(concurrency_limit)
            .collect::<Vec<_>>()
            .await;

        // Apply only results from the configuration that was actually probed.
        let mut config = self.config.write().await;
        for (id, fingerprint, is_healthy, latency) in results {
            if let Some(proxy) = config.proxies.iter_mut().find(|proxy| proxy.id == id) {
                if proxy_health_check_key(proxy) != fingerprint {
                    tracing::debug!(
                        "Discarding obsolete health-check result for proxy {}",
                        proxy.id
                    );
                    continue;
                }
                proxy.is_healthy = is_healthy;
                proxy.latency = latency;
                proxy.last_check_time = Some(chrono::Utc::now().timestamp());
            }
        }

        Ok(())
    }

    /// 检查单个代理健康状态
    async fn check_proxy_health(&self, entry: &ProxyEntry) -> (bool, Option<u64>) {
        const DEFAULT_HEALTH_CHECK_URL: &str = "https://cp.cloudflare.com/generate_204";

        let check_url = if let Some(url) = &entry.health_check_url {
            if url.trim().is_empty() {
                DEFAULT_HEALTH_CHECK_URL
            } else {
                url.as_str()
            }
        } else {
            DEFAULT_HEALTH_CHECK_URL
        };

        // 尝试构建 Client，如果失败直接视为不健康
        let proxy_res = self.build_proxy_config(entry);
        if let Err(error) = proxy_res {
            tracing::error!(
                "Proxy {} build config failed: {}",
                redact_proxy_url(&entry.url),
                error
            );
            return (false, None);
        }
        let proxy_cfg = proxy_res.unwrap();

        let client_result = Client::builder()
            .proxy(proxy_cfg.proxy)
            .emulation(Emulation::Chrome123)
            .timeout(Duration::from_secs(10))
            .user_agent("Mozilla/5.0 (Macintosh; Intel Mac OS X 10_15_7) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/123.0.0.0 Safari/537.36")
            .build();

        let client = match client_result {
            Ok(client) => client,
            Err(error) => {
                tracing::error!(
                    "Proxy {} build client failed: {}",
                    redact_proxy_url(&entry.url),
                    error
                );
                return (false, None);
            }
        };

        let start = std::time::Instant::now();
        match client.get(check_url).send().await {
            Ok(resp) => {
                let latency = start.elapsed().as_millis() as u64;
                if resp.status().is_success() {
                    (true, Some(latency))
                } else {
                    tracing::warn!(
                        "Proxy {} health check status error: {}",
                        redact_proxy_url(&entry.url),
                        resp.status()
                    );
                    (false, None)
                }
            }
            Err(error) => {
                tracing::warn!(
                    "Proxy {} health check request failed: {}",
                    redact_proxy_url(&entry.url),
                    error
                );
                (false, None)
            }
        }
    }

    /// 启动健康检查循环
    pub fn start_health_check_loop(self: Arc<Self>) {
        if self
            .health_check_started
            .compare_exchange(false, true, Ordering::AcqRel, Ordering::Acquire)
            .is_err()
        {
            return;
        }
        tokio::spawn(async move {
            tracing::info!("Starting proxy pool health check loop...");
            loop {
                let enabled = self.config.read().await.enabled;
                if enabled {
                    if let Err(error) = self.health_check().await {
                        tracing::error!("Proxy pool health check failed: {}", error);
                    }
                }
                let interval_secs = {
                    let config = self.config.read().await;
                    if !config.enabled {
                        60
                    } else {
                        config.health_check_interval.max(30)
                    }
                };
                tokio::time::sleep(Duration::from_secs(interval_secs)).await;
            }
        });
    }
}

/// Removes URL userinfo before a proxy URL reaches logs.
pub fn redact_proxy_url(raw_url: &str) -> String {
    let normalized = crate::proxy::config::normalize_proxy_url(raw_url);
    match url::Url::parse(&normalized) {
        Ok(mut url) => {
            if !url.username().is_empty() || url.password().is_some() {
                let _ = url.set_username("");
                let _ = url.set_password(None);
            }
            url.to_string()
        }
        Err(_) => "<invalid proxy URL>".to_string(),
    }
}

/// Decodes userinfo percent escapes without treating a literal `+` as a space.
fn decode_url_userinfo(value: &str) -> Option<String> {
    let mut bytes = Vec::with_capacity(value.len());
    let raw = value.as_bytes();
    let mut index = 0;
    while index < raw.len() {
        if raw[index] == b'%' && index + 2 < raw.len() {
            bytes.push((hex_value(raw[index + 1])? << 4) | hex_value(raw[index + 2])?);
            index += 3;
        } else {
            bytes.push(raw[index]);
            index += 1;
        }
    }
    String::from_utf8(bytes).ok()
}

fn hex_value(byte: u8) -> Option<u8> {
    match byte {
        b'0'..=b'9' => Some(byte - b'0'),
        b'a'..=b'f' => Some(byte - b'a' + 10),
        b'A'..=b'F' => Some(byte - b'A' + 10),
        _ => None,
    }
}

fn proxy_cache_key(entry: &ProxyEntry) -> String {
    let auth = entry
        .auth
        .as_ref()
        .map(|auth| format!("{}:{}", auth.username, auth.password))
        .unwrap_or_default();
    format!("{}\u{0}{}\u{0}{}", entry.id, entry.url, auth)
}

/// Identifies every health-probe input so old results cannot affect replacement entries.
fn proxy_health_check_key(entry: &ProxyEntry) -> String {
    format!(
        "{}\u{0}{}\u{0}{}",
        proxy_cache_key(entry),
        entry.enabled,
        entry.health_check_url.as_deref().unwrap_or_default()
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::proxy::config::ProxyAuth;

    #[test]
    fn test_build_proxy_config_with_explicit_auth() {
        let pool = ProxyPoolManager::new(Arc::new(RwLock::new(ProxyPoolConfig::default())));
        let entry = ProxyEntry {
            id: "p1".to_string(),
            name: "test".to_string(),
            url: "http://127.0.0.1:8080".to_string(),
            auth: Some(ProxyAuth {
                username: "user".to_string(),
                password: "pass".to_string(),
            }),
            enabled: true,
            priority: 1,
            tags: vec![],
            max_accounts: None,
            health_check_url: None,
            last_check_time: None,
            is_healthy: true,
            latency: None,
        };

        let res = pool.build_proxy_config(&entry);
        assert!(res.is_ok());
        assert_eq!(res.unwrap().entry_id, "p1");
    }

    #[test]
    fn test_build_proxy_config_with_url_auth() {
        let pool = ProxyPoolManager::new(Arc::new(RwLock::new(ProxyPoolConfig::default())));
        let entry = ProxyEntry {
            id: "p2".to_string(),
            name: "test_url_auth".to_string(),
            url: "http://user:pass@127.0.0.1:10080".to_string(),
            auth: None,
            enabled: true,
            priority: 1,
            tags: vec![],
            max_accounts: None,
            health_check_url: None,
            last_check_time: None,
            is_healthy: true,
            latency: None,
        };

        let res = pool.build_proxy_config(&entry);
        assert!(res.is_ok());
        assert_eq!(res.unwrap().entry_id, "p2");
    }

    #[test]
    fn url_userinfo_decoding_preserves_literal_plus() {
        assert_eq!(
            decode_url_userinfo("user%40example+p%3A%25"),
            Some("user@example+p:%".to_string())
        );
    }

    #[test]
    fn proxy_log_redaction_removes_encoded_credentials() {
        let redacted = redact_proxy_url("http://user%40name:p%40ss@127.0.0.1:8080");
        assert_eq!(redacted, "http://127.0.0.1:8080/");
        assert!(!redacted.contains("user"));
        assert!(!redacted.contains("p%40ss"));
    }

    #[tokio::test]
    async fn binding_snapshot_keeps_dedicated_proxy_out_of_public_pool() {
        let entry = ProxyEntry {
            id: "dedicated".to_string(),
            name: "test".to_string(),
            url: "http://127.0.0.1:8080".to_string(),
            auth: None,
            enabled: true,
            priority: 1,
            tags: vec![],
            max_accounts: Some(1),
            health_check_url: None,
            last_check_time: None,
            is_healthy: true,
            latency: None,
        };
        let config = Arc::new(RwLock::new(ProxyPoolConfig {
            enabled: true,
            proxies: vec![entry],
            account_bindings: HashMap::from([("account-a".to_string(), "dedicated".to_string())]),
            ..ProxyPoolConfig::default()
        }));
        let pool = ProxyPoolManager::new(config);

        assert!(pool
            .get_proxy_for_account("account-b")
            .await
            .unwrap()
            .is_none());
    }
}
