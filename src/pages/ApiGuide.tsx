import { useEffect, useRef, useState } from 'react';
import { useTranslation } from 'react-i18next';
import { AlertTriangle, ArrowRight, BookOpen, Check, Copy, KeyRound, RefreshCw, ShieldCheck, Terminal, Workflow } from 'lucide-react';
import { showToast } from '../components/common/ToastContainer';
import { copyToClipboard } from '../utils/clipboard';
import { request } from '../utils/request';

const content = {
    zh: {
        title: 'API 使用说明', subtitle: '从选择入口到连接客户端，在一个页面完成接入。', badge: '当前服务器',
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
        catalog: '模型目录', inference: '推理', copy: '复制', copied: '已复制', copyFailed: '复制失败，请手动选择并复制。',
        setupTitle: '02 · 在本地准备网关 Key', setupHint: '以下为 Bash 命令。运行后在隐藏输入提示中粘贴网关 Key，避免将明文 Key 写入命令历史。不要粘贴管理员密码。只在可信的本机终端执行，不要开启 shell 调试（set -x）。',
        setupFooter: '环境变量仅供当前终端及其子进程使用。不要提交到代码仓库或截图分享；使用完毕可执行 unset API_MANAGER_KEY。',
        examplesTitle: '03 · 查询目录，再发起请求', examplesHint: '示例不会自动执行。先运行所选入口的目录查询，将模型占位符替换成实际返回的 ID。Codex 示例会同步使用下方目录选择器中的模型。',
        googleExample: 'Google / OpenAI 兼容', geminiExample: 'Gemini 原生', codexExample: 'Codex / Responses',
        modelHint: '兼容接口使用 /v1/models 返回的 data[].id；Gemini 原生使用 /v1beta/models 返回的 models[].name，并在 URL 中去掉已有的 models/ 前缀。不要假设不同入口共享模型 ID。',
        codexModelHint: 'Codex 使用 /codex/v1/models 返回的 data[].id；也可在下方加载真实目录后选择。示例中的单次问答是新会话，不含任何历史会话标识。',
        configTitle: '04 · 连接 Codex CLI', configHint: '先在 Codex 账号页添加并启用账号，再加载上游真实模型目录。只从返回的原生 ID 中选择；不预设模型、不使用 Gemini 映射名称。',
        loadModels: '加载模型目录', refreshModels: '刷新模型目录', loading: '正在加载真实模型目录…', modelLabel: 'Codex 原生模型', chooseModel: '请选择目录中的模型',
        notLoaded: '尚未加载模型目录。加载并选择后即可复制完整配置。', noModels: '目录未返回可用模型。请检查 Codex 账号是否已授权并启用，然后重试。',
        modelsError: '无法加载模型目录。请检查管理员登录状态、Codex 账号授权和上游连接后重试。', invalidCatalog: '模型目录格式无效',
        configHelp: '将以下设置合并到用户级 ~/.codex/config.toml。model 与 model_provider 必须放在文件顶层、任何 [表] 之前；已有同名字段或 provider 表应替换而非重复追加。若设置了 CODEX_HOME，请使用该目录下的 config.toml。不要覆盖其他无关配置。',
        transport: '传输为 HTTP SSE：wire_api = "responses"，supports_websockets = false。requires_openai_auth = false 表示客户端不使用 OpenAI 登录凭据，但网关仍按其鉴权策略检查 API_MANAGER_KEY；这不代表免鉴权。',
        launch: '在已设置 API_MANAGER_KEY 的同一个终端运行 codex。env_key 写的是环境变量名称，不是密钥本身；不要把真实 Key 写进 TOML。',
        protocolTitle: '会话与上下文压缩',
        compactTitle: '原生压缩与兼容适配', compact: '当前 Codex 原生协议通过 /codex/v1/responses 的 input 中的 {"type":"compaction_trigger"} 触发压缩。/codex/v1/responses/compact 是网关适配器：追加此原生触发项、收集上游响应，并返回 object 为 response.compaction 的 JSON。它不是独立的上游 /compact 服务，也不是 Google 兼容入口的压缩逻辑。',
        compactNote: '让客户端管理压缩上下文；不要将其他账号或旧会话的加密 reasoning / compaction 内容复制到新会话。',
        affinityTitle: '同一会话固定到同一账号', affinity: '网关根据会话标识、prompt_cache_key 和 previous_response_id 维护账号亲和性。会话中途不会静默切换账号；更改首选账号只影响新会话。固定账号被禁用、删除或不可用时，旧会话会报错。',
        restart: '亲和性保存在服务器内存中，重启或过期后旧会话不能可靠续接。遇到未知 previous_response_id 或丢失亲和性的错误，请开启全新对话、使用新会话 ID，并重新提供原始任务文本；不要携带旧响应 ID、turn-state 或加密上下文。',
    },
    en: {
        title: 'API usage guide', subtitle: 'Choose an endpoint, prepare your key, and connect your client.', badge: 'Current server',
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
        catalog: 'Model catalog', inference: 'Inference', copy: 'Copy', copied: 'Copied', copyFailed: 'Copy failed. Please select and copy the text manually.',
        setupTitle: '02 · Prepare your gateway key locally', setupHint: 'These are Bash commands. Paste your gateway key at the hidden prompt instead of placing the plaintext key in shell history. Do not enter the administrator password. Run only in a trusted local terminal with shell tracing (set -x) disabled.',
        setupFooter: 'The variable is available to this terminal and its child processes only. Never commit it or share it in screenshots; run unset API_MANAGER_KEY when finished.',
        examplesTitle: '03 · Query the catalog, then make a request', examplesHint: 'Examples are not executed automatically. Run the selected catalog query first, then replace the model placeholder with an actual returned ID. The Codex example also follows the live catalog selector below.',
        googleExample: 'Google / OpenAI compatible', geminiExample: 'Native Gemini', codexExample: 'Codex / Responses',
        modelHint: 'The compatible API uses data[].id from /v1/models. Native Gemini uses models[].name from /v1beta/models; remove the existing models/ prefix when inserting it into the example URL. Do not assume different endpoints share model IDs.',
        codexModelHint: 'Codex uses data[].id from /codex/v1/models, or a model selected from the live catalog below. This one-turn example starts a new conversation without previous session identifiers.',
        configTitle: '04 · Connect Codex CLI', configHint: 'Add and enable an account on the Codex accounts page, then load the real upstream model catalog. Select a returned native ID only: no assumed models or Gemini mapping names.',
        loadModels: 'Load model catalog', refreshModels: 'Refresh model catalog', loading: 'Loading the live model catalog…', modelLabel: 'Native Codex model', chooseModel: 'Select a model from the catalog',
        notLoaded: 'The catalog has not been loaded. Load it and select a model to copy a complete configuration.', noModels: 'The catalog returned no usable models. Check that a Codex account is authorized and enabled, then retry.',
        modelsError: 'Could not load the catalog. Check your administrator session, Codex account authorization, and upstream connectivity, then retry.', invalidCatalog: 'Invalid model catalog',
        configHelp: 'Merge these settings into your user-level ~/.codex/config.toml. Put model and model_provider at the top level, before any [table]. Replace existing matching fields or provider tables rather than appending duplicates. If CODEX_HOME is set, use config.toml in that directory. Preserve unrelated settings.',
        transport: 'Transport is HTTP SSE: wire_api = "responses", supports_websockets = false. requires_openai_auth = false prevents the client from using OpenAI login credentials; the gateway still checks API_MANAGER_KEY according to its authentication policy. It does not mean authentication is disabled.',
        launch: 'Run codex in the same terminal where API_MANAGER_KEY is set. env_key names the environment variable, not the secret itself. Never put the real key in TOML.',
        protocolTitle: 'Sessions and context compaction',
        compactTitle: 'Native compaction and the compatibility adapter', compact: 'The current native Codex protocol triggers compaction with {"type":"compaction_trigger"} in the input to /codex/v1/responses. /codex/v1/responses/compact is a gateway adapter: it appends that native trigger, collects the upstream response, and returns JSON with object set to response.compaction. It is not a separate upstream /compact service or the Google-compatible compaction path.',
        compactNote: 'Let the client manage compacted context. Do not copy encrypted reasoning or compaction items from another account or an old session into a new conversation.',
        affinityTitle: 'One conversation stays with one account', affinity: 'The gateway maintains account affinity using session identifiers, prompt_cache_key, and previous_response_id. It never silently switches an ongoing conversation to another account. Changing the preferred account affects new conversations; disabling, deleting, or losing a pinned account causes the old conversation to fail.',
        restart: 'Affinity is held in server memory. After a restart or expiry, an old conversation cannot be reliably resumed. For unknown previous_response_id or missing-affinity errors, start a fresh conversation with a new session ID and restate the original task as text. Do not reuse old response IDs, turn-state, or encrypted context.',
    },
};

const panel = 'rounded-2xl border border-gray-200 bg-white p-5 shadow-sm dark:border-base-300 dark:bg-base-100 sm:p-6';
const button = 'inline-flex items-center justify-center gap-2 rounded-lg border border-gray-200 px-3 py-2 text-xs font-medium text-gray-700 transition-colors hover:bg-gray-100 focus-visible:outline-none focus-visible:ring-2 focus-visible:ring-blue-500 disabled:cursor-not-allowed disabled:opacity-40 dark:border-base-300 dark:text-gray-200 dark:hover:bg-base-200';
const paragraph = 'text-sm leading-6 text-gray-600 dark:text-gray-400';
const shellQuote = (value: string) => `'${value.replace(/'/g, `'"'"'`)}'`;
type CatalogModel = { id: string; name: string };
type Example = 'google' | 'gemini' | 'codex';

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
    const { i18n } = useTranslation();
    const text = content[(i18n.resolvedLanguage || i18n.language).toLowerCase().startsWith('zh') ? 'zh' : 'en'];
    const origin = window.location.origin;
    const insecure = window.location.protocol === 'http:' && !['localhost', '127.0.0.1', '[::1]'].includes(window.location.hostname) && !/^127\.\d+\.\d+\.\d+$/.test(window.location.hostname);
    const [example, setExample] = useState<Example>('codex');
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
    ];
    const endpoint = endpoints[example === 'google' ? 0 : example === 'gemini' ? 1 : 2];
    const catalogCurl = `curl --fail-with-body ${shellQuote(origin + endpoint.catalog)} \\\n  -H "Authorization: Bearer \${API_MANAGER_KEY:?Set API_MANAGER_KEY first}"`;
    const body = example === 'google'
        ? { model: '<MODEL_ID_FROM_V1_CATALOG>', messages: [{ role: 'user', content: 'Hello' }], stream: true }
        : example === 'gemini'
            ? { contents: [{ role: 'user', parts: [{ text: 'Hello' }] }] }
            : { model: codexModel, instructions: 'Be helpful and concise.', input: [{ role: 'user', content: 'Hello' }], stream: true, store: false };
    const inferencePath = example === 'gemini' ? '/v1beta/models/<MODEL_ID_WITHOUT_MODELS_PREFIX>:generateContent' : endpoint.inference;
    const inferenceCurl = `curl --fail-with-body${example === 'gemini' ? '' : ' --no-buffer'} ${shellQuote(origin + inferencePath)} \\\n  -H "Authorization: Bearer \${API_MANAGER_KEY:?Set API_MANAGER_KEY first}" \\\n  -H 'Content-Type: application/json' \\\n  --data-raw ${shellQuote(JSON.stringify(body, null, 2))}`;
    const config = `model = ${JSON.stringify(codexModel)}\nmodel_provider = "api_manager_codex"\n\n[model_providers.api_manager_codex]\nname = "API Manager Codex"\nbase_url = ${JSON.stringify(`${origin}/codex/v1`)}\nenv_key = "API_MANAGER_KEY"\nwire_api = "responses"\nsupports_websockets = false\nrequires_openai_auth = false`;

    return (
        <div className="mx-auto h-full w-full max-w-7xl space-y-6 overflow-y-auto p-4 sm:p-6">
            <header className="flex flex-wrap items-start justify-between gap-4">
                <div><h1 className="flex items-center gap-3 text-2xl font-bold text-gray-900 dark:text-gray-100"><BookOpen className="shrink-0 text-blue-500" />{text.title}</h1><p className={`${paragraph} mt-2`}>{text.subtitle}</p></div>
                <span className="inline-flex max-w-full items-center gap-2 rounded-full border border-gray-200 bg-white px-3 py-2 text-xs dark:border-base-300 dark:bg-base-100"><ShieldCheck size={14} className="shrink-0 text-emerald-500" /><span className="truncate" title={origin}>{text.badge} · {origin}</span></span>
            </header>

            <aside className="rounded-2xl border border-amber-200 bg-amber-50 p-5 text-amber-950 dark:border-amber-800 dark:bg-amber-950/20 dark:text-amber-200">
                <h2 className="flex items-center gap-2 font-semibold"><ShieldCheck size={19} className="shrink-0" />{text.securityTitle}</h2>
                {insecure && <p role="alert" className="mt-3 flex items-start gap-2 text-sm font-semibold"><AlertTriangle size={17} className="mt-0.5 shrink-0" />{text.insecure}</p>}
                <p className="mt-2 text-sm leading-6">{text.security}</p>
            </aside>

            <section className={panel} aria-labelledby="guide-credentials">
                <h2 id="guide-credentials" className="flex items-center gap-2 text-lg font-semibold"><KeyRound size={20} className="text-blue-500" />{text.credentialsTitle}</h2>
                <div className="mt-4 grid gap-5 md:grid-cols-2">
                    <div><h3 className="mb-2 font-medium">{text.adminTitle}</h3><p className={paragraph}>{text.admin}</p></div>
                    <div className="border-t border-gray-100 pt-4 dark:border-base-300 md:border-l md:border-t-0 md:pl-5 md:pt-0"><h3 className="mb-2 font-medium">{text.gatewayTitle}</h3><p className={paragraph}>{text.gateway}</p></div>
                </div>
            </section>

            <section aria-labelledby="guide-endpoints">
                <h2 id="guide-endpoints" className="text-lg font-semibold">{text.endpointsTitle}</h2><p className={`${paragraph} mt-2`}>{text.endpointsHint}</p>
                <div className="mt-4 grid gap-4 xl:grid-cols-3">
                    {endpoints.map(item => <article key={item.base} className={`${panel} min-w-0 !p-5`}>
                        <span className={`inline-block rounded-lg px-2.5 py-1 font-mono text-xs font-semibold ${item.accent}`}>{item.base}</span>
                        <h3 className="mt-4 font-semibold">{item.title}</h3><p className={`${paragraph} mt-2`}>{item.description}</p>
                        <div className="mt-4 flex items-center gap-2 rounded-lg bg-gray-50 p-2 dark:bg-base-200"><code className="min-w-0 flex-1 break-all text-xs leading-5">{origin}{item.base}</code><button type="button" className={`${button} shrink-0 !p-2`} aria-label={`${text.copy} ${item.title} Base URL`} onClick={() => void copy(origin + item.base)}><Copy size={14} /></button></div>
                        <dl className="mt-4 space-y-2 text-xs"><div><dt className="text-gray-500 dark:text-gray-400">{text.catalog}</dt><dd className="mt-1 break-all font-mono">GET {item.catalog}</dd></div><div><dt className="text-gray-500 dark:text-gray-400">{text.inference}</dt><dd className="mt-1 break-all font-mono">POST {item.inference}</dd></div></dl>
                    </article>)}
                </div>
            </section>

            <section className={`${panel} space-y-4`} aria-labelledby="guide-setup">
                <h2 id="guide-setup" className="text-lg font-semibold">{text.setupTitle}</h2><p className={paragraph}>{text.setupHint}</p>
                <CodeBlock title="Bash · API_MANAGER_KEY" code={setup} {...copyProps} />
                <p className="text-xs leading-5 text-gray-500 dark:text-gray-400">{text.setupFooter}</p>
            </section>

            <section className={`${panel} space-y-4`} aria-labelledby="guide-examples">
                <h2 id="guide-examples" className="text-lg font-semibold">{text.examplesTitle}</h2><p className={paragraph}>{text.examplesHint}</p>
                <div className="flex flex-wrap gap-2" role="group" aria-label={text.endpointsTitle}>
                    {(['google', 'gemini', 'codex'] as const).map(value => <button key={value} type="button" aria-pressed={example === value} onClick={() => setExample(value)} className={`${button} ${example === value ? '!border-blue-500 !bg-blue-50 !text-blue-700 dark:!bg-blue-950/40 dark:!text-blue-300' : ''}`}>{text[`${value}Example`]}</button>)}
                </div>
                <CodeBlock title={`GET ${endpoint.catalog}`} code={catalogCurl} {...copyProps} />
                <p className={`${paragraph} flex items-start gap-2`}><ArrowRight size={17} className="mt-1 shrink-0 text-blue-500" /><span>{example === 'codex' ? text.codexModelHint : text.modelHint}</span></p>
                <CodeBlock title={`POST ${endpoint.inference}`} code={inferenceCurl} {...copyProps} />
            </section>

            <section className={`${panel} space-y-4`} aria-labelledby="guide-codex">
                <h2 id="guide-codex" className="flex items-center gap-2 text-lg font-semibold"><Terminal size={20} className="text-emerald-500" />{text.configTitle}</h2><p className={paragraph}>{text.configHint}</p>
                <div className="flex flex-wrap items-end gap-3">
                    <label className="block min-w-0 flex-1 text-sm sm:min-w-64"><span className="mb-2 block font-medium">{text.modelLabel}</span><select value={model} disabled={loading || models.length === 0} onChange={event => setModel(event.target.value)} className="w-full rounded-lg border border-gray-300 bg-white px-3 py-2.5 text-sm focus:outline-none focus:ring-2 focus:ring-blue-500 disabled:opacity-50 dark:border-base-300 dark:bg-base-200"><option value="">{text.chooseModel}</option>{models.map(item => <option key={item.id} value={item.id}>{item.name === item.id ? item.id : `${item.name} · ${item.id}`}</option>)}</select></label>
                    <button type="button" className={`${button} !py-2.5`} disabled={loading} onClick={() => void loadModels()}><RefreshCw size={15} className={loading ? 'animate-spin' : ''} />{loaded ? text.refreshModels : text.loadModels}</button>
                </div>
                {loading ? <p role="status" className={paragraph}>{text.loading}</p> : modelsError ? <p role="alert" className="text-sm leading-6 text-red-600 dark:text-red-400">{text.modelsError}</p> : models.length === 0 ? <p className={paragraph}>{loaded ? text.noModels : text.notLoaded}</p> : null}
                <p className={paragraph}>{text.configHelp}</p>
                <CodeBlock title="~/.codex/config.toml" code={config} disabled={!selectedModel} {...copyProps} />
                <div className="rounded-xl border border-blue-100 bg-blue-50 p-4 text-sm leading-6 text-blue-900 dark:border-blue-900 dark:bg-blue-950/20 dark:text-blue-200">{text.transport}</div>
                <p className={paragraph}>{text.launch}</p>
            </section>

            <section className={`${panel} space-y-5`} aria-labelledby="guide-sessions">
                <h2 id="guide-sessions" className="flex items-center gap-2 text-lg font-semibold"><Workflow size={20} className="text-violet-500" />{text.protocolTitle}</h2>
                <div className="grid gap-6 lg:grid-cols-2">
                    <article className="min-w-0"><h3 className="mb-2 font-semibold">{text.compactTitle}</h3><p className={`${paragraph} break-words`}>{text.compact}</p><p className={`${paragraph} mt-3`}>{text.compactNote}</p></article>
                    <article className="min-w-0"><h3 className="mb-2 font-semibold">{text.affinityTitle}</h3><p className={paragraph}>{text.affinity}</p><p className={`${paragraph} mt-3`}>{text.restart}</p></article>
                </div>
            </section>
        </div>
    );
}
