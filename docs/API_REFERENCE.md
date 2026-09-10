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
| GET | `/api/codex/accounts` | `{accounts, active_account_id}`；时间戳为 Unix 秒 |
| POST | `/api/codex/accounts/import` | `{auth_json: <auth.json 对象>, label?: string}`；不接受 API Key 账号 |
| PATCH | `/api/codex/accounts/:id` | `{label?: string, enabled?: boolean}` |
| DELETE | `/api/codex/accounts/:id` | 删除本地凭据；不取消 ChatGPT 订阅 |
| POST | `/api/codex/accounts/:id/activate` | 设置新会话首选账号 |
| POST | `/api/codex/accounts/:id/refresh` | 刷新并验证订阅凭据 |
| GET | `/api/codex/accounts/:id/usage` | 返回实际上游用量 JSON；不估算剩余额度 |
| GET | `/api/codex/models` | 返回首选可用账号的实际模型目录 |
| POST | `/api/codex/auth/device` | 返回 `{id, verification_url, user_code, interval, expires_at}` |
| GET | `/api/codex/auth/device/:id` | 查询 `pending/completed/failed/cancelled/expired` |
| DELETE | `/api/codex/auth/device/:id` | 取消授权，返回最终状态；已完成的授权不会被伪装成已取消 |

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
| POST | `/codex/v1/responses/compact` | 转发订阅上游的独立压缩请求 |

同一会话绑定同一账号，绑定同时按下游密钥隔离。更换首选账号只影响新会话；已绑定账号被禁用或删除时明确失败，不将续接静默转给其他账号。绑定最多保留 8192 项、闲置 24 小时，进程重启会清空；未知续接返回 `409`，需要新建会话。只允许在未输出响应前对同一账号的 HTTP `401` 刷新重试一次，不在流式输出后重放请求。

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
