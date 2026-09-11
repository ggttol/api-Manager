import { useState, useEffect, useRef } from 'react';
import { createPortal } from 'react-dom';
import { Plus, Database, Globe, FileClock, Loader2, CheckCircle2, XCircle, Copy, Check, Info, Link2 } from 'lucide-react';
import { useAccountStore } from '../../stores/useAccountStore';
import { useTranslation } from 'react-i18next';
import { listen } from '@tauri-apps/api/event';
import { open } from '@tauri-apps/plugin-dialog';
import { request as invoke } from '../../utils/request';
import { isTauri } from '../../utils/env';
import { copyToClipboard } from '../../utils/clipboard';

interface AddAccountDialogProps {
    onAdd: (email: string, refreshToken: string) => Promise<void>;
    showText?: boolean;
}

type Status = 'idle' | 'loading' | 'success' | 'error';

function AddAccountDialog({ onAdd, showText = true }: AddAccountDialogProps) {
    const { t } = useTranslation();
    const fetchAccounts = useAccountStore(state => state.fetchAccounts);
    const [isOpen, setIsOpen] = useState(false);
    const [activeTab, setActiveTab] = useState<'oauth' | 'token' | 'import'>(isTauri() ? 'oauth' : 'token');
    const [refreshToken, setRefreshToken] = useState('');
    const [oauthUrl, setOauthUrl] = useState('');
    const [oauthUrlCopied, setOauthUrlCopied] = useState(false);
    const [manualCode, setManualCode] = useState('');

    // UI State
    const [status, setStatus] = useState<Status>('idle');
    const [message, setMessage] = useState('');

    const { startOAuthLogin, completeOAuthLogin, cancelOAuthLogin, importFromDb, importV1Accounts, importFromCustomDb } = useAccountStore();

    const oauthUrlRef = useRef(oauthUrl);
    const statusRef = useRef(status);
    const activeTabRef = useRef(activeTab);
    const isOpenRef = useRef(isOpen);
    const mountedRef = useRef(true);
    const oauthFlowOwnedRef = useRef(false);
    const oauthCompletionRef = useRef<Promise<void> | null>(null);
    const oauthPreparationRef = useRef<Promise<void> | null>(null);
    const oauthSessionRef = useRef(0);
    const oauthCloseTimerRef = useRef<number | null>(null);
    const clearWebOAuthWorkRef = useRef<(() => void) | null>(null);

    const isActiveOAuthSession = (session: number) =>
        mountedRef.current && isOpenRef.current && oauthSessionRef.current === session;

    const clearOAuthUiWork = () => {
        clearWebOAuthWorkRef.current?.();
        clearWebOAuthWorkRef.current = null;
        if (oauthCloseTimerRef.current !== null) {
            window.clearTimeout(oauthCloseTimerRef.current);
            oauthCloseTimerRef.current = null;
        }
    };

    const cancelOwnedOAuth = () => {
        oauthSessionRef.current += 1;
        clearOAuthUiWork();
        oauthCompletionRef.current = null;
        if (oauthFlowOwnedRef.current) {
            oauthFlowOwnedRef.current = false;
            void cancelOAuthLogin();
        }
    };

    const resetState = () => {
        setStatus('idle');
        setMessage('');
        setRefreshToken('');
        setOauthUrl('');
        setOauthUrlCopied(false);
    };

    const closeDialog = () => {
        cancelOwnedOAuth();
        setIsOpen(false);
        resetState();
    };

    const scheduleOAuthClose = (session: number) => {
        oauthCloseTimerRef.current = window.setTimeout(() => {
            if (isActiveOAuthSession(session)) closeDialog();
        }, 1500);
    };

    const waitForOAuthCompletion = (startInBrowser: boolean) => {
        if (oauthCompletionRef.current) return oauthCompletionRef.current;

        const completion = startInBrowser ? startOAuthLogin() : completeOAuthLogin();
        oauthCompletionRef.current = completion;
        void completion.then(
            () => {
                if (oauthCompletionRef.current === completion) oauthCompletionRef.current = null;
            },
            () => {
                if (oauthCompletionRef.current === completion) oauthCompletionRef.current = null;
            }
        );
        return completion;
    };

    const completeOwnedOAuth = async (startInBrowser: boolean) => {
        const session = oauthSessionRef.current;
        setStatus('loading');
        setMessage(`${t('accounts.add.tabs.oauth')}...`);
        try {
            await waitForOAuthCompletion(startInBrowser);
            if (!isActiveOAuthSession(session)) return;
            setStatus('success');
            setMessage(`${t('accounts.add.tabs.oauth')} ${t('common.success')}!`);
            scheduleOAuthClose(session);
        } catch (error) {
            if (!isActiveOAuthSession(session)) return;
            const errorMsg = String(error);
            setStatus('error');
            if (errorMsg.includes('Refresh Token') || errorMsg.includes('refresh_token')) {
                setMessage(errorMsg);
            } else if (errorMsg.includes('Tauri') || errorMsg.toLowerCase().includes('environment') || errorMsg.includes('环境')) {
                setMessage(t('common.environment_error', { error: errorMsg }));
            } else {
                setMessage(`${t('accounts.add.tabs.oauth')} ${t('common.error')}: ${errorMsg}`);
            }
        }
    };

    useEffect(() => {
        oauthUrlRef.current = oauthUrl;
        statusRef.current = status;
        activeTabRef.current = activeTab;
        isOpenRef.current = isOpen;
    }, [oauthUrl, status, activeTab, isOpen]);

    useEffect(() => () => {
        mountedRef.current = false;
        cancelOwnedOAuth();
    }, []);

    // Listen for OAuth URL
    useEffect(() => {
        if (!isTauri()) return;
        let unlisten: (() => void) | undefined;

        void listen('oauth-url-generated', (event) => {
            if (isOpenRef.current && activeTabRef.current === 'oauth') {
                setOauthUrl(event.payload as string);
            }
        }).then(listener => {
            unlisten = listener;
        });

        return () => {
            unlisten?.();
        };
    }, []);

    // Listen for OAuth callback completion (user may open the URL manually without clicking Start)
    useEffect(() => {
        if (!isTauri()) return;
        let unlisten: (() => void) | undefined;

        void listen('oauth-callback-received', () => {
            if (!isOpenRef.current || activeTabRef.current !== 'oauth' || !oauthUrlRef.current) return;
            void completeOwnedOAuth(false);
        }).then(listener => {
            unlisten = listener;
        });

        return () => {
            unlisten?.();
        };
    }, []);

    // Pre-generate OAuth URL when dialog opens on OAuth tab (so URL is shown BEFORE "Start OAuth").
    useEffect(() => {
        if (!isOpen || activeTab !== 'oauth' || oauthUrl) return;
        const session = oauthSessionRef.current;
        const previousPreparation = oauthPreparationRef.current;
        const preparation = (async () => {
            await previousPreparation?.catch(() => undefined);
            if (!isActiveOAuthSession(session)) return;

            oauthFlowOwnedRef.current = true;
            const res = await invoke<unknown>('prepare_oauth_url');
            const url = typeof res === 'string'
                ? res
                : res && typeof res === 'object' && 'url' in res && typeof res.url === 'string'
                    ? res.url
                    : '';
            if (!isActiveOAuthSession(session)) {
                void cancelOAuthLogin();
                return;
            }
            if (url) setOauthUrl(url);
        })();
        oauthPreparationRef.current = preparation;
        void preparation.catch((error) => {
            if (isActiveOAuthSession(session)) console.error('Failed to prepare OAuth URL:', error);
        });

    }, [isOpen, activeTab, oauthUrl]);

    // If user navigates away from OAuth tab, cancel the dialog-owned prepared flow.
    useEffect(() => {
        if (isOpen && activeTab !== 'oauth') {
            cancelOwnedOAuth();
            setOauthUrl('');
            setOauthUrlCopied(false);

        }
    }, [isOpen, activeTab]);

    const handleAction = async (
        actionName: string,
        actionFn: () => Promise<any>,
        options?: { clearOauthUrl?: boolean }
    ) => {
        setStatus('loading');
        setMessage(`${actionName}...`);
        if (options?.clearOauthUrl !== false) {
            setOauthUrl(''); // Clear previous URL
        }
        try {
            await actionFn();
            setStatus('success');
            setMessage(`${actionName} ${t('common.success')}!`);

            // 延迟关闭,让用户看到成功状态
            setTimeout(() => {
                setIsOpen(false);
                resetState();
            }, 1500);
        } catch (error) {
            setStatus('error');

            // 改进错误信息显示
            let errorMsg = String(error);

            // 如果是 refresh_token 缺失错误,显示完整信息(包含解决方案)
            if (errorMsg.includes('Refresh Token') || errorMsg.includes('refresh_token')) {
                setMessage(errorMsg);
            } else if (errorMsg.includes('Tauri') || errorMsg.toLowerCase().includes('environment') || errorMsg.includes('环境')) {
                // 环境错误
                setMessage(t('common.environment_error', { error: errorMsg }));
            } else {
                // 其他错误
                setMessage(`${actionName} ${t('common.error')}: ${errorMsg}`);
            }
        }
    };

    const handleSubmit = async () => {
        if (!refreshToken) {
            setStatus('error');
            setMessage(t('accounts.add.token.error_token'));
            return;
        }

        setStatus('loading');

        // 1. 尝试解析输入
        let tokens: string[] = [];
        const input = refreshToken.trim();

        try {
            // 尝试解析为 JSON
            if (input.startsWith('[') && input.endsWith(']')) {
                const parsed = JSON.parse(input);
                if (Array.isArray(parsed)) {
                    tokens = parsed
                        .map((item: any) => item.refresh_token)
                        .filter((t: any) => typeof t === 'string' && t.startsWith('1//'));
                }
            }
        } catch (e) {
            // JSON 解析失败,忽略
            console.debug('JSON parse failed, falling back to regex', e);
        }

        // 2. 如果 JSON 解析没有结果,尝试正则提取 (或者输入不是 JSON)
        if (tokens.length === 0) {
            const regex = /1\/\/[a-zA-Z0-9_\-]+/g;
            const matches = input.match(regex);
            if (matches) {
                tokens = matches;
            }
        }

        // 去重
        tokens = [...new Set(tokens)];

        if (tokens.length === 0) {
            setStatus('error');
            setMessage(t('accounts.add.token.error_token')); // 或者提示"未找到有效 Token"
            return;
        }

        // 3. 批量添加
        let successCount = 0;
        let failCount = 0;

        for (let i = 0; i < tokens.length; i++) {
            const currentToken = tokens[i];
            setMessage(t('accounts.add.token.batch_progress', { current: i + 1, total: tokens.length }));

            try {
                await onAdd("", currentToken);
                successCount++;
            } catch (error) {
                console.error(`Failed to add token ${i + 1}:`, error);
                failCount++;
            }
            // 稍微延迟一下,避免太快
            await new Promise(r => setTimeout(r, 100));
        }

        // 4. 结果反馈
        if (successCount === tokens.length) {
            setStatus('success');
            setMessage(t('accounts.add.token.batch_success', { count: successCount }));
            setTimeout(() => {
                setIsOpen(false);
                resetState();
            }, 1500);
        } else if (successCount > 0) {
            // 部分成功
            setStatus('success'); // 还是用绿色,但提示部分失败
            setMessage(t('accounts.add.token.batch_partial', { success: successCount, fail: failCount }));
            // 不自动关闭,让用户看到结果
        } else {
            // 全部失败
            setStatus('error');
            setMessage(t('accounts.add.token.batch_fail'));
        }
    };


    const handleOAuthWeb = async () => {
        const session = oauthSessionRef.current;
        try {
            setStatus('loading');
            setMessage(t('accounts.add.oauth.btn_start') + '...');
            oauthFlowOwnedRef.current = true;
            const res = await invoke<unknown>('prepare_oauth_url');
            const url = typeof res === 'string' ? res : res && typeof res === 'object' && 'url' in res && typeof res.url === 'string' ? res.url : '';
            if (!url) throw new Error(t('accounts.add.oauth.error_no_url', 'OAuth URLを取得できませんでした'));
            if (!isActiveOAuthSession(session)) {
                void cancelOAuthLogin();
                return;
            }
            setOauthUrl(url);

            const popup = window.open(url, '_blank');
            if (!popup) {
                setStatus('error');
                setMessage(t('accounts.add.oauth.popup_blocked', 'ポップアップがブロックされました'));
                return;
            }

            const handleMessage = async (event: MessageEvent) => {
                if (event.data?.type !== 'oauth-success' || !isActiveOAuthSession(session)) return;
                clearWebOAuthWorkRef.current?.();
                await fetchAccounts();
                if (!isActiveOAuthSession(session)) return;
                setStatus('success');
                setMessage(t('accounts.add.oauth_success') || t('common.success'));
                scheduleOAuthClose(session);
            };
            const timer = window.setInterval(() => {
                if (!popup.closed) return;
                clearWebOAuthWorkRef.current?.();
                if (isActiveOAuthSession(session) && statusRef.current === 'loading') {
                    setStatus('idle');
                    setMessage('');
                }
            }, 1000);
            clearWebOAuthWorkRef.current = () => {
                window.clearInterval(timer);
                window.removeEventListener('message', handleMessage);
                if (!popup.closed) popup.close();
            };
            window.addEventListener('message', handleMessage);
        } catch (error) {
            if (!isActiveOAuthSession(session)) return;
            console.error('OAuth Web Error:', error);
            setStatus('error');
            setMessage(`${t('common.error')}: ${error}`);
        }
    };

    const handleOAuth = () => {
        if (!isTauri()) {
            void handleOAuthWeb();
            return;
        }
        oauthFlowOwnedRef.current = true;
        void completeOwnedOAuth(true);
    };

    const handleCompleteOAuth = () => {
        oauthFlowOwnedRef.current = true;
        void completeOwnedOAuth(false);
    };

    const handleCopyUrl = async () => {
        if (oauthUrl) {
            const success = await copyToClipboard(oauthUrl);
            if (success) {
                setOauthUrlCopied(true);
                window.setTimeout(() => setOauthUrlCopied(false), 1500);
            }
        }
    };

    const handleManualSubmit = async () => {
        if (!manualCode.trim()) return;
        const session = oauthSessionRef.current;
        setStatus('loading');
        setMessage(t('accounts.add.oauth.manual_submitting', '認可コードを送信中...'));

        try {
            oauthFlowOwnedRef.current = true;
            await invoke('submit_oauth_code', { code: manualCode.trim(), state: null });
            await waitForOAuthCompletion(false);
            if (!isActiveOAuthSession(session)) return;
            setStatus('success');
            setMessage(`${t('accounts.add.tabs.oauth')} ${t('common.success')}!`);
            setManualCode('');
            scheduleOAuthClose(session);
        } catch (error) {
            if (!isActiveOAuthSession(session)) return;
            const errStr = String(error);
            setStatus('error');
            setMessage(errStr.includes('No active OAuth flow')
                ? t('accounts.add.oauth.error_no_flow')
                : `${t('common.error')}: ${errStr}`);
        }
    };

    const handleImportDb = () => {
        handleAction(t('accounts.add.import.btn_db'), async () => {
            const accounts = await importFromDb();
            if (Array.isArray(accounts) && accounts.length > 0) {
                setMessage(t('accounts.add.token.batch_success', { count: accounts.length }));
            }
            return accounts;
        });
    };

    const handleImportV1 = () => {
        handleAction(t('accounts.add.import.btn_v1'), importV1Accounts);
    };

    const handleImportCustomDb = async () => {
        try {
            if (!isTauri()) {
                alert(t('common.tauri_api_not_loaded') || 'Storage import only works in desktop app.');
                return;
            }
            const selected = await open({
                multiple: false,
                filters: [{
                    name: 'VSCode DB',
                    extensions: ['vscdb']
                }, {
                    name: 'All Files',
                    extensions: ['*']
                }]
            });

            if (selected && typeof selected === 'string') {
                handleAction(t('accounts.add.import.btn_custom_db') || 'Import Custom DB', () => importFromCustomDb(selected));
            }
        } catch (err) {
            console.error('Failed to open dialog:', err);
        }
    };

    // 状态提示组件
    const StatusAlert = () => {
        if (status === 'idle' || !message) return null;

        const styles = {
            loading: 'alert-info',
            success: 'alert-success',
            error: 'alert-error'
        };

        const icons = {
            loading: <Loader2 className="w-5 h-5 animate-spin" />,
            success: <CheckCircle2 className="w-5 h-5" />,
            error: <XCircle className="w-5 h-5" />
        };

        return (
            <div className={`alert ${styles[status]} mb-4 text-sm py-2 shadow-sm`}>
                {icons[status]}
                <span>{message}</span>
            </div>
        );
    };

    return (
        <>
            <button
                className="console-button console-button-primary"
                onClick={() => {
                    console.log('AddAccountDialog button clicked');
                    oauthSessionRef.current += 1;
                    setIsOpen(true);
                }}
                title={!showText ? t('accounts.add_account') : undefined}
            >
                <Plus className="w-4 h-4" />
                {showText && <span>{t('accounts.add_account')}</span>}
            </button>

            {isOpen && createPortal(
                <div
                    className="fixed inset-0 z-[99999] flex items-center justify-center bg-black/50 backdrop-blur-sm"
                    style={{ position: 'fixed', top: 0, left: 0, right: 0, bottom: 0 }}
                >
                    {/* Draggable Top Region */}
                    <div data-tauri-drag-region className="fixed top-0 left-0 right-0 h-8 z-[1]" />

                    {/* Click outside to close */}
                    <div className="absolute inset-0 z-[0]" onClick={closeDialog} />

                    <div className="bg-white dark:bg-base-100 text-gray-900 dark:text-base-content rounded-2xl shadow-2xl w-full max-w-lg p-6 relative z-[10] m-4 max-h-[90vh] overflow-y-auto">
                        <h3 className="font-bold text-lg mb-4">{t('accounts.add.title')}</h3>

                        {/* Tab 导航 - 胶囊风格 */}
                        <div className="bg-gray-100 dark:bg-base-200 p-1 rounded-xl mb-6 grid grid-cols-3 gap-1">
                            <button
                                className={`py-2 px-3 rounded-lg text-sm font-medium transition-all duration-200 ${activeTab === 'oauth'
                                    ? 'bg-white dark:bg-base-100 shadow-sm text-blue-600 dark:text-blue-400'
                                    : 'text-gray-500 dark:text-gray-400 hover:text-gray-900 dark:hover:text-gray-200 hover:bg-gray-200/50 dark:hover:bg-base-300'
                                    } `}
                                onClick={() => setActiveTab('oauth')}
                            >
                                {t('accounts.add.tabs.oauth')}
                            </button>
                            <button
                                className={`py-2 px-3 rounded-lg text-sm font-medium transition-all duration-200 ${activeTab === 'token'
                                    ? 'bg-white dark:bg-base-100 shadow-sm text-blue-600 dark:text-blue-400'
                                    : 'text-gray-500 dark:text-gray-400 hover:text-gray-900 dark:hover:text-gray-200 hover:bg-gray-200/50 dark:hover:bg-base-300'
                                    } `}
                                onClick={() => setActiveTab('token')}
                            >
                                {t('accounts.add.tabs.token')}
                            </button>
                            <button
                                className={`py-2 px-3 rounded-lg text-sm font-medium transition-all duration-200 ${activeTab === 'import'
                                    ? 'bg-white dark:bg-base-100 shadow-sm text-blue-600 dark:text-blue-400'
                                    : 'text-gray-500 dark:text-gray-400 hover:text-gray-900 dark:hover:text-gray-200 hover:bg-gray-200/50 dark:hover:bg-base-300'
                                    } `}
                                onClick={() => setActiveTab('import')}
                            >
                                {t('accounts.add.tabs.import')}
                            </button>
                        </div>

                        {/* 添加 Web 模式提示 */}
                        {!isTauri() && (
                            <div className="alert alert-info mb-4 text-xs py-2 flex items-center gap-2 bg-blue-50 dark:bg-blue-900/10 text-blue-600 dark:text-blue-400 border-blue-100 dark:border-blue-800">
                                <Info className="w-4 h-4" />
                                <span>{t('accounts.add.oauth.web_hint', '将在新窗口中打开 Google 登录页')}</span>
                            </div>
                        )}

                        {/* 状态提示区 */}
                        <StatusAlert />

                        <div className="min-h-[200px]">
                            {/* OAuth 授权 */}
                            {activeTab === 'oauth' && (
                                <div className="space-y-6 py-4">
                                    <div className="text-center space-y-3">
                                        <div className="bg-blue-50 dark:bg-blue-900/20 p-6 rounded-full w-20 h-20 mx-auto flex items-center justify-center">
                                            <Globe className="w-10 h-10 text-blue-500" />
                                        </div>
                                        <div className="space-y-1">
                                            <h4 className="font-medium text-gray-900 dark:text-gray-100">{t('accounts.add.oauth.recommend')}</h4>
                                            <p className="text-sm text-gray-500 dark:text-gray-400 max-w-xs mx-auto">
                                                {t('accounts.add.oauth.desc')}
                                            </p>
                                        </div>
                                    </div>
                                    <div className="space-y-3">
                                        <button
                                            className="w-full px-4 py-3 bg-blue-600 hover:bg-blue-700 text-white font-medium rounded-xl shadow-lg shadow-blue-500/20 transition-all flex items-center justify-center gap-2 disabled:opacity-70 disabled:cursor-not-allowed"
                                            onClick={handleOAuth}
                                            disabled={status === 'loading' || status === 'success'}
                                        >
                                            {status === 'loading' ? t('accounts.add.oauth.btn_waiting') : t('accounts.add.oauth.btn_start')}
                                        </button>

                                        {oauthUrl && (
                                            <div className="space-y-2">
                                                <div className="text-[11px] text-gray-500 dark:text-gray-400 text-left">
                                                    {t('accounts.add.oauth.link_label')}
                                                </div>
                                                <button
                                                    type="button"
                                                    className="w-full px-4 py-2 bg-white dark:bg-base-100 text-gray-600 dark:text-gray-300 text-sm font-medium rounded-xl border border-dashed border-gray-300 dark:border-gray-600 hover:bg-gray-50 dark:hover:bg-base-200 transition-all flex items-center gap-2"
                                                    onClick={handleCopyUrl}
                                                    title={t('accounts.add.oauth.link_click_to_copy')}
                                                >
                                                    {oauthUrlCopied ? (
                                                        <Check className="w-3.5 h-3.5 text-emerald-600" />
                                                    ) : (
                                                        <Copy className="w-3.5 h-3.5" />
                                                    )}
                                                    <code className="text-[11px] font-mono truncate flex-1 text-left">
                                                        {oauthUrl}
                                                    </code>
                                                    <span className="text-[11px] whitespace-nowrap">
                                                        {oauthUrlCopied ? t('accounts.add.oauth.copied') : t('accounts.add.oauth.copy_link')}
                                                    </span>
                                                </button>

                                                <button
                                                    type="button"
                                                    className="w-full px-4 py-2 bg-white dark:bg-base-100 text-gray-700 dark:text-gray-300 text-sm font-medium rounded-xl border border-gray-200 dark:border-base-300 hover:bg-gray-50 dark:hover:bg-base-200 transition-all flex items-center justify-center gap-2 disabled:opacity-70 disabled:cursor-not-allowed"
                                                    onClick={handleCompleteOAuth}
                                                    disabled={status === 'loading' || status === 'success'}
                                                >
                                                    <CheckCircle2 className="w-4 h-4" />
                                                    {t('accounts.add.oauth.btn_finish')}
                                                </button>
                                            </div>
                                        )}

                                        {/* Manual Code Entry - Always enabled to rescue stuck flows */}
                                        <div className="pt-4 mt-2 border-t border-gray-100 dark:border-base-200">
                                            <div className="text-[11px] font-medium text-gray-400 dark:text-gray-500 mb-2 uppercase tracking-wider">
                                                {t('accounts.add.oauth.manual_hint')}
                                            </div>
                                            <div className="relative group/manual flex gap-2">
                                                <div className="relative flex-1">
                                                    <input
                                                        type="text"
                                                        className="w-full text-xs py-2 px-3 bg-white dark:bg-base-100 border border-gray-200 dark:border-base-300 rounded-xl focus:ring-2 focus:ring-blue-500/20 focus:border-blue-500 outline-none transition-all placeholder:text-gray-300 dark:placeholder:text-gray-600"
                                                        placeholder={t('accounts.add.oauth.manual_placeholder')}
                                                        value={manualCode}
                                                        onChange={(e) => setManualCode(e.target.value)}
                                                    />
                                                </div>
                                                <button
                                                    className="px-4 py-2 bg-neutral text-white dark:bg-white dark:text-neutral text-xs font-semibold rounded-xl hover:opacity-90 active:scale-95 transition-all disabled:opacity-50 disabled:scale-100 flex items-center gap-1.5"
                                                    onClick={handleManualSubmit}
                                                    disabled={!manualCode.trim()}
                                                >
                                                    <Link2 className="w-3.5 h-3.5" />
                                                    {t('common.submit')}
                                                </button>
                                            </div>
                                        </div>
                                    </div>
                                </div>
                            )}

                            {/* Refresh Token */}
                            {activeTab === 'token' && (
                                <div className="space-y-4 py-2">
                                    <div className="bg-gray-50 dark:bg-base-200 p-4 rounded-lg border border-gray-200 dark:border-base-300">
                                        <div className="flex justify-between items-center mb-2">
                                            <span className="text-sm font-medium text-gray-500 dark:text-gray-400">{t('accounts.add.token.label')}</span>
                                        </div>
                                        <textarea
                                            className="textarea textarea-bordered w-full h-32 font-mono text-xs leading-relaxed focus:outline-none focus:border-blue-500 transition-colors bg-white dark:bg-base-100 text-gray-900 dark:text-base-content border-gray-300 dark:border-base-300 placeholder:text-gray-400"
                                            placeholder={t('accounts.add.token.placeholder')}
                                            value={refreshToken}
                                            onChange={(e) => setRefreshToken(e.target.value)}
                                            disabled={status === 'loading' || status === 'success'}
                                        />
                                        <p className="text-[10px] text-gray-400 mt-2">
                                            {t('accounts.add.token.hint')}
                                        </p>
                                    </div>
                                </div>
                            )}

                            {/* 从数据库导入 */}
                            {activeTab === 'import' && (
                                <div className="space-y-6 py-2">
                                    <div className="space-y-2">
                                        <h4 className="font-semibold flex items-center gap-2 text-gray-800 dark:text-gray-200">
                                            <Database className="w-4 h-4 text-gray-600 dark:text-gray-400" />
                                            {t('accounts.add.import.scheme_a')}
                                        </h4>
                                        <p className="text-xs text-gray-500 dark:text-gray-400">
                                            {t('accounts.add.import.scheme_a_desc')}
                                        </p>
                                        <button
                                            className="w-full px-4 py-3 bg-gray-50 dark:bg-base-200 text-gray-700 dark:text-gray-300 font-medium rounded-xl border border-gray-200 dark:border-base-300 hover:bg-blue-50 dark:hover:bg-blue-900/20 hover:border-blue-200 dark:hover:border-blue-800 hover:text-blue-600 dark:hover:text-blue-400 transition-all flex items-center justify-center gap-2 disabled:opacity-50 disabled:cursor-not-allowed mb-2 shadow-sm"
                                            onClick={handleImportDb}
                                            disabled={status === 'loading' || status === 'success'}
                                        >
                                            <CheckCircle2 className="w-4 h-4 opacity-0 group-hover:opacity-100 transition-opacity" />
                                            {t('accounts.add.import.btn_db')}
                                        </button>
                                        <button
                                            className="w-full px-4 py-3 bg-gray-50 dark:bg-base-200 text-gray-700 dark:text-gray-300 font-medium rounded-xl border border-gray-200 dark:border-base-300 hover:bg-indigo-50 dark:hover:bg-indigo-900/20 hover:border-indigo-200 dark:hover:border-indigo-800 hover:text-indigo-600 dark:hover:text-indigo-400 transition-all flex items-center justify-center gap-2 disabled:opacity-50 disabled:cursor-not-allowed shadow-sm"
                                            onClick={handleImportCustomDb}
                                            disabled={status === 'loading' || status === 'success'}
                                        >
                                            <Database className="w-4 h-4" />
                                            {t('accounts.add.import.btn_custom_db') || 'Custom DB (state.vscdb)'}
                                        </button>
                                    </div>

                                    <div className="divider text-xs text-gray-300 dark:text-gray-600">{t('accounts.add.import.or')}</div>

                                    <div className="space-y-2">
                                        <h4 className="font-semibold flex items-center gap-2 text-gray-800 dark:text-gray-200">
                                            <FileClock className="w-4 h-4 text-gray-600 dark:text-gray-400" />
                                            {t('accounts.add.import.scheme_b')}
                                        </h4>
                                        <p className="text-xs text-gray-500 dark:text-gray-400">
                                            {t('accounts.add.import.scheme_b_desc')}
                                        </p>
                                        <button
                                            className="w-full px-4 py-3 bg-gray-50 dark:bg-base-200 text-gray-700 dark:text-gray-300 font-medium rounded-xl border border-gray-200 dark:border-base-300 hover:bg-emerald-50 dark:hover:bg-emerald-900/20 hover:border-emerald-200 dark:hover:border-emerald-800 hover:text-emerald-600 dark:hover:text-emerald-400 transition-all flex items-center justify-center gap-2 disabled:opacity-50 disabled:cursor-not-allowed shadow-sm"
                                            onClick={handleImportV1}
                                            disabled={status === 'loading' || status === 'success'}
                                        >
                                            <FileClock className="w-4 h-4" />
                                            {t('accounts.add.import.btn_v1')}
                                        </button>
                                    </div>
                                </div>
                            )}
                        </div>

                        <div className="flex gap-3 w-full mt-6">
                            <button
                                className="flex-1 px-4 py-2.5 bg-gray-100 dark:bg-base-200 text-gray-700 dark:text-gray-300 font-medium rounded-xl hover:bg-gray-200 dark:hover:bg-base-300 transition-colors focus:outline-none focus:ring-2 focus:ring-200 dark:focus:ring-base-300"
                                onClick={closeDialog}
                                disabled={status === 'success'}
                            >
                                {t('accounts.add.btn_cancel')}
                            </button>
                            {activeTab === 'token' && (
                                <button
                                    className="flex-1 px-4 py-2.5 text-white font-medium rounded-xl shadow-md transition-all focus:outline-none focus:ring-2 focus:ring-offset-2 bg-blue-500 hover:bg-blue-600 focus:ring-blue-500 shadow-blue-100 dark:shadow-blue-900/30 flex justify-center items-center gap-2"
                                    onClick={handleSubmit}
                                    disabled={status === 'loading' || status === 'success'}
                                >
                                    {status === 'loading' ? <Loader2 className="w-4 h-4 animate-spin" /> : null}
                                    {t('accounts.add.btn_confirm')}
                                </button>
                            )}
                        </div>
                    </div>
                </div >,
                document.body
            )
            }
        </>
    );
}

export default AddAccountDialog;
