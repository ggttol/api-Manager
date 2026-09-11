use crate::utils::atomic_file::{lock_client_config, write_atomic};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::fs;
use std::path::PathBuf;
use std::process::Command;

#[cfg(target_os = "windows")]
use std::os::windows::process::CommandExt;

#[cfg(target_os = "windows")]
const CREATE_NO_WINDOW: u32 = 0x08000000;

/// Windows 常见 CLI 安装路径扫描
#[cfg(target_os = "windows")]
fn scan_windows_cli_paths(cmd: &str) -> Option<PathBuf> {
    let mut common_paths: Vec<PathBuf> = Vec::new();

    // 常见 Windows 安装路径，按优先级排序（仅加入可推导出的绝对路径，避免空/相对路径误判）
    if let Some(app_data) = std::env::var_os("APPDATA") {
        let npm_base = PathBuf::from(app_data).join("npm");
        common_paths.push(npm_base.join(format!("{}.cmd", cmd)));
        common_paths.push(npm_base.join(cmd));
    }

    if let Some(local_app_data) = std::env::var_os("LOCALAPPDATA") {
        let pnpm_base = PathBuf::from(&local_app_data).join("pnpm");
        common_paths.push(pnpm_base.join(format!("{}.cmd", cmd)));
        common_paths.push(pnpm_base.join(cmd));

        let yarn_base = PathBuf::from(local_app_data).join("Yarn").join("bin");
        common_paths.push(yarn_base.join(format!("{}.cmd", cmd)));
        common_paths.push(yarn_base.join(cmd));
    }

    if let Some(home) = dirs::home_dir() {
        let bun_base = home.join(".bun").join("bin");
        common_paths.push(bun_base.join(format!("{}.exe", cmd)));
        common_paths.push(bun_base.join(cmd));
    }

    for path in common_paths {
        if is_safe_path(&path) {
            tracing::debug!(
                "[CLI-Sync] Detected {} via Windows explicit path: {:?}",
                cmd,
                path
            );
            return Some(path);
        }
    }

    // 扫描 NVM Windows 目录
    if let Ok(nvm_home) = std::env::var("NVM_HOME") {
        let nvm_path = PathBuf::from(nvm_home);
        if nvm_path.is_dir() {
            // NVM Windows 结构: %NVM_HOME%\v{version}\{cmd}.cmd
            if let Ok(entries) = fs::read_dir(&nvm_path) {
                for entry in entries.flatten() {
                    let cmd_path = entry.path().join(format!("{}.cmd", cmd));
                    if is_safe_path(&cmd_path) {
                        tracing::debug!("[CLI-Sync] Detected {} via NVM_HOME: {:?}", cmd, cmd_path);
                        return Some(cmd_path);
                    }
                    // 也检查 .exe 版本
                    let exe_path = entry.path().join(format!("{}.exe", cmd));
                    if is_safe_path(&exe_path) {
                        tracing::debug!("[CLI-Sync] Detected {} via NVM_HOME: {:?}", cmd, exe_path);
                        return Some(exe_path);
                    }
                }
            }
        }
    }

    None
}

/// 解析 where 命令输出获取第一个有效路径
#[cfg(target_os = "windows")]
fn parse_where_output(output: &[u8]) -> Option<PathBuf> {
    let stdout = String::from_utf8_lossy(output);
    for line in stdout.lines() {
        let trimmed = line.trim();
        if !trimmed.is_empty() {
            let path = PathBuf::from(trimmed);
            if is_safe_path(&path) {
                return Some(path);
            }
        }
    }
    None
}

/// 检查路径是否是 .cmd/.bat 文件
#[cfg(target_os = "windows")]
fn is_cmd_file(path: &PathBuf) -> bool {
    path.extension()
        .and_then(|e| e.to_str())
        .map(|e| e.eq_ignore_ascii_case("cmd") || e.eq_ignore_ascii_case("bat"))
        .unwrap_or(false)
}

/// 验证路径是否安全（防止命令注入）
#[cfg(target_os = "windows")]
fn is_safe_path(path: &PathBuf) -> bool {
    // 检查路径是否存在且是文件
    if !path.exists() || !path.is_file() {
        return false;
    }

    // 必须为绝对路径，避免执行相对路径文件
    if !path.is_absolute() {
        return false;
    }

    // 检查路径是否包含危险字符
    let path_str = path.to_string_lossy();
    let dangerous_chars = ['&', '|', ';', '<', '>', '(', ')', '`', '$', '^', '%', '!'];
    if path_str.chars().any(|c| dangerous_chars.contains(&c)) {
        tracing::warn!(
            "[CLI-Sync] Path contains dangerous characters: {}",
            path_str
        );
        return false;
    }

    true
}

/// 执行版本命令（Windows 特殊处理 .cmd/.bat）
#[cfg(target_os = "windows")]
fn run_version_command(executable_path: &PathBuf) -> Option<String> {
    // 安全校验：验证路径不包含危险字符
    if !is_safe_path(executable_path) {
        return None;
    }

    let output = if is_cmd_file(executable_path) {
        // 使用引号包裹路径防止注入，使用 /S 开关确保安全解析
        let quoted_path = format!("\"{}\"", executable_path.to_string_lossy());
        Command::new("cmd.exe")
            .arg("/C")
            .arg(&quoted_path)
            .arg("--version")
            .creation_flags(CREATE_NO_WINDOW)
            .output()
    } else {
        let mut cmd = Command::new(executable_path);
        cmd.arg("--version");
        cmd.creation_flags(CREATE_NO_WINDOW);
        cmd.output()
    };

    match output {
        Ok(out) if out.status.success() => {
            let s = String::from_utf8_lossy(&out.stdout).trim().to_string();
            // 使用正则提取版本号（更精确）
            extract_version(&s)
        }
        _ => None,
    }
}

/// 提取版本号（使用更精确的 semver 匹配）
fn extract_version(s: &str) -> Option<String> {
    // 匹配 semver 格式: x.y.z 或 x.y
    let re = regex::Regex::new(r"(\d+\.\d+(?:\.\d+)?)").ok()?;
    re.captures(s)
        .and_then(|caps| caps.get(1))
        .map(|m| m.as_str().to_string())
}

#[derive(Debug, Serialize, Deserialize, Clone, PartialEq, Eq)]
pub enum CliApp {
    Claude,
    Codex,
    Gemini,
    OpenCode,
}

#[derive(Debug, Serialize, Deserialize, Clone, PartialEq, Eq)]
pub struct CliConfigFile {
    pub name: String,
    pub path: PathBuf,
}

impl CliApp {
    pub fn as_str(&self) -> &'static str {
        match self {
            CliApp::Claude => "claude",
            CliApp::Codex => "codex",
            CliApp::Gemini => "gemini",
            CliApp::OpenCode => "opencode",
        }
    }

    pub fn config_files(&self) -> Vec<CliConfigFile> {
        let home = match dirs::home_dir() {
            Some(p) => p,
            None => return vec![],
        };
        match self {
            CliApp::Claude => vec![
                CliConfigFile {
                    name: ".claude.json".to_string(),
                    path: home.join(".claude.json"),
                },
                CliConfigFile {
                    name: "settings.json".to_string(),
                    path: home.join(".claude").join("settings.json"),
                },
            ],
            CliApp::Codex => {
                let codex_dir = std::env::var_os("CODEX_HOME")
                    .map(PathBuf::from)
                    .unwrap_or_else(|| {
                        let modern = home.join(".codex");
                        if modern.exists() || !home.join(".chatgpt").exists() {
                            modern
                        } else {
                            home.join(".chatgpt")
                        }
                    });
                vec![
                    CliConfigFile {
                        name: "auth.json".to_string(),
                        path: codex_dir.join("auth.json"),
                    },
                    CliConfigFile {
                        name: "config.toml".to_string(),
                        path: codex_dir.join("config.toml"),
                    },
                ]
            }
            CliApp::Gemini => vec![
                CliConfigFile {
                    name: ".env".to_string(),
                    path: home.join(".gemini").join(".env"),
                },
                CliConfigFile {
                    name: "settings.json".to_string(),
                    path: home.join(".gemini").join("settings.json"),
                },
                CliConfigFile {
                    name: "config.json".to_string(),
                    path: home.join(".gemini").join("config.json"),
                },
            ],
            CliApp::OpenCode => vec![CliConfigFile {
                name: "config.json".to_string(),
                path: home.join(".opencode").join("config.json"),
            }],
        }
    }

    pub fn default_url(&self) -> &'static str {
        match self {
            CliApp::Claude => "https://api.anthropic.com",
            CliApp::Codex => "https://api.openai.com/v1",
            CliApp::Gemini => "https://generativelanguage.googleapis.com",
            CliApp::OpenCode => "https://api.openai.com/v1",
        }
    }
}

#[derive(Debug, Serialize, Deserialize, Clone)]
pub struct CliStatus {
    pub installed: bool,
    pub version: Option<String>,
    pub is_synced: bool,
    pub has_backup: bool,
    pub current_base_url: Option<String>,
    pub files: Vec<String>, // 返回关联的文件名列表供前端展示
}

/// 检测 CLI 是否安装并获取版本
pub fn check_cli_installed(app: &CliApp) -> (bool, Option<String>) {
    let cmd = app.as_str();
    // 默认使用命令名，如果 fallback 找到路径则更新为绝对路径
    let mut executable_path = PathBuf::from(cmd);

    // 1. 优先使用 which/where 检测 (遵循 PATH)
    let which_output = if cfg!(target_os = "windows") {
        let mut c = Command::new("where");
        c.arg(cmd);
        #[cfg(target_os = "windows")]
        c.creation_flags(CREATE_NO_WINDOW);
        c.output()
    } else {
        Command::new("which").arg(cmd).output()
    };

    let mut installed = match &which_output {
        Ok(out) => out.status.success(),
        Err(_) => false,
    };

    #[cfg(target_os = "windows")]
    if installed {
        if let Ok(out) = &which_output {
            if let Some(found_path) = parse_where_output(&out.stdout) {
                executable_path = found_path;
            }
        }
    }

    #[cfg(target_os = "windows")]
    if !installed {
        if let Some(found_path) = scan_windows_cli_paths(cmd) {
            installed = true;
            executable_path = found_path;
        }
    }

    // [FIX #765] macOS 增强检测: 如果 which 失败,显式搜索常用二进制路径
    // 解决 Tauri 进程 PATH 可能不完整导致检测不到已安装 CLI 的问题
    if !installed && !cfg!(target_os = "windows") {
        let home = dirs::home_dir().unwrap_or_default();
        let mut common_paths = vec![
            home.join(".local/bin"),
            home.join(".bun/bin"),
            home.join(".bun/install/global/node_modules/.bin"),
            home.join(".npm-global/bin"),
            home.join(".volta/bin"),
            home.join("bin"),
            PathBuf::from("/opt/homebrew/bin"),
            PathBuf::from("/usr/local/bin"),
            PathBuf::from("/usr/bin"),
        ];

        // 增强：扫描 nvm 目录下的所有 node 版本
        let nvm_base = home.join(".nvm/versions/node");
        if nvm_base.exists() {
            if let Ok(entries) = std::fs::read_dir(&nvm_base) {
                for entry in entries.flatten() {
                    let bin_path = entry.path().join("bin");
                    if bin_path.exists() {
                        common_paths.push(bin_path);
                    }
                }
            }
        }

        for path in common_paths {
            let full_path = path.join(cmd);
            if full_path.exists() {
                tracing::debug!(
                    "[CLI-Sync] Detected {} via explicit path: {:?}",
                    cmd,
                    full_path
                );
                installed = true;
                executable_path = full_path;
                break;
            }
        }
    }

    if !installed {
        return (false, None);
    }

    // 2. 获取版本（Windows 使用特殊处理 .cmd/.bat）
    #[cfg(target_os = "windows")]
    let version = run_version_command(&executable_path);

    #[cfg(not(target_os = "windows"))]
    let version = {
        let mut ver_cmd = Command::new(&executable_path);
        ver_cmd.arg("--version");
        let version_output = ver_cmd.output();
        match version_output {
            Ok(out) if out.status.success() => {
                let s = String::from_utf8_lossy(&out.stdout).trim().to_string();
                let cleaned = s
                    .split(|c: char| !c.is_numeric() && c != '.')
                    .filter(|part| !part.is_empty())
                    .last()
                    .map(|p| p.trim())
                    .unwrap_or(&s)
                    .to_string();
                Some(cleaned)
            }
            _ => None,
        }
    };

    (true, version)
}

/// 读取当前配置并检测同步状态
pub fn get_sync_status(app: &CliApp, proxy_url: &str) -> (bool, bool, Option<String>) {
    let files = app.config_files();
    if files.is_empty() {
        return (false, false, None);
    }

    let mut all_synced = true;
    let mut has_backup = false;
    let mut current_base_url = None;

    for file in &files {
        // 使用更简单的命名规则: original_name + .antigravity.bak
        let backup_path = file
            .path
            .with_file_name(format!("{}.antigravity.bak", file.name));

        if backup_path.exists() {
            has_backup = true;
        }

        // 如果物理文件不存在
        // 如果物理文件不存在
        if !file.path.exists() {
            // Gemini 的 settings.json/config.json 只要有一个存在即可，或者都不存在（视为未同步）
            if app == &CliApp::Gemini
                && (file.name == "settings.json" || file.name == "config.json")
            {
                continue;
            }
            all_synced = false;
            continue;
        }

        let content = match fs::read_to_string(&file.path) {
            Ok(c) => c,
            Err(_) => {
                all_synced = false;
                continue;
            }
        };

        match app {
            CliApp::Claude => {
                if file.name == "settings.json" {
                    let json: Value = serde_json::from_str(&content).unwrap_or_default();
                    let url = json
                        .get("env")
                        .and_then(|e| e.get("ANTHROPIC_BASE_URL"))
                        .and_then(|v| v.as_str());
                    if let Some(u) = url {
                        current_base_url = Some(u.to_string());
                        if u.trim_end_matches('/') != proxy_url.trim_end_matches('/') {
                            all_synced = false;
                        }
                    } else {
                        all_synced = false;
                    }
                } else if file.name == ".claude.json" {
                    let json: Value = serde_json::from_str(&content).unwrap_or_default();
                    if json.get("hasCompletedOnboarding") != Some(&Value::Bool(true)) {
                        all_synced = false;
                    }
                }
            }
            CliApp::Codex => {
                if file.name == "config.toml" {
                    // 正则匹配 base_url
                    let re =
                        regex::Regex::new(r#"(?m)^\s*base_url\s*=\s*['"]([^'"]+)['"]"#).unwrap();
                    if let Some(caps) = re.captures(&content) {
                        let url = &caps[1];
                        current_base_url = Some(url.to_string());
                        if url.trim_end_matches('/') != proxy_url.trim_end_matches('/') {
                            all_synced = false;
                        }
                    } else {
                        all_synced = false;
                    }
                }
            }
            CliApp::Gemini => {
                if file.name == ".env" {
                    let re = regex::Regex::new(r#"(?m)^GOOGLE_GEMINI_BASE_URL=(.*)$"#).unwrap();
                    if let Some(caps) = re.captures(&content) {
                        let url = caps[1].trim();
                        current_base_url = Some(url.to_string());
                        if url.trim_end_matches('/') != proxy_url.trim_end_matches('/') {
                            all_synced = false;
                        }
                    } else {
                        all_synced = false;
                    }
                }
            }
            CliApp::OpenCode => {
                if file.name == "config.json" {
                    let json: Value = serde_json::from_str(&content).unwrap_or_default();
                    let url = json
                        .get("providers")
                        .and_then(|p| p.get("openai"))
                        .and_then(|o| o.get("baseURL"))
                        .and_then(|v| v.as_str());
                    if let Some(u) = url {
                        current_base_url = Some(u.to_string());
                        if u.trim_end_matches('/') != proxy_url.trim_end_matches('/') {
                            all_synced = false;
                        }
                    } else {
                        all_synced = false;
                    }
                }
            }
        }
    }

    (all_synced, has_backup, current_base_url)
}
fn backup_path(file: &CliConfigFile) -> PathBuf {
    file.path
        .with_file_name(format!("{}.antigravity.bak", file.name))
}

fn created_marker_path(file: &CliConfigFile) -> PathBuf {
    file.path
        .with_file_name(format!("{}.antigravity-manager.created", file.name))
}

fn validate_existing_config(app: &CliApp, file: &CliConfigFile) -> Result<(), String> {
    if !file.path.exists() {
        return Ok(());
    }
    let content = fs::read_to_string(&file.path)
        .map_err(|error| format!("无法读取 {}: {error}", file.name))?;
    if matches!(app, CliApp::Gemini) && file.name == ".env" {
        return Ok(());
    }
    if matches!(app, CliApp::Codex) && file.name == "config.toml" {
        let document = content
            .parse::<toml_edit::DocumentMut>()
            .map_err(|error| format!("{} 不是有效 TOML: {error}", file.name))?;
        for (parent, child) in [("model_providers", Some("custom")), ("features", None)] {
            let Some(item) = document.get(parent) else {
                continue;
            };
            let table = item.as_table().ok_or_else(|| {
                format!(
                    "{file_name} 的 {parent} 必须是 TOML 表",
                    file_name = file.name
                )
            })?;
            if let Some(child) = child {
                if let Some(item) = table.get(child) {
                    item.as_table().ok_or_else(|| {
                        format!(
                            "{file_name} 的 {parent}.{child} 必须是 TOML 表",
                            file_name = file.name
                        )
                    })?;
                }
            }
        }
        return Ok(());
    }

    let json: Value = serde_json::from_str(&content)
        .map_err(|error| format!("{} 不是有效 JSON: {error}", file.name))?;
    let root = json
        .as_object()
        .ok_or_else(|| format!("{} 的根节点必须是 JSON 对象", file.name))?;
    let require_object =
        |parent: &serde_json::Map<String, Value>, key: &str| -> Result<(), String> {
            if let Some(value) = parent.get(key) {
                value
                    .as_object()
                    .ok_or_else(|| format!("{} 的 {key} 必须是 JSON 对象", file.name))?;
            }
            Ok(())
        };
    match app {
        CliApp::Claude if file.name == "settings.json" => require_object(root, "env")?,
        CliApp::Gemini if file.name == "settings.json" || file.name == "config.json" => {
            require_object(root, "security")?;
            if let Some(security) = root.get("security").and_then(Value::as_object) {
                require_object(security, "auth")?;
            }
        }
        CliApp::OpenCode if file.name == "config.json" => {
            require_object(root, "providers")?;
            if let Some(providers) = root.get("providers").and_then(Value::as_object) {
                require_object(providers, "openai")?;
            }
        }
        _ => {}
    }
    Ok(())
}

/// 执行同步逻辑
fn sync_config_locked(
    app: &CliApp,
    proxy_url: &str,
    api_key: &str,
    model: Option<&str>,
) -> Result<(), String> {
    let files = app.config_files();
    for file in &files {
        validate_existing_config(app, file)?;
    }

    for file in &files {
        // Gemini 兼容性逻辑：优先使用 settings.json
        if app == &CliApp::Gemini && file.name == "config.json" && !file.path.exists() {
            let settings_path = file.path.with_file_name("settings.json");
            if settings_path.exists() {
                continue;
            }
        }

        let existed_before_sync = file.path.exists();
        if let Some(parent) = file.path.parent() {
            fs::create_dir_all(parent).map_err(|e| format!("无法创建目录: {}", e))?;
        }

        // A creation marker means this file has no user-owned original state, even
        // after a later sync has made the file exist.
        let manager_created = created_marker_path(file).exists();
        if existed_before_sync && !manager_created {
            let backup = backup_path(file);
            if !backup.exists() {
                let bytes = fs::read(&file.path)
                    .map_err(|error| format!("无法读取 {}: {error}", file.name))?;
                write_atomic(&backup, &bytes)
                    .map_err(|error| format!("无法备份 {}: {error}", file.name))?;
            }
        } else if !existed_before_sync {
            write_atomic(&created_marker_path(file), b"created by api-manager\n")
                .map_err(|error| format!("无法记录 {} 的创建所有权: {error}", file.name))?;
        }

        let mut content = if existed_before_sync {
            fs::read_to_string(&file.path)
                .map_err(|error| format!("无法读取 {}: {error}", file.name))?
        } else {
            String::new()
        };

        match app {
            CliApp::Claude => {
                if file.name == ".claude.json" {
                    let mut json: Value =
                        serde_json::from_str(&content).unwrap_or_else(|_| serde_json::json!({}));
                    if let Some(obj) = json.as_object_mut() {
                        obj.insert("hasCompletedOnboarding".to_string(), Value::Bool(true));
                    }
                    content = serde_json::to_string_pretty(&json).unwrap();
                } else if file.name == "settings.json" {
                    let mut json: serde_json::Value =
                        serde_json::from_str(&content).unwrap_or_else(|_| serde_json::json!({}));
                    if json.as_object().is_none() {
                        json = serde_json::json!({});
                    }
                    let env = json
                        .as_object_mut()
                        .unwrap()
                        .entry("env")
                        .or_insert(serde_json::json!({}));
                    if let Some(env_obj) = env.as_object_mut() {
                        env_obj.insert(
                            "ANTHROPIC_BASE_URL".to_string(),
                            Value::String(proxy_url.to_string()),
                        );
                        if !api_key.is_empty() {
                            if proxy_url.contains("apikey.fun") {
                                env_obj.insert(
                                    "ANTHROPIC_AUTH_TOKEN".to_string(),
                                    Value::String(api_key.to_string()),
                                );
                                env_obj.insert(
                                    "CLAUDE_CODE_DISABLE_NONESSENTIAL_TRAFFIC".to_string(),
                                    Value::String("1".to_string()),
                                );
                                env_obj.insert(
                                    "CLAUDE_CODE_ATTRIBUTION_HEADER".to_string(),
                                    Value::String("0".to_string()),
                                );
                                env_obj.remove("ANTHROPIC_API_KEY");
                            } else {
                                env_obj.insert(
                                    "ANTHROPIC_API_KEY".to_string(),
                                    Value::String(api_key.to_string()),
                                );
                                // [FIX] 避免冲突：如果存在则移除 ANTHROPIC_AUTH_TOKEN
                                env_obj.remove("ANTHROPIC_AUTH_TOKEN");
                            }

                            // [FIX] 清理可能来自其他 Provider 的模型覆盖设置
                            env_obj.remove("ANTHROPIC_MODEL");
                            env_obj.remove("ANTHROPIC_DEFAULT_HAIKU_MODEL");
                            env_obj.remove("ANTHROPIC_DEFAULT_OPUS_MODEL");
                            env_obj.remove("ANTHROPIC_DEFAULT_SONNET_MODEL");
                        } else {
                            // 如果 API Key 为空，则移除该键，避免设置为空字符串
                            env_obj.remove("ANTHROPIC_API_KEY");
                            env_obj.remove("ANTHROPIC_AUTH_TOKEN");
                        }
                    }

                    if let Some(m) = model {
                        // 注意：Claude Code 的官方配置中，当前选定模型放在根节点的 model 字段
                        json.as_object_mut()
                            .unwrap()
                            .insert("model".to_string(), Value::String(m.to_string()));
                    }
                    content = serde_json::to_string_pretty(&json).unwrap();
                }
            }
            CliApp::Codex => {
                if file.name == "auth.json" {
                    let mut json: Value =
                        serde_json::from_str(&content).unwrap_or_else(|_| serde_json::json!({}));
                    if let Some(obj) = json.as_object_mut() {
                        obj.insert(
                            "OPENAI_API_KEY".to_string(),
                            Value::String(api_key.to_string()),
                        );
                        if proxy_url.contains("apikey.fun") {
                            obj.remove("OPENAI_BASE_URL");
                        } else {
                            // Codex 的 auth.json 似乎也支持 OPENAI_BASE_URL，但 ccs 没写，我们也同步写一下
                            obj.insert(
                                "OPENAI_BASE_URL".to_string(),
                                Value::String(proxy_url.to_string()),
                            );
                        }
                    }
                    content = serde_json::to_string_pretty(&json).unwrap();
                } else if file.name == "config.toml" {
                    use toml_edit::{value, DocumentMut};
                    let mut doc = content
                        .parse::<DocumentMut>()
                        .unwrap_or_else(|_| DocumentMut::new());

                    // 必须使用 custom 提供商，Codex 不支持原生的 codex provider
                    let provider_key = "custom";
                    let display_name = if proxy_url.contains("apikey.fun") {
                        "APIKEY.FUN"
                    } else {
                        "Custom Node"
                    };

                    // 优先设置 Root Keys 确保位于顶部
                    doc.insert("model_provider", value(provider_key));

                    if proxy_url.contains("apikey.fun") {
                        doc.insert("model", value("gpt-5.5"));
                        doc.insert("review_model", value("gpt-5.5"));
                        doc.insert("model_reasoning_effort", value("high"));
                        doc.insert("disable_response_storage", value(true));
                        doc.insert("network_access", value("enabled"));
                        doc.insert("windows_wsl_setup_acknowledged", value(true));
                        doc.insert("model_context_window", value(270000));
                        doc.insert("model_auto_compact_token_limit", value(270000));
                        doc.insert("effective_context_window_percent", value(95));
                    } else {
                        if let Some(m) = model {
                            doc.insert("model", value(m));
                        }
                    }

                    // 移除可能的根级别旧配置
                    doc.remove("openai_api_key");
                    doc.remove("openai_base_url");

                    // 设置层级 [model_providers.custom]
                    let providers = doc
                        .entry("model_providers")
                        .or_insert(toml_edit::Item::Table(toml_edit::Table::new()));
                    if let Some(p_table) = providers.as_table_mut() {
                        let custom = p_table
                            .entry(provider_key)
                            .or_insert(toml_edit::Item::Table(toml_edit::Table::new()));
                        if let Some(c_table) = custom.as_table_mut() {
                            c_table.insert("name", value(display_name));
                            c_table.insert("wire_api", value("responses"));
                            c_table.insert("requires_openai_auth", value(true));
                            c_table.insert("base_url", value(proxy_url.to_string()));
                            if let Some(m) = model {
                                c_table.insert("model", value(m));
                            }
                        }
                    }

                    if proxy_url.contains("apikey.fun") {
                        let features = doc
                            .entry("features")
                            .or_insert(toml_edit::Item::Table(toml_edit::Table::new()));
                        if let Some(f_table) = features.as_table_mut() {
                            f_table.insert("goals", value(true));
                        }
                    }
                    content = doc.to_string();
                }
            }
            CliApp::Gemini => {
                if file.name == ".env" {
                    let mut lines: Vec<String> = content.lines().map(|s| s.to_string()).collect();
                    let mut found_url = false;
                    let mut found_key = false;
                    for line in lines.iter_mut() {
                        if line.starts_with("GOOGLE_GEMINI_BASE_URL=") {
                            *line = format!("GOOGLE_GEMINI_BASE_URL={}", proxy_url);
                            found_url = true;
                        } else if line.trim().starts_with("GEMINI_API_KEY=") {
                            *line = format!("GEMINI_API_KEY={}", api_key);
                            found_key = true;
                        }
                    }
                    if !found_url {
                        lines.push(format!("GOOGLE_GEMINI_BASE_URL={}", proxy_url));
                    }
                    if !found_key {
                        lines.push(format!("GEMINI_API_KEY={}", api_key));
                    }
                    if let Some(m) = model {
                        let mut found_model = false;
                        for line in lines.iter_mut() {
                            if line.starts_with("GOOGLE_GEMINI_MODEL=") {
                                *line = format!("GOOGLE_GEMINI_MODEL={}", m);
                                found_model = true;
                            }
                        }
                        if !found_model {
                            lines.push(format!("GOOGLE_GEMINI_MODEL={}", m));
                        }
                    }
                    content = lines.join("\n");
                } else if file.name == "settings.json" || file.name == "config.json" {
                    let mut json: Value =
                        serde_json::from_str(&content).unwrap_or_else(|_| serde_json::json!({}));
                    if json.as_object().is_none() {
                        json = serde_json::json!({});
                    }
                    let sec = json
                        .as_object_mut()
                        .unwrap()
                        .entry("security")
                        .or_insert(serde_json::json!({}));
                    let auth = sec
                        .as_object_mut()
                        .unwrap()
                        .entry("auth")
                        .or_insert(serde_json::json!({}));
                    if let Some(auth_obj) = auth.as_object_mut() {
                        auth_obj.insert(
                            "selectedType".to_string(),
                            Value::String("gemini-api-key".to_string()),
                        );
                    }
                    content = serde_json::to_string_pretty(&json).unwrap();
                }
            }
            CliApp::OpenCode => {
                if file.name == "config.json" {
                    let mut json: Value =
                        serde_json::from_str(&content).unwrap_or_else(|_| serde_json::json!({}));
                    if json.as_object().is_none() {
                        json = serde_json::json!({});
                    }
                    let providers = json
                        .as_object_mut()
                        .unwrap()
                        .entry("providers")
                        .or_insert(serde_json::json!({}));
                    let openai = providers
                        .as_object_mut()
                        .unwrap()
                        .entry("openai")
                        .or_insert(serde_json::json!({}));
                    if let Some(openai_obj) = openai.as_object_mut() {
                        openai_obj
                            .insert("baseURL".to_string(), Value::String(proxy_url.to_string()));
                        if !api_key.is_empty() {
                            openai_obj
                                .insert("apiKey".to_string(), Value::String(api_key.to_string()));
                        }
                    }
                    content = serde_json::to_string_pretty(&json).unwrap();
                }
            }
        }

        write_atomic(&file.path, content.as_bytes())
            .map_err(|error| format!("写入配置文件失败: {error}"))?;
    }

    Ok(())
}

pub fn sync_config(
    app: &CliApp,
    proxy_url: &str,
    api_key: &str,
    model: Option<&str>,
) -> Result<(), String> {
    let _transaction = lock_client_config();
    sync_config_locked(app, proxy_url, api_key, model)
}

fn restore_files_locked(files: &[CliConfigFile]) -> Result<bool, String> {
    let mut restored_any = false;

    for file in files {
        let backup = backup_path(file);
        let marker = created_marker_path(file);
        // Marker ownership wins over a stale backup produced by older repeated
        // syncs: the managed file did not exist before the first sync.
        if marker.exists() {
            if file.path.exists() {
                fs::remove_file(&file.path)
                    .map_err(|error| format!("删除由集成创建的 {} 失败: {error}", file.name))?;
            }
            fs::remove_file(&marker)
                .map_err(|error| format!("删除 {} 的所有权记录失败: {error}", file.name))?;
            if backup.exists() {
                fs::remove_file(&backup)
                    .map_err(|error| format!("删除过期备份失败 {}: {error}", file.name))?;
            }
            restored_any = true;
        } else if backup.exists() {
            let bytes = fs::read(&backup)
                .map_err(|error| format!("恢复备份失败 {}: {error}", file.name))?;
            write_atomic(&file.path, &bytes)
                .map_err(|error| format!("恢复备份失败 {}: {error}", file.name))?;
            fs::remove_file(&backup)
                .map_err(|error| format!("删除已恢复备份失败 {}: {error}", file.name))?;
            restored_any = true;
        }
    }

    Ok(restored_any)
}

pub fn restore_config(app: &CliApp) -> Result<(), String> {
    let _transaction = lock_client_config();
    let files = app.config_files();
    if restore_files_locked(&files)? {
        Ok(())
    } else {
        sync_config_locked(app, app.default_url(), "", None)
    }
}

// Tauri Commands

#[tauri::command]
pub async fn get_cli_sync_status(app_type: CliApp, proxy_url: String) -> Result<CliStatus, String> {
    tokio::task::spawn_blocking(move || {
        let (installed, version) = check_cli_installed(&app_type);
        let (is_synced, has_backup, current_base_url) = if installed {
            get_sync_status(&app_type, &proxy_url)
        } else {
            (false, false, None)
        };

        Ok(CliStatus {
            installed,
            version,
            is_synced,
            has_backup,
            current_base_url,
            files: app_type
                .config_files()
                .into_iter()
                .map(|f| f.name)
                .collect(),
        })
    })
    .await
    .unwrap_or_else(|_| Err("Task panicked".to_string()))
}

#[tauri::command]
pub async fn execute_cli_sync(
    app_type: CliApp,
    proxy_url: String,
    api_key: String,
    model: Option<String>,
) -> Result<(), String> {
    tokio::task::spawn_blocking(move || {
        sync_config(&app_type, &proxy_url, &api_key, model.as_deref())
    })
    .await
    .unwrap_or_else(|_| Err("Task panicked".to_string()))
}

#[tauri::command]
pub async fn execute_cli_restore(app_type: CliApp) -> Result<(), String> {
    tokio::task::spawn_blocking(move || restore_config(&app_type))
        .await
        .unwrap_or_else(|_| Err("Task panicked".to_string()))
}

#[tauri::command]
pub async fn get_cli_config_content(
    app_type: CliApp,
    file_name: Option<String>,
) -> Result<String, String> {
    tokio::task::spawn_blocking(move || {
        let files = app_type.config_files();
        let file = if let Some(name) = file_name {
            files
                .into_iter()
                .find(|f| f.name == name)
                .ok_or("找不到指定的文件".to_string())?
        } else {
            files
                .into_iter()
                .next()
                .ok_or("找不到配置文件".to_string())?
        };

        if !file.path.exists() {
            return Err("配置文件不存在".to_string());
        }
        fs::read_to_string(&file.path).map_err(|e| format!("读取配置文件失败: {}", e))
    })
    .await
    .unwrap_or_else(|_| Err("Task panicked".to_string()))
}

#[cfg(test)]
mod tests {
    use super::{
        backup_path, created_marker_path, restore_files_locked, validate_existing_config, CliApp,
        CliConfigFile,
    };
    use std::fs;

    #[test]
    fn malformed_existing_config_is_rejected_before_mutation() {
        let directory = tempfile::tempdir().unwrap();
        let file = CliConfigFile {
            name: "settings.json".to_string(),
            path: directory.path().join("settings.json"),
        };
        fs::write(&file.path, b"{ malformed").unwrap();

        assert!(validate_existing_config(&CliApp::Claude, &file).is_err());
        assert_eq!(fs::read(&file.path).unwrap(), b"{ malformed");
        assert!(!backup_path(&file).exists());
    }

    #[test]
    fn invalid_nested_configuration_is_rejected_before_mutation() {
        let directory = tempfile::tempdir().unwrap();
        let file = CliConfigFile {
            name: "settings.json".to_string(),
            path: directory.path().join("settings.json"),
        };
        fs::write(&file.path, br#"{"security":null}"#).unwrap();

        assert!(validate_existing_config(&CliApp::Gemini, &file).is_err());
        assert_eq!(fs::read(&file.path).unwrap(), br#"{"security":null}"#);
        assert!(!backup_path(&file).exists());
    }

    #[test]
    fn restore_removes_only_explicitly_owned_companion_files() {
        let directory = tempfile::tempdir().unwrap();
        let primary = CliConfigFile {
            name: ".claude.json".to_string(),
            path: directory.path().join(".claude.json"),
        };
        let companion = CliConfigFile {
            name: "settings.json".to_string(),
            path: directory.path().join(".claude").join("settings.json"),
        };
        fs::create_dir_all(companion.path.parent().unwrap()).unwrap();
        fs::write(&primary.path, b"managed").unwrap();
        fs::write(backup_path(&primary), b"original").unwrap();
        fs::write(&companion.path, b"managed-companion").unwrap();
        // Simulates the stale backup created by older repeated-sync releases.
        fs::write(backup_path(&companion), b"managed-companion").unwrap();
        fs::write(created_marker_path(&companion), b"created by api-manager\n").unwrap();

        assert!(restore_files_locked(&[primary.clone(), companion.clone()]).unwrap());
        assert_eq!(fs::read(primary.path).unwrap(), b"original");
        assert!(!companion.path.exists());
        assert!(!created_marker_path(&companion).exists());
        assert!(!backup_path(&companion).exists());
    }
}
