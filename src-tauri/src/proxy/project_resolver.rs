use serde_json::Value;

/// 使用 Antigravity 的 loadCodeAssist API 获取 project_id
/// 这是获取 cloudaicompanionProject 的正确方式
pub async fn fetch_project_id(access_token: &str) -> Result<String, String> {
    fetch_project_id_for_account(access_token, None).await
}

/// Resolve a project through the same effective route as requests for this account.
pub async fn fetch_project_id_for_account(
    access_token: &str,
    account_id: Option<&str>,
) -> Result<String, String> {
    const LOAD_CODE_ASSIST_ENDPOINTS: [&str; 3] = [
        "https://daily-cloudcode-pa.sandbox.googleapis.com/v1internal:loadCodeAssist",
        "https://daily-cloudcode-pa.googleapis.com/v1internal:loadCodeAssist",
        "https://cloudcode-pa.googleapis.com/v1internal:loadCodeAssist",
    ];
    let request_body = serde_json::json!({
        "metadata": {
            "ideType": "ANTIGRAVITY"
        }
    });

    // Resolve through the account's effective proxy route for every fallback.
    let client = if let Some(pool) = crate::proxy::proxy_pool::get_global_proxy_pool() {
        pool.get_effective_client(account_id, 30).await?
    } else {
        crate::utils::http::get_client()
    };

    let mut last_error = None;
    for url in LOAD_CODE_ASSIST_ENDPOINTS {
        let response = match client
            .post(url)
            .bearer_auth(access_token)
            .header("User-Agent", crate::constants::USER_AGENT.as_str())
            .header("Content-Type", "application/json")
            .json(&request_body)
            .send()
            .await
        {
            Ok(response) => response,
            Err(error) => {
                last_error = Some(format!("loadCodeAssist 请求失败: {}", error));
                continue;
            }
        };

        if !response.status().is_success() {
            let status = response.status();
            let body = response.text().await.unwrap_or_default();
            last_error = Some(format!("loadCodeAssist 返回错误 {}: {}", status, body));
            continue;
        }

        let data: Value = match response.json().await {
            Ok(data) => data,
            Err(error) => {
                last_error = Some(format!("解析响应失败: {}", error));
                continue;
            }
        };
        if let Some(project_id) = data
            .get("cloudaicompanionProject")
            .and_then(|value| value.as_str())
        {
            return Ok(project_id.to_string());
        }
        last_error = Some("账号无资格获取官方 cloudaicompanionProject".to_string());
    }

    Err(last_error.unwrap_or_else(|| "loadCodeAssist 请求失败".to_string()))
}
