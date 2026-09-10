import { NavLink } from 'react-router-dom';
import { useConfigStore } from '../../stores/useConfigStore';
import type { NavGroup } from './constants';

interface NavMenuProps {
    groups: NavGroup[];
    label: string;
    collapsed?: boolean;
    onNavigate?: () => void;
}

export function NavMenu({ groups, label, collapsed = false, onNavigate }: NavMenuProps) {
    const { isMenuItemHidden } = useConfigStore();

    return (
        <nav className="console-navigation" aria-label={label}>
            {groups.map((group) => {
                const items = group.items.filter(item => !isMenuItemHidden(item.path));
                if (!items.length) return null;
                return (
                    <div key={group.id} className="console-nav-group" role="group" aria-label={group.label}>
                        {!collapsed && <div className="console-nav-heading">{group.label}</div>}
                        {items.map((item) => (
                            <NavLink
                                key={item.path}
                                to={item.path}
                                end={item.path === '/'}
                                draggable="false"
                                onClick={onNavigate}
                                title={item.label}
                                aria-label={collapsed ? item.label : undefined}
                                className={({ isActive }) => `console-nav-link${isActive ? ' active' : ''}`}
                            >
                                <item.icon size={18} aria-hidden="true" />
                                {!collapsed && <span className="console-nav-label">{item.label}</span>}
                            </NavLink>
                        ))}
                    </div>
                );
            })}
        </nav>
    );
}
