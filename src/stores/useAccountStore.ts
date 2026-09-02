import { create } from 'zustand';
import { Account } from '../types/account';
import * as accountService from '../services/accountService';

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

    // New actions
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
        set({ loading: true, error: null });
        try {
            console.log('[Store] Fetching accounts...');
            const accounts = await accountService.listAccounts();
            set({ accounts, loading: false });
        } catch (error) {
            console.error('[Store] Fetch accounts failed:', error);
            set({ error: String(error), loading: false });
        }
    },

    fetchCurrentAccount: async () => {
        set({ loading: true, error: null });
        try {
            const account = await accountService.getCurrentAccount();
            set({ currentAccount: account, loading: false });
        } catch (error) {
            set({ error: String(error), loading: false });
        }
    },

    addAccount: async (email: string, refreshToken: string) => {
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
        set({ loading: true, error: null });
        try {
            await accountService.deleteAccount(accountId);
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
        set({ loading: true, error: null });
        try {
            await accountService.deleteAccounts(accountIds);
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
        try {
            await accountService.switchAccount(accountId, targetIde);
            await Promise.all([
                get().fetchCurrentAccount(),
                get().fetchAccounts()
            ]);
            
            // Asynchronously refresh quota to update the dashboard, do not block the UI
            // NOTE: We call the service directly instead of get().refreshQuota() to avoid
            // re-setting loading=true after we just set loading=false.
            accountService.fetchAccountQuota(accountId)
                .then(() => Promise.all([get().fetchAccounts(), get().fetchCurrentAccount()]))
                .catch(e => {
                    console.error('[Store] Background quota refresh failed after switch:', e);
                });
            
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
            await get().fetchAccounts();
            
            const { currentAccount } = get();
            if (currentAccount && currentAccount.id === accountId) {
                await get().fetchCurrentAccount();
            }
            
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
            await get().fetchAccounts();
            set({ loading: false });
            return stats;
        } catch (error) {
            set({ error: String(error), loading: false });
            throw error;
        }
    },

    /**
     * Reorder the account list
     * Uses an optimistic update strategy: update local state first, then call the backend to persist, for a smooth drag experience
     */
    reorderAccounts: async (accountIds: string[]) => {
        const { accounts } = get();

        // Create an ID-to-account map
        const accountMap = new Map(accounts.map(acc => [acc.id, acc]));

        // Rebuild the account array in the new order
        const reorderedAccounts = accountIds
            .map(id => accountMap.get(id))
            .filter((acc): acc is Account => acc !== undefined);

        // Append accounts not present in the new order (preserving their original order)
        const remainingAccounts = accounts.filter(acc => !accountIds.includes(acc.id));
        const finalAccounts = [...reorderedAccounts, ...remainingAccounts];

        // Optimistically update local state
        set({ accounts: finalAccounts });

        try {
            await accountService.reorderAccounts(accountIds);
        } catch (error) {
            // Roll back to the original order if the backend call fails
            console.error('[AccountStore] Reorder accounts failed:', error);
            set({ accounts });
            throw error;
        }
    },

    startOAuthLogin: async () => {
        set({ loading: true, error: null });
        try {
            await accountService.startOAuthLogin();
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
            // Optimistically update local state
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
