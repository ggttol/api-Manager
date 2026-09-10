import React, { useEffect, useState } from 'react';
import { useTranslation } from 'react-i18next';
import { request as invoke } from '../../utils/request';
import { Trash2, Check, Plus, Search, X } from 'lucide-react';

interface IpWhitelistEntry {
    ip_pattern: string;
    description?: string;
    added_at: number;
    added_by?: string;
}

interface Props {
    refreshKey?: number;
}

export const WhitelistManager: React.FC<Props> = ({ refreshKey }) => {
    const { t } = useTranslation();
    const [entries, setEntries] = useState<IpWhitelistEntry[]>([]);
    const [loading, setLoading] = useState(true);
    const [loadError, setLoadError] = useState(false);
    const [search, setSearch] = useState('');

    // Add Modal State
    const [isAddOpen, setIsAddOpen] = useState(false);
    const [newIp, setNewIp] = useState('');
    const [newDescription, setNewDescription] = useState('');

    const loadWhitelist = async () => {
        setLoading(true);
        setLoadError(false);
        try {
            const data = await invoke<IpWhitelistEntry[]>('get_ip_whitelist');
            setEntries(data);
        } catch (e) {
            setLoadError(true);
            console.error('Failed to load whitelist', e);
        } finally {
            setLoading(false);
        }
    };

    useEffect(() => {
        loadWhitelist();
    }, [refreshKey]);

    const handleAdd = async () => {
        try {
            await invoke('add_ip_to_whitelist', {
                request: {
                    ipPattern: newIp,
                    description: newDescription || null,
                }
            });
            setIsAddOpen(false);
            setNewIp('');
            setNewDescription('');
            loadWhitelist();
        } catch (e) {
            console.error('Failed to add to whitelist', e);
            alert(`${t('security.blacklist.error_add_failed')}: ${String(e)}`);
        }
    };

    const handleRemove = async (ipPattern: string) => {
        // 乐观更新：立即从UI中移除
        setEntries(prev => prev.filter(e => e.ip_pattern !== ipPattern));

        try {
            await invoke('remove_ip_from_whitelist', { ipPattern: ipPattern });
        } catch (e) {
            console.error('Failed to remove from whitelist', e);
            // 如果删除失败，重新加载数据恢复UI
            loadWhitelist();
        }
    };

    const filteredEntries = entries.filter(e =>
        e.ip_pattern.includes(search) || (e.description && e.description.toLowerCase().includes(search.toLowerCase()))
    );

    return (
        <div className="flex flex-col h-full min-h-0 min-w-0">
            <div className="console-toolbar p-4 border-b border-[var(--console-border)]">
                <button
                    onClick={() => setIsAddOpen(true)}
                    className="console-button console-button-primary"
                >
                    <Plus size={16} /> {t('security.whitelist.add_ip')}
                </button>

                <div className="relative flex-1 min-w-[180px] max-w-md">
                    <Search className="absolute left-3 top-2.5 text-gray-400" size={16} />
                    <input
                        type="text"
                        placeholder={t('security.blacklist.search_placeholder')}
                        aria-label={t('security.blacklist.search_placeholder')}
                        className="input input-sm input-bordered w-full pl-9"
                        value={search}
                        onChange={(e) => setSearch(e.target.value)}
                    />
                </div>

                <div className="flex-1"></div>
            </div>

            <div className="flex-1 min-h-0 overflow-auto p-4">
                {loadError && <div role="alert" className="py-4 text-sm text-error">{t('common.load_failed')}</div>}
                {loading && entries.length === 0 && <div role="status" className="py-10 text-center text-gray-500">{t('common.loading')}</div>}
                <div className="grid grid-cols-1 md:grid-cols-2 xl:grid-cols-3 gap-4">
                    {filteredEntries.map(entry => (
                        <div key={entry.ip_pattern} className="console-panel min-w-0">
                            <div className="flex items-start justify-between gap-3 mb-2">
                                <h3 className="font-mono font-semibold text-sm break-all">{entry.ip_pattern}</h3>
                                <button
                                    onClick={() => handleRemove(entry.ip_pattern)}
                                    className="console-button !p-2 shrink-0 text-error"
                                    aria-label={t('common.delete')}
                                >
                                    <Trash2 size={14} />
                                </button>
                            </div>

                            {entry.description && (
                                <p className="text-sm text-gray-600 dark:text-gray-400 mb-2 flex items-center gap-1 break-all">
                                    <Check size={12} className="text-green-500" /> {entry.description}
                                </p>
                            )}

                            <div className="text-xs text-gray-400 flex flex-col gap-1 mt-3 pt-3 border-t border-gray-50 dark:border-base-200 relative z-10">
                                <span>{t('security.blacklist.added_at')}: {new Date(entry.added_at * 1000).toLocaleString()}</span>
                            </div>
                        </div>
                    ))}
                    {!loading && !loadError && filteredEntries.length === 0 && (
                        <div className="col-span-full text-center py-10 text-gray-400">
                            {t('security.whitelist.no_data')}
                        </div>
                    )}
                </div>
            </div>

            {/* Add Modal */}
            {isAddOpen && (
                <div className="fixed inset-0 z-50 flex items-center justify-center bg-black/50 p-4">
                    <div role="dialog" aria-modal="true" aria-label={t('security.whitelist.add_title')} className="console-panel w-full max-w-md max-h-[90vh] overflow-y-auto">
                        <div className="flex justify-between items-center mb-4">
                            <h3 className="text-lg font-bold">{t('security.whitelist.add_title')}</h3>
                            <button onClick={() => setIsAddOpen(false)} aria-label={t('common.close')} className="btn btn-ghost btn-sm btn-circle">
                                <X size={18} />
                            </button>
                        </div>

                        <div className="space-y-4">
                            <div>
                                <label className="label">{t('security.blacklist.ip_cidr_label')}</label>
                                <input
                                    type="text"
                                    className="input input-bordered w-full"
                                    placeholder={t('security.blacklist.ip_cidr_placeholder')}
                                    value={newIp}
                                    onChange={e => setNewIp(e.target.value)}
                                />
                            </div>
                            <div>
                                <label className="label">{t('security.whitelist.description_label')}</label>
                                <input
                                    type="text"
                                    className="input input-bordered w-full"
                                    placeholder={t('security.whitelist.description_placeholder')}
                                    value={newDescription}
                                    onChange={e => setNewDescription(e.target.value)}
                                />
                            </div>

                            <div className="flex justify-end gap-3 mt-6">
                                <button
                                    className="console-button"
                                    onClick={() => setIsAddOpen(false)}
                                >
                                    {t('security.whitelist.cancel')}
                                </button>
                                <button
                                    className="console-button console-button-primary"
                                    onClick={handleAdd}
                                    disabled={!newIp}
                                >
                                    {t('security.whitelist.add_btn')}
                                </button>
                            </div>
                        </div>
                    </div>
                </div>
            )}
        </div>
    );
};
