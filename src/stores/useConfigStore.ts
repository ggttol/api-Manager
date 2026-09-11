import { create } from 'zustand';
import { AppConfig } from '../types/config';
import * as configService from '../services/configService';

interface ConfigState {
    config: AppConfig | null;
    loading: boolean;
    error: string | null;
    loadConfig: () => Promise<void>;
    saveConfig: (config: AppConfig, silent?: boolean) => Promise<void>;
    updateConfig: (update: (config: AppConfig) => AppConfig, silent?: boolean) => Promise<void>;
    updateTheme: (theme: string) => Promise<void>;
    updateLanguage: (language: string) => Promise<void>;
    toggleShowAllQuotas: () => void;
    showAllQuotas: boolean;
    toggleMenuItem: (path: string) => Promise<void>;
    isMenuItemHidden: (path: string) => boolean;
}

interface PendingMutation {
    update: (config: AppConfig) => AppConfig;
    silent: boolean;
    resolve: () => void;
    reject: (error: unknown) => void;
}

let confirmedConfig: AppConfig | null = null;
let pendingMutations: PendingMutation[] = [];
let drainingMutations = false;
let mutationVersion = 0;
let readVersion = 0;

const desiredConfig = (): AppConfig | null => pendingMutations.reduce<AppConfig | null>(
    (config, mutation) => config === null ? null : mutation.update(config),
    confirmedConfig,
);

const applyDesiredConfig = (set: (state: Partial<ConfigState>) => void) => {
    set({ config: desiredConfig(), loading: drainingMutations || pendingMutations.some(mutation => !mutation.silent) });
};

const drainMutations = async (set: (state: Partial<ConfigState>) => void) => {
    if (drainingMutations) return;
    drainingMutations = true;
    while (pendingMutations.length > 0 && confirmedConfig) {
        const mutation = pendingMutations[0];
        const snapshot = mutation.update(confirmedConfig);
        try {
            await configService.saveConfig(snapshot);
            confirmedConfig = snapshot;
            pendingMutations.shift();
            if (typeof window !== 'undefined') {
                const { isTauri } = await import('../utils/env');
                if (isTauri()) {
                    const { invoke } = await import('@tauri-apps/api/core');
                    await invoke('set_window_theme', { theme: snapshot.theme }).catch(() => {});
                }
            }
            mutation.resolve();
            applyDesiredConfig(set);
        } catch (error) {
            pendingMutations.shift();
            mutationVersion += 1;
            mutation.reject(error);
            set({ error: String(error) });
            applyDesiredConfig(set);
        }
    }
    drainingMutations = false;
    applyDesiredConfig(set);
};

export const useConfigStore = create<ConfigState>((set, get) => ({
    config: null,
    loading: false,
    error: null,
    showAllQuotas: localStorage.getItem('antigravity_show_all_quotas') === 'true',

    loadConfig: async () => {
        const requestVersion = ++readVersion;
        const requestMutationVersion = mutationVersion;
        set({ loading: true, error: null });
        try {
            const config = await configService.loadConfig();
            if (requestVersion !== readVersion || requestMutationVersion !== mutationVersion || pendingMutations.length > 0) return;
            confirmedConfig = config;
            set({ config, loading: false });
        } catch (error) {
            if (requestVersion === readVersion) set({ error: String(error), loading: false });
        }
    },

    saveConfig: async (config: AppConfig, silent: boolean = false) => {
        await get().updateConfig(() => config, silent);
    },

    updateConfig: async (update, silent: boolean = false) => {
        if (!confirmedConfig && !get().config) throw new Error('Configuration has not loaded');
        if (!confirmedConfig) confirmedConfig = get().config;
        mutationVersion += 1;
        const promise = new Promise<void>((resolve, reject) => {
            pendingMutations.push({ update, silent, resolve, reject });
        });
        set({ config: desiredConfig(), error: null, ...(silent ? {} : { loading: true }) });
        void drainMutations(set);
        await promise;
    },

    updateTheme: async (theme: string) => get().updateConfig(config => config.theme === theme ? config : { ...config, theme }, true),
    updateLanguage: async (language: string) => get().updateConfig(config => config.language === language ? config : { ...config, language }, true),

    toggleShowAllQuotas: () => {
        const next = !get().showAllQuotas;
        localStorage.setItem('antigravity_show_all_quotas', String(next));
        set({ showAllQuotas: next });
    },

    toggleMenuItem: async (path: string) => {
        const { config } = get();
        if (!config) return;
        const hiddenItems = config.hidden_menu_items || [];
        await get().updateConfig(current => ({
            ...current,
            hidden_menu_items: hiddenItems.includes(path) ? hiddenItems.filter(item => item !== path) : [...hiddenItems, path],
        }), true);
    },

    isMenuItemHidden: (path: string) => (get().config?.hidden_menu_items || []).includes(path),
}));
