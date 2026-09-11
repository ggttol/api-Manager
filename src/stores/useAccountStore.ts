import { create } from 'zustand';
import { Account } from '../types/account';
import * as accountService from '../services/accountService';

let accountsRequestVersion = 0;
let currentAccountRequestVersion = 0;

const invalidateAccountsRead = () => {
    accountsRequestVersion += 1;
};

const invalidateCurrentAccountRead = () => {
    currentAccountRequestVersion += 1;
};

const invalidateAccountReads = () => {
    invalidateAccountsRead();
    invalidateCurrentAccountRead();
};

interface AccountState {
    accounts: Account[];
    currentAccount: Account | null;
    loading: boolean;
    error: string | null;

    // Actions
    fetchAccounts: () => Promise<void>;
    fetchCurrentAccount: () => Promise<void>;
    addAccount: (email: string, refreshToken: string) => Promise<void>;
    deleteAccount: (accountId: string) => Promise<void>;
    deleteAccounts: (accountIds: string[]) => Promise<void>;
    switchAccount: (accountId: string, targetIde?: string) => Promise<void>;
    refreshQuota: (accountId: string) => Promise<void>;
    refreshAllQuotas: () => Promise<accountService.RefreshStats>;
    reorderAccounts: (accountIds: string[]) => Promise<void>;

    // 新增 actions
    startOAuthLogin: () => Promise<void>;
    completeOAuthLogin: () => Promise<void>;
    cancelOAuthLogin: () => Promise<void>;
    importV1Accounts: () => Promise<void>;
    importFromDb: () => Promise<void>;
    importFromCustomDb: (path: string) => Promise<void>;
    syncAccountFromDb: () => Promise<void>;
    toggleProxyStatus: (accountId: string, enable: boolean, reason?: string) => Promise<void>;
    warmUpAccounts: () => Promise<string>;
    warmUpAccount: (accountId: string) => Promise<string>;
    updateAccountLabel: (accountId: string, label: string) => Promise<void>;
}

export const useAccountStore = create<AccountState>((set, get) => ({
    accounts: [],
    currentAccount: null,
    loading: false,
    error: null,

    fetchAccounts: async () => {
        const requestVersion = ++accountsRequestVersion;
        set({ loading: true, error: null });
        try {
            const accounts = await accountService.listAccounts();
            if (requestVersion === accountsRequestVersion) {
                set({ accounts, loading: false });
            }
        } catch (error) {
            if (requestVersion === accountsRequestVersion) {
                set({ error: String(error), loading: false });
            }
        }
    },

    fetchCurrentAccount: async () => {
        const requestVersion = ++currentAccountRequestVersion;
        set({ loading: true, error: null });
        try {
            const account = await accountService.getCurrentAccount();
            if (requestVersion === currentAccountRequestVersion) {
                set({ currentAccount: account, loading: false });
            }
        } catch (error) {
            if (requestVersion === currentAccountRequestVersion) {
                set({ error: String(error), loading: false });
            }
        }
    },

    addAccount: async (email: string, refreshToken: string) => {
        invalidateAccountsRead();
        set({ loading: true, error: null });
        try {
            await accountService.addAccount(email, refreshToken);
            await get().fetchAccounts();
            set({ loading: false });
        } catch (error) {
            set({ error: String(error), loading: false });
            throw error;
        }
    },

    deleteAccount: async (accountId: string) => {
        invalidateAccountReads();
        set({ loading: true, error: null });
        try {
            await accountService.deleteAccount(accountId);
            invalidateAccountReads();
            await Promise.all([
                get().fetchAccounts(),
                get().fetchCurrentAccount()
            ]);
            set({ loading: false });
        } catch (error) {
            set({ error: String(error), loading: false });
            throw error;
        }
    },

    deleteAccounts: async (accountIds: string[]) => {
        invalidateAccountReads();
        set({ loading: true, error: null });
        try {
            await accountService.deleteAccounts(accountIds);
            invalidateAccountReads();
            await Promise.all([
                get().fetchAccounts(),
                get().fetchCurrentAccount()
            ]);
            set({ loading: false });
        } catch (error) {
            set({ error: String(error), loading: false });
            throw error;
        }
    },

    switchAccount: async (accountId: string, targetIde?: string) => {
        set({ loading: true, error: null });
        invalidateCurrentAccountRead();
        try {
            await accountService.switchAccount(accountId, targetIde);
            invalidateCurrentAccountRead();
            await get().fetchCurrentAccount();
            set({ loading: false });
        } catch (error) {
            set({ error: String(error), loading: false });
            throw error;
        }
    },

    refreshQuota: async (accountId: string) => {
        set({ loading: true, error: null });
        try {
            await accountService.fetchAccountQuota(accountId);
            invalidateAccountsRead();
            await get().fetchAccounts();
            set({ loading: false });
        } catch (error) {
            set({ error: String(error), loading: false });
            throw error;
        }
    },

    refreshAllQuotas: async () => {
        set({ loading: true, error: null });
        try {
            const stats = await accountService.refreshAllQuotas();
            invalidateAccountsRead();
            await get().fetchAccounts();
            set({ loading: false });
            return stats;
        } catch (error) {
            set({ error: String(error), loading: false });
            throw error;
        }
    },

    reorderAccounts: async (accountIds: string[]) => {
        const { accounts } = get();
        if (accountIds.length !== accounts.length || new Set(accountIds).size !== accounts.length) {
            throw new Error('Account reorder requires a complete account order');
        }

        const accountMap = new Map(accounts.map(acc => [acc.id, acc]));
        const finalAccounts = accountIds
            .map(id => accountMap.get(id))
            .filter((acc): acc is Account => acc !== undefined);
        if (finalAccounts.length !== accounts.length) {
            throw new Error('Account reorder contains an unknown account');
        }

        invalidateAccountsRead();
        set({ accounts: finalAccounts });
        try {
            await accountService.reorderAccounts(accountIds);
            invalidateAccountsRead();
        } catch (error) {
            set({ accounts });
            throw error;
        }
    },

    startOAuthLogin: async () => {
        set({ loading: true, error: null });
        try {
            await accountService.startOAuthLogin();
            invalidateAccountsRead();
            await get().fetchAccounts();
            set({ loading: false });
        } catch (error) {
            set({ error: String(error), loading: false });
            throw error;
        }
    },

    completeOAuthLogin: async () => {
        set({ loading: true, error: null });
        try {
            await accountService.completeOAuthLogin();
            invalidateAccountsRead();
            await get().fetchAccounts();
            set({ loading: false });
        } catch (error) {
            set({ error: String(error), loading: false });
            throw error;
        }
    },

    cancelOAuthLogin: async () => {
        try {
            await accountService.cancelOAuthLogin();
            set({ loading: false, error: null });
        } catch (error) {
            console.error('[Store] Cancel OAuth failed:', error);
        }
    },

    importV1Accounts: async () => {
        set({ loading: true, error: null });
        try {
            await accountService.importV1Accounts();
            invalidateAccountsRead();
            await get().fetchAccounts();
            set({ loading: false });
        } catch (error) {
            set({ error: String(error), loading: false });
            throw error;
        }
    },

    importFromDb: async () => {
        set({ loading: true, error: null });
        try {
            await accountService.importFromDb();
            invalidateAccountReads();
            await Promise.all([
                get().fetchAccounts(),
                get().fetchCurrentAccount()
            ]);
            set({ loading: false });
        } catch (error) {
            set({ error: String(error), loading: false });
            throw error;
        }
    },

    importFromCustomDb: async (path: string) => {
        set({ loading: true, error: null });
        try {
            await accountService.importFromCustomDb(path);
            invalidateAccountReads();
            await Promise.all([
                get().fetchAccounts(),
                get().fetchCurrentAccount()
            ]);
            set({ loading: false });
        } catch (error) {
            set({ error: String(error), loading: false });
            throw error;
        }
    },

    syncAccountFromDb: async () => {
        try {
            const syncedAccount = await accountService.syncAccountFromDb();
            if (syncedAccount) {
                console.log('[AccountStore] Account synced from DB:', syncedAccount.email);
                await get().fetchAccounts();
                set({ currentAccount: syncedAccount });
            }
        } catch (error) {
            console.error('[AccountStore] Sync from DB failed:', error);
        }
    },

    toggleProxyStatus: async (accountId: string, enable: boolean, reason?: string) => {
        try {
            await accountService.toggleProxyStatus(accountId, enable, reason);
            invalidateAccountsRead();
            await get().fetchAccounts();
        } catch (error) {
            console.error('[AccountStore] Toggle proxy status failed:', error);
            throw error;
        }
    },

    warmUpAccounts: async () => {
        set({ loading: true, error: null });
        try {
            const result = await accountService.warmUpAllAccounts();
            set({ loading: false });
            return result;
        } catch (error) {
            set({ error: String(error), loading: false });
            throw error;
        } finally {
            await get().fetchAccounts();
        }
    },

    warmUpAccount: async (accountId: string) => {
        set({ loading: true, error: null });
        try {
            const result = await accountService.warmUpAccount(accountId);
            set({ loading: false });
            return result;
        } catch (error) {
            set({ error: String(error), loading: false });
            throw error;
        } finally {
            await get().fetchAccounts();
        }
    },

    updateAccountLabel: async (accountId: string, label: string) => {
        try {
            await accountService.updateAccountLabel(accountId, label);
            invalidateAccountsRead();
            // 乐观更新本地状态
            const { accounts } = get();
            const updatedAccounts = accounts.map(acc =>
                acc.id === accountId ? { ...acc, custom_label: label || undefined } : acc
            );
            set({ accounts: updatedAccounts });
        } catch (error) {
            console.error('[AccountStore] Update label failed:', error);
            throw error;
        }
    },
}));
