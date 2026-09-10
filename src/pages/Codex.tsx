import { useCallback, useEffect, useRef, useState } from 'react';
import { useTranslation } from 'react-i18next';
import { AlertTriangle, Check, Copy, ExternalLink, RefreshCw, ShieldCheck, Terminal, Upload, Users } from 'lucide-react';
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
const panel = 'bg-white dark:bg-base-100 rounded-xl shadow-sm border border-gray-100 dark:border-base-200 p-5';
const controls = '[&_.btn]:rounded-lg [&_.btn]:border [&_.btn]:border-gray-200 [&_.btn]:px-3 [&_.btn]:transition-colors [&_.btn:disabled]:opacity-40 [&_.btn:disabled]:cursor-not-allowed [&_.btn-primary]:bg-blue-600 [&_.btn-primary]:text-white [&_.btn-primary]:border-blue-600 [&_.input]:rounded-lg [&_.input]:border [&_.input]:border-gray-300 [&_.input]:px-3 [&_.file-input]:rounded-lg [&_.file-input]:border [&_.file-input]:border-gray-300 [&_.select]:rounded-lg [&_.select]:border [&_.select]:border-gray-300 [&_.select]:px-3 dark:[&_.btn]:border-gray-600 dark:[&_.input]:border-gray-600 dark:[&_.file-input]:border-gray-600 dark:[&_.select]:border-gray-600';
const errorMessage = (error: unknown) => error instanceof Error ? error.message : String(error);
const aborted = (error: unknown) => error instanceof DOMException && error.name === 'AbortError';
const objectValue = (value: unknown): Record<string, unknown> | null =>
    value !== null && typeof value === 'object' && !Array.isArray(value) ? value as Record<string, unknown> : null;

function rateLimitWindow(value: unknown): RateLimitWindow | null {
    const window = objectValue(value);
    if (!window || typeof window.used_percent !== 'number') return null;
    const usedPercent = Math.min(100, Math.max(0, window.used_percent));
    return {
        usedPercent,
        remainingPercent: 100 - usedPercent,
        resetAt: typeof window.reset_at === 'number' ? window.reset_at : null,
    };
}

function accountQuota(value: unknown): AccountQuota | null {
    const rateLimit = objectValue(objectValue(value)?.rate_limit);
    if (!rateLimit) return null;
    const quota = {
        fiveHour: rateLimitWindow(rateLimit.primary_window),
        weekly: rateLimitWindow(rateLimit.secondary_window),
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
    const [busy, setBusy] = useState('');
    const busyRef = useRef(false);
    const mounted = useRef(false);
    const controllers = useRef(new Set<AbortController>());
    const accountLoad = useRef<AbortController | null>(null);
    const modelLoad = useRef<AbortController | null>(null);
    const usageLoad = useRef<AbortController | null>(null);
    const quotaLoads = useRef(new Map<string, AbortController>());
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

    const loadAccountQuota = useCallback(async (account: Account) => {
        quotaLoads.current.get(account.id)?.abort();
        const controller = new AbortController();
        quotaLoads.current.set(account.id, controller);
        setAccountQuotas(current => ({
            ...current,
            [account.id]: { ...current[account.id], loading: true, error: undefined },
        }));
        try {
            const value = await call<unknown>('codex_account_usage', { id: account.id }, controller);
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
    }, [call]);

    const loadAccounts = useCallback(async () => {
        accountLoad.current?.abort();
        const controller = new AbortController();
        accountLoad.current = controller;
        setAccountsLoading(true);
        setAccountsError('');
        try {
            const next = await call<AccountList>('codex_list_accounts', undefined, controller);
            setAccounts(next);
            for (const account of next.accounts) void loadAccountQuota(account);
        } catch (error) {
            if (!aborted(error)) setAccountsError(errorMessage(error));
        } finally {
            if (mounted.current && !controller.signal.aborted) setAccountsLoading(false);
        }
    }, [call, loadAccountQuota]);

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
        if (!device || device.status !== 'pending') return;
        const timer = window.setInterval(() => setNow(Date.now()), 1000);
        return () => window.clearInterval(timer);
    }, [device?.id, device?.status]);

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
            showToast(t('codex.imported'), 'success');
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
            const data = await call<unknown>('codex_account_usage', { id: account.id }, controller);
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

    if (desktop) return <div className="max-w-7xl mx-auto p-5"><div className={`${panel} flex items-start gap-3`}><Terminal className="shrink-0 text-blue-500" /><div><h1 className="text-xl font-bold">{t('codex.title')}</h1><p className="mt-2 text-sm text-gray-500">{t('codex.server_only')}</p></div></div></div>;

    return (
        <div className={`h-full overflow-y-auto p-4 sm:p-5 space-y-5 max-w-7xl mx-auto w-full ${controls}`}>
            <header className="flex flex-wrap items-start justify-between gap-3">
                <div><h1 className="text-2xl font-bold flex items-center gap-2"><Terminal className="text-blue-500" />{t('codex.title')}</h1><p className="text-sm text-gray-500 dark:text-gray-400 mt-1">{t('codex.subtitle')}</p></div>
                <span className="badge badge-outline gap-1"><ShieldCheck size={14} />{t('codex.server_badge')}</span>
            </header>
            <div role={insecure ? 'alert' : undefined} className={`rounded-xl border p-4 flex items-start gap-3 text-sm ${insecure ? 'border-amber-300 bg-amber-50 text-amber-900 dark:bg-amber-950/30 dark:text-amber-200' : 'border-blue-100 bg-blue-50 text-blue-900 dark:bg-blue-950/20 dark:text-blue-200'}`}>
                <AlertTriangle size={20} className="shrink-0 mt-0.5" /><div>{insecure && <strong className="block mb-1">{t('codex.insecure_title')}</strong>}{t('codex.security_warning')}</div>
            </div>
            <section className={panel} aria-labelledby="codex-add-title">
                <h2 id="codex-add-title" className="text-lg font-semibold">{t('codex.add_account')}</h2>
                <div className="grid lg:grid-cols-2 gap-6 mt-4">
                    <form onSubmit={event => { event.preventDefault(); void importAccount(); }} className="space-y-3">
                        <p className="text-sm text-gray-500 dark:text-gray-400">{t('codex.import_help')}</p>
                        <label className="block text-sm"><span>{t('codex.label')}</span><input value={label} onChange={event => setLabel(event.target.value)} maxLength={200} disabled={!!busy} className="input input-bordered input-sm w-full mt-1" autoComplete="off" /></label>
                        <label className="block text-sm"><span>{t('codex.auth_file')}</span><input ref={fileInput} type="file" accept=".json,application/json" disabled={!!busy} onChange={event => setFileSelected(!!event.target.files?.length)} className="file-input file-input-bordered file-input-sm w-full mt-1" /></label>
                        <button type="submit" className="btn btn-primary btn-sm gap-2" disabled={!fileSelected || !!busy}><Upload size={15} />{busy === 'import' ? t('common.loading') : t('codex.import')}</button>
                    </form>
                    <div className="lg:border-l lg:pl-6 border-gray-100 dark:border-base-200 space-y-3"><h3 className="font-medium">{t('codex.device_title')}</h3><p className="text-sm text-gray-500 dark:text-gray-400">{t('codex.device_help')}</p><button className="btn btn-outline btn-sm" disabled={!!busy || deviceOpen} onClick={startDevice}>{t('codex.device_start')}</button></div>
                </div>
            </section>
            <section className={panel} aria-labelledby="codex-accounts-title">
                <div className="flex items-center justify-between gap-2"><h2 id="codex-accounts-title" className="text-lg font-semibold flex items-center gap-2"><Users size={20} />{t('codex.accounts')} <span className="badge badge-ghost">{accounts.accounts.length}</span></h2><button className="btn btn-ghost btn-sm gap-1" disabled={accountsLoading || !!busy} onClick={() => void loadAccounts()}><RefreshCw size={15} className={accountsLoading ? 'animate-spin' : ''} />{t('common.refresh')}</button></div>
                {accountsError && <p role="alert" className="text-error text-sm mt-3 break-words">{accountsError}</p>}
                {accountsLoading && accounts.accounts.length === 0 ? <p role="status" className="py-8 text-center text-gray-500">{t('common.loading')}</p> : accounts.accounts.length === 0 && !accountsError ? <div className="py-8 text-center"><p className="font-medium">{t('codex.no_accounts')}</p><p className="text-sm text-gray-500 mt-1">{t('codex.no_accounts_help')}</p></div> : null}
                <div className="grid xl:grid-cols-2 gap-4 mt-4">
                    {accounts.accounts.map(account => {
                        const quota = accountQuotas[account.id];
                        return <article key={account.id} className="border border-gray-200 dark:border-base-300 rounded-xl p-4 min-w-0">
                        <div className="flex flex-wrap items-start justify-between gap-2"><div className="min-w-0"><h3 className="font-semibold break-words">{account.label || account.email || account.id}</h3>{account.email && account.email !== account.label && <p className="text-sm text-gray-500 break-all">{account.email}</p>}</div><div className="flex flex-wrap gap-1">{accounts.active_account_id === account.id && <span className="badge badge-primary badge-outline gap-1"><Check size={12} />{t('codex.preferred')}</span>}<span className={`badge ${account.enabled ? 'badge-success badge-outline' : 'badge-ghost'}`}>{t(account.enabled ? 'common.enabled' : 'common.disabled')}</span></div></div>
                        <dl className="text-xs grid grid-cols-1 sm:grid-cols-2 gap-2 mt-4 text-gray-500 dark:text-gray-400"><div><dt>{t('codex.plan')}</dt><dd className="text-base-content mt-0.5">{account.plan_type || t('codex.not_available')}</dd></div><div><dt>{t('codex.expires')}</dt><dd className="text-base-content mt-0.5">{date(account.expires_at)}</dd></div><div><dt>{t('codex.last_used')}</dt><dd className="text-base-content mt-0.5">{date(account.last_used_at)}</dd></div></dl>
                        <div className="mt-4">
                            {quota?.loading ? <div className="grid sm:grid-cols-2 gap-3" role="status">
                                <div className="h-24 animate-pulse rounded-lg bg-gray-100 dark:bg-base-200" />
                                <div className="h-24 animate-pulse rounded-lg bg-gray-100 dark:bg-base-200" />
                            </div> : quota?.data ? <div className="grid sm:grid-cols-2 gap-3">
                                {([
                                    [t('codex.five_hour_limit'), quota.data.fiveHour],
                                    [t('codex.weekly_limit'), quota.data.weekly],
                                ] as const).map(([label, window]) => <div key={label} className="rounded-lg border border-gray-100 bg-gray-50/70 p-3 dark:border-base-300 dark:bg-base-200/60">
                                    <div className="flex items-center justify-between gap-2 text-xs">
                                        <span className="font-medium text-gray-700 dark:text-gray-200">{label}</span>
                                        <span className="font-semibold text-emerald-600 dark:text-emerald-400">{window ? t('codex.remaining_percent', { percent: window.remainingPercent }) : t('codex.not_available')}</span>
                                    </div>
                                    {window && <>
                                        <div className="mt-2 h-2 overflow-hidden rounded-full bg-gray-200 dark:bg-base-300" role="progressbar" aria-label={label} aria-valuemin={0} aria-valuemax={100} aria-valuenow={window.usedPercent}>
                                            <div className="h-full rounded-full bg-blue-500 transition-[width]" style={{ width: `${window.usedPercent}%` }} />
                                        </div>
                                        <div className="mt-2 flex flex-wrap justify-between gap-x-2 gap-y-1 text-[11px] text-gray-500 dark:text-gray-400">
                                            <span>{t('codex.used_percent', { percent: window.usedPercent })}</span>
                                            <span>{window.resetAt ? t('codex.resets_at', { time: date(window.resetAt) }) : t('codex.reset_unknown')}</span>
                                        </div>
                                    </>}
                                </div>)}
                            </div> : quota?.error ? <p className="text-xs text-error break-words">{t('codex.quota_load_failed')}</p> : null}
                        </div>
                        {account.last_error && <p className="mt-3 text-xs text-error break-words">{account.last_error}</p>}
                        <div className="flex flex-wrap gap-2 mt-4" aria-busy={busy === account.id}>
                            <button className="btn btn-xs btn-outline" disabled={!!busy || !account.enabled || accounts.active_account_id === account.id} onClick={() => void mutate('codex_activate_account', account)}>{t('codex.activate')}</button>
                            <button className="btn btn-xs btn-ghost" disabled={!!busy} onClick={() => void mutate('codex_update_account', account, { enabled: !account.enabled })}>{t(account.enabled ? 'codex.disable' : 'codex.enable')}</button>
                            <button className="btn btn-xs btn-ghost" disabled={!!busy} onClick={() => void mutate('codex_refresh_account', account)}>{t('codex.refresh_auth')}</button>
                            <button className="btn btn-xs btn-ghost" disabled={!!busy} onClick={() => void loadUsage(account)}>{t('codex.usage')}</button>
                            <button className="btn btn-xs btn-ghost" disabled={!!busy} onClick={() => { setEditing(account); setEditLabel(account.label); }}>{t('codex.rename')}</button>
                            <button className="btn btn-xs btn-ghost text-error" disabled={!!busy} onClick={() => setDeleting(account)}>{t('common.delete')}</button>
                        </div>
                        </article>;
                    })}
                </div>
            </section>
            {usage && <section className={panel} aria-labelledby="codex-usage-title"><div className="flex flex-wrap justify-between gap-2"><h2 id="codex-usage-title" className="text-lg font-semibold break-all">{t('codex.usage')} · {usage.account.label || usage.account.email || usage.account.id}</h2><div className="flex gap-2"><button className="btn btn-sm btn-ghost" disabled={usage.loading} onClick={() => void loadUsage(usage.account)}>{t('common.refresh')}</button><button className="btn btn-sm btn-ghost" onClick={() => { usageLoad.current?.abort(); setUsage(null); }}>{t('common.close')}</button></div></div><p className="text-sm text-gray-500 my-3">{t('codex.usage_help')}</p>{usage.loading ? <p role="status">{t('common.loading')}</p> : usage.error ? <p role="alert" className="text-error break-words">{usage.error}</p> : <pre className="bg-gray-50 dark:bg-base-200 rounded-lg p-4 text-xs overflow-auto max-h-96">{JSON.stringify(usage.data, null, 2)}</pre>}</section>}
            <section className={panel} aria-labelledby="codex-connect-title"><div className="flex items-center justify-between gap-2"><h2 id="codex-connect-title" className="text-lg font-semibold">{t('codex.connect')}</h2><button className="btn btn-sm btn-ghost gap-1" disabled={modelsLoading || !hasEnabled || !!busy} onClick={() => void loadModels()}><RefreshCw size={15} className={modelsLoading ? 'animate-spin' : ''} />{t('codex.refresh_models')}</button></div><p className="text-sm text-gray-500 dark:text-gray-400 mt-2">{t('codex.connect_help')}</p>
                {!hasEnabled ? <p className="text-sm mt-4">{t('codex.models_need_account')}</p> : modelsLoading ? <p role="status" className="text-sm mt-4">{t('common.loading')}</p> : modelsError ? <p role="alert" className="text-sm text-error mt-4 break-words">{modelsError}</p> : models.length === 0 ? <p className="text-sm mt-4">{t('codex.no_models')}</p> : <>
                    <label className="block text-sm mt-4 max-w-lg"><span>{t('codex.model')}</span><select className="select select-bordered select-sm w-full mt-1" value={model} onChange={event => setModel(event.target.value)}>{models.map(item => <option key={item.id} value={item.id}>{item.name} · {item.id}</option>)}</select></label>
                    <div className="mt-4 flex justify-between items-center gap-2"><code className="text-xs">~/.codex/config.toml</code><button className="btn btn-sm btn-ghost gap-1" disabled={!config} onClick={() => void copy(config)}><Copy size={14} />{t('common.copy')}</button></div><pre className="bg-gray-50 dark:bg-base-200 rounded-lg p-4 text-xs overflow-x-auto mt-2">{config}</pre>
                </>}
                <p className="text-sm mt-4">{t('codex.gateway_key_help')}</p><pre className="bg-gray-50 dark:bg-base-200 rounded-lg p-4 text-xs overflow-x-auto mt-2">{'export API_MANAGER_KEY=\'<YOUR_GATEWAY_API_KEY>\'\ncodex'}</pre><p className="text-xs text-gray-500 dark:text-gray-400 mt-3">{t('codex.transport_help')}</p>
            </section>
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
