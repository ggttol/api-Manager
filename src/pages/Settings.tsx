import { useState, useEffect, useRef } from 'react';
import { Save, User, RefreshCw, LayoutDashboard, Users, Network, Activity, BarChart3, Settings as SettingsIcon, Lock, CheckCircle2, Globe } from 'lucide-react';
import { request as invoke } from '../utils/request';
import { open } from '@tauri-apps/plugin-dialog';
import { useConfigStore } from '../stores/useConfigStore';
import { AppConfig } from '../types/config';
import ModalDialog from '../components/common/ModalDialog';
import { showToast } from '../components/common/ToastContainer';
import QuotaProtection from '../components/settings/QuotaProtection';
import SmartWarmup from '../components/settings/SmartWarmup';
import PinnedQuotaModels from '../components/settings/PinnedQuotaModels';
import { useDebugConsole } from '../stores/useDebugConsole';

import { useTranslation } from 'react-i18next';
import { isTauri } from '../utils/env';

import DebugConsole from '../components/debug/DebugConsole';
import ProxyPoolSettings from '../components/settings/ProxyPoolSettings';
import { PageHeader } from '../components/common/ConsolePage';


const formatArguments = (args?: string[] | null) => (args ?? []).map(arg => JSON.stringify(arg)).join(' ');


function Settings() {
    const { t, i18n } = useTranslation();
    const { config, loading, error: configError, loadConfig, saveConfig, updateConfig } = useConfigStore();
    const { enable, disable, isEnabled } = useDebugConsole();
    const [activeTab, setActiveTab] = useState<'general' | 'account' | 'proxy' | 'advanced' | 'debug'>('general');
    const [formData, setFormDataState] = useState<AppConfig>({
        language: 'zh',
        theme: 'system',
        auto_refresh: false,
        refresh_interval: 15,
        auto_sync: false,
        sync_interval: 5,
        proxy: {
            enabled: false,
            port: 8080,
            api_key: '',
            auto_start: false,
            request_timeout: 120,
            enable_logging: false,
            upstream_proxy: {
                enabled: false,
                url: ''
            },
            debug_logging: {
                enabled: false,
                output_dir: undefined
            } as { enabled: boolean; output_dir?: string },
            proxy_pool: {
                enabled: false,
                proxies: [],
                health_check_interval: 300,
                auto_failover: true,
                strategy: 'priority',
                account_bindings: {}
            }
        },
        scheduled_warmup: {
            enabled: false,
            monitored_models: []
        },
        quota_protection: {
            enabled: false,
            threshold_percentage: 10,
            monitored_models: []
        },
        pinned_quota_models: {
            models: ['gemini-pro-agent', 'gemini-3-flash-agent', 'gemini-3.1-flash-image', 'claude-opus-4-6-thinking']
        },
        cloudflared: {
            enabled: false,
            mode: 'quick',
            port: 7860,
            use_http2: true
        },
        circuit_breaker: {
            enabled: false,
            backoff_steps: [30, 60, 120, 300, 600]
        },
        hidden_menu_items: [],  // 菜单显示设置：默认不隐藏任何菜单项

    });
    const [hydrated, setHydrated] = useState(false);
    const [rawAntigravityArgs, setRawAntigravityArgs] = useState('');
    const dirtyRef = useRef(false);
    const draftBaselineRef = useRef<AppConfig | null>(null);

    const mergeDirtyFields = (latest: AppConfig, draft: AppConfig, baseline: AppConfig): AppConfig => {
        const merge = (current: unknown, candidate: unknown, original: unknown): unknown => {
            if (Object.is(candidate, original)) return current;
            if (Array.isArray(candidate) || candidate === null || typeof candidate !== 'object') return candidate;
            if (current === null || typeof current !== 'object' || Array.isArray(current)
                || original === null || typeof original !== 'object' || Array.isArray(original)) return candidate;
            const result: Record<string, unknown> = { ...(current as Record<string, unknown>) };
            for (const key of Object.keys(candidate as Record<string, unknown>)) {
                result[key] = merge(
                    (current as Record<string, unknown>)[key],
                    (candidate as Record<string, unknown>)[key],
                    (original as Record<string, unknown>)[key],
                );
            }
            return result;
        };
        return merge(latest, draft, baseline) as AppConfig;
    };

    const updateDraft = (update: AppConfig | ((current: AppConfig) => AppConfig)) => {
        if (!hydrated) return;
        dirtyRef.current = true;
        setFormDataState(current => typeof update === 'function' ? update(current) : update);
    };

    const setFormData = updateDraft;


    const parseArguments = (input: string): string[] => {
        const args: string[] = [];
        let index = 0;
        while (index < input.length) {
            while (/\s/.test(input[index] ?? '')) index += 1;
            if (index >= input.length) break;
            if (input[index] === '"') {
                let end = index + 1;
                let escaped = false;
                while (end < input.length) {
                    const character = input[end++];
                    if (!escaped && character === '"') break;
                    escaped = !escaped && character === '\\';
                    if (character !== '\\') escaped = false;
                }
                const quoted = input.slice(index, end);
                try {
                    args.push(JSON.parse(quoted));
                    index = end;
                    continue;
                } catch {
                    // Treat malformed quoted input as a single literal token.
                }
            }
            const start = index;
            while (index < input.length && !/\s/.test(input[index])) index += 1;
            args.push(input.slice(start, index));
        }
        return args;
    };

    const commitArguments = () => {
        if (!hydrated) return;
        const args = parseArguments(rawAntigravityArgs);
        updateDraft(current => ({ ...current, antigravity_args: args }));
    };

    // Dialog state
    // Dialog state
    const [isClearLogsOpen, setIsClearLogsOpen] = useState(false);
    const [dataDirPath, setDataDirPath] = useState<string>('~/.antigravity_tools/');

    // Antigravity cache clearing state
    const [isClearCacheOpen, setIsClearCacheOpen] = useState(false);
    const [cachePaths, setCachePaths] = useState<string[]>([]);
    const [isClearingCache, setIsClearingCache] = useState(false);


    useEffect(() => {
        loadConfig();
        invoke<string>('get_data_dir_path')
            .then(path => setDataDirPath(path))
            .catch(err => console.error('Failed to get data dir:', err));
        if (isTauri()) {
            invoke<boolean>('is_auto_launch_enabled')
                .then(enabled => setFormDataState(prev => ({ ...prev, auto_launch: enabled })))
                .catch(err => console.error('Failed to get auto launch status:', err));
        }
    }, [loadConfig]);

    useEffect(() => {
        if (!config) return;
        if (!hydrated) {
            draftBaselineRef.current = config;
            setFormDataState(config);
            setRawAntigravityArgs(formatArguments(config.antigravity_args));
            setHydrated(true);
            return;
        }
        if (dirtyRef.current && draftBaselineRef.current) {
            setFormDataState(current => mergeDirtyFields(config, current, draftBaselineRef.current!));
        } else {
            draftBaselineRef.current = config;
            setFormDataState(config);
            setRawAntigravityArgs(formatArguments(config.antigravity_args));
        }
    }, [config, hydrated]);

    const handleSave = async () => {
        if (!hydrated || !draftBaselineRef.current) return;
        const draft = { ...formData, antigravity_args: parseArguments(rawAntigravityArgs) };
        const savedFormData = mergeDirtyFields(config ?? formData, draft, draftBaselineRef.current);
        const proxyEnabled = savedFormData.proxy?.upstream_proxy?.enabled;
        const proxyUrl = savedFormData.proxy?.upstream_proxy?.url?.trim();
        if (proxyEnabled && !proxyUrl) {
            showToast(t('proxy.config.upstream_proxy.validation_error'), 'error');
            return;
        }
        try {
            await updateConfig(latest => mergeDirtyFields(latest, draft, draftBaselineRef.current!));
            dirtyRef.current = false;
            draftBaselineRef.current = savedFormData;
            setFormDataState(savedFormData);
            showToast(t('common.saved'), 'success');
            if (proxyEnabled && proxyUrl) showToast(t('proxy.config.upstream_proxy.restart_hint'), 'info');
        } catch (error) {
            showToast(`${t('common.error')}: ${error}`, 'error');
        }
    };

    const confirmClearLogs = async () => {
        try {
            await invoke('clear_log_cache');
            showToast(t('settings.advanced.logs_cleared'), 'success');
        } catch (error) {
            showToast(`${t('common.error')}: ${error}`, 'error');
        }
        setIsClearLogsOpen(false);
    };

    const handleOpenDataDir = async () => {
        try {
            await invoke('open_data_folder');
        } catch (error) {
            showToast(`${t('common.error')}: ${error}`, 'error');
        }
    };

    const handleSelectExportPath = async () => {
        try {
            // @ts-ignore
            const selected = await open({
                directory: true,
                multiple: false,
                title: t('settings.advanced.export_path'),
            });
            if (selected && typeof selected === 'string') {
                setFormData({ ...formData, default_export_path: selected });
            }
        } catch (error) {
            showToast(`${t('common.error')}: ${error}`, 'error');
        }
    };

    const handleSelectAntigravityPath = async () => {
        try {
            const selected = await open({
                directory: false,
                multiple: false,
                title: t('settings.advanced.antigravity_path_select'),
            });
            if (selected && typeof selected === 'string') {
                setFormData({ ...formData, antigravity_executable: selected });
            }
        } catch (error) {
            showToast(`${t('common.error')}: ${error}`, 'error');
        }
    };

    const handleSelectAntigravityIdePath = async () => {
        try {
            const selected = await open({
                directory: false,
                multiple: false,
                title: t('settings.advanced.antigravity_ide_path_select', 'Select Antigravity IDE Executable'),
            });
            if (selected && typeof selected === 'string') {
                setFormData({ ...formData, antigravity_ide_executable: selected });
            }
        } catch (error) {
            showToast(`${t('common.error')}: ${error}`, 'error');
        }
    };

    const handleSelectDebugLogDir = async () => {
        try {
            const selected = await open({
                directory: true,
                multiple: false,
                title: t('settings.advanced.debug_log_dir_select'),
            });
            if (selected && typeof selected === 'string') {
                setFormData({
                    ...formData,
                    proxy: {
                        ...formData.proxy,
                        debug_logging: {
                            enabled: formData.proxy?.debug_logging?.enabled ?? false,
                            output_dir: selected,
                        },
                    },
                });
            }
        } catch (error) {
            showToast(`${t('common.error')}: ${error}`, 'error');
        }
    };

    const handleDetectAntigravityPath = async () => {
        try {
            const command = isTauri() ? 'get_antigravity_path' : 'get_antigravity_path'; // 后端已统一
            const path = await invoke<string>(command, { bypassConfig: true });
            setFormData({ ...formData, antigravity_executable: path });
            showToast(t('settings.advanced.antigravity_path_detected'), 'success');
        } catch (error) {
            showToast(`${t('common.error')}: ${error}`, 'error');
        }
    };

    const handleSelectAntigravityCliPath = async () => {
        try {
            const selected = await open({
                directory: false,
                multiple: false,
                title: t('settings.advanced.antigravity_cli_path_select', 'Select Antigravity CLI (agy) Executable'),
            });
            if (selected && typeof selected === 'string') {
                setFormData({ ...formData, antigravity_cli_executable: selected });
            }
        } catch (error) {
            showToast(`${t('common.error')}: ${error}`, 'error');
        }
    };

    const handleDetectAntigravityCliPath = async () => {
        try {
            const path = await invoke<string>('get_antigravity_cli_path', { bypassConfig: true });
            setFormData({ ...formData, antigravity_cli_executable: path });
            showToast(t('settings.advanced.antigravity_cli_path_detected', 'Detected CLI path updated'), 'success');
        } catch (error) {
            showToast(`${t('common.error')}: ${error}`, 'error');
        }
    };

    // Handle opening cache clear dialog
    const handleOpenClearCacheDialog = async () => {
        try {
            const paths = await invoke<string[]>('get_antigravity_cache_paths');
            setCachePaths(paths);
            setIsClearCacheOpen(true);
        } catch (error) {
            // If no cache paths found, still allow opening the dialog
            setCachePaths([]);
            setIsClearCacheOpen(true);
        }
    };

    // Handle clearing Antigravity cache
    const confirmClearAntigravityCache = async () => {
        setIsClearingCache(true);
        try {
            const result = await invoke<{
                cleared_paths: string[];
                total_size_freed: number;
                errors: string[];
            }>('clear_antigravity_cache');

            const sizeMB = (result.total_size_freed / 1024 / 1024).toFixed(2);

            if (result.cleared_paths.length > 0) {
                showToast(t('settings.advanced.cache_cleared_success', { size: sizeMB }), 'success');
            } else if (result.errors.length > 0) {
                showToast(`${t('common.error')}: ${result.errors[0]}`, 'error');
            } else {
                showToast(t('settings.advanced.cache_not_found'), 'info');
            }
        } catch (error) {
            showToast(`${t('common.error')}: ${error}`, 'error');
        } finally {
            setIsClearingCache(false);
            setIsClearCacheOpen(false);
        }
    };

    return (
        <div className="console-page console-page-scroll h-full">
            <div className="space-y-5">
                <PageHeader
                    actions={<button type="button" className="console-button console-button-primary" onClick={handleSave} disabled={!hydrated || loading}><Save size={16} />{t('settings.save')}</button>}
                    title={t('nav.settings')}
                    description={t('console.settings_description', { defaultValue: i18n.language.startsWith('zh') ? '管理控制台偏好、账号策略与服务设置。' : 'Manage console preferences, account policies, and service settings.' })}
                />
                {configError && (
                    <div role="alert" className="console-panel text-error flex items-center justify-between gap-3">
                        <span>{configError}</span>
                        <button type="button" className="console-button" onClick={loadConfig}>{t('common.refresh')}</button>
                    </div>
                )}
                <nav className="console-tabs flex flex-wrap gap-1" aria-label={t('nav.settings')}>
                    {(['general', 'account', 'proxy', 'advanced', 'debug'] as const).map(tab => (
                        <button
                            key={tab}
                            type="button"
                            className={`console-tab ${activeTab === tab ? 'active' : ''}`}
                            aria-current={activeTab === tab ? 'page' : undefined}
                            onClick={() => setActiveTab(tab)}
                        >
                            {t(`settings.tabs.${tab}`)}
                        </button>
                    ))}
                </nav>

                {/* 设置表单 */}
                <div className="console-panel min-w-0">
                    {/* 通用设置 */}
                    {activeTab === 'general' && (
                        <div className="space-y-6">
                            <h2 className="text-lg font-semibold text-gray-900 dark:text-base-content">{t('settings.general.title')}</h2>

                            {/* 语言选择 */}
                            <div>
                                <label className="block text-sm font-medium text-gray-900 dark:text-base-content mb-2">{t('settings.general.language')}</label>
                                <select
                                    value={formData.language}
                                    onChange={(e) => {
                                        if (!hydrated) return;
                                        const newLang = e.target.value;
                                        updateDraft(current => ({ ...current, language: newLang }));
                                        i18n.changeLanguage(newLang);
                                        updateConfig(current => ({ ...current, language: newLang }), true).catch(error => showToast(`${t('common.error')}: ${error}`, 'error'));
                                    }}
                                    className="w-full px-4 py-4 border border-gray-200 dark:border-base-300 rounded-lg focus:outline-none focus:ring-2 focus:ring-blue-500 focus:border-transparent text-gray-900 dark:text-base-content bg-gray-50 dark:bg-base-200"
                                >
                                    <option value="zh">简体中文</option>
                                    <option value="zh-TW">繁體中文</option>
                                    <option value="en">English</option>
                                    <option value="ja">日本語</option>
                                    <option value="tr">Türkçe</option>
                                    <option value="vi">Tiếng Việt</option>
                                    <option value="pt">Português</option>
                                    <option value="ko">한국어</option>
                                    <option value="ru">Русский</option>
                                    <option value="ar">العربية</option>
                                </select>
                            </div>

                            {/* 主题选择 */}
                            <div>
                                <label className="block text-sm font-medium text-gray-900 dark:text-base-content mb-2">{t('settings.general.theme')}</label>
                                <select
                                    value={formData.theme}
                                    onChange={(e) => {
                                        if (!hydrated) return;
                                        const newTheme = e.target.value;
                                        updateDraft(current => ({ ...current, theme: newTheme }));
                                        updateConfig(current => ({ ...current, theme: newTheme }), true).catch(error => showToast(`${t('common.error')}: ${error}`, 'error'));
                                    }}
                                    className="w-full px-4 py-4 border border-gray-200 dark:border-base-300 rounded-lg focus:outline-none focus:ring-2 focus:ring-blue-500 focus:border-transparent text-gray-900 dark:text-base-content bg-gray-50 dark:bg-base-200"
                                >
                                    <option value="light">{t('settings.general.theme_light')}</option>
                                    <option value="dark">{t('settings.general.theme_dark')}</option>
                                    <option value="system">{t('settings.general.theme_system')}</option>
                                </select>
                            </div>

                            {/* 开机自动启动 */}
                            {isTauri() && (
                            <div>
                                <div className="flex justify-between items-center mb-2">
                                    <label className="block text-sm font-medium text-gray-900 dark:text-base-content">{t('settings.general.auto_launch')}</label>
                                </div>
                                <select
                                    className="w-full px-4 py-4 border border-gray-200 dark:border-base-300 rounded-lg focus:outline-none focus:ring-2 focus:ring-blue-500 focus:border-transparent text-gray-900 dark:text-base-content bg-gray-50 dark:bg-base-200"
                                    value={formData.auto_launch ? 'enabled' : 'disabled'}
                                    onChange={async (e) => {
                                        const enabled = e.target.value === 'enabled';
                                        try {
                                            await invoke('toggle_auto_launch', { enable: enabled });
                                            setFormData({ ...formData, auto_launch: enabled });
                                            showToast(enabled ? t('settings.general.auto_launch_enabled') : t('settings.general.auto_launch_disabled'), 'success');
                                        } catch (error) {
                                            showToast(`${t('common.error')}: ${error}`, 'error');
                                        }
                                    }}
                                >
                                    <option value="disabled">{t('settings.general.auto_launch_disabled')}</option>
                                    <option value="enabled" disabled={!isTauri()}>{t('settings.general.auto_launch_enabled')}</option>

                                </select>
                                <p className="text-sm text-gray-500 dark:text-gray-400 mt-2">{t('settings.general.auto_launch_desc')}</p>
                            </div>
                            )}

                                {/* 菜单显示设置 */}
                                <div className="border-t border-gray-200 dark:border-base-200 pt-6 mt-6">
                                    <h3 className="font-medium text-gray-900 dark:text-base-content mb-3">{t('settings.menu.title')}</h3>
                                    <p className="text-sm text-gray-600 dark:text-gray-400 mb-4">
                                        {t('settings.menu.desc')}
                                    </p>
                                    <div className="grid grid-cols-2 lg:grid-cols-4 gap-3">
                                        {[
                                            { path: '/', label: t('nav.dashboard'), icon: LayoutDashboard },
                                            { path: '/accounts', label: t('nav.accounts'), icon: Users },
                                            { path: '/api-proxy', label: t('nav.proxy'), icon: Network },
                                            { path: '/monitor', label: t('nav.call_records'), icon: Activity },
                                            { path: '/token-stats', label: t('nav.token_stats'), icon: BarChart3 },
                                            { path: '/user-token', label: t('nav.user_token', 'User Tokens'), icon: Users },
                                            { path: '/security', label: t('nav.security'), icon: Lock },
                                            { path: '/api-guide', label: t('nav.api_guide'), icon: Globe },
                                            { path: '/settings', label: t('nav.settings'), icon: SettingsIcon },
                                        ].map((item) => {
                                            const hiddenItems = formData.hidden_menu_items || [];
                                            const isVisible = !hiddenItems.includes(item.path);
                                            const isSettings = item.path === '/settings';

                                            return (
                                                <button
                                                    type="button"
                                                    aria-pressed={isVisible}
                                                    disabled={isSettings}
                                                    key={item.path}
                                                    onClick={async () => {
                                                        if (!isSettings) {
                                                            const originalConfig = { ...formData };
                                                            const hiddenItems = formData.hidden_menu_items || [];
                                                            const newHiddenItems = isVisible
                                                                ? [...hiddenItems, item.path]
                                                                : hiddenItems.filter(p => p !== item.path);

                                                            // 乐观更新 UI
                                                            const newConfig = {
                                                                ...formData,
                                                                hidden_menu_items: newHiddenItems
                                                            };
                                                            setFormData(newConfig);

                                                            // 尝试保存
                                                            try {
                                                                await saveConfig(newConfig);
                                                            } catch (error) {
                                                                // 保存失败，回滚到原始快照
                                                                setFormData(originalConfig);
                                                                showToast(`保存失败，已恢复设置: ${error}`, 'error');
                                                            }
                                                        }
                                                    }}
                                                    className={`
                                                        relative flex flex-col items-center justify-center gap-3 p-4 rounded-xl border-2 transition-all cursor-pointer select-none
                                                        ${isSettings
                                                            ? 'bg-gray-50 dark:bg-base-200 border-gray-100 dark:border-base-300 opacity-60 cursor-not-allowed'
                                                            : isVisible
                                                                ? 'bg-blue-50/50 dark:bg-blue-900/10 border-blue-500 dark:border-blue-500 shadow-sm'
                                                                : 'bg-white dark:bg-base-100 border-gray-200 dark:border-base-300 hover:border-gray-300 dark:hover:border-base-content/20 text-gray-500'
                                                        }
                                                    `}
                                                >
                                                    {/* 选中标记 */}
                                                    {isVisible && (
                                                        <div className="absolute top-2 right-2 text-blue-500">
                                                            <CheckCircle2 size={16} fill="currentColor" className="text-white dark:text-base-100" />
                                                        </div>
                                                    )}

                                                    {isSettings && (
                                                        <div className="absolute top-2 right-2 text-xs font-bold text-gray-400 bg-gray-200 dark:bg-base-300 px-1.5 py-0.5 rounded">
                                                            {t('settings.menu.required')}
                                                        </div>
                                                    )}

                                                    <div className={`
                                                        p-3 rounded-xl transition-colors
                                                        ${isVisible
                                                            ? 'bg-blue-100 dark:bg-blue-900/30 text-blue-600 dark:text-blue-400'
                                                            : 'bg-gray-100 dark:bg-base-200 text-gray-400 dark:text-base-content/50'
                                                        }
                                                    `}>
                                                        <item.icon size={24} />
                                                    </div>

                                                    <span className={`font-medium text-sm ${isVisible ? 'text-blue-900 dark:text-blue-100' : 'text-gray-500'}`}>
                                                        {item.label}
                                                    </span>
                                                </button>
                                            );
                                        })}
                                    </div>
                                    <p className="text-xs text-gray-500 dark:text-gray-400 mt-4 flex items-center gap-1.5">
                                        <span className="w-1.5 h-1.5 shrink-0 rounded-full bg-gray-400" />
                                        {t('settings.menu.selected_items_note')}
                                    </p>
                                </div>
                        </div>
                    )}

                    {/* 账号设置 */}
                    {activeTab === 'account' && (
                        <div className="space-y-4 animate-in fade-in duration-500">
                            {/* 自动刷新配额 */}
                            <div className="group bg-white dark:bg-base-100 rounded-xl p-5 border border-gray-100 dark:border-base-200 hover:border-blue-200 transition-all duration-300 shadow-sm">
                                <div className="flex items-center justify-between">
                                    <div className="flex items-center gap-4">
                                        <div className="w-10 h-10 rounded-xl bg-blue-50 dark:bg-blue-900/20 flex items-center justify-center text-blue-500 group-hover:bg-blue-500 group-hover:text-white transition-all duration-300">
                                            <RefreshCw size={20} />
                                        </div>
                                        <div>
                                            <div className="font-bold text-gray-900 dark:text-gray-100">{t('settings.account.auto_refresh')}</div>
                                            <p className="text-xs text-gray-500 dark:text-gray-400 mt-0.5">{t('settings.account.auto_refresh_desc')}</p>
                                        </div>
                                    </div>
                                    <label className={`relative inline-flex items-center ${formData.quota_protection.enabled ? 'cursor-not-allowed opacity-80' : 'cursor-pointer'}`}>
                                        <input
                                            type="checkbox"
                                            className="sr-only peer"
                                            checked={formData.auto_refresh}
                                            disabled={formData.quota_protection.enabled}
                                            onChange={async (e) => {
                                                const enabled = e.target.checked;
                                                const newConfig = { ...formData, auto_refresh: enabled };
                                                setFormData(newConfig);
                                                // Hot Save
                                                try {
                                                    await saveConfig(newConfig);
                                                } catch (error) {
                                                    showToast(`${t('common.error')}: ${error}`, 'error');
                                                }
                                            }}
                                        />
                                        <div className={`w-11 h-6 bg-gray-200 dark:bg-base-300 peer-focus:outline-none rounded-full peer peer-checked:after:translate-x-full peer-checked:after:border-white after:content-[''] after:absolute after:top-[2px] after:left-[2px] after:bg-white after:border-gray-300 after:border after:rounded-full after:h-5 after:w-5 after:transition-all peer-checked:bg-blue-500 shadow-inner ${formData.quota_protection.enabled ? 'peer-checked:bg-blue-500' : ''}`}></div>
                                    </label>
                                </div>

                                <div className="mt-5 pt-5 border-t border-gray-50 dark:border-base-300 flex items-center gap-4 animate-in slide-in-from-top-1 duration-200">
                                    <label className="text-xs font-bold text-gray-500 dark:text-gray-400 uppercase tracking-wider">{t('settings.account.refresh_interval')}</label>
                                    <div className="relative">
                                        <input
                                            type="number"
                                            className="w-24 px-3 py-2 bg-gray-50 dark:bg-base-200 border border-gray-100 dark:border-base-300 rounded-lg focus:ring-2 focus:ring-blue-500 outline-none text-sm font-bold text-blue-600 dark:text-blue-400"
                                            min="1"
                                            max="35791"
                                            value={formData.refresh_interval}
                                            onChange={(e) => setFormData({ ...formData, refresh_interval: isNaN(parseInt(e.target.value)) ? 1 : Math.min(Math.max(parseInt(e.target.value), 1), 35791) })}
                                        />
                                    </div>
                                </div>
                            </div>

                            {/* 自动获取当前账号 */}
                            <div className="group bg-white dark:bg-base-100 rounded-xl p-5 border border-gray-100 dark:border-base-200 hover:border-emerald-200 transition-all duration-300 shadow-sm">
                                <div className="flex items-center justify-between">
                                    <div className="flex items-center gap-4">
                                        <div className="w-10 h-10 rounded-xl bg-emerald-50 dark:bg-emerald-900/20 flex items-center justify-center text-emerald-500 group-hover:bg-emerald-500 group-hover:text-white transition-all duration-300">
                                            <User size={20} />
                                        </div>
                                        <div>
                                            <div className="font-bold text-gray-900 dark:text-gray-100">{t('settings.account.auto_sync')}</div>
                                            <p className="text-xs text-gray-500 dark:text-gray-400 mt-0.5">{t('settings.account.auto_sync_desc')}</p>
                                        </div>
                                    </div>
                                    <label className="relative inline-flex items-center cursor-pointer">
                                        <input
                                            type="checkbox"
                                            className="sr-only peer"
                                            checked={formData.auto_sync}
                                            onChange={(e) => setFormData({ ...formData, auto_sync: e.target.checked })}
                                        />
                                        <div className="w-11 h-6 bg-gray-200 dark:bg-base-300 peer-focus:outline-none rounded-full peer peer-checked:after:translate-x-full peer-checked:after:border-white after:content-[''] after:absolute after:top-[2px] after:left-[2px] after:bg-white after:border-gray-300 after:border after:rounded-full after:h-5 after:w-5 after:transition-all peer-checked:bg-emerald-500 shadow-inner"></div>
                                    </label>
                                </div>

                                {formData.auto_sync && (
                                    <div className="mt-5 pt-5 border-t border-gray-50 dark:border-base-300 flex items-center gap-4 animate-in slide-in-from-top-1 duration-200">
                                        <label className="text-xs font-bold text-gray-500 dark:text-gray-400 uppercase tracking-wider">{t('settings.account.sync_interval')}</label>
                                        <input
                                            type="number"
                                            className="w-24 px-3 py-2 bg-gray-50 dark:bg-base-200 border border-gray-100 dark:border-base-300 rounded-lg focus:ring-2 focus:ring-emerald-500 outline-none text-sm font-bold text-emerald-600 dark:text-emerald-400"
                                            min="1"
                                            max="35791"
                                            value={formData.sync_interval}
                                            onChange={(e) => setFormData({ ...formData, sync_interval: isNaN(parseInt(e.target.value)) ? 1 : Math.min(Math.max(parseInt(e.target.value), 1), 35791) })}
                                        />
                                    </div>
                                )}
                            </div>

                            {/* 7天周配额智能预热 (Smart Warmup) */}
                            <div className="group bg-white dark:bg-base-100 rounded-xl p-5 border border-gray-100 dark:border-base-200 hover:border-orange-200 transition-all duration-300 shadow-sm">
                                <SmartWarmup
                                    config={formData.scheduled_warmup}
                                    onChange={async (newConfig) => {
                                        const newFormData = {
                                            ...formData,
                                            scheduled_warmup: newConfig
                                        };
                                        setFormData(newFormData);
                                        // Hot Save
                                        try {
                                            await saveConfig(newFormData);
                                        } catch (error) {
                                            showToast(`${t('common.error')}: ${error}`, 'error');
                                        }
                                    }}
                                />
                            </div>

                            {/* 配额保护 (Quota Protection) */}
                            <div className="group bg-white dark:bg-base-100 rounded-xl p-5 border border-gray-100 dark:border-base-200 hover:border-rose-200 transition-all duration-300 shadow-sm">
                                <QuotaProtection
                                    config={formData.quota_protection}
                                    onChange={async (newConfig) => {
                                        const updates: any = {
                                            quota_protection: newConfig
                                        };
                                        // 联动逻辑：开启配额保护时，强制开启后台自动刷新 (不仅仅是预热)
                                        if (newConfig.enabled) {
                                            updates.auto_refresh = true;
                                        }

                                        const newFormData = {
                                            ...formData,
                                            ...updates
                                        };
                                        setFormData(newFormData);

                                        // Hot Save
                                        try {
                                            await saveConfig(newFormData);
                                        } catch (error) {
                                            showToast(`${t('common.error')}: ${error}`, 'error');
                                        }
                                    }}
                                />
                            </div>

                            {/* 配额关注列表 (Pinned Quota Models) */}
                            <div className="group bg-white dark:bg-base-100 rounded-xl p-5 border border-gray-100 dark:border-base-200 hover:border-indigo-200 transition-all duration-300 shadow-sm">
                                <PinnedQuotaModels
                                    config={formData.pinned_quota_models}
                                    onChange={(newConfig) => setFormData({
                                        ...formData,
                                        pinned_quota_models: newConfig
                                    })}
                                />
                            </div>
                        </div>
                    )}

                    {/* 高级设置 */}
                    {activeTab === 'advanced' && (
                        <>
                            <div className="space-y-4">
                                {/* 默认导出路径 */}
                                {isTauri() && (
                                <div>
                                    <label className="block text-sm font-medium text-gray-900 dark:text-base-content mb-1">{t('settings.advanced.export_path')}</label>
                                    <div className="flex flex-wrap gap-2">
                                        <input
                                            type="text"
                                            className="min-w-0 flex-1 px-4 py-3 border border-gray-200 dark:border-base-300 rounded-lg bg-gray-50 dark:bg-base-200 text-gray-900 dark:text-base-content font-medium"
                                            value={formData.default_export_path || t('settings.advanced.export_path_placeholder')}
                                            readOnly
                                        />
                                        {formData.default_export_path && (
                                            <button
                                                className="px-4 py-2 border border-gray-200 dark:border-base-300 text-red-600 dark:text-red-400 rounded-lg hover:bg-red-50 dark:hover:bg-red-900/10 transition-colors"
                                                onClick={() => setFormData({ ...formData, default_export_path: undefined })}
                                            >
                                                {t('common.clear')}
                                            </button>
                                        )}
                                            <button
                                                className="px-4 py-2 border border-gray-200 dark:border-base-300 text-gray-700 dark:text-gray-300 rounded-lg hover:bg-gray-50 dark:hover:bg-base-200 hover:text-gray-900 dark:hover:text-base-content transition-colors"
                                                onClick={handleSelectExportPath}
                                            >
                                                {t('settings.advanced.select_btn')}
                                            </button>
                                    </div>
                                    <p className="text-sm text-gray-500 dark:text-gray-400 mt-2">{t('settings.advanced.default_export_path_desc')}</p>
                                </div>
                                )}

                                {/* 数据目录 */}
                                <div>
                                    <label className="block text-sm font-medium text-gray-900 dark:text-base-content mb-1">{t('settings.advanced.data_dir')}</label>
                                    <div className="flex flex-wrap gap-2">
                                        <input
                                            type="text"
                                            className="min-w-0 flex-1 px-4 py-3 border border-gray-200 dark:border-base-300 rounded-lg bg-gray-50 dark:bg-base-200 text-gray-900 dark:text-base-content font-medium"
                                            value={dataDirPath}
                                            readOnly
                                        />
                                        {isTauri() && (
                                            <button
                                                className="px-4 py-2 border border-gray-200 dark:border-base-300 text-gray-700 dark:text-gray-300 rounded-lg hover:bg-gray-50 dark:hover:bg-base-200 hover:text-gray-900 dark:hover:text-base-content transition-colors"
                                                onClick={handleOpenDataDir}
                                            >
                                                {t('settings.advanced.open_btn')}
                                            </button>
                                        )}
                                    </div>
                                    <p className="text-sm text-gray-500 dark:text-gray-400 mt-2">{t('settings.advanced.data_dir_desc')}</p>
                                </div>

                                {/* 反重力程序路径 */}
                                <div>
                                    <label className="block text-sm font-medium text-gray-900 dark:text-base-content mb-1">
                                        {t('settings.advanced.antigravity_path')}
                                    </label>
                                    <div className="flex flex-wrap gap-2">
                                        <input
                                            type="text"
                                            className="min-w-0 flex-1 px-4 py-3 border border-gray-200 dark:border-base-300 rounded-lg bg-gray-50 dark:bg-base-200 text-gray-900 dark:text-base-content font-medium"
                                            value={formData.antigravity_executable || ''}
                                            placeholder={t('settings.advanced.antigravity_path_placeholder')}
                                            onChange={(e) => setFormData({ ...formData, antigravity_executable: e.target.value })}
                                        />
                                        {formData.antigravity_executable && (
                                            <button
                                                className="px-4 py-2 border border-gray-200 dark:border-base-300 text-red-600 dark:text-red-400 rounded-lg hover:bg-red-50 dark:hover:bg-red-900/10 transition-colors"
                                                onClick={() => setFormData({ ...formData, antigravity_executable: undefined })}
                                            >
                                                {t('common.clear')}
                                            </button>
                                        )}
                                        <button
                                            className="px-4 py-2 border border-gray-200 dark:border-base-300 text-gray-700 dark:text-gray-300 rounded-lg hover:bg-gray-50 dark:hover:bg-base-200 transition-colors"
                                            onClick={handleDetectAntigravityPath}
                                        >
                                            {t('settings.advanced.detect_btn')}
                                        </button>
                                        {isTauri() && (
                                            <button
                                                className="px-4 py-2 border border-gray-200 dark:border-base-300 text-gray-700 dark:text-gray-300 rounded-lg hover:bg-gray-50 dark:hover:bg-base-200 transition-colors"
                                                onClick={handleSelectAntigravityPath}
                                            >
                                                {t('settings.advanced.select_btn')}
                                            </button>
                                        )}
                                    </div>
                                    <p className="text-sm text-gray-500 dark:text-gray-400 mt-2">
                                        {t('settings.advanced.antigravity_path_desc')}
                                    </p>
                                </div>

                                {/* Antigravity CLI (agy) 程序路径 */}
                                {isTauri() && (
                                <div>
                                    <label className="block text-sm font-medium text-gray-900 dark:text-base-content mb-1">
                                        {t('settings.advanced.antigravity_cli_path', 'Antigravity CLI (agy) Path')}
                                    </label>
                                    <div className="flex flex-wrap gap-2">
                                        <input
                                            type="text"
                                            className="min-w-0 flex-1 px-4 py-3 border border-gray-200 dark:border-base-300 rounded-lg bg-gray-50 dark:bg-base-200 text-gray-900 dark:text-base-content font-medium"
                                            value={formData.antigravity_cli_executable || ''}
                                            placeholder={t('settings.advanced.antigravity_cli_path_placeholder', '未设置 (将使用自动探测)')}
                                            onChange={(e) => setFormData({ ...formData, antigravity_cli_executable: e.target.value })}
                                        />
                                        {formData.antigravity_cli_executable && (
                                            <button
                                                className="px-4 py-2 border border-gray-200 dark:border-base-300 text-red-600 dark:text-red-400 rounded-lg hover:bg-red-50 dark:hover:bg-red-900/10 transition-colors"
                                                onClick={() => setFormData({ ...formData, antigravity_cli_executable: undefined })}
                                            >
                                                {t('common.clear')}
                                            </button>
                                        )}
                                        <button
                                            className="px-4 py-2 border border-gray-200 dark:border-base-300 text-gray-700 dark:text-gray-300 rounded-lg hover:bg-gray-50 dark:hover:bg-base-200 transition-colors"
                                            onClick={handleDetectAntigravityCliPath}
                                        >
                                            {t('settings.advanced.detect_btn')}
                                        </button>
                                            <button
                                                className="px-4 py-2 border border-gray-200 dark:border-base-300 text-gray-700 dark:text-gray-300 rounded-lg hover:bg-gray-50 dark:hover:bg-base-200 transition-colors"
                                                onClick={handleSelectAntigravityCliPath}
                                            >
                                                {t('settings.advanced.select_btn')}
                                            </button>
                                    </div>
                                    <p className="text-sm text-gray-500 dark:text-gray-400 mt-2">
                                        {t('settings.advanced.antigravity_cli_path_desc', '设置您的命令行客户端 (agy) 的可执行文件路径，用于一键解除账号限制。')}
                                    </p>
                                    
                                    {/* 新增：解密/修补准入限制一键修补按钮 */}
                                    <div className={`mt-3 flex items-center gap-4 p-3 rounded-lg border ${formData.antigravity_cli_executable ? 'bg-blue-50 dark:bg-blue-950/20 border-blue-100 dark:border-blue-900/30' : 'bg-gray-50 dark:bg-gray-800 border-gray-200 dark:border-gray-700'}`}>
                                        <div className="flex-1">
                                            <h4 className={`text-sm font-semibold ${formData.antigravity_cli_executable ? 'text-blue-900 dark:text-blue-200' : 'text-gray-500 dark:text-gray-400'}`}>
                                                {t('settings.advanced.patch_eligibility_title', '账号准入限制解除')}
                                            </h4>
                                            <p className={`text-xs mt-0.5 ${formData.antigravity_cli_executable ? 'text-blue-700 dark:text-blue-300/80' : 'text-gray-400 dark:text-gray-500'}`}>
                                                {t('settings.advanced.patch_eligibility_desc', '新版 agy 二进制强制拦截未授权账号，此操作一键跳过本地准入拦截检查。')}
                                                {!formData.antigravity_cli_executable && " (需先在上方设置或探测路径)"}
                                            </p>
                                        </div>
                                        <button
                                            className={`px-4 py-2 rounded-lg transition-colors font-medium text-sm shadow-sm ${formData.antigravity_cli_executable ? 'bg-blue-600 hover:bg-blue-700 active:bg-blue-800 text-white' : 'bg-gray-200 dark:bg-gray-700 text-gray-400 dark:text-gray-500 cursor-not-allowed'}`}
                                            disabled={!formData.antigravity_cli_executable}
                                            onClick={async () => {
                                                if (!formData.antigravity_cli_executable) return;
                                                try {
                                                    const res = await invoke<string>('patch_agy_binary', { filePath: formData.antigravity_cli_executable });
                                                    showToast(res, 'success');
                                                } catch (err) {
                                                    showToast(String(err), 'error');
                                                }
                                            }}
                                        >
                                            {t('settings.advanced.patch_btn', '一键解除')}
                                        </button>
                                    </div>
                                </div>
                                )}

                                {/* Antigravity IDE 程序路径 */}
                                <div>
                                    <label className="block text-sm font-medium text-gray-900 dark:text-base-content mb-1">
                                        {t('settings.advanced.antigravity_ide_path', 'Antigravity IDE Path')}
                                    </label>
                                    <div className="flex flex-wrap gap-2">
                                        <input
                                            type="text"
                                            className="min-w-0 flex-1 px-4 py-3 border border-gray-200 dark:border-base-300 rounded-lg bg-gray-50 dark:bg-base-200 text-gray-900 dark:text-base-content font-medium"
                                            value={formData.antigravity_ide_executable || ''}
                                            placeholder={t('settings.advanced.antigravity_ide_path_placeholder', 'D:\\Antigravity\\Antigravity.exe')}
                                            onChange={(e) => setFormData({ ...formData, antigravity_ide_executable: e.target.value })}
                                        />
                                        {formData.antigravity_ide_executable && (
                                            <button
                                                className="px-4 py-2 border border-gray-200 dark:border-base-300 text-red-600 dark:text-red-400 rounded-lg hover:bg-red-50 dark:hover:bg-red-900/10 transition-colors"
                                                onClick={() => setFormData({ ...formData, antigravity_ide_executable: undefined })}
                                            >
                                                {t('common.clear')}
                                            </button>
                                        )}
                                        {isTauri() && (
                                            <button
                                                className="px-4 py-2 border border-gray-200 dark:border-base-300 text-gray-700 dark:text-gray-300 rounded-lg hover:bg-gray-50 dark:hover:bg-base-200 transition-colors"
                                                onClick={handleSelectAntigravityIdePath}
                                            >
                                                {t('settings.advanced.select_btn')}
                                            </button>
                                        )}
                                    </div>
                                    <p className="text-sm text-gray-500 dark:text-gray-400 mt-2">
                                        {t('settings.advanced.antigravity_ide_path_desc', 'Specify the executable path for Antigravity IDE (code editor). Once set, account switching will strictly protect processes at this path from being terminated.')}
                                    </p>
                                </div>

                                {/* 反重力程序启动参数 */}
                                <div>
                                    <label className="block text-sm font-medium text-gray-900 dark:text-base-content mb-1">
                                        {t('settings.advanced.antigravity_args')}
                                    </label>
                                    <div className="flex flex-wrap gap-2">
                                        <input
                                            type="text"
                                            className="min-w-0 flex-1 px-4 py-3 border border-gray-200 dark:border-base-300 rounded-lg bg-gray-50 dark:bg-base-200 text-gray-900 dark:text-base-content font-medium"
                                            value={rawAntigravityArgs}
                                            onChange={(e) => {
                                                if (!hydrated) return;
                                                setRawAntigravityArgs(e.target.value);
                                                dirtyRef.current = true;
                                            }}
                                            onBlur={commitArguments}
                                        />
                                        <button
                                            className="px-4 py-2 border border-gray-200 dark:border-base-300 text-gray-700 dark:text-gray-300 rounded-lg hover:bg-gray-100 dark:hover:bg-base-200 transition-colors"
                                            onClick={async () => {
                                                try {
                                                    const args = await invoke<string[]>('get_antigravity_args');
                                                    setRawAntigravityArgs(formatArguments(args));
                                                    updateDraft(current => ({ ...current, antigravity_args: args }));
                                                    showToast(t('settings.advanced.antigravity_args_detected'), 'success');
                                                } catch (error) {
                                                    showToast(`${t('settings.advanced.antigravity_args_detect_error')}: ${error}`, 'error');
                                                }
                                            }}
                                        >
                                            {t('settings.advanced.detect_args_btn')}
                                        </button>
                                    </div>
                                    <p className="text-sm text-gray-500 dark:text-gray-400 mt-2">
                                        {t('settings.advanced.antigravity_args_desc')}
                                    </p>
                                </div>

                                {/* 日志缓存清理 */}
                                <div className="border-t border-gray-200 dark:border-base-200 pt-4">
                                    <h3 className="font-medium text-gray-900 dark:text-base-content mb-3">{t('settings.advanced.logs_title')}</h3>
                                    <div className="bg-gray-50 dark:bg-base-200 border border-gray-200 dark:border-base-300 rounded-lg p-3 mb-3">
                                        <p className="text-sm text-gray-600 dark:text-gray-400">{t('settings.advanced.logs_desc')}</p>
                                    </div>
                                    <div className="flex items-center gap-4">
                                        <button
                                            className="px-4 py-2 border border-gray-300 dark:border-base-300 text-gray-700 dark:text-gray-300 rounded-lg hover:bg-gray-100 dark:hover:bg-base-200 transition-colors"
                                            onClick={() => setIsClearLogsOpen(true)}
                                        >
                                            {t('settings.advanced.clear_logs')}
                                        </button>
                                    </div>
                                </div>

                                {/* Antigravity 缓存清理 */}
                                <div className="border-t border-gray-200 dark:border-base-200 pt-4">
                                    <h3 className="font-medium text-gray-900 dark:text-base-content mb-3">{t('settings.advanced.antigravity_cache_title')}</h3>
                                    <div className="bg-amber-50 dark:bg-amber-900/20 border border-amber-200 dark:border-amber-700/30 rounded-lg p-3 mb-3">
                                        <p className="text-sm text-amber-700 dark:text-amber-400">{t('settings.advanced.antigravity_cache_warning')}</p>
                                    </div>
                                    <div className="bg-gray-50 dark:bg-base-200 border border-gray-200 dark:border-base-300 rounded-lg p-3 mb-3">
                                        <p className="text-sm text-gray-600 dark:text-gray-400">{t('settings.advanced.antigravity_cache_desc')}</p>
                                    </div>
                                    <div className="flex items-center gap-4">
                                        <button
                                            className="px-4 py-2 border border-orange-300 dark:border-orange-700 text-orange-700 dark:text-orange-400 rounded-lg hover:bg-orange-50 dark:hover:bg-orange-900/20 transition-colors"
                                            onClick={handleOpenClearCacheDialog}
                                        >
                                            {t('settings.advanced.clear_antigravity_cache')}
                                        </button>
                                    </div>
                                </div>



                                <div className="border-t border-gray-200 dark:border-base-200 pt-4">
                                    <div className="space-y-3">
                                        <div className="flex items-center justify-between p-4 bg-gray-50 dark:bg-base-200 rounded-lg border border-gray-100 dark:border-base-300">
                                            <div>
                                                <div className="font-medium text-gray-900 dark:text-base-content">
                                                    {t('settings.advanced.debug_logs_title')}
                                                </div>
                                                <p className="text-sm text-gray-600 dark:text-gray-400 mt-1">
                                                    {t('settings.advanced.debug_logs_enable_desc')}
                                                </p>
                                            </div>
                                            <label className="relative inline-flex items-center cursor-pointer">
                                                <input
                                                    type="checkbox"
                                                    className="sr-only peer"
                                                    checked={formData.proxy?.debug_logging?.enabled ?? false}
                                                    onChange={(e: React.ChangeEvent<HTMLInputElement>) => setFormData({
                                                        ...formData,
                                                        proxy: {
                                                            ...formData.proxy,
                                                            debug_logging: {
                                                                enabled: e.target.checked,
                                                                output_dir: formData.proxy?.debug_logging?.output_dir,
                                                            },
                                                        },
                                                    })}
                                                />
                                                <div className="w-11 h-6 bg-gray-200 dark:bg-base-300 peer-focus:outline-none peer-focus:ring-4 peer-focus:ring-blue-300 dark:peer-focus:ring-blue-800 rounded-full peer peer-checked:after:translate-x-full peer-checked:after:border-white after:content-[''] after:absolute after:top-[2px] after:left-[2px] after:bg-white after:border-gray-300 after:border after:rounded-full after:h-5 after:w-5 after:transition-all peer-checked:bg-blue-500"></div>
                                            </label>
                                        </div>
                                        {(formData.proxy?.debug_logging?.enabled ?? false) && (
                                            <>
                                                <div className="bg-amber-50 dark:bg-amber-900/20 border border-amber-200 dark:border-amber-700/30 rounded-lg p-3">
                                                    <p className="text-sm text-amber-700 dark:text-amber-400">
                                                        {t('settings.advanced.debug_logs_desc')}
                                                    </p>
                                                </div>
                                                <div>
                                                    <label className="block text-sm font-medium text-gray-900 dark:text-base-content mb-1">
                                                        {t('settings.advanced.debug_log_dir')}
                                                    </label>
                                                    <div className="flex flex-wrap gap-2">
                                                        <input
                                                            type="text"
                                                            className="min-w-0 flex-1 px-4 py-3 border border-gray-200 dark:border-base-300 rounded-lg bg-gray-50 dark:bg-base-200 text-gray-900 dark:text-base-content font-medium"
                                                            value={formData.proxy?.debug_logging?.output_dir || ''}
                                                            placeholder={`${dataDirPath.replace(/\/$/, '')}/debug_logs`}
                                                            onChange={(e: React.ChangeEvent<HTMLInputElement>) => setFormData({
                                                                ...formData,
                                                                proxy: {
                                                                    ...formData.proxy,
                                                                    debug_logging: {
                                                                        enabled: formData.proxy?.debug_logging?.enabled ?? false,
                                                                        output_dir: e.target.value || undefined,
                                                                    },
                                                                },
                                                            })}
                                                        />
                                                        {isTauri() && (
                                                            <button
                                                                className="px-4 py-2 border border-gray-200 dark:border-base-300 text-gray-700 dark:text-gray-300 rounded-lg hover:bg-gray-50 dark:hover:bg-base-200 transition-colors"
                                                                onClick={handleSelectDebugLogDir}
                                                            >
                                                                {t('settings.advanced.select_btn')}
                                                            </button>
                                                        )}
                                                    </div>
                                                    <p className="text-xs text-gray-500 dark:text-gray-400 mt-2">
                                                        {t('settings.advanced.debug_log_dir_hint', { path: dataDirPath.replace(/\/$/, '') })}
                                                    </p>
                                                </div>
                                            </>
                                        )}
                                    </div>
                                </div>

                            </div>
                        </>
                    )}


                    {/* 调试设置 */}
                    {activeTab === 'debug' && (
                        <div className="space-y-4 animate-in fade-in duration-500">
                            {/* 标题和开关 */}
                            <div className="flex items-center justify-between">
                                <div>
                                    <h2 className="text-lg font-semibold text-gray-900 dark:text-base-content">
                                        {t('settings.debug.title')}
                                    </h2>
                                    <p className="text-sm text-gray-500 dark:text-gray-400 mt-1">
                                        {t('settings.debug.desc')}
                                    </p>
                                </div>
                                <label className="relative inline-flex items-center cursor-pointer">
                                    <input
                                        type="checkbox"
                                        className="sr-only peer"
                                        checked={isEnabled}
                                        onChange={(e) => e.target.checked ? enable() : disable()}
                                    />
                                    <div className="w-11 h-6 bg-gray-200 dark:bg-base-300 peer-focus:outline-none peer-focus:ring-4 peer-focus:ring-blue-300 dark:peer-focus:ring-blue-800 rounded-full peer peer-checked:after:translate-x-full peer-checked:after:border-white after:content-[''] after:absolute after:top-[2px] after:left-[2px] after:bg-white after:border-gray-300 after:border after:rounded-full after:h-5 after:w-5 after:transition-all peer-checked:bg-blue-500"></div>
                                    <span className="ml-3 text-sm font-medium text-gray-700 dark:text-gray-300">
                                        {isEnabled ? t('settings.debug.enabled') : t('settings.debug.disabled')}
                                    </span>
                                </label>
                            </div>

                            {/* 控制台或提示 */}
                            {isEnabled ? (
                                <div className="h-[calc(100vh-320px)] min-h-[400px]">
                                    <DebugConsole embedded />
                                </div>
                            ) : (
                                <div className="h-[calc(100vh-320px)] min-h-[400px] flex items-center justify-center bg-gray-50 dark:bg-base-200 rounded-xl border border-gray-200 dark:border-base-300">
                                    <div className="text-center">
                                        <p className="text-gray-500 dark:text-gray-400 text-lg font-medium">
                                            {t('settings.debug.disabled_hint')}
                                        </p>
                                        <p className="text-gray-400 dark:text-gray-500 text-sm mt-2">
                                            {t('settings.debug.disabled_desc')}
                                        </p>
                                    </div>
                                </div>
                            )}
                        </div>
                    )}

                    {/* 代理设置 */}
                    {activeTab === 'proxy' && (
                        <div className="space-y-4 animate-in fade-in duration-300">
                            <ProxyPoolSettings
                                config={formData.proxy?.proxy_pool || {
                                    enabled: false,
                                    proxies: [],
                                    health_check_interval: 300,
                                    auto_failover: true,
                                    strategy: 'priority',
                                }}
                                onChange={async (newConfig, silent = false) => {
                                    const applyPool = (current: AppConfig) => {
                                        const currentPool = current.proxy.proxy_pool;
                                        const proxyPool = silent && currentPool
                                            ? {
                                                ...currentPool,
                                                proxies: currentPool.proxies.map(proxy => {
                                                    const health = newConfig.proxies.find(candidate => candidate.id === proxy.id);
                                                    return health ? { ...proxy, is_healthy: health.is_healthy, latency: health.latency, last_check_time: health.last_check_time } : proxy;
                                                }),
                                            }
                                            : newConfig;
                                        return { ...current, proxy: { ...current.proxy, proxy_pool: proxyPool } };
                                    };
                                    updateDraft(applyPool);
                                    if (!silent) await updateConfig(applyPool);
                                }}
                                onBindingsChange={async (accountBindings) => {
                                    setFormData(current => ({
                                        ...current,
                                        proxy: {
                                            ...current.proxy,
                                            proxy_pool: {
                                                ...(current.proxy.proxy_pool || {
                                                    enabled: false,
                                                    proxies: [],
                                                    health_check_interval: 300,
                                                    auto_failover: true,
                                                    strategy: 'priority' as const,
                                                }),
                                                account_bindings: accountBindings,
                                            },
                                        },
                                    }));
                                    await updateConfig(current => ({
                                        ...current,
                                        proxy: {
                                            ...current.proxy,
                                            proxy_pool: {
                                                ...(current.proxy.proxy_pool || {
                                                    enabled: false,
                                                    proxies: [],
                                                    health_check_interval: 300,
                                                    auto_failover: true,
                                                    strategy: 'priority' as const,
                                                }),
                                                account_bindings: accountBindings,
                                            },
                                        },
                                    }), true);
                                }}
                            />

                            {/* [FIX #1701] 恢复全局上游代理设置 */}
                            <div className="group bg-white dark:bg-base-100 rounded-xl p-5 border border-gray-100 dark:border-base-200 hover:border-blue-200 transition-all duration-300 shadow-sm relative overflow-hidden">
                                <div className="absolute top-0 right-0 w-24 h-24 bg-blue-500/5 -mr-12 -mt-12 rounded-full blur-2xl group-hover:bg-blue-500/10 transition-colors"></div>
                                <div className="flex items-center justify-between mb-5 relative z-10">
                                    <div className="flex items-center gap-4">
                                        <div className="w-10 h-10 rounded-xl bg-blue-50 dark:bg-blue-900/20 flex items-center justify-center text-blue-500 group-hover:bg-blue-500 group-hover:text-white transition-all duration-300 shadow-sm">
                                            <Globe size={18} />
                                        </div>
                                        <div>
                                            <div className="font-bold text-gray-900 dark:text-gray-100 text-sm">{t('proxy.config.upstream_proxy.title')}</div>
                                            <p className="text-[11px] text-gray-500 dark:text-gray-400 mt-0.5 leading-tight max-w-[280px]">
                                                {t('proxy.config.upstream_proxy.desc_short')}
                                            </p>
                                        </div>
                                    </div>
                                    <label className="relative inline-flex items-center cursor-pointer scale-90">
                                        <input
                                            type="checkbox"
                                            className="sr-only peer"
                                            checked={formData.proxy?.upstream_proxy?.enabled ?? false}
                                            onChange={(e) => setFormData({
                                                ...formData,
                                                proxy: {
                                                    ...formData.proxy,
                                                    upstream_proxy: {
                                                        ...formData.proxy?.upstream_proxy,
                                                        enabled: e.target.checked
                                                    }
                                                }
                                            })}
                                        />
                                        <div className="w-11 h-6 bg-gray-200 dark:bg-base-300 peer-focus:outline-none rounded-full peer peer-checked:after:translate-x-full peer-checked:after:border-white after:content-[''] after:absolute after:top-[2px] after:left-[2px] after:bg-white after:border-gray-300 after:border after:rounded-full after:h-5 after:w-5 after:transition-all peer-checked:bg-blue-500 shadow-inner"></div>
                                    </label>
                                </div>

                                {formData.proxy?.upstream_proxy?.enabled && (
                                    <div className="space-y-4 animate-in slide-in-from-top-2 duration-300 relative z-10">
                                        <div className="pt-4 border-t border-gray-50 dark:border-base-300">
                                            <label className="block text-[10px] font-bold text-gray-400 dark:text-gray-500 uppercase tracking-widest mb-2.5">
                                                {t('proxy.config.upstream_proxy.url')}
                                            </label>
                                            <div className="relative group/input">
                                                <input
                                                    type="text"
                                                    className="w-full px-4 py-2.5 bg-gray-50 dark:bg-base-200 border border-gray-100 dark:border-base-300 rounded-xl focus:ring-2 focus:ring-blue-500/20 focus:border-blue-500 outline-none text-sm font-medium transition-all shadow-inner"
                                                    placeholder={t('proxy.config.upstream_proxy.url_placeholder')}
                                                    value={formData.proxy?.upstream_proxy?.url || ''}
                                                    onChange={(e) => setFormData({
                                                        ...formData,
                                                        proxy: {
                                                            ...formData.proxy,
                                                            upstream_proxy: {
                                                                ...formData.proxy?.upstream_proxy,
                                                                url: e.target.value
                                                            }
                                                        }
                                                    })}
                                                />
                                            </div>
                                            <div className="mt-4 bg-amber-50/40 dark:bg-amber-900/10 rounded-xl p-3.5 border border-amber-100/50 dark:border-amber-800/20 text-[11px] text-amber-700 dark:text-amber-400 flex items-start gap-3 transition-colors hover:bg-amber-50/60">
                                                <div className="mt-0.5 p-1 bg-amber-100/80 dark:bg-amber-800/40 rounded-lg shadow-sm">
                                                    <Network size={12} className="text-amber-600 dark:text-amber-400" />
                                                </div>
                                                <div className="leading-relaxed">
                                                    <span className="font-bold mr-1.5 opacity-80 uppercase tracking-tighter">Tip:</span>
                                                    {t('proxy.config.upstream_proxy.socks5h_hint')}
                                                </div>
                                            </div>
                                        </div>
                                    </div>
                                )}
                            </div>
                        </div>
                    )}

                </div >

                <ModalDialog
                    isOpen={isClearLogsOpen}
                    title={t('settings.advanced.clear_logs_title')}
                    message={t('settings.advanced.clear_logs_msg')}
                    type="confirm"
                    confirmText={t('common.clear')}
                    cancelText={t('common.cancel')}
                    isDestructive={true}
                    onConfirm={confirmClearLogs}
                    onCancel={() => setIsClearLogsOpen(false)}
                />

                {/* Antigravity Cache Clear Modal */}
                <ModalDialog
                    isOpen={isClearCacheOpen}
                    title={t('settings.advanced.clear_cache_confirm_title')}
                    type="confirm"
                    confirmText={isClearingCache ? t('common.clearing') : t('common.clear')}
                    cancelText={t('common.cancel')}
                    isDestructive={true}
                    onConfirm={confirmClearAntigravityCache}
                    onCancel={() => setIsClearCacheOpen(false)}
                >
                    <div className="space-y-3">
                        <p className="text-sm text-gray-600 dark:text-gray-400">
                            {t('settings.advanced.clear_cache_confirm_msg')}
                        </p>
                        {cachePaths.length > 0 ? (
                            <div className="bg-gray-50 dark:bg-base-200 rounded-lg p-3 max-h-40 overflow-y-auto">
                                <ul className="text-xs font-mono text-gray-600 dark:text-gray-400 space-y-1">
                                    {cachePaths.map((path, index) => (
                                        <li key={index} className="truncate">• {path}</li>
                                    ))}
                                </ul>
                            </div>
                        ) : (
                            <div className="bg-gray-50 dark:bg-base-200 rounded-lg p-3">
                                <p className="text-xs text-gray-500 dark:text-gray-400">
                                    {t('settings.advanced.cache_not_found')}
                                </p>
                            </div>
                        )}
                        <div className="bg-amber-50 dark:bg-amber-900/20 border border-amber-200 dark:border-amber-700/30 rounded-lg p-2">
                            <p className="text-xs text-amber-700 dark:text-amber-400">
                                {t('settings.advanced.antigravity_cache_warning')}
                            </p>
                        </div>
                    </div>
                </ModalDialog>

            </div >
        </div >
    );
}

export default Settings;
