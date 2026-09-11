import React, { useEffect, useState, useRef, useMemo } from 'react';
import { listen } from '@tauri-apps/api/event';
import ModalDialog from '../common/ModalDialog';
import { useTranslation } from 'react-i18next';
import { request as invoke } from '../../utils/request';
import { Trash2, Search, X, Copy, CheckCircle, ChevronLeft, ChevronRight, RefreshCw, User } from 'lucide-react';

import { AppConfig } from '../../types/config';
import { formatCompactNumber } from '../../utils/format';
import { useAccountStore } from '../../stores/useAccountStore';
import { useConfigStore } from '../../stores/useConfigStore';
import { isTauri } from '../../utils/env';
import { copyToClipboard } from '../../utils/clipboard';


interface ProxyRequestLog {
    id: string;
    timestamp: number;
    method: string;
    url: string;
    status: number;
    duration: number;
    model?: string;
    mapped_model?: string;
    error?: string;
    request_body?: string;
    response_body?: string;
    input_tokens?: number;
    output_tokens?: number;
    cached_tokens?: number;
    account_email?: string;
    protocol?: string;  // "openai" | "anthropic" | "gemini"
}

interface ProxyStats {
    total_requests: number;
    success_count: number;
    error_count: number;
}

interface ProxyMonitorProps {
    className?: string;
}

// Log Table Component
interface LogTableProps {
    logs: ProxyRequestLog[];
    loading: boolean;
    onLogClick: (log: ProxyRequestLog) => void;
    t: any;
}

const LogTable: React.FC<LogTableProps> = ({
    logs,
    loading,
    onLogClick,
    t
}) => {
    return (
        <div
            className="flex-1 min-h-0 min-w-0 overflow-auto"
        >
            <table className="table table-sm w-full min-w-[1100px] text-sm">
                <thead className="bg-gray-50 dark:bg-base-200 text-gray-500 sticky top-0 z-10">
                    <tr>
                        <th style={{ width: '60px' }}>{t('monitor.table.status')}</th>
                        <th style={{ width: '60px' }}>{t('monitor.table.method')}</th>
                        <th style={{ width: '220px' }}>{t('monitor.table.model')}</th>
                        <th style={{ width: '70px' }}>{t('monitor.table.protocol')}</th>
                        <th style={{ width: '140px' }}>{t('monitor.table.account')}</th>
                        <th style={{ width: '180px' }}>{t('monitor.table.path')}</th>
                        <th className="text-right" style={{ width: '90px' }}>{t('monitor.table.usage')}</th>
                        <th className="text-right" style={{ width: '80px' }}>{t('monitor.table.duration')}</th>
                        <th className="text-right" style={{ width: '80px' }}>{t('monitor.table.time')}</th>
                    </tr>
                </thead>
                <tbody className="font-mono text-gray-700 dark:text-gray-300">
                    {logs.map((log) => (
                        <tr
                            key={log.id}
                            className="hover:bg-blue-50 dark:hover:bg-blue-900/20 cursor-pointer"
                            onClick={() => onLogClick(log)}
                            tabIndex={0}
                            onKeyDown={(event) => {
                                if (event.key === 'Enter' || event.key === ' ') {
                                    event.preventDefault();
                                    onLogClick(log);
                                }
                            }}
                        >
                            <td style={{ width: '60px' }}>
                                <span className={`badge badge-xs text-white border-none ${log.status >= 200 && log.status < 400 && !log.error ? 'badge-success' : 'badge-error'}`} title={log.error}>
                                    {log.status}
                                </span>
                            </td>
                            <td className="font-bold" style={{ width: '60px' }}>{log.method}</td>
                            <td className="text-blue-600 truncate" style={{ width: '220px', maxWidth: '220px' }}>
                                {log.mapped_model && log.model !== log.mapped_model
                                    ? `${log.model} => ${log.mapped_model}`
                                    : (log.model || '-')}
                            </td>
                            <td style={{ width: '70px' }}>
                                {log.protocol && (
                                    <span className={`badge badge-xs text-white border-none ${log.protocol === 'openai' ? 'bg-green-500' :
                                        log.protocol === 'anthropic' ? 'bg-orange-500' :
                                            log.protocol === 'gemini' ? 'bg-blue-500' : 'bg-gray-400'
                                        }`}>
                                        {log.protocol === 'openai' ? 'OpenAI' :
                                            log.protocol === 'anthropic' ? 'Claude' :
                                                log.protocol === 'gemini' ? 'Gemini' : log.protocol}
                                    </span>
                                )}
                            </td>
                            <td className="text-gray-600 dark:text-gray-400 truncate text-xs" style={{ width: '140px', maxWidth: '140px' }} title={log.account_email || ''}>
                                {log.account_email ? log.account_email.replace(/(.{3}).*(@.*)/, '$1***$2') : '-'}
                            </td>
                            <td className="truncate" style={{ width: '180px', maxWidth: '180px' }}>{log.url}</td>
                            <td className="text-right text-xs" style={{ width: '90px' }}>
                                {log.input_tokens != null && <div>{t('monitor.input')}: {formatCompactNumber(log.input_tokens)}</div>}
                                {log.output_tokens != null && <div>{t('monitor.output')}: {formatCompactNumber(log.output_tokens)}</div>}
                            </td>
                            <td className="text-right" style={{ width: '80px' }}>{log.duration}ms</td>
                            <td className="text-right text-xs" style={{ width: '80px' }}>
                                {new Date(log.timestamp).toLocaleTimeString()}
                            </td>
                        </tr>
                    ))}
                </tbody>
            </table>

            {/* Loading indicator */}
            {loading && (
                <div className="flex items-center justify-center p-4 bg-white dark:bg-base-100">
                    <div className="loading loading-spinner loading-md"></div>
                    <span className="ml-3 text-sm text-gray-500">{t('common.loading')}</span>
                </div>
            )}

            {/* Empty state */}
            {!loading && logs.length === 0 && (
                <div className="flex items-center justify-center p-8 text-gray-400">
                    {t('monitor.table.empty') || '暂无请求记录'}
                </div>
            )}
        </div>
    );
};


export const ProxyMonitor: React.FC<ProxyMonitorProps> = ({ className }) => {
    const { t } = useTranslation();
    const [logs, setLogs] = useState<ProxyRequestLog[]>([]);
    const [stats, setStats] = useState<ProxyStats>({ total_requests: 0, success_count: 0, error_count: 0 });
    const [filter, setFilter] = useState('');
    const [accountFilter, setAccountFilter] = useState('');
    const [reloadSequence, setReloadSequence] = useState(0);
    const [knownAccounts, setKnownAccounts] = useState<string[]>([]);
    const [accountsLoadError, setAccountsLoadError] = useState(false);
    const dataGeneration = useRef(0);
    const detailGeneration = useRef(0);
    const dataLoading = useRef(false);
    const refreshPending = useRef(false);
    const loggingGeneration = useRef(0);
    const [loggingBusy, setLoggingBusy] = useState(false);
    const [actionError, setActionError] = useState<string | null>(null);
    const [selectedLog, setSelectedLog] = useState<ProxyRequestLog | null>(null);
    const [isLoggingEnabled, setIsLoggingEnabled] = useState(false);
    const [isClearConfirmOpen, setIsClearConfirmOpen] = useState(false);
    const [copiedRequestId, setCopiedRequestId] = useState<string | null>(null);

    const { accounts, fetchAccounts } = useAccountStore();

    // Pagination state
    const PAGE_SIZE_OPTIONS = [50, 100, 200, 500];
    const [pageSize, setPageSize] = useState(100);
    const [currentPage, setCurrentPage] = useState(1);
    const [totalCount, setTotalCount] = useState(0);
    const [loading, setLoading] = useState(false);
    const [loadingDetail, setLoadingDetail] = useState(false);
    const [loadError, setLoadError] = useState(false);

    const uniqueAccounts = useMemo(() => {
        const emailSet = new Set(knownAccounts);
        logs.forEach(log => {
            if (log.account_email) {
                emailSet.add(log.account_email);
            }
        });
        accounts.forEach(acc => {
            emailSet.add(acc.email);
        });
        if (accountFilter) emailSet.add(accountFilter);
        return Array.from(emailSet).sort();
    }, [logs, accounts, knownAccounts, accountFilter]);

    const reloadData = () => setReloadSequence(sequence => sequence + 1);

    const totalPages = Math.ceil(totalCount / pageSize);
    const pageStart = totalCount === 0 ? 0 : (currentPage - 1) * pageSize + 1;
    const pageEnd = totalCount === 0 ? 0 : Math.min(currentPage * pageSize, totalCount);

    const goToPage = (page: number) => {
        if (page >= 1 && page <= totalPages && page !== currentPage) {
            setCurrentPage(page);
            setLoadError(false);
        }
    };

    const toggleLogging = async () => {
        if (loggingBusy) return;
        const newState = !isLoggingEnabled;
        loggingGeneration.current += 1;
        setLoggingBusy(true);
        setActionError(null);
        try {
            if (!useConfigStore.getState().config) await useConfigStore.getState().loadConfig();
            await useConfigStore.getState().updateConfig(config => ({
                ...config,
                proxy: { ...config.proxy, enable_logging: newState },
            }), true);
            await invoke('set_proxy_monitor_enabled', { enabled: newState });
            setIsLoggingEnabled(newState);
        } catch (e) {
            console.error('Failed to toggle logging', e);
            setActionError(e instanceof Error ? e.message : String(e));
        } finally {
            loggingGeneration.current += 1;
            setLoggingBusy(false);
            reloadData();
        }
    };

    useEffect(() => {
        const generation = ++dataGeneration.current;
        const loggingRevision = loggingGeneration.current;
        dataLoading.current = true;
        refreshPending.current = false;
        const controller = new AbortController();
        let active = true;
        let timeoutId: number | undefined;
        const current = () => active && generation === dataGeneration.current;
        const timeout = new Promise<never>((_, reject) => {
            timeoutId = window.setTimeout(() => {
                controller.abort();
                reject(new Error('Request timeout'));
            }, 10000);
        });

        const load = async () => {
            setLoading(true);
            setLoadError(false);
            const query = {
                filter: filter === '__ERROR__' ? '' : filter,
                errorsOnly: filter === '__ERROR__',
                accountEmail: accountFilter || undefined,
            };
            try {
                const [config, count, currentStats] = await Promise.race([
                    Promise.all([
                        invoke<AppConfig>('load_config', undefined, { signal: controller.signal }),
                        invoke<number>('get_proxy_logs_count_filtered', query, { signal: controller.signal }),
                        invoke<ProxyStats>('get_proxy_stats', undefined, { signal: controller.signal }),
                    ]),
                    timeout,
                ]);
                if (!current()) return;
                const page = Math.min(currentPage, Math.max(1, Math.ceil(count / pageSize)));
                if (page !== currentPage) {
                    setCurrentPage(page);
                    return;
                }
                const history = await Promise.race([
                    invoke<ProxyRequestLog[]>('get_proxy_logs_filtered', {
                        ...query,
                        limit: pageSize,
                        offset: (page - 1) * pageSize,
                    }, { signal: controller.signal }),
                    timeout,
                ]);
                if (!current()) return;
                setLogs(history);
                setTotalCount(count);
                setStats(currentStats);
                if (loggingRevision === loggingGeneration.current) {
                    setIsLoggingEnabled(config.proxy.enable_logging);
                }
            } catch (error) {
                if (current()) {
                    setLoadError(true);
                    console.error('Failed to load proxy data', error);
                }
            } finally {
                window.clearTimeout(timeoutId);
                if (current()) {
                    dataLoading.current = false;
                    setLoading(false);
                    if (refreshPending.current) {
                        refreshPending.current = false;
                        setReloadSequence(sequence => sequence + 1);
                    }
                }
            }
        };
        void load();
        return () => {
            active = false;
            controller.abort();
            window.clearTimeout(timeoutId);
        };
    }, [currentPage, pageSize, filter, accountFilter, reloadSequence]);

    useEffect(() => {
        const controller = new AbortController();
        let active = true;
        setAccountsLoadError(false);
        void Promise.all([
            invoke<string[]>('get_proxy_log_accounts', undefined, { signal: controller.signal }),
            isTauri()
                ? Promise.resolve({ accounts: [] })
                : invoke<{ accounts: { id: string; email: string | null }[] }>(
                    'codex_list_accounts', undefined, { signal: controller.signal }),
        ]).then(([history, codex]) => {
            if (active) setKnownAccounts([...history, ...codex.accounts.map(account => account.email ?? account.id)]);
        }).catch(error => {
            if (active) {
                setAccountsLoadError(true);
                console.error('Failed to load log account options', error);
            }
        });
        return () => {
            active = false;
            controller.abort();
        };
    }, [reloadSequence]);

    useEffect(() => {
        void fetchAccounts();
        let active = true;
        let unlisten: (() => void) | undefined;
        let updateTimeout: number | undefined;
        const refreshWhenIdle = () => {
            if (!active) return;
            if (dataLoading.current) refreshPending.current = true;
            else setReloadSequence(sequence => sequence + 1);
        };
        // Refresh authoritative filtered pages instead of prepending unfiltered events.
        if (isTauri()) {
            void listen<ProxyRequestLog>('proxy://request', () => {
                if (!active || updateTimeout !== undefined) return;
                updateTimeout = window.setTimeout(() => {
                    updateTimeout = undefined;
                    refreshWhenIdle();
                }, 500);
            }).then(dispose => {
                if (active) unlisten = dispose;
                else dispose();
            }).catch(error => console.error('Failed to listen for proxy requests', error));
        }
        const pollInterval = window.setInterval(refreshWhenIdle, 10000);
        return () => {
            active = false;
            detailGeneration.current += 1;
            unlisten?.();
            window.clearTimeout(updateTimeout);
            window.clearInterval(pollInterval);
        };
    }, [fetchAccounts]);

    useEffect(() => {
        setCopiedRequestId(null);
    }, [selectedLog?.id]);

    const closeDetail = () => {
        detailGeneration.current += 1;
        setSelectedLog(null);
        setLoadingDetail(false);
    };

    const quickFilters = [
        { label: t('monitor.filters.all'), value: '' },
        { label: t('monitor.filters.error'), value: '__ERROR__' },
        { label: t('monitor.filters.chat'), value: 'completions' },
        { label: t('monitor.filters.gemini'), value: 'gemini' },
        { label: t('monitor.filters.claude'), value: 'claude' },
        { label: t('monitor.filters.images'), value: 'images' }
    ];

    const clearLogs = () => {
        setIsClearConfirmOpen(true);
    };

    const executeClearLogs = async () => {
        setIsClearConfirmOpen(false);
        setActionError(null);
        try {
            dataGeneration.current += 1;
            await invoke('clear_proxy_logs');
            setLogs([]);
            setStats({ total_requests: 0, success_count: 0, error_count: 0 });
            setTotalCount(0);
            setCurrentPage(1);
            reloadData();
        } catch (e) {
            console.error("Failed to clear logs", e);
            setActionError(e instanceof Error ? e.message : String(e));
            reloadData();
        }
    };

    const formatBody = (body?: string) => {
        if (!body) return <span className="text-gray-400 italic">{t('monitor.details.payload_empty')}</span>;
        try {
            const obj = JSON.parse(body);
            return <pre className="text-xs font-mono whitespace-pre-wrap break-all text-gray-700 dark:text-gray-300">{JSON.stringify(obj, null, 2)}</pre>;
        } catch (e) {
            return <pre className="text-xs font-mono whitespace-pre-wrap break-all text-gray-700 dark:text-gray-300">{body}</pre>;
        }
    };

    const getCopyPayload = (body: string) => {
        try {
            const obj = JSON.parse(body);
            return JSON.stringify(obj, null, 2);
        } catch (e) {
            return body;
        }
    };


    return (
        <div className={`console-panel !p-0 min-w-0 flex flex-col overflow-hidden ${className || 'flex-1'}`}>
            <div className="p-4 border-b border-[var(--console-border)] space-y-4">
                <div className="console-toolbar">
                    <button
                        onClick={toggleLogging}
                        aria-pressed={isLoggingEnabled}
                        disabled={loggingBusy}
                        aria-busy={loggingBusy}
                        className={`console-button ${isLoggingEnabled ? 'text-emerald-700 dark:text-emerald-400' : ''}`}
                    >
                        <div className={`w-2 h-2 rounded-full ${isLoggingEnabled ? 'bg-emerald-500' : 'bg-gray-400'}`} />
                        {isLoggingEnabled ? t('monitor.logging_status.active') : t('monitor.logging_status.paused')}
                    </button>

                    <div className="relative flex-1 min-w-[180px]">
                        <Search className="absolute left-2.5 top-2 text-gray-400" size={14} />
                        <input
                            type="text"
                            placeholder={t('monitor.filters.placeholder')}
                            aria-label={t('monitor.filters.placeholder')}
                            className="input input-sm input-bordered w-full pl-9 text-xs"
                            value={filter}
                            onChange={(e) => { setFilter(e.target.value); setCurrentPage(1); }}
                        />
                    </div>

                    <div className="relative">
                        <User className="absolute left-2.5 top-2 text-gray-400 z-10" size={14} />
                        <select
                            className="select select-sm select-bordered pl-8 text-xs min-w-[140px] max-w-[220px]"
                            value={accountFilter}
                            onChange={(e) => { setAccountFilter(e.target.value); setCurrentPage(1); }}
                            title={t('monitor.filters.by_account')}
                        >
                            <option value="">{t('monitor.filters.all_accounts')}</option>
                            {uniqueAccounts.map(email => (
                                <option key={email} value={email} title={email}>
                                    {email}
                                </option>
                            ))}
                        </select>
                    </div>

                    <div className="flex flex-wrap gap-4 text-xs font-medium">
                        <span className="text-blue-500">{formatCompactNumber(stats.total_requests)} {t('monitor.stats.total')}</span>
                        <span className="text-green-500">{formatCompactNumber(stats.success_count)} {t('monitor.stats.ok')}</span>
                        <span className="text-red-500">{formatCompactNumber(stats.error_count)} {t('monitor.stats.err')}</span>
                    </div>

                    <button onClick={reloadData} disabled={loading} className="console-button" title={t('common.refresh')}>
                        <RefreshCw size={16} className={loading ? 'animate-spin' : ''} />
                    </button>
                    <button onClick={clearLogs} className="console-button text-error" title={t('common.delete')}>
                        <Trash2 size={16} />
                    </button>
                </div>

                <div className="flex flex-wrap items-center gap-2">
                    <span className="text-xs font-medium text-gray-500">{t('monitor.filters.quick_filters')}</span>
                    {quickFilters.map(q => (
                        <button key={q.label} onClick={() => { setFilter(q.value); setCurrentPage(1); }} aria-pressed={filter === q.value} className={`console-button !py-1 !px-3 ${filter === q.value ? 'console-button-primary' : ''}`}>
                            {q.label}
                        </button>
                    ))}
                    {(filter || accountFilter) && <button onClick={() => { setFilter(''); setAccountFilter(''); setCurrentPage(1); }} className="console-button"> {t('monitor.filters.reset')} </button>}
                </div>
            </div>
            {(loadError || accountsLoadError || actionError) && <div role="alert" className="p-4 text-sm text-error">{actionError || t('common.load_failed')}</div>}

            <LogTable
                logs={logs}
                loading={loading}
                onLogClick={async (log: ProxyRequestLog) => {
                    const generation = ++detailGeneration.current;
                    setSelectedLog(log);
                    setLoadingDetail(true);
                    try {
                        const detail = await invoke<ProxyRequestLog>('get_proxy_log_detail', { logId: log.id });
                        if (generation === detailGeneration.current) setSelectedLog(detail);
                    } catch (e) {
                        console.error('Failed to load log detail', e);
                        if (generation === detailGeneration.current) setLoadError(true);
                    } finally {
                        if (generation === detailGeneration.current) setLoadingDetail(false);
                    }
                }}
                t={t}
            />

            {/* Pagination Controls */}
            <div className="flex flex-wrap shrink-0 items-center justify-between gap-3 px-4 py-3 border-t border-[var(--console-border)] text-xs">
                <div className="flex items-center gap-2 whitespace-nowrap">
                    <span className="text-gray-500">{t('common.per_page')}</span>
                    <select
                        value={pageSize}
                        onChange={(e) => { setPageSize(Number(e.target.value)); setCurrentPage(1); }}
                        className="select select-xs select-bordered w-16"
                    >
                        {PAGE_SIZE_OPTIONS.map(size => (
                            <option key={size} value={size}>{size}</option>
                        ))}
                    </select>
                </div>

                <div className="flex items-center gap-3">
                    <button
                        onClick={() => goToPage(currentPage - 1)}
                        aria-label={t('security.logs.prev_page')}
                        disabled={currentPage <= 1 || loading}
                        className="btn btn-xs btn-ghost"
                    >
                        <ChevronLeft size={14} />
                    </button>
                    <span className="text-gray-600 dark:text-gray-400 min-w-[80px] text-center">
                        {currentPage} / {totalPages || 1}
                    </span>
                    <button
                        onClick={() => goToPage(currentPage + 1)}
                        aria-label={t('security.logs.next_page')}
                        disabled={currentPage >= totalPages || loading}
                        className="btn btn-xs btn-ghost"
                    >
                        <ChevronRight size={14} />
                    </button>
                </div>

                <div className="text-gray-500">
                    {t('common.pagination_info', { start: pageStart, end: pageEnd, total: totalCount })}
                </div>
            </div>

            {selectedLog && (
                <div className="fixed inset-0 z-50 flex items-center justify-center bg-black/60 backdrop-blur-sm p-4" onClick={closeDetail}>
                    <div className="bg-white dark:bg-base-100 rounded-xl shadow-2xl w-full max-w-4xl max-h-[90vh] flex flex-col overflow-hidden border border-gray-200 dark:border-base-300" onClick={e => e.stopPropagation()}>
                        {/* Modal Header */}
                        <div className="px-4 py-3 border-b border-gray-100 dark:border-base-300 flex items-center justify-between bg-gray-50 dark:bg-base-200">
                            <div className="flex items-center gap-3">
                                {loadingDetail && <div className="loading loading-spinner loading-sm"></div>}
                                <span className={`badge badge-sm text-white border-none ${selectedLog.status >= 200 && selectedLog.status < 400 && !selectedLog.error ? 'badge-success' : 'badge-error'}`} title={selectedLog.error}>{selectedLog.status}</span>
                                <span className="font-mono font-bold text-gray-900 dark:text-base-content text-sm">{selectedLog.method}</span>
                                <span className="text-xs text-gray-500 dark:text-gray-400 font-mono truncate max-w-md hidden sm:inline">{selectedLog.url}</span>
                            </div>
                            <button onClick={closeDetail} aria-label={t('common.close')} className="btn btn-ghost btn-sm btn-circle text-gray-500 dark:text-gray-400 hover:dark:bg-base-300"><X size={18} /></button>
                        </div>

                        {/* Modal Content */}
                        <div className="flex-1 overflow-y-auto p-4 space-y-6 bg-white dark:bg-base-100">
                            {/* Metadata Section */}
                            <div className="bg-gray-50 dark:bg-base-200 p-5 rounded-xl border border-gray-200 dark:border-base-300 shadow-inner">
                                <div className="grid grid-cols-1 sm:grid-cols-2 lg:grid-cols-3 gap-y-5 gap-x-10">
                                    <div className="space-y-1.5">
                                        <span className="block text-gray-500 dark:text-gray-400 uppercase font-black text-[10px] tracking-widest">{t('monitor.details.time')}</span>
                                        <span className="font-mono font-semibold text-gray-900 dark:text-base-content text-xs">{new Date(selectedLog.timestamp).toLocaleString()}</span>
                                    </div>
                                    <div className="space-y-1.5">
                                        <span className="block text-gray-500 dark:text-gray-400 uppercase font-black text-[10px] tracking-widest">{t('monitor.details.duration')}</span>
                                        <span className="font-mono font-semibold text-gray-900 dark:text-base-content text-xs">{selectedLog.duration}ms</span>
                                    </div>
                                    <div className="space-y-1.5">
                                        <span className="block text-gray-500 dark:text-gray-400 uppercase font-black text-[10px] tracking-widest">{t('monitor.details.tokens')}</span>
                                        <div className="font-mono text-[11px] flex gap-2">
                                            <span className="text-blue-700 dark:text-blue-300 bg-blue-100 dark:bg-blue-900/40 px-2.5 py-1 rounded-md border border-blue-200 dark:border-blue-800/50 font-bold">In: {formatCompactNumber(selectedLog.input_tokens ?? 0)}</span>
                                            <span className="text-green-700 dark:text-green-300 bg-green-100 dark:bg-green-900/40 px-2.5 py-1 rounded-md border border-green-200 dark:border-green-800/50 font-bold">Out: {formatCompactNumber(selectedLog.output_tokens ?? 0)}</span>
                                        </div>
                                    </div>
                                </div>
                                <div className="mt-5 pt-5 border-t border-gray-200 dark:border-base-300">
                                    <div className="grid grid-cols-1 sm:grid-cols-2 lg:grid-cols-3 gap-5">
                                        {selectedLog.protocol && (
                                            <div className="space-y-1.5">
                                                <span className="block text-gray-500 dark:text-gray-400 uppercase font-black text-[10px] tracking-widest">{t('monitor.details.protocol')}</span>
                                                <span className={`inline-block px-2.5 py-1 rounded-md font-mono font-black text-xs uppercase ${selectedLog.protocol === 'openai' ? 'bg-emerald-100 text-emerald-700 dark:bg-emerald-900/40 dark:text-emerald-400 border border-emerald-200 dark:border-emerald-800/50' :
                                                    selectedLog.protocol === 'anthropic' ? 'bg-orange-100 text-orange-700 dark:bg-orange-900/40 dark:text-orange-400 border border-orange-200 dark:border-orange-800/50' :
                                                        selectedLog.protocol === 'gemini' ? 'bg-blue-100 text-blue-700 dark:bg-blue-900/40 dark:text-blue-400 border border-blue-200 dark:border-blue-800/50' :
                                                            'bg-gray-100 text-gray-700 dark:bg-gray-900/40 dark:text-gray-400'
                                                    }`}>
                                                    {selectedLog.protocol}
                                                </span>
                                            </div>
                                        )}
                                        <div className="space-y-1.5">
                                            <span className="block text-gray-500 dark:text-gray-400 uppercase font-black text-[10px] tracking-widest">{t('monitor.details.model')}</span>
                                            <span className="font-mono font-black text-blue-600 dark:text-blue-400 break-all text-sm">{selectedLog.model || '-'}</span>
                                        </div>
                                        {selectedLog.mapped_model && selectedLog.model !== selectedLog.mapped_model && (
                                            <div className="space-y-1.5">
                                                <span className="block text-gray-500 dark:text-gray-400 uppercase font-black text-[10px] tracking-widest">{t('monitor.details.mapped_model')}</span>
                                                <span className="font-mono font-black text-green-600 dark:text-green-400 break-all text-sm">{selectedLog.mapped_model}</span>
                                            </div>
                                        )}
                                    </div>
                                </div>
                                {selectedLog.account_email && (
                                    <div className="mt-5 pt-5 border-t border-gray-200 dark:border-base-300">
                                        <span className="block text-gray-500 dark:text-gray-400 uppercase font-black text-[10px] tracking-widest mb-2">{t('monitor.details.account_used')}</span>
                                        <span className="font-mono font-semibold break-all text-gray-900 dark:text-base-content text-xs">{selectedLog.account_email}</span>
                                    </div>
                                )}
                            </div>

                            {/* Payloads */}
                            <div className="space-y-4">
                                <div>
                                    <div className="flex items-center justify-between mb-2">
                                        <h3 className="text-xs font-bold uppercase text-gray-400 flex items-center gap-2">{t('monitor.details.request_payload')}</h3>
                                        <button
                                            type="button"
                                            className="btn btn-ghost btn-xs gap-1"
                                            onClick={async () => {
                                                if (!selectedLog.request_body) return;
                                                const success = await copyToClipboard(getCopyPayload(selectedLog.request_body));
                                                if (success) {
                                                    setCopiedRequestId(selectedLog.id);
                                                    setTimeout(() => {
                                                        setCopiedRequestId((current) => (current === selectedLog.id ? null : current));
                                                    }, 2000);
                                                }
                                            }}
                                            disabled={!selectedLog.request_body}
                                            title={copiedRequestId === selectedLog.id ? t('proxy.config.btn_copied') : t('proxy.config.btn_copy')}
                                            aria-label={t('proxy.config.btn_copy')}
                                        >
                                            {copiedRequestId === selectedLog.id ? (
                                                <CheckCircle size={12} className="text-green-500" />
                                            ) : (
                                                <Copy size={12} />
                                            )}
                                            <span className="text-[10px]">
                                                {copiedRequestId === selectedLog.id ? t('proxy.config.btn_copied') : t('proxy.config.btn_copy')}
                                            </span>
                                        </button>
                                    </div>
                                    <div className="bg-gray-50 dark:bg-base-300 rounded-lg p-3 border border-gray-100 dark:border-base-300 overflow-hidden">{formatBody(selectedLog.request_body)}</div>
                                </div>
                                <div>
                                    <div className="flex items-center justify-between mb-2">
                                        <h3 className="text-xs font-bold uppercase text-gray-400 flex items-center gap-2">{t('monitor.details.response_payload')}</h3>
                                        <button
                                            type="button"
                                            className="btn btn-ghost btn-xs gap-1"
                                            onClick={async () => {
                                                if (!selectedLog.response_body) return;
                                                const success = await copyToClipboard(getCopyPayload(selectedLog.response_body));
                                                if (success) {
                                                    setCopiedRequestId(selectedLog.id ? `${selectedLog.id}-response` : null);
                                                    setTimeout(() => {
                                                        setCopiedRequestId((current) =>
                                                            current === `${selectedLog.id}-response` ? null : current
                                                        );
                                                    }, 2000);
                                                }
                                            }}
                                            disabled={!selectedLog.response_body}
                                            title={copiedRequestId === `${selectedLog.id}-response` ? t('proxy.config.btn_copied') : t('proxy.config.btn_copy')}
                                            aria-label={t('proxy.config.btn_copy')}
                                        >
                                            {copiedRequestId === `${selectedLog.id}-response` ? (
                                                <CheckCircle size={12} className="text-green-500" />
                                            ) : (
                                                <Copy size={12} />
                                            )}
                                            <span className="text-[10px]">
                                                {copiedRequestId === `${selectedLog.id}-response` ? t('proxy.config.btn_copied') : t('proxy.config.btn_copy')}
                                            </span>
                                        </button>
                                    </div>
                                    <div className="bg-gray-50 dark:bg-base-300 rounded-lg p-3 border border-gray-100 dark:border-base-300 overflow-hidden">{formatBody(selectedLog.response_body)}</div>
                                </div>
                            </div>
                        </div>
                    </div>
                </div>
            )}

            <ModalDialog
                isOpen={isClearConfirmOpen}
                title={t('monitor.dialog.clear_title')}
                message={t('monitor.dialog.clear_msg')}
                type="confirm"
                confirmText={t('common.delete')}
                isDestructive={true}
                onConfirm={executeClearLogs}
                onCancel={() => setIsClearConfirmOpen(false)}
            />
        </div>
    );
};
