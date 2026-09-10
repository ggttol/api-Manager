import type { ReactNode } from 'react';
import { NavLink } from 'react-router-dom';
import { useTranslation } from 'react-i18next';

interface PageHeaderProps {
    title: ReactNode;
    description?: ReactNode;
    actions?: ReactNode;
    eyebrow?: ReactNode;
}

export function PageHeader({ title, description, actions, eyebrow }: PageHeaderProps) {
    return <header className="console-page-header">
        <div className="min-w-0">
            {eyebrow && <div className="console-eyebrow">{eyebrow}</div>}
            <h1 className="console-page-title">{title}</h1>
            {description && <p className="console-page-description">{description}</p>}
        </div>
        {actions && <div className="console-page-actions">{actions}</div>}
    </header>;
}

export function AccountPoolTabs() {
    const { i18n } = useTranslation();
    const zh = i18n.language.startsWith('zh');
    return <nav className="console-tabs" aria-label={zh ? '账号池类型' : 'Account pools'}>
        <NavLink to="/accounts" className={({ isActive }) => `console-tab${isActive ? ' active' : ''}`}>Google</NavLink>
        <NavLink to="/codex" className={({ isActive }) => `console-tab${isActive ? ' active' : ''}`}>Codex</NavLink>
    </nav>;
}
