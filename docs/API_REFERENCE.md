# API Reference — API Manager / Antigravity v4.7

本文档详细介绍了 **Antigravity Tools** 暴露的 HTTP API 接口。

> **注意**: 在 v4.0.1 版本中，所有的服务（包括 AI 反代和系统管理）均已整合至统一端口 **8045**。原有的 19527 端口已废弃。

## 1. 概览 (Overview)

Antigravity Gateway 是一个双重角色的服务器：
1.  **AI Proxy Interface**: 兼容 OpenAI/Anthropic/Google 官方 SDK 的标准接口。
2.  **Management Admin API**: 用于管理账号、配置系统、监控流量的 RESTful 接口。

### 鉴权体系 (Authentication)

| 接口类型 | 路径前缀 | 鉴权方式 | Header 示例 | 说明 |
| :--- | :--- | :--- | :--- | :--- |
| **AI Protocol** | `/v1/*`, `/v1beta/*`, `/codex/v1/*` | API Key / User Token | `Authorization: Bearer <API_KEY>` | 用于 AI 客户端调用 |
| **Admin API** | `/api/*` | 独立管理密码 | `Authorization: Bearer <WEB_PASSWORD>` | 用于管理后台或脚本控制 |

> **提示**: 请单独设置 `WEB_PASSWORD`（配置字段 `proxy.admin_password`）。未设置时管理鉴权回退到全局 `API_KEY`；共享全局 Key 会同时共享管理权限。`x-api-key` 也是受支持的请求头，`x-admin-token` 不是当前实现支持的鉴权头。

---

## Codex 订阅通道 (API Manager)

这是服务端 / Web 功能，独立于 Google 账号池。访问 `/codex` 管理账号。空 Google 账号池不再阻止共享网关启动；各上游自行判断可用账号。原有代理停止控制仍同时暂停推理入口，但不会阻断管理页面与静态资源。

### 授权与管理

以下路径均要求管理员鉴权，响应不包含订阅 Token，并设置 `Cache-Control: no-store`：

| 方法 | 完整路径 | 请求或用途 |
| :--- | :--- | :--- |
| GET | `/api/codex/accounts` | `{accounts, active_account_id}`；后者是用户首选，不代表当前请求实际账号；时间戳为 Unix 秒 |
| POST | `/api/codex/accounts/import` | `{auth_json: <auth.json 对象>, label?: string}`；不接受 API Key 账号 |
| PATCH | `/api/codex/accounts/:id` | `{label?: string, enabled?: boolean}` |
| DELETE | `/api/codex/accounts/:id` | 删除本地凭据；不取消 ChatGPT 订阅 |
| POST | `/api/codex/accounts/:id/activate` | 设置新会话首选账号 |
| POST | `/api/codex/accounts/:id/refresh` | 刷新并验证订阅凭据 |
| GET | `/api/codex/accounts/:id/usage` | 返回实际上游用量 JSON，并同步额度冷却状态；不估算剩余额度 |
| GET | `/api/codex/models` | 返回首选可用账号的实际模型目录 |
| POST | `/api/codex/auth/device` | 返回 `{id, verification_url, user_code, interval, expires_at}` |
| GET | `/api/codex/auth/device/:id` | 查询 `pending/completed/failed/cancelled/expired` |
| DELETE | `/api/codex/auth/device/:id` | 取消授权，返回最终状态；已完成的授权不会被伪装成已取消 |

账号元数据包含可空的 `cooldown_until`（Unix 秒）和 `cooldown_reason`（`quota_exhausted` / `rate_limited`）。未来的截止时间表示调度暂时跳过该账号，不会改为禁用。额度查询批次完成后，Web 账号池重新获取一次列表以显示最新状态；客户端本地计时移除到期提示，不轮询上游。

设备码由后端按发行方间隔轮询，15 分钟超时；用户可能需要在 ChatGPT 安全设置中启用设备码授权。若当前网络不能访问 OpenAI 授权端点，请先解决上游网络问题，或在可信环境使用官方 Codex 登录后导入授权文件。导入会先安全保存禁用、未验证的凭据，再调用上游验证；验证失败时账号保持禁用，并显示错误，之后可用“刷新授权”重试。不要让多个程序并发刷新同一份授权缓存。

Codex HTTP 客户端沿用并支持热更新现有上游代理设置。授权失败不会自动降级为 OpenAI API Key 计费模式。管理端的上游订阅 `401` 转为 `422`，避免误触发后台退出；网关管理密码错误仍返回 `401`。

### Codex 客户端

在页面加载真实模型目录后，复制生成的 `~/.codex/config.toml` 配置。提供商配置必须位于用户级配置文件，而不是项目级配置中。关键设置如下，模型 ID 从页面选择：

```toml
model_provider = "api_manager_codex"

[model_providers.api_manager_codex]
name = "API Manager Codex"
base_url = "http://127.0.0.1:8045/codex/v1"
env_key = "API_MANAGER_KEY"
wire_api = "responses"
supports_websockets = false
requires_openai_auth = false
```

这里的回环地址适用于同机访问或 SSH 隧道；跨主机请替换为可信 HTTPS 网关地址。启动客户端前设置 `API_MANAGER_KEY` 为网关 API Key 或受支持的 User Token，不能使用管理员密码、ChatGPT Access Token 或 Refresh Token。

| 方法 | 路径 | 行为 |
| :--- | :--- | :--- |
| GET | `/codex/v1/models` | 将实际可用模型目录转换为 OpenAI 模型列表 |
| POST | `/codex/v1/responses` | 原生 Responses；保留工具、推理与 SSE 事件；上游 `store=false` |
| POST | `/codex/v1/responses/compact` | 通过当前官方 Responses `compaction_trigger` 协议生成加密压缩项，汇总为 JSON 返回 |
| POST | `/codex/v1/messages` | Anthropic Messages 兼容；文本、图片、自定义工具往返、非流式及增量 SSE |
| POST | `/codex/v1/messages/count_tokens` | 返回 Anthropic 格式 `501`；没有可用的准确上游计数接口，不伪造计数 |

### Anthropic SDK / Claude Code

Base URL 使用 `http://127.0.0.1:8045/codex`（远程使用可信 HTTPS 或 SSH 隧道），客户端自行追加 `/v1/messages`。模型必须来自 `/codex/v1/models`，不支持将 Claude/Gemini 别名当作 Codex 原生模型。鉴权可使用 `x-api-key` 或 Bearer 网关 Key；管理员密码不是推理凭据。Google 原有 `/v1/messages` 不变。

在 Web **接入指南 → Codex → Anthropic Messages** 加载并选择模型，可复制 cURL 或 Claude Code 配置。Claude Code 使用 `ANTHROPIC_BASE_URL`、`ANTHROPIC_API_KEY`、`ANTHROPIC_MODEL`；指南同时设置三种默认模型别名及子代理模型，避免客户端自动请求 `claude-*`。

协议差异：正整数 `max_tokens` 仅兼容接收，订阅上游不接受 `max_output_tokens`，不保证这个输出硬上限；thinking 预算映射为推理强度，cache_control 为自动缓存提示。响应 `x-codex-compatibility` 头说明这些差异。不会输出 Claude 签名思维块或推理摘要。采样参数、非空 stop_sequences、结构化 output_config.format、assistant 预填充、Claude 签名回放、文档/PDF、服务端工具与未支持的上下文编辑会明确报错，不静默丢弃。

工具 ID 是网关生成的随机句柄，按调用密钥及原账号隔离，关联的 Codex 推理状态只留在服务器内存。必须原样回传 tool_use ID；未知、过期、跨密钥或混合账号的句柄返回 `409`，原账号失效返回 `503`、原账号冷却返回 `429`，均不跨账号重试。空闲 24 小时或服务重启后，旧网关工具历史不能续接。普通完整文字历史可以重新建立绑定；经适配器验证的完整外部工具历史不依赖网关句柄。

### 额度冷却与账号切换

Responses 与 Anthropic Messages 共用默认开启的独立 Codex 调度。新会话优先使用已启用、已验证且不在冷却期的首选账号，否则按账号池顺序选择其他可用账号。收到完整、合法 JSON 的上游 HTTP `429`，且尚未向客户端输出时，可安全迁移的请求会尝试其他候选账号，不回头重复尝试已访问账号；同一账号遇到 `401` 仍允许刷新授权后重试一次。账号池全部冷却时返回 `429` 和最早候选恢复时间对应的 `Retry-After`；没有已启用且已验证账号时返回 `503`。

冷却状态与账号凭据一起持久化，优先采用实际上游耗尽窗口、错误中的重置时间和 `Retry-After`；没有可用重置时间时，已知额度耗尽等待 300 秒，其他 `429` 等待 60 秒。未耗尽的周窗口不会延长未知 5 小时窗口的冷却。到期只恢复候选资格，不代表保证有额度；版本校验后的真实额度查询可以提前确认额度恢复，旧的查询或设备授权结果不能清除更新的额度失败。自动切换不修改 `active_account_id` 或 `enabled`。

普通完整文字历史遇到额度耗尽可沿用原始 `session_id` / `prompt_cache_key` 切换账号；后续请求继续使用切换后的绑定。缓存键规范化及压缩触发项只处理一次，各账号尝试发送同一请求体。响应 ID、加密推理、压缩项、原生工具回传、turn-state 和网关工具句柄不随这些别名移动；原生私有状态另存按调用密钥隔离的不可变来源绑定。已切换别名与旧账号状态混用，或不能证明私有状态来源时返回 `409`；新账号实际签发并已记录的状态仍可在新账号继续使用。

更换首选只影响未绑定请求；被禁用或删除的已绑定账号明确失败。原生会话、响应及私有状态绑定共用 8192 项上限、闲置 24 小时，进程重启会清空；无法识别的私有续接返回 `409`，应使用完整文字上下文重新开始。网络错误、格式损坏或读取中断的响应、上游 `5xx`、已向客户端开始输出的流均不跨账号重放；HTTP `200` 流中的额度错误事件也不会触发重放。

HTTP 代理不是托管 Codex 执行器：终端工具、工作目录、沙箱和审批仍在客户端。此实现不支持 WebSocket、后台 Responses 任务或 FedRAMP 专用上游。上游订阅权益、限流、地区与工作区权限仍然生效；成功登录不构成第三方转售或共享授权。

---

## 2. 管理接口 (Management API)

**Base URL**: `http://<host>:8045/api`

### 2.1 账号管理 (Account Management)

| 方法 | 路径 | 说明 | 参数示例 |
| :--- | :--- | :--- | :--- |
| **GET** | `/accounts` | 获取账号列表 | - |
| **GET** | `/accounts/current` | 获取当前活跃账号 | - |
| **POST** | `/accounts` | 添加账号 (OAuth Refresh Token) | `{"refreshToken": "..."}` |
| **DELETE**| `/accounts/:id` | 删除账号 | - |
| **POST** | `/accounts/switch` | 切换活跃账号 | `{"accountId": "acc_123", "targetIde": "agy"}` (targetIde 可选，如传 `"agy"` 则仅写入凭据免重启 IDE) |
| **POST** | `/accounts/refresh` | **刷新所有账号配额** | - |
| **GET** | `/accounts/:id/quota` | **查询特定账号配额** | - |
| **POST** | `/accounts/:id/toggle-proxy` | 禁用/启用账号代理 | - |
| **POST** | `/accounts/:id/bind-device` | 绑定设备指纹 | `{"mode": "generate"}` |
| **POST** | `/accounts/bulk-delete` | 批量删除账号 | `{"accountIds": ["id1", "id2"]}` |
| **POST** | `/accounts/reorder` | 账号排序 | `{"accountIds": [...]}` |

### 2.2 系统配置 (System Config)
| 方法 | 路径 | 说明 |
| :--- | :--- | :--- |
| **GET** | `/config` | 获取全量配置 |
| **POST** | `/config` | 保存全量配置 |
| **GET** | `/proxy/status` | 获取反代服务运行状态 |
| **POST** | `/proxy/start` | 启动反代服务 |
| **POST** | `/proxy/stop` | 停止反代服务 |
| **POST** | `/proxy/mapping` | 更新模型映射规则 |
| **GET** | `/health` | 系统健康检查 |

### 2.3 监控与统计 (Monitoring & Stats)
#### 流量日志
*   **GET** `/logs`: 获取日志列表 (支持 `limit`, `offset`, `filter`, `errorsOnly` 参数)
*   **GET** `/logs/count`: 获取日志总数
*   **GET** `/logs/:id`: 获取日志详情
*   **POST** `/logs/clear`: 清空日志

#### Token 统计 (v4.0.1 New)
*   **GET** `/stats/token/summary`: 获取 Token 消耗摘要 (今日/本周/总量)
*   **GET** `/stats/token/hourly`: 获取按小时统计数据
*   **GET** `/stats/token/daily`: 获取按日统计数据
*   **GET** `/stats/token/by-account`: 按账号统计消耗占比
*   **GET** `/stats/token/by-model`: 按模型统计消耗占比
*   **POST** `/stats/token/clear`: 重置统计数据

### 2.4 高级功能 (Advanced)
*   **POST** `/proxy/cli/sync`: 执行 CLI (Claude/Codex) 配置文件同步
*   **POST** `/accounts/import/db`: 从 v1 旧数据库导入账号
*   **POST** `/accounts/oauth/start`: 发起 OAuth 授权流程 (Headless)
*   **POST** `/proxy/cloudflared/start`: 启动 Cloudflare Tunnel

---

## 3. AI 协议接口 (AI Protocol Interface)

**Base URL**: `http://<host>:8045`

本服务完全兼容主流 AI 厂商的官方协议规范。您可以直接将本服务的地址填入到支持 OpenAI / Claude 的客户端中。

### OpenAI Compatible
*   **对话生成 (Chat Completions)**
    *   **POST** `/v1/chat/completions`
    *   **支持模型**: 任何映射后的模型 ID (如 `gpt-4o`, `gemini-1.5-pro`)
    *   **兼容性**: 完全兼容 OpenAI 官方 Response 格式 (包括流式 SSE)。

*   **图片生成 (Image Generation)**
    *   **POST** `/v1/images/generations`
    *   **支持模型**: `gemini-3-pro-image` (自动映射到 Imagen 3)
    *   **参数扩展**: 支持 `size: "1920x1080"`, `quality: "hd"` 等高级参数。

### Anthropic Compatible
*   **Claude Messages**
    *   **POST** `/v1/messages`
    *   **用途**: 支持 Claude CLI (`claude`), Cursor, Cherry Studio 等客户端。
    *   **特性**: 完整支持 Tool Use (工具调用) 和 Thinking (思维链) 模式。

### Gemini Native
*   **Google AI Studio**
    *   **GET/POST** `/v1beta/models/*`
    *   **用途**: 供使用 Google 官方 SDK (Python/Node.js) 的应用调用。
