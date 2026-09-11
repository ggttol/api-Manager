import { useEffect, useRef, useState } from 'react';
import { useTranslation } from 'react-i18next';
import { AlertTriangle, ArrowRight, Check, Copy, KeyRound, RefreshCw, ShieldCheck, Terminal, Workflow } from 'lucide-react';
import { showToast } from '../components/common/ToastContainer';
import { copyToClipboard } from '../utils/clipboard';
import { request } from '../utils/request';
import { PageHeader } from '../components/common/ConsolePage';
import { getProxyBaseUrl, isTauri } from '../utils/env';

const content = {
    zh: {
        title: '接入指南', subtitle: '从选择入口到连接客户端，在一个页面完成接入。', badge: '当前服务器',
        securityTitle: '先保护连接，再发送凭据',
        security: '远程访问请使用可信的 HTTPS，或通过 SSH 隧道访问本机回环地址。不要在公共 HTTP 连接上传输管理员密码、网关 Key 或账号授权文件；不要使用 curl -k 跳过证书校验。SSH 隧道建立后，请从隧道的本地地址重新打开本页面再复制示例。',
        insecure: '当前是非本机 HTTP 连接：请先切换到 HTTPS 或 SSH 隧道。',
        credentialsTitle: '两种凭据，两个用途',
        adminTitle: '管理员密码', admin: '仅用于登录本管理界面及管理 API。不要填入推理客户端，也不要作为下面示例的 API Key。',
        gatewayTitle: '网关 API Key', gateway: '在 API 反代配置中管理，用于客户端访问推理入口。本页不读取或展示任何密钥；所有示例只引用你在本地设置的 API_MANAGER_KEY 环境变量。',
        endpointsTitle: '01 · 选择正确的入口', endpointsHint: '地址自动取自当前浏览器域名与端口。客户端填写 Base URL 时，不要重复追加 /v1。两个账号池相互独立，路径不可混用。',
        google: 'Google 账号 · 兼容接口', googleHint: 'OpenAI 兼容客户端使用此入口，由 Google 账号池处理；这里的模型名称与映射不代表 Codex 原生模型。',
        gemini: 'Google 账号 · Gemini 原生', geminiHint: '使用 Gemini 原生 generateContent 协议。先查询该入口的模型目录，再使用返回的模型名称。',
        codex: 'Codex 账号 · 原生 Responses', codexHint: '独立的 Codex 账号池与模型目录。使用 Responses API，不要发送到 Google 的 /v1，也不要使用 Gemini 别名。',
        anthropic: 'Codex 账号 · Anthropic 兼容', anthropicHint: '通过 Anthropic Messages 兼容协议使用 ChatGPT 订阅账号可用的 Codex 原生模型；不是官方 Claude 模型推理，也不使用 Google 账号池。',
        anthropicExample: 'Codex / Anthropic Messages', accountPool: '账号池', protocol: '协议',
        anthropicBase: 'Anthropic SDK / Claude Code 的 Base URL 填写上方的 /codex，不要填 /codex/v1：客户端会追加 /v1/messages。Google 原有的 POST /v1/messages 保持独立，不会转发到 Codex。',
        anthropicSupport: '支持文本、图片、system、自定义工具定义与选择、tool_use / tool_result 多轮往返，以及非流式响应和增量 SSE 流（含工具参数、搜索结果、URL 引文、usage 与 stop_reason）。web_search_20250305 映射到 Codex 原生 web_search，不是让模型假装搜索。其他不支持的服务端工具、搜索约束或选项（如 temperature、top_p、top_k、assistant 预填充、Claude 签名推理回放）会返回 Anthropic 格式错误。',
        anthropicTokens: 'POST /codex/v1/messages/count_tokens 当前返回 HTTP 501（不支持）的 Anthropic 格式错误，不提供估算或伪造的精确 token 数。依赖请求前 token 计数的客户端功能可能不可用，响应中的实际 usage 与此不同。',
        anthropicBudget: '重要限制：max_tokens 必须为正整数，但仅作为客户端兼容字段接受，不能强制限制输出长度。ChatGPT 订阅上游不支持 max_output_tokens，实际输出预算由订阅后端控制；不要将示例的 1024 视为硬上限。',
        anthropicSearch: '搜索限制：max_uses 仅作为保留所填次数的指令建议，不是调用次数或计费硬上限。订阅后端拒绝 max_tool_calls；响应 x-codex-compatibility 头通过 web_search_max_uses=advisory 明示此差异。不要依赖 max_uses: 8 保证最多 8 次搜索。',
        anthropicCitations: '搜索结果及引文忠实保留上游 URL 和可用标题；来源摘录可能缺失，引用文字为空时不代表逐字原文。搜索/引文的不透明句柄绑定网关、密钥与来源账号，并非 Anthropic 加密文本。续接必须原样回传整个 assistant 内容，包括搜索结果和引文；未知、过期或外来句柄会被拒绝。状态过期或重启后，请用文本重述任务，不携带旧搜索块或引文句柄。',
        anthropicSchema: 'output_config.format: {type: "json_schema", schema: ...} 映射为原生 text.format 的 strict JSON Schema 约束，不是提示词建议。原始 schema 原样传递；需满足上游严格模式支持的 Schema 子集，不兼容的 Schema 会明确报错，不降级为提示词生成。',
        anthropicExamples: '可选 · 搜索与 JSON Schema 请求字段',
        anthropicExamplesHint: '将下列字段合并到上方 cURL 的 JSON 请求体，并把 messages 改成实际任务；保留已选择的原生 model、max_tokens 和 stream。可分别使用或合并；合并时将 effort 和 format 放在同一个 output_config 中。搜索是否执行由 auto 决定，schema 示例要求输出 answer 字符串。',
        anthropicSearchExample: 'JSON · 原生搜索（次数仅建议）', anthropicSchemaExample: 'JSON · 原生严格 Schema',
        anthropicThinking: 'thinking 的 enabled / adaptive 仅映射到 Codex 推理设置；budget_tokens 是推理强度参考，不是精确预算。output_config.effort 的 low / medium / high / max 映射为 low / medium / high / xhigh。不会生成 Claude 签名 thinking 块或推理摘要；续接需要的隐藏推理仅保留在服务器端。',
        anthropicSessions: '新请求和携带完整文本上下文的请求支持自动额度切换。网关生成的工具调用 ID 则绑定原账号与访问范围；tool_result 必须原样返回对应 ID，不要跨账号或网关 Key 重用。此类工具续接遇到原账号冷却会明确报错，不会换号重放。工具续接状态保存在内存中，空闲 24 小时后过期；服务器重启或状态过期后，请新建对话并用文本重述任务，不要携带旧 tool_use / tool_result。',
        claudeTitle: '04 · 连接 Claude Code', claudeHelp: '在已设置 API_MANAGER_KEY 的同一 Bash 终端运行以下配置。三个默认模型别名和子代理都使用所选 Codex 原生模型，避免自动请求 claude-* ID；不要用 /model 或项目配置覆盖为 Claude 模型。先移除已有的其他云提供商或鉴权配置冲突。',
        claudeLaunch: '配置仅引用本地网关 Key，不需要 Anthropic API Key 或 Claude 订阅。命令在当前终端设置环境变量并启动 claude；不要提交环境变量或分享含密钥的终端输出。使用完毕请 unset ANTHROPIC_API_KEY API_MANAGER_KEY。',
        catalog: '模型目录', inference: '推理', copy: '复制', copied: '已复制', copyFailed: '复制失败，请手动选择并复制。',
        setupTitle: '02 · 在本地准备网关 Key', setupHint: '以下为 Bash 命令。运行后在隐藏输入提示中粘贴网关 Key，避免将明文 Key 写入命令历史。不要粘贴管理员密码。只在可信的本机终端执行，不要开启 shell 调试（set -x）。',
        setupFooter: '环境变量仅供当前终端及其子进程使用。不要提交到代码仓库或截图分享；使用完毕可执行 unset API_MANAGER_KEY。',
        examplesTitle: '03 · 查询目录，再发起请求', examplesHint: '示例不会自动执行。先运行所选入口的目录查询，将模型占位符替换成实际返回的 ID。两种 Codex 协议与客户端配置同步使用上方真实目录中选择的模型。',
        googleExample: 'Google / OpenAI 兼容', geminiExample: 'Gemini 原生', codexExample: 'Codex / Responses',
        modelHint: '兼容接口使用 /v1/models 返回的 data[].id；Gemini 原生使用 /v1beta/models 返回的 models[].name，并在 URL 中去掉已有的 models/ 前缀。不要假设不同入口共享模型 ID。',
        codexModelHint: 'Codex 两种协议均使用 /codex/v1/models 返回的 data[].id；可在上方加载并选择真实目录中的模型（例如目录实际返回 gpt-5.6-sol 时才使用它），不要填写 Claude 或 Gemini 别名。示例中的单次问答是新会话，不含任何历史会话标识。',
        configTitle: '04 · 连接 Codex CLI', configHint: '先在 Codex 账号页添加并启用账号，再加载上游真实模型目录。只从返回的原生 ID 中选择；不预设模型、不使用 Gemini 映射名称。',
        loadModels: '加载模型目录', refreshModels: '刷新模型目录', loading: '正在加载真实模型目录…', modelLabel: 'Codex 原生模型', chooseModel: '请选择目录中的模型',
        notLoaded: '尚未加载模型目录。加载并选择后即可复制完整配置。', noModels: '目录未返回可用模型。请检查 Codex 账号是否已授权并启用，然后重试。',
        modelsError: '无法加载模型目录。请检查管理员登录状态、Codex 账号授权和上游连接后重试。', invalidCatalog: '模型目录格式无效',
        configHelp: '将以下设置合并到用户级 ~/.codex/config.toml。model 与 model_provider 必须放在文件顶层、任何 [表] 之前；已有同名字段或 provider 表应替换而非重复追加。若设置了 CODEX_HOME，请使用该目录下的 config.toml。不要覆盖其他无关配置。',
        transport: '传输为 HTTP SSE：wire_api = "responses"，supports_websockets = false。requires_openai_auth = false 表示客户端不使用 OpenAI 登录凭据，但网关仍按其鉴权策略检查 API_MANAGER_KEY；这不代表免鉴权。',
        launch: '在已设置 API_MANAGER_KEY 的同一个终端运行 codex。env_key 写的是环境变量名称，不是密钥本身；不要把真实 Key 写进 TOML。',
        protocolTitle: '自动切换、会话与上下文',
        failoverTitle: '额度不足时自动选择其他账号',
        failover: 'Responses 与 Anthropic Messages 共用自动切换：先选已启用、已授权且未冷却的首选账号，否则选择其他可用账号。上游返回 429 后将原账号暂时跳过，并为可安全迁移的请求尝试其他可用账号，不在账号之间循环重试；401 仍允许在同一账号刷新授权后重试一次。首选标记保持您的设置，不随实际处理账号改变。冷却时间依据上游重置时间或 Retry-After；无法确定时使用有限的等待时间，并不代表额度已恢复。账号保持启用，到期后自动恢复候选资格。',
        failoverLimits: '所有可用账号都在冷却时返回 429，并用 Retry-After 给出最早可重试时间；没有启用的账号时返回 503。网络错误、上游 5xx 或已经向客户端输出的流不会跨账号重放。',
        compactTitle: '原生压缩与兼容适配', compact: '当前 Codex 原生协议通过 /codex/v1/responses 的 input 中的 {"type":"compaction_trigger"} 触发压缩。/codex/v1/responses/compact 是网关适配器：追加此原生触发项、收集上游响应，并返回 object 为 response.compaction 的 JSON。它不是独立的上游 /compact 服务，也不是 Google 兼容入口的压缩逻辑。',
        compactNote: '让客户端管理压缩上下文；不要将其他账号或旧会话的加密 reasoning / compaction 内容复制到新会话。',
        affinityTitle: '可迁移的文本与不可迁移的账号状态', affinity: '普通文本请求只要携带继续任务所需的完整文本上下文，即使沿用 session_id 或 prompt_cache_key，也可在原账号额度不足或限流后安全重新绑定到其他可用账号，不必手动新建对话。此操作不会迁移旧响应 ID 或工具句柄。previous_response_id、x-codex-turn-state、加密 reasoning / compaction 项和网关工具续接状态始终绑定原账号；原账号冷却时，此类请求会明确报错，而不是把私有状态发送到另一个账号。',
        restart: '账号亲和性与工具续接状态保存在服务器内存中。重启或过期后，旧的账号绑定状态不能可靠续接。只有遇到未知 previous_response_id、丢失的绑定状态或过期工具句柄时，才需要新建对话、使用新会话 ID 并重新提供原始任务文本；不要携带旧响应 ID、turn-state、加密上下文或旧 tool_use / tool_result。普通完整文本请求的额度切换不要求这一步。',
    },
    en: {
        title: 'Integration guide', subtitle: 'Choose an endpoint, prepare your key, and connect your client.', badge: 'Current server',
        securityTitle: 'Secure the connection before sending credentials',
        security: 'Use trusted HTTPS for remote access, or access a loopback address through an SSH tunnel. Never send an administrator password, gateway key, or account authorization file over public HTTP. Do not bypass certificate checks with curl -k. After establishing a tunnel, reopen this page at its local address before copying examples.',
        insecure: 'This is a non-local HTTP connection. Switch to HTTPS or an SSH tunnel first.',
        credentialsTitle: 'Two credentials, two purposes',
        adminTitle: 'Administrator password', admin: 'For signing into this management interface and calling management APIs only. Do not put it in an inference client or use it as the API key in these examples.',
        gatewayTitle: 'Gateway API key', gateway: 'Managed in the API proxy configuration and used by inference clients. This page never reads or displays keys. Every example references the API_MANAGER_KEY environment variable that you set locally.',
        endpointsTitle: '01 · Choose the right endpoint', endpointsHint: 'Addresses use the current browser origin, including its port. Do not append /v1 twice in a client Base URL. The two account pools are independent; their routes are not interchangeable.',
        google: 'Google accounts · Compatible API', googleHint: 'Use this endpoint with OpenAI-compatible clients. Requests use the Google account pool; its model names and mappings are not native Codex models.',
        gemini: 'Google accounts · Native Gemini', geminiHint: 'Uses the native Gemini generateContent protocol. Query this endpoint’s model catalog first, then use a returned model name.',
        codex: 'Codex accounts · Native Responses', codexHint: 'An independent Codex account pool and model catalog. Use Responses, not the Google /v1 endpoint, and never substitute Gemini aliases.',
        anthropic: 'Codex accounts · Anthropic compatible', anthropicHint: 'Access native Codex models available to ChatGPT subscription accounts through the Anthropic Messages-compatible protocol. This is not official Claude inference and does not use the Google account pool.',
        anthropicExample: 'Codex / Anthropic Messages', accountPool: 'Account pool', protocol: 'Protocol',
        anthropicBase: 'For Anthropic SDKs / Claude Code, use the /codex Base URL above, not /codex/v1: the client appends /v1/messages. The existing Google POST /v1/messages remains independent and does not route to Codex.',
        anthropicSupport: 'Supports text, images, system, custom tool definitions and choice, multi-turn tool_use / tool_result round trips, non-streaming responses, and incremental SSE (tool arguments, search results, URL citations, usage, and stop_reason). web_search_20250305 maps to native Codex web_search, not simulated search in a prompt. Other unsupported server tools, search constraints, or options (such as temperature, top_p, top_k, assistant prefill, or Claude-signed reasoning replay) return Anthropic-shaped errors.',
        anthropicTokens: 'POST /codex/v1/messages/count_tokens currently returns an Anthropic-shaped HTTP 501 unsupported error, not an estimate or fabricated exact count. Client features requiring preflight token counting may not work. Actual response usage is separate.',
        anthropicBudget: 'Important limit: max_tokens must be a positive integer, but is accepted only for client compatibility and cannot enforce an output length limit. The ChatGPT subscription upstream rejects max_output_tokens; its backend controls the actual output budget. The example’s 1024 is not a hard cap.',
        anthropicSearch: 'Search limit: max_uses becomes instruction guidance preserving your requested number, not a hard call or billing cap. The subscription backend rejects max_tool_calls; the x-codex-compatibility response header discloses web_search_max_uses=advisory. Do not rely on max_uses: 8 to guarantee at most eight searches.',
        anthropicCitations: 'Search results and citations preserve upstream URLs and available titles. Source excerpts may be unavailable; empty quote text is not a verbatim quotation. Opaque search/citation handles are bound to the gateway, key, and issuer account, not Anthropic-encrypted text. Replay the entire assistant content unchanged, including search results and citations; unknown, expired, or foreign handles are rejected. After expiry or restart, restate the task as text without old search blocks or citation handles.',
        anthropicSchema: 'output_config.format: {type: "json_schema", schema: ...} maps to native strict JSON Schema in text.format, not a prompt hint. The original schema is forwarded unchanged and must satisfy the upstream strict-mode Schema subset. Incompatible schemas fail explicitly rather than falling back to prompt-only generation.',
        anthropicExamples: 'Optional · Search and JSON Schema request fields',
        anthropicExamplesHint: 'Merge these fields into the cURL JSON body above and change messages to your task; retain the selected native model, max_tokens, and stream. Use either example or combine them, placing effort and format in a single output_config. auto lets the model decide whether to search; the schema example requires an answer string.',
        anthropicSearchExample: 'JSON · Native search (advisory limit)', anthropicSchemaExample: 'JSON · Native strict schema',
        anthropicThinking: 'enabled / adaptive thinking maps to Codex reasoning settings; budget_tokens is an advisory effort tier, not an exact budget. output_config.effort low / medium / high / max maps to low / medium / high / xhigh. No Claude-signed thinking blocks or reasoning summaries are produced. Hidden reasoning needed for continuation stays on the server.',
        anthropicSessions: 'New requests and requests carrying their complete text context support automatic quota failover. Gateway-issued tool IDs remain bound to the original account and access scope. Return the exact matching ID in tool_result; do not reuse it across accounts or gateway keys. These tool continuations fail explicitly while the original account is cooling down, rather than replaying elsewhere. Tool continuation state is in memory and expires after 24 idle hours. After a server restart or state expiry, start a new conversation and restate the task as text without old tool_use / tool_result blocks.',
        claudeTitle: '04 · Connect Claude Code', claudeHelp: 'Run this configuration in the same Bash terminal where API_MANAGER_KEY is set. All three default model aliases and subagents use the selected native Codex model so they do not automatically request claude-* IDs. Do not override them with Claude models via /model or project settings. Remove conflicts with existing cloud-provider or authentication configuration first.',
        claudeLaunch: 'This references your local gateway key, not an Anthropic API key or Claude subscription. The commands set environment variables in this terminal and launch claude. Never commit these variables or share terminal output containing secrets. Run unset ANTHROPIC_API_KEY API_MANAGER_KEY when finished.',
        catalog: 'Model catalog', inference: 'Inference', copy: 'Copy', copied: 'Copied', copyFailed: 'Copy failed. Please select and copy the text manually.',
        setupTitle: '02 · Prepare your gateway key locally', setupHint: 'These are Bash commands. Paste your gateway key at the hidden prompt instead of placing the plaintext key in shell history. Do not enter the administrator password. Run only in a trusted local terminal with shell tracing (set -x) disabled.',
        setupFooter: 'The variable is available to this terminal and its child processes only. Never commit it or share it in screenshots; run unset API_MANAGER_KEY when finished.',
        examplesTitle: '03 · Query the catalog, then make a request', examplesHint: 'Examples are not executed automatically. Run the selected catalog query first, then replace the model placeholder with an actual returned ID. Both Codex protocols and client configurations follow the model selected from the live catalog above.',
        googleExample: 'Google / OpenAI compatible', geminiExample: 'Native Gemini', codexExample: 'Codex / Responses',
        modelHint: 'The compatible API uses data[].id from /v1/models. Native Gemini uses models[].name from /v1beta/models; remove the existing models/ prefix when inserting it into the example URL. Do not assume different endpoints share model IDs.',
        codexModelHint: 'Both Codex protocols use data[].id from /codex/v1/models. Load and select a live model above (for example, use gpt-5.6-sol only if the catalog actually returns it), never a Claude or Gemini alias. This one-turn example starts a new conversation without previous session identifiers.',
        configTitle: '04 · Connect Codex CLI', configHint: 'Add and enable an account on the Codex accounts page, then load the real upstream model catalog. Select a returned native ID only: no assumed models or Gemini mapping names.',
        loadModels: 'Load model catalog', refreshModels: 'Refresh model catalog', loading: 'Loading the live model catalog…', modelLabel: 'Native Codex model', chooseModel: 'Select a model from the catalog',
        notLoaded: 'The catalog has not been loaded. Load it and select a model to copy a complete configuration.', noModels: 'The catalog returned no usable models. Check that a Codex account is authorized and enabled, then retry.',
        modelsError: 'Could not load the catalog. Check your administrator session, Codex account authorization, and upstream connectivity, then retry.', invalidCatalog: 'Invalid model catalog',
        configHelp: 'Merge these settings into your user-level ~/.codex/config.toml. Put model and model_provider at the top level, before any [table]. Replace existing matching fields or provider tables rather than appending duplicates. If CODEX_HOME is set, use config.toml in that directory. Preserve unrelated settings.',
        transport: 'Transport is HTTP SSE: wire_api = "responses", supports_websockets = false. requires_openai_auth = false prevents the client from using OpenAI login credentials; the gateway still checks API_MANAGER_KEY according to its authentication policy. It does not mean authentication is disabled.',
        launch: 'Run codex in the same terminal where API_MANAGER_KEY is set. env_key names the environment variable, not the secret itself. Never put the real key in TOML.',
        protocolTitle: 'Failover, sessions and context',
        failoverTitle: 'Automatic account failover on quota exhaustion',
        failover: 'Responses and Anthropic Messages share automatic failover. The preferred account is selected when enabled, authorized and not cooling down; otherwise another eligible account is used. An upstream 429 temporarily skips that account and retries safely portable requests with other eligible accounts without cycling back. A 401 still permits one authorization-refresh retry on the same account. Your preferred setting does not change with the account actually serving a request. Cooldown uses upstream reset times or Retry-After, with a bounded waiting period when unknown; expiry is not a guarantee of restored quota. Accounts stay enabled and become eligible again automatically at expiry.',
        failoverLimits: 'When all eligible accounts are cooling down, the gateway returns 429 with Retry-After for the earliest retry window. No enabled accounts returns 503. Network errors, upstream 5xx responses and streams already delivering output to the client are not replayed across accounts.',
        compactTitle: 'Native compaction and the compatibility adapter', compact: 'The current native Codex protocol triggers compaction with {"type":"compaction_trigger"} in the input to /codex/v1/responses. /codex/v1/responses/compact is a gateway adapter: it appends that native trigger, collects the upstream response, and returns JSON with object set to response.compaction. It is not a separate upstream /compact service or the Google-compatible compaction path.',
        compactNote: 'Let the client manage compacted context. Do not copy encrypted reasoning or compaction items from another account or an old session into a new conversation.',
        affinityTitle: 'Portable text versus account-bound state', affinity: 'Ordinary text requests that include the complete text context needed to continue can safely rebind to another eligible account after quota exhaustion or rate limiting, even with an existing session_id or prompt_cache_key. There is no need to manually start a new chat. This does not move old response IDs or tool handles. previous_response_id, x-codex-turn-state, encrypted reasoning / compaction items and gateway tool continuation state stay bound to the original account. These requests fail explicitly while that account is cooling down, rather than sending private state to another account.',
        restart: 'Account affinity and tool continuation state are held in server memory. After a restart or expiry, old account-bound state cannot be reliably resumed. For unknown previous_response_id, missing bound state or expired tool handles, start a fresh conversation with a new session ID and restate the original task as text. Do not reuse old response IDs, turn-state, encrypted context or old tool_use / tool_result blocks. Ordinary complete-text quota failover does not require this step.',
    },
};

const panel = 'console-panel min-w-0';
const button = 'console-button text-xs disabled:cursor-not-allowed disabled:opacity-40';
const paragraph = 'text-sm leading-6 text-gray-600 dark:text-gray-400';
const shellQuote = (value: string) => `'${value.replace(/'/g, `'"'"'`)}'`;
type CatalogModel = { id: string; name: string };
type Example = 'google' | 'gemini' | 'codex' | 'anthropic';
type Client = 'curl' | 'codex' | 'claude';

const anthropicSearchFields = `{
  "tools": [{"type": "web_search_20250305", "name": "web_search", "max_uses": 8}],
  "tool_choice": {"type": "auto"},
  "output_config": {"effort": "high"}
}`;
const anthropicSchemaFields = `{
  "output_config": {
    "format": {
      "type": "json_schema",
      "schema": {
        "type": "object",
        "properties": {"answer": {"type": "string"}},
        "required": ["answer"],
        "additionalProperties": false
      }
    }
  }
}`;

function CodeBlock({ title, code, copyLabel, copiedLabel, disabled = false, onCopy }: {
    title: string;
    code: string;
    copyLabel: string;
    copiedLabel: string;
    disabled?: boolean;
    onCopy: (text: string) => Promise<boolean>;
}) {
    const [copied, setCopied] = useState(false);
    const timer = useRef<number | undefined>(undefined);
    useEffect(() => {
        setCopied(false);
        return () => window.clearTimeout(timer.current);
    }, [code]);
    return (
        <div className="min-w-0 overflow-hidden rounded-xl border border-gray-200 dark:border-base-300">
            <div className="flex flex-wrap items-center justify-between gap-2 bg-gray-50 px-4 py-2 dark:bg-base-200">
                <span className="break-all font-mono text-xs text-gray-600 dark:text-gray-400">{title}</span>
                <button type="button" className={button} disabled={disabled} aria-label={`${copyLabel} ${title}`} onClick={async () => {
                    if (await onCopy(code)) {
                        setCopied(true);
                        window.clearTimeout(timer.current);
                        timer.current = window.setTimeout(() => setCopied(false), 2000);
                    }
                }}>{copied ? <Check size={14} /> : <Copy size={14} />}{copied ? copiedLabel : copyLabel}</button>
            </div>
            <pre tabIndex={0} className="overflow-x-auto bg-gray-950 p-4 text-xs leading-6 text-gray-100 sm:p-5"><code>{code}</code></pre>
        </div>
    );
}

export default function ApiGuide() {
    const { t, i18n } = useTranslation();
    const [proxyPort, setProxyPort] = useState(8045);
    useEffect(() => {
        if (!isTauri()) return;
        void request<{ port: number }>('get_proxy_status').then(status => {
            if (status.port > 0) setProxyPort(status.port);
        }).catch(() => {});
    }, []);
    const text = content[(i18n.resolvedLanguage || i18n.language).toLowerCase().startsWith('zh') ? 'zh' : 'en'];
    const origin = getProxyBaseUrl(proxyPort);
    const insecure = window.location.protocol === 'http:' && !['localhost', '127.0.0.1', '[::1]'].includes(window.location.hostname) && !/^127\.\d+\.\d+\.\d+$/.test(window.location.hostname);
    const [example, setExample] = useState<Example>('codex');
    const [client, setClient] = useState<Client>('curl');
    const isCodex = example === 'codex' || example === 'anthropic';
    const isAnthropic = example === 'anthropic';
    const clientTitle = isAnthropic ? text.claudeTitle : text.configTitle;
    const isChinese = i18n.language.startsWith('zh');
    const [models, setModels] = useState<CatalogModel[]>([]);
    const [model, setModel] = useState('');
    const [loading, setLoading] = useState(false);
    const [loaded, setLoaded] = useState(false);
    const [modelsError, setModelsError] = useState(false);
    const controller = useRef<AbortController | null>(null);
    useEffect(() => () => controller.current?.abort(), []);

    const loadModels = async () => {
        controller.current?.abort();
        const pending = new AbortController();
        controller.current = pending;
        setLoading(true);
        setModelsError(false);
        setModels([]);
        setModel('');
        try {
            const catalog = await request<unknown>('codex_models', undefined, { signal: pending.signal });
            if (pending.signal.aborted) return;
            if (!catalog || typeof catalog !== 'object' || !('models' in catalog) || !Array.isArray(catalog.models)) throw new Error(text.invalidCatalog);
            const next: CatalogModel[] = [];
            for (const entry of catalog.models) {
                if (!entry || typeof entry !== 'object') continue;
                const id = 'slug' in entry ? entry.slug : 'id' in entry ? entry.id : undefined;
                if (typeof id !== 'string' || !id || next.some(item => item.id === id)) continue;
                next.push({ id, name: 'display_name' in entry && typeof entry.display_name === 'string' ? entry.display_name : id });
            }
            setModels(next);
            setLoaded(true);
        } catch {
            if (!pending.signal.aborted) setModelsError(true);
        } finally {
            if (!pending.signal.aborted) setLoading(false);
        }
    };
    const copy = async (value: string) => {
        const success = await copyToClipboard(value);
        showToast(success ? text.copied : text.copyFailed, success ? 'success' : 'error');
        return success;
    };
    const copyProps = { copyLabel: text.copy, copiedLabel: text.copied, onCopy: copy };
    const selectedModel = models.find(item => item.id === model)?.id;
    const codexModel = selectedModel || '<CODEX_MODEL_ID_FROM_CATALOG>';
    const setup = `read -r -s -p 'Gateway API key: ' API_MANAGER_KEY; printf '\\n'\nexport API_MANAGER_KEY`;
    const endpoints = [
        { title: text.google, description: text.googleHint, base: '/v1', catalog: '/v1/models', inference: '/v1/chat/completions', accent: 'bg-blue-50 text-blue-700 dark:bg-blue-950/40 dark:text-blue-300' },
        { title: text.gemini, description: text.geminiHint, base: '/v1beta', catalog: '/v1beta/models', inference: '/v1beta/models/{model}:generateContent', accent: 'bg-violet-50 text-violet-700 dark:bg-violet-950/40 dark:text-violet-300' },
        { title: text.codex, description: text.codexHint, base: '/codex/v1', catalog: '/codex/v1/models', inference: '/codex/v1/responses', accent: 'bg-emerald-50 text-emerald-700 dark:bg-emerald-950/40 dark:text-emerald-300' },
        { title: text.anthropic, description: text.anthropicHint, base: '/codex', catalog: '/codex/v1/models', inference: '/codex/v1/messages', accent: 'bg-emerald-50 text-emerald-700 dark:bg-emerald-950/40 dark:text-emerald-300' },
    ];
    const endpoint = endpoints[example === 'google' ? 0 : example === 'gemini' ? 1 : isAnthropic ? 3 : 2];
    const catalogCurl = `curl --fail-with-body ${shellQuote(origin + endpoint.catalog)} \\\n  -H "Authorization: Bearer \${API_MANAGER_KEY:?Set API_MANAGER_KEY first}"`;
    const body = example === 'google'
        ? { model: '<MODEL_ID_FROM_V1_CATALOG>', messages: [{ role: 'user', content: 'Hello' }], stream: true }
        : example === 'gemini'
            ? { contents: [{ role: 'user', parts: [{ text: 'Hello' }] }] }
            : isAnthropic
                ? { model: codexModel, max_tokens: 1024, messages: [{ role: 'user', content: 'Hello' }], stream: true }
                : { model: codexModel, instructions: 'Be helpful and concise.', input: [{ role: 'user', content: 'Hello' }], stream: true, store: false };
    const inferencePath = example === 'gemini' ? '/v1beta/models/<MODEL_ID_WITHOUT_MODELS_PREFIX>:generateContent' : endpoint.inference;
    const inferenceAuth = isAnthropic
        ? '  -H "x-api-key: ${API_MANAGER_KEY:?Set API_MANAGER_KEY first}" \\\n  -H \'anthropic-version: 2023-06-01\''
        : '  -H "Authorization: Bearer ${API_MANAGER_KEY:?Set API_MANAGER_KEY first}"';
    const inferenceCurl = `curl --fail-with-body${example === 'gemini' ? '' : ' --no-buffer'} ${shellQuote(origin + inferencePath)} \\\n${inferenceAuth} \\\n  -H 'Content-Type: application/json' \\\n  --data-raw ${shellQuote(JSON.stringify(body, null, 2))}`;
    const config = `model = ${JSON.stringify(codexModel)}\nmodel_provider = "api_manager_codex"\n\n[model_providers.api_manager_codex]\nname = "API Manager Codex"\nbase_url = ${JSON.stringify(`${origin}/codex/v1`)}\nenv_key = "API_MANAGER_KEY"\nwire_api = "responses"\nsupports_websockets = false\nrequires_openai_auth = false`;
    const claudeConfig = `export ANTHROPIC_BASE_URL=${shellQuote(`${origin}/codex`)}\nexport ANTHROPIC_API_KEY="\${API_MANAGER_KEY:?Set API_MANAGER_KEY first}"\nexport ANTHROPIC_MODEL=${shellQuote(codexModel)}\nexport ANTHROPIC_DEFAULT_HAIKU_MODEL="$ANTHROPIC_MODEL"\nexport ANTHROPIC_DEFAULT_SONNET_MODEL="$ANTHROPIC_MODEL"\nexport ANTHROPIC_DEFAULT_OPUS_MODEL="$ANTHROPIC_MODEL"\nexport CLAUDE_CODE_SUBAGENT_MODEL="$ANTHROPIC_MODEL"\nclaude`;

    return (
        <div className="console-page console-page-scroll h-full space-y-5">
            <PageHeader
                title={text.title}
                description={text.subtitle}
                actions={<span className="inline-flex max-w-full items-center gap-2 rounded-lg border border-gray-200 px-3 py-2 text-xs dark:border-base-300"><ShieldCheck size={14} className="shrink-0 text-emerald-500" /><span className="truncate" title={origin}>{text.badge} · {origin}</span></span>}
            />

            {insecure && <aside role="alert" className="flex items-start gap-3 rounded-xl border border-amber-300 bg-amber-50 p-4 text-sm font-medium text-amber-950 dark:border-amber-800 dark:bg-amber-950/20 dark:text-amber-200"><AlertTriangle size={18} className="mt-0.5 shrink-0" />{text.insecure}</aside>}

            <nav className="console-tabs flex flex-wrap gap-1" aria-label={text.title}>
                <a className="console-tab" href="#guide-endpoints">{text.endpointsTitle}</a>
                <a className="console-tab" href="#guide-setup">{text.setupTitle}</a>
                <a className="console-tab" href={client !== 'curl' ? '#guide-codex' : '#guide-examples'}>{client !== 'curl' ? clientTitle : text.examplesTitle}</a>
                {isCodex && <a className="console-tab" href="#guide-sessions">{text.protocolTitle}</a>}
            </nav>

            <section className={`${panel} space-y-5 scroll-mt-4`} aria-labelledby="guide-endpoints">
                <div><h2 id="guide-endpoints" className="text-lg font-semibold">{text.endpointsTitle}</h2><p className={`${paragraph} mt-2`}>{text.endpointsHint}</p></div>
                <div className="grid gap-4 md:grid-cols-3">
                    <fieldset className="min-w-0">
                        <legend className="mb-2 text-sm font-medium">{t('console.guide_provider', { defaultValue: isChinese ? '账号池' : 'Account pool' })}</legend>
                        <div className="console-tabs flex flex-wrap gap-1">
                            <button type="button" className={`console-tab ${!isCodex ? 'active' : ''}`} aria-pressed={!isCodex} onClick={() => { setExample('google'); setClient('curl'); }}>Google</button>
                            <button type="button" className={`console-tab ${isCodex ? 'active' : ''}`} aria-pressed={isCodex} onClick={() => { if (!isCodex) { setExample('codex'); setClient('curl'); } }}>Codex</button>
                        </div>
                    </fieldset>
                    <label className="block min-w-0 text-sm font-medium">
                        <span className="mb-2 block">{t('console.guide_protocol', { defaultValue: isChinese ? '接口协议' : 'Protocol' })}</span>
                        <select className="w-full rounded-lg border border-gray-200 bg-gray-50 px-3 py-2.5 text-sm dark:border-base-300 dark:bg-base-200" value={example} onChange={event => { setExample(event.target.value as Example); setClient('curl'); }}>
                            {isCodex ? <><option value="codex">{text.codexExample}</option><option value="anthropic">{text.anthropicExample}</option></> : <><option value="google">{text.googleExample}</option><option value="gemini">{text.geminiExample}</option></>}
                        </select>
                    </label>
                    <label className="block min-w-0 text-sm font-medium">
                        <span className="mb-2 block">{t('console.guide_client', { defaultValue: isChinese ? '接入方式' : 'Client' })}</span>
                        <select className="w-full rounded-lg border border-gray-200 bg-gray-50 px-3 py-2.5 text-sm dark:border-base-300 dark:bg-base-200" value={client} onChange={event => setClient(event.target.value as Client)}>
                            <option value="curl">cURL / HTTP</option>
                            {example === 'codex' && <option value="codex">Codex CLI</option>}
                            {isAnthropic && <option value="claude">Claude Code</option>}
                        </select>
                    </label>
                </div>
                <article className="grid min-w-0 gap-5 rounded-xl border border-gray-200 bg-gray-50 p-4 dark:border-base-300 dark:bg-base-200 lg:grid-cols-2">
                    <div className="min-w-0"><span className={`inline-block rounded-lg px-2.5 py-1 font-mono text-xs font-semibold ${endpoint.accent}`}>{endpoint.base}</span><h3 className="mt-3 font-semibold">{endpoint.title}</h3><p className={`${paragraph} mt-2`}>{endpoint.description}</p></div>
                    <div className="min-w-0">
                        <div className="flex items-center gap-2 rounded-lg border border-gray-200 bg-white p-2 dark:border-base-300 dark:bg-base-100"><code className="min-w-0 flex-1 break-all text-xs leading-5">{origin}{endpoint.base}</code><button type="button" className={`${button} shrink-0 !p-2`} aria-label={`${text.copy} ${endpoint.title} Base URL`} onClick={() => void copy(origin + endpoint.base)}><Copy size={14} /></button></div>
                        <dl className="mt-3 space-y-2 text-xs"><div><dt className="text-gray-500 dark:text-gray-400">{text.accountPool} · {text.protocol}</dt><dd className="mt-1">{isCodex ? 'Codex' : 'Google'} · {isAnthropic ? 'Anthropic Messages' : example === 'codex' ? 'OpenAI Responses' : example === 'gemini' ? 'Gemini generateContent' : 'OpenAI Chat Completions'}</dd></div><div><dt className="text-gray-500 dark:text-gray-400">{text.catalog}</dt><dd className="mt-1 break-all font-mono">GET {endpoint.catalog}</dd></div><div><dt className="text-gray-500 dark:text-gray-400">{text.inference}</dt><dd className="mt-1 break-all font-mono">POST {endpoint.inference}</dd></div></dl>
                    </div>
                </article>
                {isAnthropic && <div className="space-y-3">
                    <p className="break-words rounded-xl border border-amber-200 bg-amber-50 p-4 text-sm leading-6 text-amber-950 dark:border-amber-800 dark:bg-amber-950/20 dark:text-amber-200">{text.anthropicBudget}</p>
                    <p className="break-words rounded-xl border border-amber-200 bg-amber-50 p-4 text-sm leading-6 text-amber-950 dark:border-amber-800 dark:bg-amber-950/20 dark:text-amber-200">{text.anthropicSearch}</p>
                    <div className="space-y-3 break-words rounded-xl border border-blue-100 bg-blue-50 p-4 text-sm leading-6 text-blue-900 dark:border-blue-900 dark:bg-blue-950/20 dark:text-blue-200"><p>{text.anthropicBase}</p><p>{text.anthropicSupport}</p><p>{text.anthropicCitations}</p><p>{text.anthropicSchema}</p><p>{text.anthropicTokens}</p><p>{text.anthropicThinking}</p><p>{text.anthropicSessions}</p></div>
                </div>}
                {isCodex && <div className="space-y-3">
                    <p className={paragraph}>{text.configHint}</p>
                    <div className="flex flex-wrap items-end gap-3">
                        <label className="block min-w-0 flex-1 text-sm sm:min-w-64"><span className="mb-2 block font-medium">{text.modelLabel}</span><select value={model} disabled={loading || models.length === 0} onChange={event => setModel(event.target.value)} className="w-full rounded-lg border border-gray-300 bg-white px-3 py-2.5 text-sm focus:outline-none focus:ring-2 focus:ring-blue-500 disabled:opacity-50 dark:border-base-300 dark:bg-base-200"><option value="">{text.chooseModel}</option>{models.map(item => <option key={item.id} value={item.id}>{item.name === item.id ? item.id : `${item.name} · ${item.id}`}</option>)}</select></label>
                        <button type="button" className={`${button} !py-2.5`} disabled={loading} onClick={() => void loadModels()}><RefreshCw size={15} className={loading ? 'animate-spin' : ''} />{loaded ? text.refreshModels : text.loadModels}</button>
                    </div>
                    {loading ? <p role="status" className={paragraph}>{text.loading}</p> : modelsError ? <p role="alert" className="text-sm leading-6 text-red-600 dark:text-red-400">{text.modelsError}</p> : models.length === 0 ? <p className={paragraph}>{loaded ? text.noModels : text.notLoaded}</p> : null}
                </div>}
            </section>

            <section className={`${panel} space-y-4 scroll-mt-4`} aria-labelledby="guide-setup">
                <h2 id="guide-setup" className="text-lg font-semibold">{text.setupTitle}</h2>
                <aside className="rounded-xl border border-amber-200 bg-amber-50 p-4 text-amber-950 dark:border-amber-800 dark:bg-amber-950/20 dark:text-amber-200">
                    <h3 className="flex items-center gap-2 text-sm font-semibold"><ShieldCheck size={18} className="shrink-0" />{text.securityTitle}</h3>
                    <p className="mt-2 text-sm leading-6">{text.security}</p>
                </aside>
                <details className="rounded-xl border border-gray-200 p-4 dark:border-base-300">
                    <summary className="cursor-pointer text-sm font-semibold"><KeyRound size={16} className="mr-2 inline text-blue-500" />{text.credentialsTitle}</summary>
                    <div className="mt-4 grid gap-4 md:grid-cols-2"><div><h3 className="mb-2 text-sm font-medium">{text.adminTitle}</h3><p className={paragraph}>{text.admin}</p></div><div><h3 className="mb-2 text-sm font-medium">{text.gatewayTitle}</h3><p className={paragraph}>{text.gateway}</p></div></div>
                </details>
                <p className={paragraph}>{text.setupHint}</p>
                <CodeBlock title="Bash · API_MANAGER_KEY" code={setup} {...copyProps} />
                <p className="text-xs leading-5 text-gray-500 dark:text-gray-400">{text.setupFooter}</p>
            </section>

            {client === 'curl' ? (
                <section className={`${panel} space-y-4 scroll-mt-4`} aria-labelledby="guide-examples">
                    <h2 id="guide-examples" className="text-lg font-semibold">{text.examplesTitle}</h2><p className={paragraph}>{text.examplesHint}</p>
                    <CodeBlock title={`GET ${endpoint.catalog}`} code={catalogCurl} {...copyProps} />
                    <p className={`${paragraph} flex items-start gap-2`}><ArrowRight size={17} className="mt-1 shrink-0 text-blue-500" /><span>{isCodex ? text.codexModelHint : text.modelHint}</span></p>
                    {isCodex && <button type="button" className={button} onClick={() => setClient(isAnthropic ? 'claude' : 'codex')}><Terminal size={15} />{clientTitle}</button>}
                    <CodeBlock title={`POST ${endpoint.inference}`} code={inferenceCurl} disabled={isCodex && !selectedModel} {...copyProps} />
                    {isAnthropic && <details className="rounded-xl border border-gray-200 p-4 dark:border-base-300">
                        <summary className="cursor-pointer text-sm font-semibold">{text.anthropicExamples}</summary>
                        <div className="mt-4 space-y-4">
                            <p className={paragraph}>{text.anthropicExamplesHint}</p>
                            <CodeBlock title={text.anthropicSearchExample} code={anthropicSearchFields} {...copyProps} />
                            <CodeBlock title={text.anthropicSchemaExample} code={anthropicSchemaFields} {...copyProps} />
                        </div>
                    </details>}
                </section>
            ) : (
                <section className={`${panel} space-y-4 scroll-mt-4`} aria-labelledby="guide-codex">
                    <h2 id="guide-codex" className="flex items-center gap-2 text-lg font-semibold"><Terminal size={20} className="text-emerald-500" />{clientTitle}</h2>
                    <p className={paragraph}>{isAnthropic ? text.claudeHelp : text.configHelp}</p>
                    <CodeBlock title={isAnthropic ? 'Bash · Claude Code' : '~/.codex/config.toml'} code={isAnthropic ? claudeConfig : config} disabled={!selectedModel} {...copyProps} />
                    {!isAnthropic && <div className="rounded-xl border border-blue-100 bg-blue-50 p-4 text-sm leading-6 text-blue-900 dark:border-blue-900 dark:bg-blue-950/20 dark:text-blue-200">{text.transport}</div>}
                    <p className={paragraph}>{isAnthropic ? text.claudeLaunch : text.launch}</p>
                    <button type="button" className={button} onClick={() => setClient('curl')}><ArrowRight size={15} />{text.examplesTitle}</button>
                </section>
            )}

            {isCodex && <section className={`${panel} scroll-mt-4`} aria-labelledby="guide-sessions">
                <h2 id="guide-sessions" className="flex items-center gap-2 text-lg font-semibold"><Workflow size={20} className="text-violet-500" />{text.protocolTitle}</h2>
                <div className="mt-4 space-y-3">
                    <h3 className="text-sm font-semibold">{text.failoverTitle}</h3>
                    <p className={paragraph}>{text.failover}</p>
                    <p className={paragraph}>{text.failoverLimits}</p>
                    {example === 'codex' && <details className="rounded-xl border border-gray-200 p-4 dark:border-base-300"><summary className="cursor-pointer text-sm font-semibold">{text.compactTitle}</summary><p className={`${paragraph} mt-3 break-words`}>{text.compact}</p><p className={`${paragraph} mt-3`}>{text.compactNote}</p></details>}
                    <details className="rounded-xl border border-gray-200 p-4 dark:border-base-300"><summary className="cursor-pointer text-sm font-semibold">{text.affinityTitle}</summary><p className={`${paragraph} mt-3`}>{text.affinity}</p><p className={`${paragraph} mt-3`}>{text.restart}</p></details>
                </div>
            </section>}
        </div>
    );
}
