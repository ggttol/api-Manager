import { useSortable } from '@dnd-kit/sortable';
import { CSS } from '@dnd-kit/utilities';
import { GripVertical, ChevronDown, ChevronRight, Trash2 } from 'lucide-react';
import { cn } from '../../utils/cn';
import { useTranslation } from 'react-i18next';
import { useSecretVisibility } from './SecretInput';

export interface PreviewModelEntry {
    _uid: string;
    model: string;
    id: string;
    index: number;
    baseUrl: string;
    apiKey: string;
    displayName: string;
    noImageSupport: boolean;
    provider: string;
    isAg: boolean;
    [key: string]: unknown;
}

export function SortableModelItem({ entry, collapsed, onToggle, onRemove }: {
    entry: PreviewModelEntry;
    collapsed: boolean;
    onToggle: () => void;
    onRemove?: () => void;
}) {
    const { t, i18n } = useTranslation();
    const { revealed, setRevealed } = useSecretVisibility(collapsed);
    const { attributes, listeners, setNodeRef, transform, transition, isDragging } = useSortable({ id: entry._uid });
    const style = {
        transform: CSS.Translate.toString(transform),
        transition: isDragging ? 'none' : transition,
    };

    return (
        <div ref={setNodeRef} style={style} className={cn(
            'rounded-lg border border-[var(--console-border)] bg-[var(--console-surface)]',
            isDragging ? 'opacity-60 z-50 shadow-lg' : ''
        )} onBlur={(event) => {
            if (!event.currentTarget.contains(event.relatedTarget as Node | null)) setRevealed(false);
        }}>
            <div className="flex items-center gap-1.5 px-2.5 py-1.5">
                <button {...attributes} {...listeners} className="cursor-grab active:cursor-grabbing p-0.5 text-gray-300 hover:text-gray-500 dark:text-gray-600 dark:hover:text-gray-400 touch-none">
                    <GripVertical size={14} />
                </button>
                <span className="text-[10px] font-mono font-bold text-gray-400 w-5 text-center shrink-0">{entry.index}</span>
                <button onClick={onToggle} className="p-0.5 text-gray-400 hover:text-gray-600" aria-expanded={!collapsed} aria-label={t('console.model_configuration', { defaultValue: i18n.language.startsWith('zh') ? '模型配置' : 'Model configuration' })}>
                    {collapsed ? <ChevronRight size={12} /> : <ChevronDown size={12} />}
                </button>
                <span className="text-xs font-medium text-gray-800 dark:text-gray-200 flex-1 truncate">{entry.displayName}</span>
                {entry.isAg && <img src="/icon.svg" alt="AG" className="w-4 h-4 rounded shrink-0" />}
                <span className="text-[9px] font-mono text-gray-400 shrink-0 hidden sm:block">{entry.provider}</span>
                {onRemove && (
                    <button onClick={onRemove} className="p-0.5 text-gray-300 hover:text-red-500 transition-colors" aria-label={t('common.delete')} title={t('common.delete')}>
                        <Trash2 size={12} />
                    </button>
                )}
            </div>
            {!collapsed && (
                <div className="px-3 pb-2 pt-0.5 border-t border-gray-100 dark:border-base-200">
                    <button type="button" className="console-button my-2 text-xs" aria-pressed={revealed} onClick={(event) => { event.currentTarget.focus(); setRevealed(!revealed); }}>
                        {revealed
                            ? t('console.hide_secret', { defaultValue: i18n.language.startsWith('zh') ? '隐藏敏感信息' : 'Hide secret' })
                            : t('console.reveal_secret', { defaultValue: i18n.language.startsWith('zh') ? '显示 30 秒' : 'Reveal for 30 seconds' })}
                    </button>
                    <pre className="text-[9px] font-mono text-gray-500 dark:text-gray-400 leading-relaxed whitespace-pre-wrap">
                        {JSON.stringify(entry, (key, value) => {
                            if (key === '_uid' || key === 'isAg') return undefined;
                            if (!revealed && /api.?key|secret|password|authorization|credential|^(?:access[_-]?|refresh[_-]?|auth[_-]?)?token$/i.test(key)) return '••••••••';
                            return value;
                        }, 2)}
                    </pre>
                </div>
            )}
        </div>
    );
}
