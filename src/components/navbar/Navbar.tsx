import { useEffect, useRef, useState, type ReactNode } from 'react';
import { useLocation } from 'react-router-dom';
import { LayoutDashboard, Users, Network, Activity, BarChart3, Settings, Lock, BookOpen, Terminal, KeyRound, Menu, X, PanelLeftClose, PanelLeftOpen, ChevronRight } from 'lucide-react';
import { useTranslation } from 'react-i18next';
import { useConfigStore } from '../../stores/useConfigStore';
import { isLinux } from '../../utils/env';
import { NavLogo } from './NavLogo';
import { NavMenu } from './NavMenu';
import { NavSettings } from './NavSettings';
import type { NavGroup } from './constants';
import './ConsoleShell.css';

function Navbar({ children }: { children: ReactNode }) {
    const { t, i18n } = useTranslation();
    const { config, saveConfig } = useConfigStore();
    const location = useLocation();
    const [collapsed, setCollapsed] = useState(false);
    const [mobileOpen, setMobileOpen] = useState(false);
    const drawerRef = useRef<HTMLDialogElement>(null);
    const menuButtonRef = useRef<HTMLButtonElement>(null);
    const label = (key: string, zh: string, en: string) => t(`console.${key}`, { defaultValue: i18n.language.startsWith('zh') ? zh : en });
    const navigationLabel = label('navigation', '主导航', 'Main navigation');
    const collapseLabel = collapsed ? label('expand_sidebar', '展开侧栏', 'Expand sidebar') : label('collapse_sidebar', '收起侧栏', 'Collapse sidebar');
    const groups: NavGroup[] = [
        { id: 'overview', label: label('overview', '概览', 'Overview'), items: [
            { path: '/', label: t('nav.dashboard'), icon: LayoutDashboard },
        ] },
        { id: 'resources', label: label('resources', '资源管理', 'Resources'), items: [
            { path: '/accounts', label: label('google_accounts', 'Google 账号池', 'Google accounts'), icon: Users },
            { path: '/codex', label: label('codex_accounts', 'Codex 账号池', 'Codex accounts'), icon: Terminal },
            { path: '/user-token', label: t('nav.user_token'), icon: KeyRound },
        ] },
        { id: 'gateway', label: label('gateway', 'API 网关', 'API gateway'), items: [
            { path: '/api-proxy', label: t('nav.proxy'), icon: Network },
        ] },
        { id: 'observability', label: label('observability', '可观测性', 'Observability'), items: [
            { path: '/monitor', label: t('nav.call_records'), icon: Activity },
            { path: '/token-stats', label: t('nav.token_stats'), icon: BarChart3 },
        ] },
        { id: 'security', label: label('access_control', '访问控制', 'Access control'), items: [
            { path: '/security', label: t('nav.security'), icon: Lock },
        ] },
        { id: 'guide', label: label('documentation', '文档', 'Documentation'), items: [
            { path: '/api-guide', label: t('nav.api_guide'), icon: BookOpen },
        ] },
        { id: 'settings', label: label('system', '系统', 'System'), items: [
            { path: '/settings', label: t('nav.settings'), icon: Settings },
        ] },
    ];
    const currentGroup = groups.find(group => group.items.some(item => item.path === location.pathname));
    const currentItem = currentGroup?.items.find(item => item.path === location.pathname);

    useEffect(() => {
        setMobileOpen(false);
    }, [location.pathname, location.search, location.hash]);

    useEffect(() => {
        const drawer = drawerRef.current;
        if (!drawer) return;
        if (mobileOpen && !drawer.open) drawer.showModal();
        if (!mobileOpen && drawer.open) drawer.close();
    }, [mobileOpen]);

    useEffect(() => {
        const desktop = window.matchMedia('(min-width: 1024px)');
        const handleResize = () => {
            if (desktop.matches) setMobileOpen(false);
        };
        desktop.addEventListener('change', handleResize);
        return () => desktop.removeEventListener('change', handleResize);
    }, []);

    const toggleTheme = async (event: React.MouseEvent<HTMLButtonElement>) => {
        if (!config) return;
        const newTheme = config.theme === 'light' ? 'dark' : 'light';
        // Keep the existing desktop-safe theme transition and config persistence.
        if ('startViewTransition' in document && !isLinux()) {
            const x = event.clientX;
            const y = event.clientY;
            const endRadius = Math.hypot(Math.max(x, window.innerWidth - x), Math.max(y, window.innerHeight - y));
            // @ts-ignore -- supported browsers expose this progressive enhancement.
            const transition = document.startViewTransition(async () => {
                await saveConfig({ ...config, theme: newTheme, language: config.language }, true);
            });
            transition.ready.then(() => {
                const isDarkMode = newTheme === 'dark';
                const clipPath = isDarkMode
                    ? [`circle(${endRadius}px at ${x}px ${y}px)`, `circle(0px at ${x}px ${y}px)`]
                    : [`circle(0px at ${x}px ${y}px)`, `circle(${endRadius}px at ${x}px ${y}px)`];
                document.documentElement.animate({ clipPath }, {
                    duration: 500,
                    easing: 'ease-in-out',
                    fill: 'forwards',
                    pseudoElement: isDarkMode ? '::view-transition-old(root)' : '::view-transition-new(root)',
                });
            });
        } else {
            await saveConfig({ ...config, theme: newTheme, language: config.language }, true);
        }
    };

    const handleLanguageChange = async (langCode: string) => {
        if (!config) return;
        await saveConfig({ ...config, language: langCode, theme: config.theme }, true);
    };

    return (
        <div className={`console-shell${collapsed ? ' console-shell-collapsed' : ''}`}>
            <a className="console-skip-link" href="#console-main">{label('skip_content', '跳转到主内容', 'Skip to content')}</a>
            <aside className="console-sidebar">
                <div className="console-sidebar-brand"><NavLogo collapsed={collapsed} /></div>
                <NavMenu groups={groups} label={navigationLabel} collapsed={collapsed} />
                <div className="console-sidebar-footer">
                    <button type="button" className="console-collapse-button" onClick={() => setCollapsed(value => !value)} aria-label={collapseLabel} title={collapseLabel} aria-expanded={!collapsed}>
                        {collapsed ? <PanelLeftOpen size={18} aria-hidden="true" /> : <PanelLeftClose size={18} aria-hidden="true" />}
                        {!collapsed && <span>{collapseLabel}</span>}
                    </button>
                </div>
            </aside>
            <div className="console-workspace">
                <header className="console-topbar">
                    <button ref={menuButtonRef} type="button" className="console-icon-button console-mobile-toggle" aria-label={navigationLabel} aria-expanded={mobileOpen} aria-controls="console-mobile-navigation" onClick={() => setMobileOpen(true)}>
                        <Menu size={20} aria-hidden="true" />
                    </button>
                    <div className="console-breadcrumb" aria-label={label('current_page', '当前位置', 'Current page')}>
                        <span className="console-breadcrumb-group">{currentGroup?.label}</span>
                        <ChevronRight size={14} className="console-breadcrumb-chevron" aria-hidden="true" />
                        <span className="console-breadcrumb-current">{currentItem?.label || t('common.app_name', 'API Manager')}</span>
                    </div>
                    <NavSettings theme={config?.theme === 'dark' ? 'dark' : 'light'} currentLanguage={config?.language || 'en'} onThemeToggle={toggleTheme} onLanguageChange={handleLanguageChange} />
                </header>
                <main id="console-main" tabIndex={-1} className="console-main">{children}</main>
            </div>
            <dialog
                ref={drawerRef}
                id="console-mobile-navigation"
                className="console-mobile-drawer"
                aria-label={navigationLabel}
                onCancel={() => setMobileOpen(false)}
                onClose={() => {
                    setMobileOpen(false);
                    if (window.matchMedia('(max-width: 1023px)').matches) menuButtonRef.current?.focus();
                }}
                onClick={(event) => {
                    if (event.target === event.currentTarget) setMobileOpen(false);
                }}
            >
                <div className="console-drawer-content">
                    <div className="console-drawer-header">
                        <div onClick={() => setMobileOpen(false)}><NavLogo /></div>
                        <button type="button" autoFocus className="console-icon-button" aria-label={label('close_navigation', '关闭导航', 'Close navigation')} onClick={() => setMobileOpen(false)}>
                            <X size={20} aria-hidden="true" />
                        </button>
                    </div>
                    <NavMenu groups={groups} label={navigationLabel} onNavigate={() => setMobileOpen(false)} />
                </div>
            </dialog>
        </div>
    );
}

export default Navbar;
