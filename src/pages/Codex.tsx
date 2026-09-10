import { useCallback, useEffect, useRef, useState } from 'react';
import { useTranslation } from 'react-i18next';
import { AlertTriangle, Check, Copy, ExternalLink, Plus, RefreshCw, Search, ShieldCheck, Terminal, Upload, Users } from 'lucide-react';
import { Link } from 'react-router-dom';
import { AccountPoolTabs, PageHeader } from '../components/common/ConsolePage';
import ModalDialog from '../components/common/ModalDialog';
import { showToast } from '../components/common/ToastContainer';
import { copyToClipboard } from '../utils/clipboard';
import { isTauri } from '../utils/env';
import { request } from '../utils/request';

type Account = {
    id: string;
    email: string | null;
    label: string;
    plan_type: string | null;
    enabled: boolean;
    expires_at: number | null;
    last_used_at: number | null;
    last_error: string | null;
    cooldown_until?: number | null;
    cooldown_reason?: string | null;
};
type AccountList = { accounts: Account[]; active_account_id: string | null };
type DeviceStatus = 'pending' | 'completed' | 'failed' | 'cancelled' | 'expired';
type DeviceSession = {
    id: string;
    verification_url: string;
    user_code: string;
    interval: number;
    expires_at: number;
    status: DeviceStatus;
    error?: string;
};
type RateLimitWindow = {
    usedPercent: number;
    remainingPercent: number;
    resetAt: number | null;
    windowSeconds: number;
};
type AccountQuota = {
    fiveHour: RateLimitWindow | null;
    weekly: RateLimitWindow | null;
};
type AccountQuotaState = {
    loading: boolean;
    data?: AccountQuota;
    error?: string;
};
type Model = { id: string; name: string };
const panel = 'console-panel';
const controls = '[&_.btn]:rounded-lg [&_.btn]:border [&_.btn]:border-gray-200 [&_.btn]:px-3 [&_.btn]:transition-colors [&_.btn:disabled]:opacity-40 [&_.btn:disabled]:cursor-not-allowed [&_.btn-primary]:bg-blue-600 [&_.btn-primary]:text-white [&_.btn-primary]:border-blue-600 [&_.input]:rounded-lg [&_.input]:border [&_.input]:border-gray-300 [&_.input]:px-3 [&_.file-input]:rounded-lg [&_.file-input]:border [&_.file-input]:border-gray-300 [&_.select]:rounded-lg [&_.select]:border [&_.select]:border-gray-300 [&_.select]:px-3 dark:[&_.btn]:border-gray-600 dark:[&_.input]:border-gray-600 dark:[&_.file-input]:border-gray-600 dark:[&_.select]:border-gray-600';
const errorMessage = (error: unknown) => error instanceof Error ? error.message : String(error);
const aborted = (error: unknown) => error instanceof DOMException && error.name === 'AbortError';
const objectValue = (value: unknown): Record<string, unknown> | null =>
    value !== null && typeof value === 'object' && !Array.isArray(value) ? value as Record<string, unknown> : null;

function rateLimitWindow(value: unknown): RateLimitWindow | null {
    const window = objectValue(value);
    if (!window || typeof window.used_percent !== 'number' || typeof window.limit_window_seconds !== 'number') return null;
    const usedPercent = Math.min(100, Math.max(0, window.used_percent));
    return {
        usedPercent,
        remainingPercent: 100 - usedPercent,
        resetAt: typeof window.reset_at === 'number' ? window.reset_at : null,
        windowSeconds: window.limit_window_seconds,
    };
}

function accountQuota(value: unknown): AccountQuota | null {
    const rateLimit = objectValue(objectValue(value)?.rate_limit);
    if (!rateLimit) return null;
    const windows = [
        rateLimitWindow(rateLimit.primary_window),
        rateLimitWindow(rateLimit.secondary_window),
    ].filter((window): window is RateLimitWindow => window !== null);
    const quota = {
        fiveHour: windows.find(window => window.windowSeconds === 5 * 60 * 60) ?? null,
        weekly: windows.find(window => window.windowSeconds === 7 * 24 * 60 * 60) ?? null,
    };
    return quota.fiveHour || quota.weekly ? quota : null;
}


function catalogModels(value: unknown): Model[] {
    if (!value || typeof value !== 'object' || !('models' in value) || !Array.isArray(value.models)) {
        throw new Error('Invalid model catalog response.');
    }
    const models: Model[] = [];
    for (const entry of value.models) {
        if (!entry || typeof entry !== 'object') continue;
        const id = 'slug' in entry ? entry.slug : 'id' in entry ? entry.id : undefined;
        if (typeof id !== 'string' || !id || models.some(model => model.id === id)) continue;
        const name = 'display_name' in entry && typeof entry.display_name === 'string' ? entry.display_name : id;
        models.push({ id, name });
    }
    return models;
}

export default function Codex() {
    const { t, i18n } = useTranslation();
    const desktop = isTauri();
    const [accounts, setAccounts] = useState<AccountList>({ accounts: [], active_account_id: null });
    const [accountsLoading, setAccountsLoading] = useState(!desktop);
    const [accountsError, setAccountsError] = useState('');
    const [models, setModels] = useState<Model[]>([]);
    const [modelsLoading, setModelsLoading] = useState(false);
    const [modelsError, setModelsError] = useState('');
    const [model, setModel] = useState('');
    const [label, setLabel] = useState('');
    const [fileSelected, setFileSelected] = useState(false);
    const fileInput = useRef<HTMLInputElement>(null);
    const [addOpen, setAddOpen] = useState(false);
    const addDialog = useRef<HTMLDialogElement>(null);
    const usageDialog = useRef<HTMLDialogElement>(null);
    const [search, setSearch] = useState('');
    const [importError, setImportError] = useState('');
    const [busy, setBusy] = useState('');
    const busyRef = useRef(false);
    const mounted = useRef(false);
    const controllers = useRef(new Set<AbortController>());
    const accountLoad = useRef<AbortController | null>(null);
    const modelLoad = useRef<AbortController | null>(null);
    const usageLoad = useRef<AbortController | null>(null);
    const quotaLoads = useRef(new Map<string, AbortController>());
    const pendingQuotaLoads = useRef(0);
    const [deleting, setDeleting] = useState<Account | null>(null);
    const [editing, setEditing] = useState<Account | null>(null);
    const [editLabel, setEditLabel] = useState('');
    const [usage, setUsage] = useState<{ account: Account; loading: boolean; data?: unknown; error?: string } | null>(null);
    const [accountQuotas, setAccountQuotas] = useState<Record<string, AccountQuotaState>>({});
    const [device, setDevice] = useState<DeviceSession | null>(null);
    const pendingDeviceId = useRef<string | null>(null);
    const [deviceStarting, setDeviceStarting] = useState(false);
    const [deviceError, setDeviceError] = useState('');
    const [now, setNow] = useState(Date.now());
    const hasActiveCooldown = accounts.accounts.some(account => (account.cooldown_until ?? 0) * 1000 > Math.max(now, Date.now()));
    const deviceDialog = useRef<HTMLDialogElement>(null);
    const deviceOpen = deviceStarting || device !== null || deviceError !== '';

    const call = useCallback(async <T,>(command: string, args?: Record<string, unknown>, controller = new AbortController()): Promise<T> => {
        controllers.current.add(controller);
        try {
            const result = await request<T>(command, args, { signal: controller.signal });
            if (controller.signal.aborted || !mounted.current) throw new DOMException('Aborted', 'AbortError');
            return result;
        } finally {
            controllers.current.delete(controller);
        }
    }, []);

    const refreshAccountList = useCallback(async () => {
        accountLoad.current?.abort();
        const controller = new AbortController();
        accountLoad.current = controller;
        setAccountsLoading(true);
        setAccountsError('');
        try {
            const next = await call<AccountList>('codex_list_accounts', undefined, controller);
            setAccounts(next);
            return next;
        } catch (error) {
            if (!aborted(error)) setAccountsError(errorMessage(error));
            return null;
        } finally {
            if (mounted.current && !controller.signal.aborted) setAccountsLoading(false);
        }
    }, [call]);

    const fetchUsage = useCallback(async (account: Account, controller: AbortController) => {
        pendingQuotaLoads.current += 1;
        try {
            return await call<unknown>('codex_account_usage', { id: account.id }, controller);
        } finally {
            pendingQuotaLoads.current -= 1;
            // Usage updates server cooldowns. Refresh once after the whole in-flight batch,
            // without starting another quota batch or replacing a newer account-list response.
            if (pendingQuotaLoads.current === 0 && mounted.current) void refreshAccountList();
        }
    }, [call, refreshAccountList]);

    const loadAccountQuota = useCallback(async (account: Account) => {
        quotaLoads.current.get(account.id)?.abort();
        const controller = new AbortController();
        quotaLoads.current.set(account.id, controller);
        setAccountQuotas(current => ({
            ...current,
            [account.id]: { ...current[account.id], loading: true, error: undefined },
        }));
        try {
            const value = await fetchUsage(account, controller);
            const data = accountQuota(value);
            if (!data) throw new Error('Codex usage response has no rate-limit windows.');
            if (mounted.current) {
                setAccountQuotas(current => ({ ...current, [account.id]: { loading: false, data } }));
            }
        } catch (error) {
            if (!aborted(error) && mounted.current) {
                setAccountQuotas(current => ({
                    ...current,
                    [account.id]: { loading: false, error: errorMessage(error) },
                }));
            }
        } finally {
            if (quotaLoads.current.get(account.id) === controller) quotaLoads.current.delete(account.id);
        }
    }, [fetchUsage]);

    const loadAccounts = useCallback(async () => {
        const next = await refreshAccountList();
        if (next) {
            for (const account of next.accounts) void loadAccountQuota(account);
        }
    }, [refreshAccountList, loadAccountQuota]);

    const loadModels = useCallback(async () => {
        modelLoad.current?.abort();
        const controller = new AbortController();
        modelLoad.current = controller;
        setModelsLoading(true);
        setModelsError('');
        setModels([]);
        try {
            const next = catalogModels(await call<unknown>('codex_models', undefined, controller));
            setModels(next);
            setModel(current => next.some(item => item.id === current) ? current : next[0]?.id ?? '');
        } catch (error) {
            if (!aborted(error)) setModelsError(errorMessage(error));
        } finally {
            if (mounted.current && !controller.signal.aborted) setModelsLoading(false);
        }
    }, [call]);

    useEffect(() => {
        mounted.current = true;
        if (!desktop) void loadAccounts();
        return () => {
            mounted.current = false;
            controllers.current.forEach(controller => controller.abort());
            controllers.current.clear();
            quotaLoads.current.clear();
            const id = pendingDeviceId.current;
            pendingDeviceId.current = null;
            if (id) {
                void request('codex_cancel_device_auth', { id }).catch(() => {
                    // Best-effort unmount cleanup; the server also expires pending sessions.
                });
            }
            if (fileInput.current) fileInput.current.value = '';
        };
    }, [desktop, loadAccounts]);

    // Account changes invalidate the catalog, including the preferred account and enabled state.
    useEffect(() => {
        if (desktop) return;
        if (accounts.accounts.some(account => account.enabled)) void loadModels();
        else {
            modelLoad.current?.abort();
            setModels([]);
            setModel('');
            setModelsError('');
            setModelsLoading(false);
        }
    }, [accounts, desktop, loadModels]);

    useEffect(() => {
        const dialog = deviceDialog.current;
        if (deviceOpen && dialog && !dialog.open) dialog.showModal();
        if (!deviceOpen && dialog?.open) dialog.close();
    }, [deviceOpen]);

    useEffect(() => {
        const dialog = addDialog.current;
        if (addOpen && dialog && !dialog.open) dialog.showModal();
        if (!addOpen && dialog?.open) dialog.close();
    }, [addOpen]);

    useEffect(() => {
        const dialog = usageDialog.current;
        if (usage && dialog && !dialog.open) dialog.showModal();
        if (!usage && dialog?.open) dialog.close();
    }, [!!usage]);

    useEffect(() => {
        if (desktop || (!hasActiveCooldown && device?.status !== 'pending')) return;
        const timer = window.setInterval(() => setNow(Date.now()), 1000);
        return () => window.clearInterval(timer);
    }, [desktop, hasActiveCooldown, device?.status]);

    useEffect(() => {
        if (!device || device.status !== 'pending') return;
        const controller = new AbortController();
        let timer: number;
        const delay = Math.max(1, device.interval) * 1000;
        const poll = async () => {
            if (controller.signal.aborted) return;
            if (Date.now() >= device.expires_at * 1000) {
                pendingDeviceId.current = null;
                setDevice(current => current?.id === device.id ? { ...current, status: 'expired' } : current);
                return;
            }
            try {
                const result = await call<{ status: DeviceStatus; account_id?: string; error?: string }>('codex_device_auth_status', { id: device.id }, controller);
                if (controller.signal.aborted) return;
                setDeviceError('');
                if (result.status !== 'pending') {
                    pendingDeviceId.current = null;
                    setDevice(current => current?.id === device.id ? { ...current, ...result } : current);
                    if (result.status === 'completed') {
                        showToast(t('codex.login_completed'), 'success');
                        void loadAccounts();
                    }
                    return;
                }
            } catch (error) {
                if (aborted(error)) return;
                setDeviceError(errorMessage(error));
            }
            timer = window.setTimeout(poll, Math.min(delay, Math.max(0, device.expires_at * 1000 - Date.now())));
        };
        timer = window.setTimeout(poll, Math.min(delay, Math.max(0, device.expires_at * 1000 - Date.now())));
        return () => { controller.abort(); window.clearTimeout(timer); };
    }, [device?.id, device?.status, device?.interval, device?.expires_at, call, loadAccounts, t]);

    const run = async (key: string, action: () => Promise<void>) => {
        if (busyRef.current) return;
        busyRef.current = true;
        setBusy(key);
        try {
            await action();
        } catch (error) {
            if (!aborted(error) && mounted.current) showToast(errorMessage(error), 'error', 6000);
        } finally {
            busyRef.current = false;
            if (mounted.current) setBusy('');
        }
    };

    const mutate = (command: string, account: Account, args?: Record<string, unknown>) => run(account.id, async () => {
        try {
            await call(command, { id: account.id, ...args });
        } finally {
            if (mounted.current) await loadAccounts();
        }
        setDeleting(null);
        setEditing(null);
        if (command === 'codex_delete_account' && usage?.account.id === account.id) {
            usageLoad.current?.abort();
            setUsage(null);
        }
        showToast(t('common.success'), 'success');
    });

    const importAccount = () => run('import', async () => {
        setImportError('');
        let file: File | null = fileInput.current?.files?.[0] ?? null;
        let auth: Record<string, unknown> | null = null;
        let text = '';
        let submitted = false;
        try {
            if (!file) throw new Error(t('codex.choose_file'));
            // Bound memory use without ever persisting or rendering credential contents.
            if (file.size > 1024 * 1024) throw new Error(t('codex.invalid_file'));
            text = await file.text();
            if (!mounted.current) throw new DOMException('Aborted', 'AbortError');
            try {
                const parsed: unknown = JSON.parse(text);
                if (!parsed || typeof parsed !== 'object' || Array.isArray(parsed)) throw new Error();
                auth = parsed as Record<string, unknown>;
            } catch {
                throw new Error(t('codex.invalid_file'));
            }
            text = '';
            file = null;
            if (fileInput.current) fileInput.current.value = '';
            setFileSelected(false);
            submitted = true;
            await call('codex_import_account', { auth_json: auth, ...(label.trim() ? { label: label.trim() } : {}) });
            auth = null;
            setLabel('');
            setAddOpen(false);
            showToast(t('codex.imported'), 'success');
        } catch (error) {
            if (!aborted(error) && mounted.current) setImportError(errorMessage(error));
            throw error;
        } finally {
            text = '';
            auth = null;
            file = null;
            if (fileInput.current) fileInput.current.value = '';
            if (mounted.current) setFileSelected(false);
            if (submitted && mounted.current) await loadAccounts();
        }
    });

    const startDevice = () => {
        if (deviceOpen || busyRef.current) return;
        setAddOpen(false);
        void run('device', async () => {
            setDeviceStarting(true);
            setDeviceError('');
            try {
                // Keep the start response observable so leaving mid-request cannot orphan its session id.
                const session = await request<Omit<DeviceSession, 'status'>>('codex_start_device_auth');
                if (!mounted.current) {
                    await request('codex_cancel_device_auth', { id: session.id });
                    return;
                }
                pendingDeviceId.current = session.id;
                let url: URL | null = null;
                try {
                    url = new URL(session.verification_url);
                } catch {
                    // Never render an untrusted authorization link.
                }
                if (!url || url.protocol !== 'https:' || url.hostname !== 'auth.openai.com' || url.port || url.username || url.password) {
                    await call('codex_cancel_device_auth', { id: session.id });
                    pendingDeviceId.current = null;
                    throw new Error(t('codex.invalid_device_url'));
                }
                setNow(Date.now());
                setDevice({ ...session, status: 'pending' });
            } catch (error) {
                if (!aborted(error) && mounted.current) setDeviceError(errorMessage(error));
            } finally {
                if (mounted.current) setDeviceStarting(false);
            }
        });
    };

    const closeDevice = () => {
        if (busyRef.current) return;
        if (device?.status === 'pending') {
            void run('device-cancel', async () => {
                const result = await call<{ status: DeviceStatus; account_id?: string; error?: string }>('codex_cancel_device_auth', { id: device.id });
                pendingDeviceId.current = null;
                setDevice(current => current?.id === device.id ? { ...current, ...result } : current);
                setDeviceError('');
                void loadAccounts();
            });
        } else {
            setDevice(null);
            setDeviceError('');
        }
    };

    const loadUsage = async (account: Account) => {
        usageLoad.current?.abort();
        const controller = new AbortController();
        usageLoad.current = controller;
        setUsage({ account, loading: true });
        try {
            const data = await fetchUsage(account, controller);
            setUsage({ account, loading: false, data });
            const quota = accountQuota(data);
            if (quota) setAccountQuotas(current => ({ ...current, [account.id]: { loading: false, data: quota } }));
        } catch (error) {
            if (!aborted(error)) setUsage({ account, loading: false, error: errorMessage(error) });
        }
    };

    const copy = async (text: string) => {
        const success = await copyToClipboard(text);
        if (mounted.current) showToast(t(success ? 'common.copied' : 'codex.copy_failed'), success ? 'success' : 'error');
    };
    const date = (seconds: number | null) => seconds ? new Date(seconds * 1000).toLocaleString(i18n.language) : t('codex.not_available');
    const hasEnabled = accounts.accounts.some(account => account.enabled);
    const insecure = window.location.protocol === 'http:' && !['localhost', '127.0.0.1', '[::1]'].includes(window.location.hostname) && !/^127\.\d+\.\d+\.\d+$/.test(window.location.hostname);
    const selectedModel = models.find(item => item.id === model);
    const config = selectedModel ? `model = ${JSON.stringify(selectedModel.id)}\nmodel_provider = "api_manager_codex"\n\n[model_providers.api_manager_codex]\nname = "API Manager Codex"\nbase_url = ${JSON.stringify(`${window.location.origin}/codex/v1`)}\nenv_key = "API_MANAGER_KEY"\nwire_api = "responses"\nsupports_websockets = false\nrequires_openai_auth = false` : '';
    const remaining = device ? Math.max(0, Math.ceil(device.expires_at - now / 1000)) : 0;

    if (desktop) return <div className="console-page console-page-scroll space-y-5"><PageHeader title={t('console.account_pool', { defaultValue: i18n.language.startsWith('zh') ? '账号池' : 'Account pool' })} description={t('codex.subtitle')} /><AccountPoolTabs /><div className={`${panel} flex items-start gap-3`}><Terminal className="shrink-0 text-blue-500" /><div><h2 className="text-lg font-semibold">{t('codex.title')}</h2><p className="mt-2 text-sm console-muted">{t('codex.server_only')}</p></div></div></div>;

    return (
        <div className={`console-page console-page-scroll space-y-5 ${controls}`}>
            <PageHeader
                title={t('console.account_pool', { defaultValue: i18n.language.startsWith('zh') ? '账号池' : 'Account pool' })}
                description={t('codex.subtitle')}
                actions={<button className="console-button console-button-primary" onClick={() => setAddOpen(true)}><Plus size={16} />{t('codex.add_account')}</button>}
            />
            <AccountPoolTabs />
            <div role={insecure ? 'alert' : undefined} className={`rounded-xl border px-4 py-3 flex flex-wrap items-center gap-2 text-sm ${insecure ? 'border-amber-300 bg-amber-50 text-amber-900 dark:bg-amber-950/30 dark:text-amber-200' : 'border-[var(--console-border)] bg-[var(--console-surface)] text-[var(--console-muted)]'}`}>
                {insecure ? <AlertTriangle size={16} className="shrink-0" /> : <ShieldCheck size={16} className="shrink-0" />}
                <span className="flex-1">{insecure ? t('codex.insecure_title') : t('console.codex_secure_notice', { defaultValue: i18n.language.startsWith('zh') ? '凭据保存在服务器端。仅在可信服务器和安全连接下导入。' : 'Credentials stay on the server. Import only over a secure connection to a trusted server.' })}</span>
                <span className="text-xs">{t('codex.server_badge')}</span>
            </div>
            <section className={panel} aria-labelledby="codex-accounts-title">
                <div className="console-toolbar justify-between"><div><h2 id="codex-accounts-title" className="text-lg font-semibold flex items-center gap-2"><Users size={20} />{t('codex.accounts')} <span className="text-sm console-muted tabular-nums">{accounts.accounts.length}</span></h2><p className="text-xs console-muted mt-1">{t('common.enabled')}: {accounts.accounts.filter(account => account.enabled).length} · {t('common.disabled')}: {accounts.accounts.filter(account => !account.enabled).length}</p></div><button className="console-button" disabled={accountsLoading || !!busy} onClick={() => void loadAccounts()}><RefreshCw size={15} className={accountsLoading ? 'animate-spin' : ''} />{t('common.refresh')}</button></div>
                <p className="mt-3 text-sm leading-6 console-muted">{t('codex.failover_help')}</p>
                {accounts.accounts.length > 0 && <div className="relative mt-4"><Search size={16} className="absolute left-3 top-1/2 -translate-y-1/2 console-muted" /><input type="search" value={search} onChange={event => setSearch(event.target.value)} aria-label={t('accounts.search_placeholder')} placeholder={t('accounts.search_placeholder')} className="w-full rounded-lg border border-[var(--console-border)] bg-transparent pl-9 pr-3 py-2 text-sm" /></div>}
                {accountsError && <p role="alert" className="text-error text-sm mt-3 break-words">{accountsError}</p>}
                {accountsLoading && accounts.accounts.length === 0 ? <p role="status" className="py-8 text-center console-muted">{t('common.loading')}</p> : accounts.accounts.length === 0 && !accountsError ? <div className="py-10 text-center"><Users size={32} className="mx-auto mb-3 console-muted" /><p className="font-medium">{t('codex.no_accounts')}</p><p className="text-sm console-muted mt-1">{t('console.codex_empty_help', { defaultValue: i18n.language.startsWith('zh') ? '导入 auth.json 或使用设备登录来添加订阅账号。' : 'Import auth.json or use device sign-in to add a subscription account.' })}</p><button className="console-button console-button-primary mt-4" onClick={() => setAddOpen(true)}><Plus size={16} />{t('codex.add_account')}</button></div> : null}
                {search && !accounts.accounts.some(account => `${account.label} ${account.email ?? ''} ${account.id}`.toLowerCase().includes(search.toLowerCase())) && <p role="status" className="py-8 text-center console-muted">{t('console.account_search_empty', { defaultValue: i18n.language.startsWith('zh') ? '没有匹配的账号。请尝试其他搜索条件。' : 'No matching accounts. Try another search.' })}</p>}
                <div className="grid xl:grid-cols-2 gap-4 mt-4">
                    {accounts.accounts.filter(account => `${account.label} ${account.email ?? ''} ${account.id}`.toLowerCase().includes(search.toLowerCase())).map(account => {
                        const quota = accountQuotas[account.id];
                        const cooling = (account.cooldown_until ?? 0) * 1000 > Math.max(now, Date.now());
                        const quotaWindows = quota?.data ? [
                            { key: 'five-hour', label: t('codex.five_hour_limit'), window: quota.data.fiveHour },
                            { key: 'weekly', label: t('codex.weekly_limit'), window: quota.data.weekly },
                        ].filter((item): item is { key: string; label: string; window: RateLimitWindow } => item.window !== null) : [];
                        return <article key={account.id} className="border border-[var(--console-border)] bg-[var(--console-surface)] rounded-xl p-4 sm:p-5 min-w-0">
                        <div className="flex flex-wrap items-start justify-between gap-2"><div className="min-w-0"><h3 className="font-semibold break-words">{account.label || account.email || account.id}</h3>{account.email && account.email !== account.label && <p className="text-sm text-gray-500 break-all">{account.email}</p>}</div><div className="flex flex-wrap gap-1">{accounts.active_account_id === account.id && <span title={t('codex.preferred_help')} className="badge badge-primary badge-outline gap-1"><Check size={12} />{t('codex.preferred')}</span>}<span className={`badge ${account.enabled ? 'badge-success badge-outline' : 'badge-ghost'}`}>{t(account.enabled ? 'common.enabled' : 'common.disabled')}</span>{cooling && <span className="badge badge-warning badge-outline">{t('codex.cooldown_skipped')}</span>}</div></div>
                        <dl className="text-xs grid grid-cols-1 sm:grid-cols-2 gap-2 mt-4 text-gray-500 dark:text-gray-400"><div><dt>{t('codex.plan')}</dt><dd className="text-base-content mt-0.5">{account.plan_type || t('codex.not_available')}</dd></div><div><dt>{t('codex.expires')}</dt><dd className="text-base-content mt-0.5">{date(account.expires_at)}</dd></div><div><dt>{t('codex.last_used')}</dt><dd className="text-base-content mt-0.5">{date(account.last_used_at)}</dd></div></dl>
                        {cooling && <div role="status" className="mt-3 rounded-lg border border-amber-200 bg-amber-50 p-3 text-xs leading-5 text-amber-900 dark:border-amber-800 dark:bg-amber-950/20 dark:text-amber-200"><p className="font-medium">{t(account.cooldown_reason === 'quota_exhausted' ? 'codex.cooldown_quota' : account.cooldown_reason === 'rate_limited' ? 'codex.cooldown_rate' : 'codex.cooldown_temporary')}</p><p className="break-words">{t('codex.cooldown_until', { time: date(account.cooldown_until ?? null) })}</p></div>}
                        <div className="mt-4">
                            {quota?.loading ? <div role="status">
                                <div className="h-24 animate-pulse rounded-lg bg-gray-100 dark:bg-base-200" />
                            </div> : quota?.data ? <div className={`grid gap-3 ${quotaWindows.length > 1 ? 'sm:grid-cols-2' : 'grid-cols-1'}`}>
                                {quotaWindows.map(({ key, label, window }) => <div key={key} className="rounded-lg border border-gray-100 bg-gray-50/70 p-3 dark:border-base-300 dark:bg-base-200">
                                    <div className="flex items-center justify-between gap-2 text-xs">
                                        <span className="font-medium text-gray-700 dark:text-gray-200">{label}</span>
                                        <span className="font-semibold text-emerald-600 dark:text-emerald-400">{t('codex.remaining_percent', { percent: window.remainingPercent })}</span>
                                    </div>
                                    <div className="mt-2 h-2 overflow-hidden rounded-full bg-gray-200 dark:bg-base-300" role="progressbar" aria-label={label} aria-valuemin={0} aria-valuemax={100} aria-valuenow={window.usedPercent}>
                                        <div className="h-full rounded-full bg-blue-500 transition-[width]" style={{ width: `${window.usedPercent}%` }} />
                                    </div>
                                    <div className="mt-2 flex flex-wrap justify-between gap-x-2 gap-y-1 text-[11px] text-gray-500 dark:text-gray-400">
                                        <span>{t('codex.used_percent', { percent: window.usedPercent })}</span>
                                        <span>{window.resetAt ? t('codex.resets_at', { time: date(window.resetAt) }) : t('codex.reset_unknown')}</span>
                                    </div>
                                </div>)}
                            </div> : quota?.error ? <p className="text-xs text-error break-words">{t('codex.quota_load_failed')}</p> : null}
                        </div>
                        {account.last_error && <p className="mt-3 text-xs text-error break-words">{account.last_error}</p>}
                        <div className="flex flex-wrap items-center gap-2 mt-4 pt-4 border-t border-[var(--console-border)]" aria-busy={busy === account.id}>
                            <button className="console-button" disabled={!!busy || !account.enabled || accounts.active_account_id === account.id} onClick={() => void mutate('codex_activate_account', account)}>{accounts.active_account_id === account.id ? <><Check size={14} />{t('codex.preferred')}</> : t('codex.activate')}</button>
                            <button className="console-button" disabled={!!busy} onClick={() => void loadUsage(account)}>{t('codex.usage')}</button>
                            <details className="relative ml-auto">
                                <summary className="console-button cursor-pointer">{t('console.more_actions', { defaultValue: i18n.language.startsWith('zh') ? '更多操作' : 'More actions' })}</summary>
                                <div className="absolute right-0 bottom-full mb-2 z-20 w-52 bg-[var(--console-surface)] border border-[var(--console-border)] shadow-lg rounded-xl p-2 flex flex-col gap-1">
                                    <button className="console-button justify-start" disabled={!!busy} onClick={() => void mutate('codex_update_account', account, { enabled: !account.enabled })}>{t(account.enabled ? 'codex.disable' : 'codex.enable')}</button>
                                    <button className="console-button justify-start" disabled={!!busy} onClick={() => void mutate('codex_refresh_account', account)}>{t('codex.refresh_auth')}</button>
                                    <button className="console-button justify-start" disabled={!!busy} onClick={() => { setEditing(account); setEditLabel(account.label); }}>{t('codex.rename')}</button>
                                    <button className="console-button justify-start text-red-600 dark:text-red-400" disabled={!!busy} onClick={() => setDeleting(account)}>{t('common.delete')}</button>
                                </div>
                            </details>
                        </div>
                        </article>;
                    })}
                </div>
            </section>
            <dialog ref={usageDialog} className="modal" aria-labelledby="codex-usage-title" onCancel={event => { event.preventDefault(); usageLoad.current?.abort(); setUsage(null); }}>
                <div className="modal-box bg-[var(--console-surface)] text-[var(--console-text)] max-w-3xl">
                    <div className="flex flex-wrap justify-between gap-3"><h2 id="codex-usage-title" className="text-lg font-semibold break-all">{t('codex.usage')} · {usage?.account.label || usage?.account.email || usage?.account.id}</h2><button className="console-button" onClick={() => { usageLoad.current?.abort(); setUsage(null); }}>{t('common.close')}</button></div>
                    <p className="text-sm console-muted my-3">{t('codex.usage_help')}</p>
                    {usage?.loading ? <p role="status">{t('common.loading')}</p> : usage?.error ? <p role="alert" className="text-error break-words">{usage.error}</p> : <pre className="bg-[var(--console-surface-muted)] rounded-lg p-4 text-xs overflow-auto max-h-96">{JSON.stringify(usage?.data, null, 2)}</pre>}
                    <div className="modal-action"><button className="console-button" disabled={usage?.loading} onClick={() => { if (usage) void loadUsage(usage.account); }}><RefreshCw size={15} />{t('common.refresh')}</button></div>
                </div>
            </dialog>
            <details className={panel}>
                <summary id="codex-connect-title" className="cursor-pointer font-semibold">{t('codex.connect')}</summary>
                <div className="flex flex-wrap items-center justify-between gap-3 mt-4"><Link className="text-sm text-blue-600 dark:text-blue-400 underline underline-offset-4" to="/api-guide">{t('console.open_integration_guide', { defaultValue: i18n.language.startsWith('zh') ? '查看接入指南' : 'Open integration guide' })}</Link><button className="console-button" disabled={modelsLoading || !hasEnabled || !!busy} onClick={() => void loadModels()}><RefreshCw size={15} className={modelsLoading ? 'animate-spin' : ''} />{t('codex.refresh_models')}</button></div><p className="text-sm console-muted mt-2">{t('codex.connect_help')}</p>
                {!hasEnabled ? <p className="text-sm mt-4">{t('codex.models_need_account')}</p> : modelsLoading ? <p role="status" className="text-sm mt-4">{t('common.loading')}</p> : modelsError ? <p role="alert" className="text-sm text-error mt-4 break-words">{modelsError}</p> : models.length === 0 ? <p className="text-sm mt-4">{t('codex.no_models')}</p> : <>
                    <label className="block text-sm mt-4 max-w-lg"><span>{t('codex.model')}</span><select className="select select-bordered select-sm w-full mt-1" value={model} onChange={event => setModel(event.target.value)}>{models.map(item => <option key={item.id} value={item.id}>{item.name} · {item.id}</option>)}</select></label>
                    <div className="mt-4 flex justify-between items-center gap-2"><code className="text-xs">~/.codex/config.toml</code><button className="btn btn-sm btn-ghost gap-1" disabled={!config} onClick={() => void copy(config)}><Copy size={14} />{t('common.copy')}</button></div><pre className="bg-gray-50 dark:bg-base-200 rounded-lg p-4 text-xs overflow-x-auto mt-2">{config}</pre>
                </>}
                <p className="text-sm mt-4">{t('codex.gateway_key_help')}</p><pre className="bg-gray-50 dark:bg-base-200 rounded-lg p-4 text-xs overflow-x-auto mt-2">{'export API_MANAGER_KEY=\'<YOUR_GATEWAY_API_KEY>\'\ncodex'}</pre><p className="text-xs text-gray-500 dark:text-gray-400 mt-3">{t('codex.transport_help')}</p>
            </details>
            <dialog ref={addDialog} className="modal" aria-labelledby="codex-add-title" onCancel={event => { event.preventDefault(); if (!busyRef.current) { setAddOpen(false); if (fileInput.current) fileInput.current.value = ''; setFileSelected(false); } }}>
                <div className="modal-box bg-[var(--console-surface)] text-[var(--console-text)] max-w-2xl">
                    <div className="flex items-start justify-between gap-3"><h2 id="codex-add-title" className="text-xl font-semibold">{t('codex.add_account')}</h2><button className="console-button" disabled={!!busy} onClick={() => { setAddOpen(false); if (fileInput.current) fileInput.current.value = ''; setFileSelected(false); }}>{t('common.close')}</button></div>
                    <div className="rounded-lg bg-amber-50 dark:bg-amber-950/30 text-amber-900 dark:text-amber-200 p-3 text-sm my-5 flex items-start gap-2"><AlertTriangle size={18} className="shrink-0 mt-0.5" /><p>{insecure && <strong className="block mb-1">{t('codex.insecure_title')}</strong>}{t('codex.security_warning')}</p></div>
                    <form onSubmit={event => { event.preventDefault(); void importAccount(); }} className="space-y-4">
                        <p className="text-sm console-muted">{t('codex.import_help')}</p>
                        {importError && <p role="alert" className="text-sm text-error break-words">{importError}</p>}
                        <label className="block text-sm"><span>{t('codex.label')}</span><input value={label} onChange={event => setLabel(event.target.value)} maxLength={200} disabled={!!busy} className="input input-bordered w-full mt-1" autoComplete="off" /></label>
                        <label className="block text-sm"><span>{t('codex.auth_file')}</span><input ref={fileInput} type="file" accept=".json,application/json" disabled={!!busy} onChange={event => setFileSelected(!!event.target.files?.length)} className="file-input file-input-bordered w-full mt-1" /></label>
                        <button type="submit" className="console-button console-button-primary" disabled={!fileSelected || !!busy}><Upload size={15} />{busy === 'import' ? t('common.loading') : t('codex.import')}</button>
                    </form>
                    <div className="mt-6 pt-5 border-t border-[var(--console-border)] space-y-3"><h3 className="font-medium">{t('codex.device_title')}</h3><p className="text-sm console-muted">{t('codex.device_help')}</p><button className="console-button" disabled={!!busy || deviceOpen} onClick={startDevice}>{t('codex.device_start')}</button></div>
                </div>
            </dialog>
            <ModalDialog isOpen={!!deleting} title={t('codex.delete_title')} message={t('codex.delete_confirm', { account: deleting?.label || deleting?.email || deleting?.id })} type="confirm" isDestructive confirmText={busy ? t('common.loading') : t('common.delete')} onConfirm={() => { if (deleting) void mutate('codex_delete_account', deleting); }} onCancel={() => { if (!busyRef.current) setDeleting(null); }} />
            <ModalDialog isOpen={!!editing} title={t('codex.rename')} type="confirm" confirmText={busy ? t('common.loading') : t('common.save')} onConfirm={() => { if (editing) void mutate('codex_update_account', editing, { label: editLabel.trim() }); }} onCancel={() => { if (!busyRef.current) setEditing(null); }}><label className="block text-sm">{t('codex.label')}<input value={editLabel} maxLength={200} disabled={!!busy} onChange={event => setEditLabel(event.target.value)} className="input input-bordered input-sm w-full mt-2" autoComplete="off" /></label></ModalDialog>
            <dialog ref={deviceDialog} className="modal" aria-labelledby="codex-device-title" onCancel={event => { event.preventDefault(); closeDevice(); }}>
                <div className="modal-box bg-white dark:bg-base-100 max-w-lg"><h2 id="codex-device-title" className="text-xl font-bold">{t('codex.device_title')}</h2>{deviceStarting && <p role="status" className="mt-4">{t('common.loading')}</p>}
                    {device && <div className="space-y-4 mt-4"><p role="status" className={device.status === 'completed' ? 'text-success' : ''}>{t(`codex.status_${device.status}`)}</p>{device.status === 'pending' && <><p className="text-sm text-gray-500">{t('codex.device_instructions')}</p><a className="link link-primary inline-flex items-center gap-1 break-all" href={device.verification_url} target="_blank" rel="noopener noreferrer">{device.verification_url}<ExternalLink size={14} /></a><div className="flex items-center justify-between gap-2 bg-gray-50 dark:bg-base-200 rounded-lg p-4"><code className="text-2xl tracking-widest select-all break-all">{device.user_code}</code><button className="btn btn-ghost btn-sm" aria-label={t('codex.copy_code')} onClick={() => void copy(device.user_code)}><Copy size={16} /></button></div><p className="text-sm tabular-nums">{t('codex.expires_in', { minutes: Math.floor(remaining / 60), seconds: String(remaining % 60).padStart(2, '0') })}</p></>}{device.error && <p role="alert" className="text-sm text-error break-words">{device.error}</p>}</div>}
                    {deviceError && <p role="alert" className="text-sm text-error mt-4 break-words">{deviceError}{device?.status === 'pending' && <span className="block mt-1">{t('codex.poll_retry')}</span>}</p>}
                    <div className="modal-action"><button className="btn btn-sm" disabled={!!busy} onClick={closeDevice}>{device?.status === 'pending' ? t('codex.cancel_login') : t('common.close')}</button></div>
                </div>
            </dialog>
        </div>
    );
}
