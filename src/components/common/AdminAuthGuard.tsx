import React, { useState, useEffect } from 'react';
import { Lock, Key, Globe, AlertCircle, Loader2 } from 'lucide-react';
import { useTranslation } from 'react-i18next';
import { isTauri } from '../../utils/env';

/**
 * AdminAuthGuard
 * 针对 Docker/Web 模式的强制鉴权保护层。
 * 如果检测到没有存储的 API Key 或后端返回 401，将拦截 UI 并要求输入 Key。
 */
export const AdminAuthGuard: React.FC<{ children: React.ReactNode }> = ({ children }) => {
    const { t, i18n } = useTranslation();
    const [isAuthenticated, setIsAuthenticated] = useState(isTauri());
    const [apiKey, setApiKey] = useState('');
    const [showLangMenu, setShowLangMenu] = useState(false);
    const [isLoading, setIsLoading] = useState(false);
    const [error, setError] = useState('');

    useEffect(() => {
        if (isTauri()) return;

        // Listen independently of restoring an existing session key.
        const handleUnauthorized = () => {
            sessionStorage.removeItem('abv_admin_api_key');
            localStorage.removeItem('abv_admin_api_key');
            setIsAuthenticated(false);
        };
        window.addEventListener('abv-unauthorized', handleUnauthorized);

        // Check session storage first.
        const sessionKey = sessionStorage.getItem('abv_admin_api_key');
        if (sessionKey) {
            setIsAuthenticated(true);
            setApiKey(sessionKey);
        } else {
            // Migrate the legacy persistent key into the session.
            const savedKey = localStorage.getItem('abv_admin_api_key');
            if (savedKey) {
                sessionStorage.setItem('abv_admin_api_key', savedKey);
                localStorage.removeItem('abv_admin_api_key');
                setIsAuthenticated(true);
                setApiKey(savedKey);
            }
        }

        return () => window.removeEventListener('abv-unauthorized', handleUnauthorized);
    }, []);

    const handleLogin = async (e: React.FormEvent) => {
        e.preventDefault();
        const trimmedKey = apiKey.trim();
        if (!trimmedKey) return;

        setIsLoading(true);
        setError('');

        try {
            // 先临时存储 key，用于验证请求
            sessionStorage.setItem('abv_admin_api_key', trimmedKey);

            // 调用一个需要认证的 API 来验证密码是否正确
            const response = await fetch('/api/accounts', {
                method: 'GET',
                headers: {
                    'Content-Type': 'application/json',
                    'Authorization': `Bearer ${trimmedKey}`,
                    'x-api-key': trimmedKey
                }
            });

            if (response.ok || response.status === 204) {
                // 验证成功
                localStorage.removeItem('abv_admin_api_key');
                setIsAuthenticated(true);
                window.location.reload();
            } else if (response.status === 401) {
                // 密码错误
                sessionStorage.removeItem('abv_admin_api_key');
                setError(t('login.error_invalid_key'));
            } else {
                sessionStorage.removeItem('abv_admin_api_key');
                setError(`${t('login.error_network')} (HTTP ${response.status})`);
            }
        } catch (err) {
            // 网络错误等
            sessionStorage.removeItem('abv_admin_api_key');
            setError(t('login.error_network'));
        } finally {
            setIsLoading(false);
        }
    };

    const changeLanguage = (lng: string) => {
        i18n.changeLanguage(lng);
        setShowLangMenu(false);
    };

    const languages = [
        { code: 'zh', name: '简体中文' },
        { code: 'zh-TW', name: '繁體中文' },
        { code: 'en', name: 'English' },
        { code: 'ja', name: '日本語' },
        { code: 'ko', name: '한국어' },
        { code: 'ru', name: 'Русский' },
        { code: 'tr', name: 'Türkçe' },
        { code: 'vi', name: 'Tiếng Việt' },
        { code: 'pt', name: 'Português' },
        { code: 'ar', name: 'العربية' },
        { code: 'es', name: 'Español' },
        { code: 'my', name: 'Bahasa Melayu' },
    ];

    if (isAuthenticated) {
        return <>{children}</>;
    }

    return (
        <div className="min-h-screen bg-[var(--console-bg)] flex items-center justify-center px-4 py-24 relative">
            {/* 语言切换按钮 */}
            <div className="absolute top-8 right-8">
                <div className="relative">
                    <button
                        onClick={() => setShowLangMenu(!showLangMenu)}
                        className="console-button"
                        aria-expanded={showLangMenu}
                        aria-label={i18n.language.startsWith('zh') ? '选择语言' : 'Choose language'}
                    >
                        <Globe className="w-4 h-4" />
                        <span className="text-sm font-medium uppercase">{i18n.language.split('-')[0]}</span>
                    </button>

                    {showLangMenu && (
                        <div className="absolute right-0 mt-2 w-40 console-panel !p-2 shadow-lg z-50">
                            {languages.map((lang) => (
                                <button
                                    key={lang.code}
                                    onClick={() => changeLanguage(lang.code)}
                                    className={`w-full text-left px-4 py-2 text-sm hover:bg-slate-50 dark:hover:bg-white/5 transition-colors ${i18n.language === lang.code ? 'text-blue-500 font-bold' : 'text-slate-600 dark:text-slate-300'
                                        }`}
                                >
                                    {lang.name}
                                </button>
                            ))}
                        </div>
                    )}
                </div>
            </div>

            <div className="max-w-md w-full console-panel !p-0 overflow-hidden">
                <div className="p-8">
                    <div className="w-12 h-12 bg-[var(--console-primary-soft)] rounded-xl flex items-center justify-center mb-5 mx-auto">
                        <Lock className="w-6 h-6 text-[var(--console-primary)]" />
                    </div>
                    <h1 className="text-2xl font-semibold text-center mb-2">API Manager</h1>
                    <h2 className="text-sm font-medium text-center mb-2">{t('login.title')}</h2>
                    <p className="text-center console-muted mb-8 text-sm leading-6">{t('login.desc')}</p>

                    <form onSubmit={handleLogin} className="space-y-6">
                        <div className="relative">
                            <Key className="absolute left-4 top-1/2 -translate-y-1/2 w-5 h-5 text-slate-400" />
                            <input
                                type="password"
                                placeholder={t('login.placeholder')}
                                className={`w-full pl-12 pr-4 py-3 bg-[var(--console-surface-muted)] border rounded-lg focus:ring-2 focus:ring-blue-500 text-[var(--console-text)] ${error ? 'border-red-400' : 'border-[var(--console-border)]'}`}
                                aria-label={t('login.placeholder')}
                                autoComplete="current-password"
                                value={apiKey}
                                onChange={(e) => { setApiKey(e.target.value); setError(''); }}
                                autoFocus
                                disabled={isLoading}
                            />
                        </div>
                        {error && (
                            <div role="alert" className="flex items-center gap-2 text-red-500 text-sm">
                                <AlertCircle className="w-4 h-4" />
                                <span>{error}</span>
                            </div>
                        )}
                        <button
                            type="submit"
                            disabled={isLoading || !apiKey.trim()}
                            className="console-button-primary w-full !py-3"
                        >
                            {isLoading ? (
                                <>
                                    <Loader2 className="w-5 h-5 animate-spin" />
                                    {t('login.btn_verifying')}
                                </>
                            ) : (
                                t('login.btn_login')
                            )}
                        </button>
                    </form>

                    <div className="mt-8 pt-6 border-t border-[var(--console-border)] text-center">
                        <p className="text-xs console-muted leading-relaxed">
                            {t('login.note')}
                            <br />
                            {t('login.lookup_hint')}
                            <br />
                            {t('login.config_hint')}
                        </p>
                    </div>
                </div>
            </div>
        </div>
    );
};
