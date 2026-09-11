import { useState, useCallback, useEffect, useRef } from 'react';
import { useTranslation } from 'react-i18next';
import { RefreshCw, X, CodeXml } from 'lucide-react';
import {
    DndContext, closestCenter, KeyboardSensor, PointerSensor,
    useSensor, useSensors, type DragEndEvent,
} from '@dnd-kit/core';
import {
    arrayMove, SortableContext, sortableKeyboardCoordinates, verticalListSortingStrategy,
} from '@dnd-kit/sortable';
import { cn } from '../../utils/cn';
import { request as invoke } from '../../utils/request';
import { showToast } from '../common/ToastContainer';
import { useProxyModels } from '../../hooks/useProxyModels';
import { SortableModelItem, type PreviewModelEntry } from './SortableModelItem';

interface OpenCodeSyncModalProps {
    proxyUrl: string;
    apiKey: string;
    getFormattedProxyUrl: (app: 'Claude' | 'Codex' | 'Gemini' | 'OpenCode' | 'Droid') => string;
    syncAccounts: boolean;
    onClose: () => void;
    onSyncDone: () => void;
}

export function OpenCodeSyncModal({ proxyUrl, apiKey, getFormattedProxyUrl, syncAccounts, onClose, onSyncDone }: OpenCodeSyncModalProps) {
    const { t } = useTranslation();
    const { models: antigravityModels, canonicalFamilies } = useProxyModels();
    const [selectedModels, setSelectedModels] = useState<Set<string>>(new Set());
    const [previewModels, setPreviewModels] = useState<PreviewModelEntry[]>([]);
    const [syncing, setSyncing] = useState(false);
    const [hasAuthPlugin, setHasAuthPlugin] = useState(false);
    const [customBaseUrl, setCustomBaseUrl] = useState(proxyUrl);
    const [configError, setConfigError] = useState<string | null>(null);
    const [configState, setConfigState] = useState<'loading' | 'ready' | 'error'>('loading');
    const [loadedConfig, setLoadedConfig] = useState<Record<string, any> | null>(null);
    const configRequest = useRef<Promise<Record<string, any>> | null>(null);
    const initialized = useRef(false);

    const sensors = useSensors(
        useSensor(PointerSensor, { activationConstraint: { distance: 3 } }),
        useSensor(KeyboardSensor, { coordinateGetter: sortableKeyboardCoordinates }),
    );

    const rebuildPreview = useCallback((selectedIds: Set<string>) => {
        const selected = antigravityModels.filter(m => selectedIds.has(m.id));
        const newEntries: PreviewModelEntry[] = selected.map((m, i) => ({
            _uid: `new-${i}`,
            model: m.id,
            id: m.id,
            index: i,
            baseUrl: '', // OpenCode uses provider-level base URL
            apiKey: apiKey,
            displayName: m.name,
            noImageSupport: false,
            provider: m.id.includes('claude') ? 'anthropic' : 'google',
            isAg: true,
        }));
        setPreviewModels(newEntries);
    }, [antigravityModels, apiKey]);

    // Keep the request alive across dependency changes and StrictMode's
    // setup/cleanup/setup cycle. The response is applied only once, after the
    // canonical catalog needed to normalize existing IDs is available.
    useEffect(() => {
        if (!configRequest.current) {
            configRequest.current = invoke<string>('get_opencode_config_content', {
                request: { fileName: 'opencode.json', parsed: true },
            }).then(content => {
                const parsed: unknown = JSON.parse(content);
                if (!parsed || typeof parsed !== 'object' || Array.isArray(parsed)) {
                    throw new Error('OpenCode configuration must be a JSON object');
                }
                return parsed as Record<string, any>;
            });
        }

        let cancelled = false;
        configRequest.current
            .then(config => {
                if (!cancelled) setLoadedConfig(config);
            })
            .catch(error => {
                if (cancelled) return;
                setConfigError(error instanceof Error ? error.message : String(error));
                setConfigState('error');
            });
        return () => { cancelled = true; };
    }, []);

    useEffect(() => {
        if (initialized.current || !loadedConfig || antigravityModels.length === 0 || canonicalFamilies.length === 0) return;
        initialized.current = true;

        const existingModelIds = new Set<string>();
        const addModelId = (id: string) => {
            const family = canonicalFamilies.find(candidate =>
                candidate.match_ids.includes(id.toLowerCase()),
            );
            existingModelIds.add(family?.canonical_id ?? id);
        };
        const provider = loadedConfig.provider as Record<string, any> | undefined;
        const managedModels = provider?.['antigravity-manager']?.models;
        if (managedModels && typeof managedModels === 'object') {
            Object.keys(managedModels).forEach(addModelId);
        }
        if (existingModelIds.size === 0) {
            for (const name of ['anthropic', 'google']) {
                const models = provider?.[name]?.models;
                if (models && typeof models === 'object') Object.keys(models).forEach(addModelId);
            }
        }
        const plugins = Array.isArray(loadedConfig.plugin) ? loadedConfig.plugin : [];
        setHasAuthPlugin(plugins.some((plugin: unknown) =>
            typeof plugin === 'string' && plugin.includes('opencode-antigravity-auth'),
        ));
        const baseUrl = provider?.['antigravity-manager']?.options?.baseURL;
        if (typeof baseUrl === 'string') setCustomBaseUrl(baseUrl);
        setSelectedModels(existingModelIds);
        rebuildPreview(existingModelIds);
        setConfigState('ready');
    }, [antigravityModels, canonicalFamilies, loadedConfig, rebuildPreview]);

    const allSelected = antigravityModels.length > 0 && antigravityModels.every(m => selectedModels.has(m.id));
    const toggleAll = () => {
        if (configState !== 'ready') return;
        const next = allSelected ? new Set<string>() : new Set(antigravityModels.map(m => m.id));
        setSelectedModels(next);
        rebuildPreview(next);
    };

    const toggleModel = (modelId: string) => {
        if (configState !== 'ready') return;
        const next = new Set(selectedModels);
        if (next.has(modelId)) next.delete(modelId); else next.add(modelId);
        setSelectedModels(next);
        rebuildPreview(next);
    };

    const handleDragEnd = (event: DragEndEvent) => {
        const { active, over } = event;
        if (!over || active.id === over.id) return;
        const oldIdx = previewModels.findIndex(m => m._uid === active.id);
        const newIdx = previewModels.findIndex(m => m._uid === over.id);
        if (oldIdx < 0 || newIdx < 0) return;
        setPreviewModels(arrayMove([...previewModels], oldIdx, newIdx).map((m, i) => ({
            ...m, index: i,
        })));
    };

    const handleRemoveModel = (uid: string) => {
        const nextPreviews = previewModels.filter(m => m._uid !== uid);
        setPreviewModels(nextPreviews);
        const nextSelected = new Set(nextPreviews.map(p => p.model));
        setSelectedModels(nextSelected);
    };

    const executeOpenCodeSync = async () => {
        if (configState !== 'ready') return;
        setSyncing(true);
        try {
            // Send both the model id and its display name so the backend can write a
            // correct, recognizable `name` for models not in its catalog (instead of a
            // machine-derived name that loses variant info like "(High)").
            const models = previewModels.map(m => ({ id: m.model, name: m.displayName }));
            // OpenCode follows the OpenAI protocol and requires a baseURL ending in /v1.
            // If the user has overridden the BaseURL in the input box, honor it as-is;
            // otherwise apply the canonical /v1 formatting via getFormattedProxyUrl.
            const effectiveProxyUrl = customBaseUrl.trim() || getFormattedProxyUrl('OpenCode');
            await invoke('execute_opencode_sync', {
                proxyUrl: effectiveProxyUrl,
                apiKey,
                syncAccounts,
                models
            });
            showToast(t('proxy.opencode_sync.toast.sync_success', { defaultValue: 'OpenCode 同步成功' }), 'success');
            onSyncDone();
            onClose();
        } catch (error: any) {
            showToast(error.toString(), 'error');
        } finally {
            setSyncing(false);
        }
    };

    const groups = [...new Set(antigravityModels.map(m => m.group))];

    return (
        <div className="fixed inset-0 z-[300] flex items-center justify-center p-4 bg-black/50 backdrop-blur-sm animate-in fade-in duration-200">
            <div className="bg-white dark:bg-base-100 rounded-2xl shadow-2xl border border-gray-200 dark:border-base-300 w-full max-w-2xl max-h-[85vh] overflow-hidden animate-in zoom-in-95 duration-200 flex flex-col">
                {/* Header */}
                <div className="px-5 pt-4 pb-3 shrink-0">
                    <div className="flex items-center justify-between">
                        <div className="flex items-center gap-2.5">
                            <div className="p-2 bg-blue-50 dark:bg-blue-900/20 rounded-lg">
                                <CodeXml size={18} className="text-blue-500" />
                            </div>
                            <div>
                                <h3 className="text-sm font-bold text-gray-900 dark:text-base-content">
                                    {t('proxy.config.opencode_sync.modal_title', { defaultValue: '选择 OpenCode 模型' })}
                                </h3>
                                <p className="text-[10px] text-gray-400 mt-0.5">~/.config/opencode/opencode.json (or .jsonc)</p>
                            </div>
                        </div>
                        <button type="button" onClick={onClose} className="p-1.5 rounded-lg hover:bg-gray-100 dark:hover:bg-base-300 transition-colors">
                            <X size={16} className="text-gray-400" />
                        </button>
                    </div>
                </div>

                {configState === 'loading' && (
                    <div className="px-5 py-2 text-xs text-gray-400 border-b border-gray-100 dark:border-base-200">
                        {t('common.loading', { defaultValue: 'Loading configuration…' })}
                    </div>
                )}
                {configState === 'error' && (
                    <div className="px-5 py-2 text-xs text-red-600 bg-red-50 dark:bg-red-900/20 border-b border-red-100 dark:border-red-900/30">
                        {t('proxy.config.opencode_sync.config_read_error', { defaultValue: 'Unable to read OpenCode configuration: {{error}}', error: configError })}
                    </div>
                )}
                <div className="px-5 py-2 shrink-0 border-b border-gray-100 dark:border-base-200 bg-gray-50/50 dark:bg-base-200">
                    <div className="flex flex-col gap-1.5">
                        <div className="flex items-center justify-between">
                            <label htmlFor="customBaseUrl" className="text-[10px] font-bold text-gray-400 uppercase tracking-wider">
                                {t('proxy.config.opencode_sync.custom_base_url_label', { defaultValue: 'Custom Manager BaseURL' })}
                            </label>
                            <span className="text-[9px] text-gray-400 italic font-medium">
                                {t('proxy.config.opencode_sync.custom_base_url_desc', { defaultValue: 'For Docker Compose networking' })}
                            </span>
                        </div>
                        <div className="relative group">
                            <input
                                id="customBaseUrl"
                                type="text"
                                value={customBaseUrl}
                                onChange={(event) => setCustomBaseUrl(event.target.value)}
                                placeholder="e.g. http://antigravity-manager:8045/v1"
                                disabled={configState !== 'ready'}
                                className="w-full px-3 py-1.5 text-xs bg-white dark:bg-base-100 border border-gray-200 dark:border-base-300 rounded-lg focus:ring-1 focus:ring-blue-500 focus:border-blue-500 outline-none transition-all"
                            />
                            {customBaseUrl !== proxyUrl && (
                                <button
                                    type="button"
                                    disabled={configState !== 'ready'}
                                    onClick={() => setCustomBaseUrl(proxyUrl)}
                                    className="absolute right-2 top-1/2 -translate-y-1/2 text-[9px] text-blue-500 hover:text-blue-600 font-medium disabled:text-gray-300"
                                >
                                    {t('proxy.config.opencode_sync.custom_base_url_reset', { defaultValue: 'Reset' })}
                                </button>
                            )}
                        </div>
                    </div>
                </div>

                {/* 模型选择区 */}
                <div className="px-5 pb-3 shrink-0 border-b border-gray-100 dark:border-base-200">
                    <div className="flex items-center justify-between mb-2">
                        <span className="text-[10px] font-bold text-gray-400 uppercase tracking-wider">
                            {t('proxy.config.opencode_sync.select_models', { defaultValue: '选择要同步的模型' })}
                            <span className="ml-2 text-gray-300">{selectedModels.size}/{antigravityModels.length}</span>
                        </span>
                        <button type="button" disabled={configState !== 'ready'} onClick={toggleAll} className="text-[10px] text-blue-500 hover:text-blue-600 font-medium transition-colors disabled:text-gray-300 disabled:cursor-not-allowed">
                            {allSelected ? t('common.deselect_all', { defaultValue: '取消全选' }) : t('common.select_all', { defaultValue: '全选' })}
                        </button>
                    </div>
                    <div className="space-y-2 max-h-[25vh] overflow-auto">
                        {groups.map(group => {
                            const groupModels = antigravityModels.filter(model => model.group === group);
                            return (
                                <div key={group}>
                                    <div className="text-[9px] font-bold text-gray-400 uppercase tracking-widest mb-1">{group}</div>
                                    <div className="flex flex-wrap gap-1.5">
                                        {groupModels.map(model => {
                                            const selected = selectedModels.has(model.id);
                                            return (
                                                <button
                                                    type="button"
                                                    key={model.id}
                                                    disabled={configState !== 'ready'}
                                                    onClick={() => toggleModel(model.id)}
                                                    className={cn(
                                                        "px-2.5 py-1 rounded-md text-[11px] font-medium transition-all duration-150 border",
                                                        selected
                                                            ? "bg-blue-500 text-white border-blue-500"
                                                            : "bg-gray-50 dark:bg-base-200 text-gray-500 dark:text-gray-400 border-gray-200 dark:border-base-300 hover:border-blue-300",
                                                    )}
                                                >
                                                    {model.name}
                                                </button>
                                            );
                                        })}
                                    </div>
                                </div>
                            );
                        })}
                    </div>
                </div>

                {/* Auth Plugin Warning */}
                {hasAuthPlugin && (
                    <div className="px-5 py-2 shrink-0 bg-amber-50 dark:bg-amber-900/20 border-y border-amber-100 dark:border-amber-900/30">
                        <p className="text-[10px] text-amber-700 dark:text-amber-400 leading-relaxed">
                            {t('proxy.config.opencode_sync.auth_plugin_warning', {
                                defaultValue: 'Sync chỉ tạo provider antigravity-manager và không ghi đè google provider/plugin.'
                            })}
                        </p>
                    </div>
                )}

                {/* Preview 主体区 */}
                <div className="flex-1 min-h-0 flex flex-col">
                    <div className="px-5 py-2 flex items-center justify-between shrink-0">
                        <span className="text-[10px] font-bold text-gray-400 uppercase tracking-wider">
                            Sync Queue Preview
                        </span>
                        <span className="text-[9px] font-mono text-gray-300">{previewModels.length} models</span>
                    </div>
                    <div className="px-4 pb-3 overflow-auto flex-1">
                        <DndContext sensors={sensors} collisionDetection={closestCenter} onDragEnd={handleDragEnd}>
                            <SortableContext items={previewModels.map(m => m._uid)} strategy={verticalListSortingStrategy}>
                                <div className="space-y-1.5">
                                    {previewModels.map(entry => (
                                        <SortableModelItem
                                            key={entry._uid}
                                            entry={entry}
                                            collapsed={true}
                                            onToggle={() => { }}
                                            onRemove={() => handleRemoveModel(entry._uid)}
                                        />
                                    ))}
                                </div>
                            </SortableContext>
                        </DndContext>
                    </div>
                </div>

                {/* Footer */}
                <div className="px-5 py-3 border-t border-gray-100 dark:border-base-200 flex items-center justify-end gap-2 shrink-0">
                    <button type="button" className="px-3 py-1.5 text-xs text-gray-500 hover:text-gray-700 dark:hover:text-gray-300 rounded-lg hover:bg-gray-100 dark:hover:bg-base-300 transition-colors" onClick={onClose}>
                        {t('common.cancel', { defaultValue: '取消' })}
                    </button>
                    <button
                        type="button"
                        className={cn(
                            "px-4 py-1.5 text-xs font-bold rounded-lg transition-all flex items-center gap-1.5",
                            configState === 'ready' && previewModels.length > 0
                                ? "bg-blue-500 hover:bg-blue-600 active:bg-blue-700 text-white shadow-sm"
                                : "bg-gray-200 dark:bg-gray-700 text-gray-400 cursor-not-allowed",
                        )}
                        disabled={configState !== 'ready' || previewModels.length === 0 || syncing}
                        onClick={executeOpenCodeSync}
                    >
                        <RefreshCw size={12} className={syncing ? 'animate-spin' : ''} />
                        {t('proxy.config.opencode_sync.btn_confirm_sync', { defaultValue: '确认同步' })}
                    </button>
                </div>
            </div>
        </div>
    );
}
