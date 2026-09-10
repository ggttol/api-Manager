import { save } from '@tauri-apps/plugin-dialog';
import { Activity, AlertTriangle, ArrowRight, Bot, Download, RefreshCw, Server, Users } from 'lucide-react';
import { Dispatch, SetStateAction, useEffect, useMemo, useRef, useState } from 'react';
import { useTranslation } from 'react-i18next';
import { useNavigate } from 'react-router-dom';
import AddAccountDialog from '../components/accounts/AddAccountDialog';
import { showToast } from '../components/common/ToastContainer';
import { PageHeader } from '../components/common/ConsolePage';
import BestAccounts from '../components/dashboard/BestAccounts';
import { findImageQuotaModel, findQuotaModel } from '../config/modelConfig';
import CurrentAccount from '../components/dashboard/CurrentAccount';
import { exportAccounts } from '../services/accountService';
import { useAccountStore } from '../stores/useAccountStore';
import { Account } from '../types/account';
import { isTauri } from '../utils/env';
import { request as invoke } from '../utils/request';

type ProxyStatus = { running: boolean; port: number; base_url: string; active_accounts: number };
type CodexPool = {
    accounts: { id: string; email: string | null; label: string; enabled: boolean; last_error: string | null }[];
    active_account_id: string | null;
};
type UsageSummary = { total_requests: number; total_tokens: number };
type Snapshot<T> = { data: T | null; loading: boolean; error: string | null };

function Dashboard() {
    const { t, i18n } = useTranslation();
    const text = (key: string, zh: string, en: string) =>
        t(`console.${key}`, { defaultValue: i18n.language.startsWith('zh') ? zh : en });
    const desktop = isTauri();
    const navigate = useNavigate();
    const {
        accounts,
        currentAccount,
        fetchAccounts,
        fetchCurrentAccount,
        switchAccount,
        addAccount,
        refreshQuota,
        loading
    } = useAccountStore();

    const [revision, setRevision] = useState(0);
    const [googleLoading, setGoogleLoading] = useState(true);
    const [googleError, setGoogleError] = useState<string | null>(null);
    const [currentError, setCurrentError] = useState<string | null>(null);
    const [status, setStatus] = useState<Snapshot<ProxyStatus>>({ data: null, loading: true, error: null });
    const [codex, setCodex] = useState<Snapshot<CodexPool>>({ data: null, loading: !desktop, error: null });
    const [usage, setUsage] = useState<Snapshot<UsageSummary>>({ data: null, loading: true, error: null });

    useEffect(() => {
        const controller = new AbortController();
        const load = async <T,>(command: string, args: unknown, setter: Dispatch<SetStateAction<Snapshot<T>>>) => {
            setter({ data: null, loading: true, error: null });
            try {
                const data = await invoke<T>(command, args, { signal: controller.signal });
                if (!controller.signal.aborted) setter({ data, loading: false, error: null });
            } catch (error) {
                if (!controller.signal.aborted) setter({ data: null, loading: false, error: String(error) });
            }
        };
        void load('get_proxy_status', undefined, setStatus);
        void load('get_token_stats_summary', { hours: 24 }, setUsage);
        if (!desktop) void load('codex_list_accounts', undefined, setCodex);
        setGoogleLoading(true);
        setGoogleError(null);
        setCurrentError(null);
        void (async () => {
            await fetchAccounts();
            if (controller.signal.aborted) return;
            setGoogleError(useAccountStore.getState().error);
            await fetchCurrentAccount();
            if (controller.signal.aborted) return;
            setCurrentError(useAccountStore.getState().error);
            setGoogleLoading(false);
        })();
        return () => controller.abort();
    }, [revision, desktop, fetchAccounts, fetchCurrentAccount]);

    const stats = useMemo(() => {
        const average = (values: (number | undefined)[]) => {
            const known = values.filter((value): value is number => typeof value === 'number' && Number.isFinite(value));
            return known.length ? Math.round(known.reduce((sum, value) => sum + value, 0) / known.length) : null;
        };
        return {
            enabled: accounts.filter(a => !a.disabled && !a.proxy_disabled).length,
            issues: accounts.filter(a => a.disabled || a.quota?.is_forbidden).length,
            avgGemini: average(accounts.map(a => findQuotaModel(a.quota?.models, 'gemini-pro')?.percentage)),
            avgGeminiImage: average(accounts.map(a => findImageQuotaModel(a.quota?.models)?.percentage)),
            avgClaude: average(accounts.map(a => findQuotaModel(a.quota?.models, 'claude')?.percentage)),
            lowQuota: accounts.filter(a => !a.quota?.is_forbidden && [
                findQuotaModel(a.quota?.models, 'gemini-pro')?.percentage,
                findQuotaModel(a.quota?.models, 'claude')?.percentage,
            ].some(value => typeof value === 'number' && value < 20)).length,
        };
    }, [accounts]);

    const isSwitchingRef = useRef(false);

    const handleSwitch = async (accountId: string) => {
        if (loading || isSwitchingRef.current) return;

        isSwitchingRef.current = true;
        try {
            await switchAccount(accountId);
            showToast(t('dashboard.toast.switch_success'), 'success');
        } catch (error) {
            console.error('切换账号失败:', error);
            showToast(`${t('dashboard.toast.switch_error')}: ${error}`, 'error');
        } finally {
            setTimeout(() => {
                isSwitchingRef.current = false;
            }, 1000);
        }
    };

    const handleAddAccount = async (email: string, refreshToken: string) => {
        await addAccount(email, refreshToken);
        await fetchAccounts(); // 刷新列表
    };

    const [isRefreshing, setIsRefreshing] = useState(false);

    const handleRefreshCurrent = async () => {
        if (!currentAccount) return;

        setIsRefreshing(true);
        try {
            await refreshQuota(currentAccount.id);
            // 刷新成功后重新获取最新数据
            await fetchCurrentAccount();
            showToast(t('dashboard.toast.refresh_success'), 'success');
        } catch (error) {
            console.error('[Dashboard] Refresh failed:', error);
            showToast(`${t('dashboard.toast.refresh_error')}: ${error}`, 'error');
        } finally {
            setIsRefreshing(false);
        }
    };

    const exportAccountsToJson = async (accountsToExport: Account[]) => {
        try {
            if (accountsToExport.length === 0) {
                showToast(t('dashboard.toast.export_no_accounts'), 'warning');
                return;
            }

            // Get export data from API (contains refresh_token)
            const accountIds = accountsToExport.map(acc => acc.id);
            const response = await exportAccounts(accountIds);

            if (!response.accounts || response.accounts.length === 0) {
                showToast(t('dashboard.toast.export_no_accounts'), 'warning');
                return;
            }

            const exportData = response.accounts;
            const content = JSON.stringify(exportData, null, 2);
            const fileName = `antigravity_accounts_${new Date().toISOString().split('T')[0]}.json`;

            if (isTauri()) {
                const path = await save({
                    filters: [{
                        name: 'JSON',
                        extensions: ['json']
                    }],
                    defaultPath: fileName
                });

                if (!path) return;

                await invoke('save_text_file', { path, content });
                showToast(t('dashboard.toast.export_success', { path }), 'success');
            } else {
                // Web 模式：使用浏览器下载
                const blob = new Blob([content], { type: 'application/json' });
                const url = URL.createObjectURL(blob);
                const a = document.createElement('a');
                a.href = url;
                a.download = fileName;
                document.body.appendChild(a);
                a.click();
                document.body.removeChild(a);
                URL.revokeObjectURL(url);
                showToast(t('dashboard.toast.export_success', { path: fileName }), 'success');
            }
        } catch (error: any) {
            console.error('Export failed:', error);
            showToast(`${t('dashboard.toast.export_error')}: ${error.toString()}`, 'error');
        }
    };

    const handleExport = () => {
        exportAccountsToJson(accounts);
    };

    const refreshing = googleLoading || status.loading || codex.loading || usage.loading;
    const statusPending = status.data?.base_url === 'starting' || status.data?.base_url === 'busy';
    const statusLabel = status.loading ? t('common.loading') : status.error ? text('overview_status_unknown', '状态不可用', 'Status unavailable')
        : statusPending ? text('overview_status_pending', '正在启动或忙碌', 'Starting or busy')
            : status.data?.running ? text('overview_running', '运行中', 'Running') : text('overview_stopped', '已停止', 'Stopped');
    const unavailable = text('overview_unavailable', '不可用', 'Unavailable');
    const poolMetric = (value: number) => googleLoading ? '—' : googleError ? '—' : value.toLocaleString();
    const errorNotice = (error: string) => (
        <p role="alert" className="mt-3 flex items-start gap-2 text-sm text-red-600 dark:text-red-400 break-words">
            <AlertTriangle className="mt-0.5 h-4 w-4 shrink-0" /><span className="min-w-0 break-all">{error}</span>
        </p>
    );

    return (
        <div className="console-page console-page-scroll space-y-6">
            <PageHeader
                title={t('nav.dashboard')}
                description={text('overview_description', '查看服务状态、独立账号池与已记录的请求用量。', 'Service status, independent account pools, and recorded request usage.')}
                actions={<button className="console-button" onClick={() => setRevision(value => value + 1)} disabled={refreshing}>
                    <RefreshCw className={`h-4 w-4 ${refreshing ? 'animate-spin' : ''}`} />
                    {refreshing ? t('common.loading') : t('common.refresh')}
                </button>}
            />

            <section className="grid grid-cols-1 gap-4 lg:grid-cols-3" aria-label={text('overview_service_usage', '服务与用量', 'Service and usage')}>
                <div className="console-panel flex flex-col gap-3">
                    <div className="flex items-center gap-2 text-sm console-muted"><Server className="h-4 w-4" />{text('overview_service', '网关服务', 'Gateway service')}</div>
                    <div role="status" className={`flex items-center gap-2 text-xl font-semibold ${status.data?.running && !statusPending ? 'text-emerald-600 dark:text-emerald-400' : 'text-base-content'}`}>
                        <span className={`h-2.5 w-2.5 rounded-full ${status.loading || status.error ? 'bg-gray-400' : statusPending ? 'bg-amber-500' : status.data?.running ? 'bg-emerald-500' : 'bg-gray-400'}`} />
                        {statusLabel}
                    </div>
                    {status.data && !statusPending && status.data.running && <code className="break-all text-xs console-muted">{status.data.base_url}</code>}
                    {status.error && errorNotice(status.error)}
                    <button className="mt-auto flex items-center gap-2 pt-2 text-sm font-medium text-blue-600 dark:text-blue-400" onClick={() => navigate('/api-proxy')}>
                        {text('overview_configure_gateway', '管理网关', 'Manage gateway')}<ArrowRight className="h-4 w-4" />
                    </button>
                </div>
                <div className="console-panel lg:col-span-2">
                    <div className="flex flex-wrap items-center justify-between gap-2">
                        <h2 className="flex items-center gap-2 text-sm font-medium"><Activity className="h-4 w-4 text-blue-500" />{text('overview_usage_day', '最近 24 小时 · 已记录用量', 'Last 24 hours · recorded usage')}</h2>
                        <button className="text-sm text-blue-600 dark:text-blue-400" onClick={() => navigate('/token-stats')}>{text('overview_usage_details', '查看统计', 'View statistics')}<ArrowRight className="ml-1 inline h-3.5 w-3.5" /></button>
                    </div>
                    <dl className="my-5 grid grid-cols-2 gap-4">
                        <div><dt className="text-xs console-muted">{text('overview_recorded_requests', '已记录请求', 'Recorded requests')}</dt><dd className="mt-1 text-3xl font-semibold tabular-nums">{usage.data ? usage.data.total_requests.toLocaleString() : '—'}</dd></div>
                        <div><dt className="text-xs console-muted">{text('overview_recorded_tokens', '已记录 Token', 'Recorded tokens')}</dt><dd className="mt-1 text-3xl font-semibold tabular-nums">{usage.data ? usage.data.total_tokens.toLocaleString() : '—'}</dd></div>
                    </dl>
                    {usage.loading ? <p role="status" className="text-sm console-muted">{t('common.loading')}</p> : usage.error ? errorNotice(usage.error) : <p className="text-xs console-muted">{text('overview_usage_scope', '来自网关用量记录，不代表所有请求，也不是订阅剩余额度。', 'From gateway usage records; not a count of all traffic or remaining subscription quota.')}</p>}
                </div>
            </section>

            <section className="grid grid-cols-1 gap-4 md:grid-cols-2" aria-label={text('overview_pools', '账号资源池', 'Account pools')}>
                <div className="console-panel flex flex-col">
                    <div className="flex items-center justify-between gap-3">
                        <h2 className="flex items-center gap-2 font-semibold"><Users className="h-5 w-5 text-blue-500" />{text('overview_google_pool', 'Google 账号池', 'Google account pool')}</h2>
                        <button className="text-sm font-medium text-blue-600 dark:text-blue-400" onClick={() => navigate('/accounts')}>{text('overview_manage', '管理', 'Manage')}<ArrowRight className="ml-1 inline h-4 w-4" /></button>
                    </div>
                    <p className="mt-2 text-xs console-muted">{text('overview_google_scope', 'Antigravity · Gemini / Claude 模型配额', 'Antigravity · Gemini / Claude model quotas')}</p>
                    <dl className="my-5 grid grid-cols-3 gap-3">
                        <div><dt className="text-xs console-muted">{t('dashboard.total_accounts')}</dt><dd className="mt-1 text-2xl font-semibold tabular-nums">{poolMetric(accounts.length)}</dd></div>
                        <div><dt className="text-xs console-muted">{text('overview_enabled', '已启用', 'Enabled')}</dt><dd className="mt-1 text-2xl font-semibold tabular-nums">{poolMetric(stats.enabled)}</dd></div>
                        <div><dt className="text-xs console-muted">{text('overview_account_issues', '禁用 / 受限', 'Disabled / restricted')}</dt><dd className={`mt-1 text-2xl font-semibold tabular-nums ${stats.issues ? 'text-amber-600 dark:text-amber-400' : ''}`}>{poolMetric(stats.issues)}</dd></div>
                    </dl>
                    {googleLoading ? <p role="status" className="text-sm console-muted">{t('common.loading')}</p> : googleError ? errorNotice(googleError) : <p className="text-xs console-muted">{accounts.length ? text('overview_enabled_note', '启用状态不等于模型可用；请检查各账号配额和限制。', 'Enabled does not guarantee model availability; check individual quotas and restrictions.') : text('overview_google_empty', '尚未添加 Google 账号，可在下方添加或前往账号池导入。', 'No Google accounts yet. Add one below or import in the account pool.')}</p>}
                </div>
                <div className="console-panel flex flex-col">
                    <div className="flex items-center justify-between gap-3">
                        <h2 className="flex items-center gap-2 font-semibold"><Bot className="h-5 w-5 text-blue-500" />{text('overview_codex_pool', 'Codex 账号池', 'Codex account pool')}</h2>
                        <button className="text-sm font-medium text-blue-600 dark:text-blue-400" onClick={() => navigate('/codex')}>{text('overview_manage', '管理', 'Manage')}<ArrowRight className="ml-1 inline h-4 w-4" /></button>
                    </div>
                    <p className="mt-2 text-xs console-muted">{text('overview_codex_scope', 'ChatGPT 订阅 · 独立的 5 小时与每周限额', 'ChatGPT subscriptions · separate five-hour and weekly limits')}</p>
                    {desktop ? <p className="my-5 text-sm console-muted">{text('overview_codex_web', 'Codex 管理仅在服务端 Web 控制台提供，请在浏览器中打开服务地址。', 'Codex management is available in the server web console. Open the server address in a browser.')}</p> : <>
                        <dl className="my-5 grid grid-cols-3 gap-3">
                            <div><dt className="text-xs console-muted">{t('dashboard.total_accounts')}</dt><dd className="mt-1 text-2xl font-semibold tabular-nums">{codex.data?.accounts.length ?? '—'}</dd></div>
                            <div><dt className="text-xs console-muted">{text('overview_enabled', '已启用', 'Enabled')}</dt><dd className="mt-1 text-2xl font-semibold tabular-nums">{codex.data?.accounts.filter(account => account.enabled).length ?? '—'}</dd></div>
                            <div><dt className="text-xs console-muted">{text('overview_last_errors', '有错误记录', 'With last error')}</dt><dd className="mt-1 text-2xl font-semibold tabular-nums">{codex.data?.accounts.filter(account => account.last_error).length ?? '—'}</dd></div>
                        </dl>
                        {codex.loading ? <p role="status" className="text-sm console-muted">{t('common.loading')}</p> : codex.error ? errorNotice(codex.error) : <p className="text-xs console-muted">{codex.data?.accounts.length ? text('overview_codex_limits', '在 Codex 账号池查看逐账号限额；不与 Google 模型配额合并。', 'View per-account limits in the Codex pool; they are not combined with Google model quotas.') : text('overview_codex_empty', '尚未添加 Codex 账号，前往账号池登录或导入订阅。', 'No Codex accounts yet. Open the pool to sign in or import a subscription.')}</p>}
                    </>}
                </div>
            </section>

            <nav className="grid grid-cols-1 gap-3 sm:grid-cols-3" aria-label={text('overview_quick_links', '快捷入口', 'Quick links')}>
                {[
                    { path: '/monitor', title: text('overview_requests', '请求日志', 'Request logs'), description: text('overview_requests_desc', '检查请求、响应与错误', 'Inspect requests, responses, and errors') },
                    { path: '/user-token', title: text('overview_tokens', '访问令牌', 'Access tokens'), description: text('overview_tokens_desc', '管理客户端接入凭证', 'Manage client access credentials') },
                    { path: '/api-guide', title: text('overview_guide', '接入指南', 'Connection guide'), description: text('overview_guide_desc', '配置客户端与 API 调用', 'Configure clients and API calls') },
                ].map(link => <button key={link.path} className="console-panel flex items-center justify-between gap-3 text-left hover:border-blue-400 transition-colors" onClick={() => navigate(link.path)}>
                    <span><span className="block text-sm font-semibold">{link.title}</span><span className="mt-1 block text-xs console-muted">{link.description}</span></span>
                    <ArrowRight className="h-4 w-4 shrink-0 text-blue-500" />
                </button>)}
            </nav>

            <section className="space-y-4">
                <div className="flex flex-wrap items-center justify-between gap-3 border-t border-gray-200 pt-6 dark:border-base-300">
                    <div><h2 className="text-lg font-semibold">{text('overview_google_workspace', 'Google 工作区', 'Google workspace')}</h2><p className="mt-1 text-sm console-muted">{text('overview_google_workspace_desc', '当前账号、模型配额与账号切换推荐，仅适用于 Google 账号池。', 'Current account, model quotas, and switching recommendations for the Google pool only.')}</p></div>
                    <div className="console-toolbar">
                        <AddAccountDialog onAdd={handleAddAccount} />
                        <button className="console-button" onClick={handleRefreshCurrent} disabled={isRefreshing || !currentAccount} title={t('dashboard.refresh_quota')}>
                            <RefreshCw className={`h-4 w-4 ${isRefreshing ? 'animate-spin' : ''}`} />{isRefreshing ? t('dashboard.refreshing') : t('dashboard.refresh_quota')}
                        </button>
                        <button className="console-button" onClick={handleExport}><Download className="h-4 w-4" />{t('dashboard.export_data')}</button>
                    </div>
                </div>
                <div className="console-panel">
                    <p className="mb-4 text-xs console-muted">{text('overview_quota_sample', '已知模型剩余额度的账号平均值，包含零额度；缺失数据不参与计算。', 'Average remaining quota across accounts with known model data, including zero; missing data is excluded.')}</p>
                    <dl className="grid grid-cols-2 gap-5 lg:grid-cols-4">
                        {[
                            { label: t('dashboard.avg_gemini'), value: stats.avgGemini },
                            { label: t('dashboard.avg_gemini_image'), value: stats.avgGeminiImage },
                            { label: t('dashboard.avg_claude'), value: stats.avgClaude },
                        ].map(metric => <div key={metric.label}><dt className="text-xs console-muted">{metric.label}</dt><dd className="mt-1 text-xl font-semibold tabular-nums">{googleLoading || googleError ? '—' : metric.value === null ? unavailable : `${metric.value}%`}</dd></div>)}
                        <div><dt className="text-xs console-muted">{t('dashboard.low_quota_accounts')}</dt><dd className="mt-1 text-xl font-semibold tabular-nums">{poolMetric(stats.lowQuota)}</dd><p className="mt-1 text-xs console-muted">{t('dashboard.quota_desc')}</p></div>
                    </dl>
                </div>
                {googleLoading ? <div className="console-panel text-sm console-muted" role="status">{t('common.loading')}</div> : <>
                    {currentError && errorNotice(currentError)}
                    {!googleError && <div className="grid grid-cols-1 gap-4 lg:grid-cols-2">
                        {!currentError && <CurrentAccount account={currentAccount} onSwitch={() => navigate('/accounts')} />}
                        <BestAccounts accounts={accounts} currentAccountId={currentAccount?.id} onSwitch={handleSwitch} />
                    </div>}
                </>}
            </section>
        </div>
    );
}

export default Dashboard;
