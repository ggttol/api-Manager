import { Sun, Moon, LogOut, Minimize2 } from 'lucide-react';
import { useTranslation } from 'react-i18next';
import { LanguageDropdown } from './NavDropdowns';
import { LANGUAGES } from './constants';
import { isTauri } from '../../utils/env';
import { useViewStore } from '../../stores/useViewStore';

interface NavSettingsProps {
    theme: 'light' | 'dark';
    currentLanguage: string;
    onThemeToggle: (event: React.MouseEvent<HTMLButtonElement>) => void;
    onLanguageChange: (langCode: string) => void;
}

export function NavSettings({ theme, currentLanguage, onThemeToggle, onLanguageChange }: NavSettingsProps) {
    const { t, i18n } = useTranslation();
    const { setMiniView } = useViewStore();
    const themeLabel = theme === 'light' ? t('nav.theme_to_dark') : t('nav.theme_to_light');
    const logoutLabel = t('console.logout', { defaultValue: i18n.language.startsWith('zh') ? '退出登录' : 'Sign out' });
    const miniLabel = t('nav.mini_view', { defaultValue: i18n.language.startsWith('zh') ? '迷你视图' : 'Mini view' });

    const handleLogout = () => {
        sessionStorage.removeItem('abv_admin_api_key');
        localStorage.removeItem('abv_admin_api_key');
        window.location.reload();
    };

    return (
        <div className="console-topbar-controls">
            {isTauri() && (
                <button type="button" onClick={() => setMiniView(true)} className="console-icon-button" title={miniLabel} aria-label={miniLabel}>
                    <Minimize2 size={18} aria-hidden="true" />
                </button>
            )}
            <button type="button" onClick={onThemeToggle} className="console-icon-button" title={themeLabel} aria-label={themeLabel}>
                {theme === 'light' ? <Moon size={18} aria-hidden="true" /> : <Sun size={18} aria-hidden="true" />}
            </button>
            <LanguageDropdown currentLanguage={currentLanguage} languages={LANGUAGES} onLanguageChange={onLanguageChange} />
            {!isTauri() && (
                <button type="button" onClick={handleLogout} className="console-icon-button console-logout-button" title={logoutLabel} aria-label={logoutLabel}>
                    <LogOut size={18} aria-hidden="true" />
                </button>
            )}
        </div>
    );
}
