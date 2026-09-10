import { Link } from 'react-router-dom';
import { useTranslation } from 'react-i18next';
import LogoIcon from '../../../src-tauri/icons/icon.png';

export function NavLogo({ collapsed = false }: { collapsed?: boolean }) {
    const { t } = useTranslation();

    return (
        <Link to="/" draggable="false" className="console-brand" aria-label={t('common.app_name', 'API Manager')}>
            <div className="relative flex items-center justify-center">
                <img
                    src={LogoIcon}
                    alt="Logo"
                    className="h-8 w-8 shrink-0"
                    draggable="false"
                />
            </div>

            {!collapsed && <span className="console-brand-name">{t('common.app_name', 'API Manager')}</span>}
        </Link>
    );
}
