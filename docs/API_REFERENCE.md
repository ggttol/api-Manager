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
| POST | `/codex/v1/images/generations` | Codex 官方客户端生图兼容；转换成订阅上游原生 Responses 生图，返回真实 `data[].b64_json` |
| POST | `/codex/v1/messages` | Anthropic Messages 兼容；文本、图片、自定义工具往返、原生搜索与引文、严格 JSON Schema、非流式及增量 SSE |
| POST | `/codex/v1/messages/count_tokens` | 返回 Anthropic 格式 `501`；没有可用的准确上游计数接口，不伪造计数 |

### Codex 官方客户端生图

客户端使用 `/codex/v1` Base URL。内置 `image_gen.imagegen` 会另外调用 `/images/generations`，由网关转换为 `gpt-5.6-luna` 的原生 Responses `image_generation` 工具调用；没有注入到普通聊天请求中，也不需要独立的官方付费 API Key。

兼容请求标识为 `model: "gpt-image-2"`，仅支持一次生成一张 (`n: 1`) 和 Base64 返回；实际图片渲染器由订阅上游选择，不保证与公共 API 的同名模型版本相同。响应 `x-codex-image-compatibility` 明示 `renderer=upstream-default`。支持转交 `size`、`quality`、`background`、`output_format`、`output_compression`、`moderation`；具体组合仍受上游约束。不支持的参数、图片 URL 返回、多图请求及上游失败会明确报错；未实现图片编辑接口。

**客户端前提**：Codex `0.153.4` 会隐藏本地登录套餐为 Free 的内置生图工具，即使服务器池里的账号是 Pro/Plus。需要在客户端登录有权限使用的 Plus/Pro 账号，并保留网关提供商的 `requires_openai_auth = true`、正确的网关鉴权及支持图片输入的模型。可显式启用 `[features] image_generation = true`。登录套餐来自客户端本地状态，服务器账号池不会改写它；不要修改 Token 中的套餐字段。现有服务不支持 WebSocket，保持 `supports_websockets = false`。

```json
{
  "model": "gpt-image-2",
  "prompt": "A cute kitten sitting on a blue cushion",
  "n": 1,
  "size": "auto",
  "quality": "auto",
  "output_format": "png"
}
```

调用 `POST /codex/v1/images/generations`，沿用网关 Bearer Key。成功返回 `created` 与 `data[0].b64_json`；Codex 客户端负责解码、保存和展示。只验证图片生成，不承诺图片编辑或任意其他客户端功能。原生 `/codex/v1/responses` 生图入口保持不变。

`usage` 仅在上游报告时保留原生 Responses 用量，供网关已有日志与令牌计量使用；不伪造图片 Token 数或图片价格，兼容头标注 `usage=responses`。

### Anthropic SDK / Claude Code

Base URL 使用 `http://127.0.0.1:8045/codex`（远程使用可信 HTTPS 或 SSH 隧道），客户端自行追加 `/v1/messages`。模型必须来自 `/codex/v1/models`，不支持将 Claude/Gemini 别名当作 Codex 原生模型。鉴权可使用 `x-api-key` 或 Bearer 网关 Key；管理员密码不是推理凭据。Google 原有 `/v1/messages` 不变。

在 Web **接入指南 → Codex → Anthropic Messages** 加载并选择模型，可复制 cURL 或 Claude Code 配置。Claude Code 使用 `ANTHROPIC_BASE_URL`、`ANTHROPIC_API_KEY`、`ANTHROPIC_MODEL`；指南同时设置三种默认模型别名及子代理模型，避免客户端自动请求 `claude-*`。

协议差异：正整数 `max_tokens` 仅兼容接收，订阅上游不接受 `max_output_tokens`，不保证这个输出硬上限；thinking 预算映射为推理强度，`output_config.effort` 的 `low / medium / high / max` 映射为 `low / medium / high / xhigh`，cache_control 为自动缓存提示。响应 `x-codex-compatibility` 头说明这些差异。不会输出 Claude 签名思维块或推理摘要。采样参数、非空 stop_sequences、assistant 预填充、Claude 签名回放、文档/PDF、其他不支持的服务端工具与上下文编辑会明确报错，不静默丢弃。`count_tokens` 仍返回 `501`。

工具 ID 是网关生成的随机句柄，按调用密钥及原账号隔离，关联的 Codex 推理状态只留在服务器内存。必须原样回传 tool_use ID；未知、过期、跨密钥或混合账号的句柄返回 `409`，原账号失效返回 `503`、原账号冷却返回 `429`，均不跨账号重试。空闲 24 小时或服务重启后，旧网关工具历史不能续接。普通完整文字历史可以重新建立绑定；经适配器验证的完整外部工具历史不依赖网关句柄。

#### 原生联网搜索与严格 JSON Schema

- **搜索**：`tools: [{"type":"web_search_20250305","name":"web_search","max_uses":8}]` 映射为 Codex 原生 `web_search`；支持 `tool_choice: {"type":"auto"}`，由模型决定是否搜索，不是提示词模拟联网。其他搜索工具版本以及不能可靠映射的约束（包括 `allowed_domains` / `blocked_domains`）会明确拒绝，不静默忽略。
- **次数仅建议**：正整数 `max_uses` 转为保留所填次数的指令建议。订阅后端拒绝 `max_tool_calls`，因此 `max_uses: 8` **不能保证最多 8 次调用，也不是计费硬上限**。响应 `x-codex-compatibility` 包含 `web_search_max_uses=advisory`。搜索实际执行情况以上游结果为准。
- **结果与引文**：搜索映射为 `server_tool_use`、`web_search_tool_result` / `web_search_result`；文本 URL 引文映射为 `web_search_result_location`，SSE 使用 `citations_delta`。保留上游 URL 及可用标题；来源可能只有 URL，没有标题或摘录。缺失引用文字保持为空，不伪造来源原文或逐字引述。
- **搜索续接**：`srvtoolu_codex_` 调用 ID 和 `gateway_search_` 搜索/引文句柄是绑定网关、访问密钥与来源账号的不透明标识。即使兼容字段名为 `encrypted_content` / `encrypted_index`，其值也**不是 Anthropic 加密文本**。后续请求必须原样回传整个原始 assistant 内容，包括搜索结果与引文；不可仅摘取或改写句柄。未知、过期或外来句柄明确拒绝；关联的原生搜索及隐藏推理状态仅在原账号续接，不随完整文字会话的额度切换迁移。重启或状态过期后，用完整文字重述任务，不携带旧搜索块或引文句柄。
- **结构化输出**：`output_config.format: {"type":"json_schema","schema":{...}}` 映射为原生 `text.format: {"type":"json_schema","name":"anthropic_response","strict":true,"schema":{...}}`，原始 Schema 原样传递。这是真正的原生严格约束，不是提示词建议。Schema 必须满足上游严格模式支持的子集；不兼容的 Schema 保留明确错误，不采用提示词降级。

下例将搜索和 JSON Schema 合并。先将 `CODEX_MODEL_ID` 设为 `/codex/v1/models` 实际返回的原生 ID，并在可信终端设置 `API_MANAGER_KEY`；不要使用管理员密码或订阅 Token。示例回环地址适用于同机访问或 SSH 隧道，远程请使用可信 HTTPS。`auto` 不保证发起搜索；`max_tokens` 与搜索次数均不是硬上限。

```bash
curl --fail-with-body --no-buffer 'http://127.0.0.1:8045/codex/v1/messages' \
  -H "x-api-key: ${API_MANAGER_KEY:?Set API_MANAGER_KEY first}" \
  -H 'anthropic-version: 2023-06-01' \
  -H 'Content-Type: application/json' \
  --data-raw "$(jq -n --arg model "${CODEX_MODEL_ID:?Select a native model from the catalog}" '{
    model: $model,
    max_tokens: 1024,
    stream: true,
    messages: [{role: "user", content: "Search for the latest Rust stable release. Return an answer and its official source URL."}],
    tools: [{type: "web_search_20250305", name: "web_search", max_uses: 8}],
    tool_choice: {type: "auto"},
    output_config: {
      effort: "high",
      format: {
        type: "json_schema",
        schema: {
          type: "object",
          properties: {answer: {type: "string"}, source_url: {type: "string"}},
          required: ["answer", "source_url"],
          additionalProperties: false
        }
      }
    }
  }')"
```

命令需要 `jq` 安全编码模型 ID。可移除 `tools` / `tool_choice` 仅使用 Schema，或移除 `format` 仅使用搜索；`effort` 与 `format` 属于同一个 `output_config`。Schema 约束最终文本的 JSON 结构，不要求搜索结果块变成该 JSON，也不保证每个结构化答案都会附带原生引文。

**English summary:** Use the gateway key and a native model returned by `/codex/v1/models`; the Anthropic client base URL ends in `/codex`, not `/codex/v1`. `web_search_20250305` uses native search with streamed result blocks and URL citations. `max_uses` is instruction guidance only, disclosed as `web_search_max_uses=advisory`; the subscription backend rejects `max_tool_calls`, so there is no hard call or billing cap. Unsupported tools and search constraints (including domain filters) fail explicitly. Search/citation handles are gateway-scoped, not Anthropic encryption; replay the entire original assistant content unchanged. Unknown, expired, or foreign handles are rejected, and unavailable source quotes stay empty. `output_config.format` forwards the original schema to native strict JSON Schema, with meaningful upstream subset errors and no prompt-only fallback. Exact token counting remains `501`, `max_tokens` remains advisory, complete-text quota failover remains available, and private state remains issuer-bound.

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
