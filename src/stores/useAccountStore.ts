import { create } from 'zustand';
import { Account, ActiveClientsStatus, ModelPriorities } from '../types/account';
import * as accountService from '../services/accountService';

// List/current-account views often refresh together. Share the client discovery
// request, which otherwise repeats the same filesystem/process work three times.
let activeClientsRefresh: Promise<void> | null = null;
let activeClientsGeneration = 0;
let accountsGeneration = 0;
let currentAccountGeneration = 0;
let currentAccountReadInFlight: number | null = null;

function invalidateAccountSnapshots() {
    // A true result requires a replacement current read: invalidation alone
    // could discard a concurrent switch's authoritative account identity.
    const needsCurrentAccountRefresh = currentAccountReadInFlight !== null;
    accountsGeneration += 1;
    currentAccountGeneration += 1;
    return needsCurrentAccountRefresh;
}

function invalidateActiveClientsRefresh() {
    activeClientsGeneration += 1;
    activeClientsRefresh = null;
}

interface AccountState {
    accounts: Account[];
    currentAccount: Account | null;
    activeClients: ActiveClientsStatus;
    loading: boolean;
    error: string | null;

    // Actions
    fetchAccounts: () => Promise<void>;
    fetchCurrentAccount: () => Promise<void>;
    fetchActiveClients: () => Promise<void>;
    addAccount: (email: string, refreshToken: string) => Promise<void>;
    deleteAccount: (accountId: string) => Promise<void>;
    deleteAccounts: (accountIds: string[]) => Promise<void>;
    switchAccount: (accountId: string, targetIde?: string) => Promise<void>;
    refreshQuota: (accountId: string, automatic?: boolean) => Promise<void>;
    refreshAllQuotas: (automatic?: boolean) => Promise<accountService.RefreshStats>;
    reorderAccounts: (accountIds: string[]) => Promise<void>;

    // New actions
    startOAuthLogin: (oauthClientKey?: string) => Promise<void>;
    completeOAuthLogin: () => Promise<void>;
    cancelOAuthLogin: () => Promise<void>;
    importV1Accounts: () => Promise<void>;
    batchImportAccounts: (entries: accountService.ImportAccountEntry[]) => Promise<accountService.BatchImportResult>;
    importFromDb: () => Promise<void>;
    importFromCustomDb: (path: string) => Promise<void>;
    syncAccountFromDb: () => Promise<void>;
    toggleProxyStatus: (accountId: string, enable: boolean, reason?: string) => Promise<void>;
    warmUpAccounts: () => Promise<string>;
    warmUpAccount: (accountId: string) => Promise<string>;
    updateAccountLabel: (accountId: string, label: string) => Promise<void>;
    updateAccountPriority: (accountId: string, priority: number, modelPriorities?: ModelPriorities) => Promise<void>;
    updateAccountProjectId: (accountId: string, projectId: string) => Promise<void>;
    autoFetchAccountProjectId: (accountId: string) => Promise<string>;
}

// Remove snapshots from older builds that may contain OAuth credentials.
try {
    for (const key of ['antigravity_accounts_cache', 'antigravity_current_account_cache', 'antigravity_active_clients_cache']) {
        localStorage.removeItem(key);
    }
} catch { /* Storage can be unavailable in embedded or private contexts. */ }

export const useAccountStore = create<AccountState>((set, get) => ({
    accounts: [],
    currentAccount: null,
    activeClients: {},
    loading: false,
    error: null,

    fetchAccounts: async () => {
        const generation = ++accountsGeneration;
        set({ loading: true, error: null });
        try {
            console.log('[Store] Fetching accounts...');
            const [accounts] = await Promise.all([
                accountService.listAccounts(),
                get().fetchActiveClients()
            ]);
            if (generation === accountsGeneration) set({ accounts, loading: false });
        } catch (error) {
            console.error('[Store] Fetch accounts failed:', error);
            if (generation === accountsGeneration) set({ error: String(error), loading: false });
        }
    },

    fetchCurrentAccount: async () => {
        const generation = ++currentAccountGeneration;
        currentAccountReadInFlight = generation;
        set({ loading: true, error: null });
        try {
            const [account] = await Promise.all([
                accountService.getCurrentAccount(),
                get().fetchActiveClients()
            ]);
            if (generation === currentAccountGeneration) set({ currentAccount: account, loading: false });
        } catch (error) {
            if (generation === currentAccountGeneration) set({ error: String(error), loading: false });
        } finally {
            if (currentAccountReadInFlight === generation) currentAccountReadInFlight = null;
        }
    },

    fetchActiveClients: () => {
        if (activeClientsRefresh) return activeClientsRefresh;
        const generation = activeClientsGeneration;
        let timeoutId: ReturnType<typeof setTimeout> | undefined;
        const timeout = new Promise<never>((_, reject) => {
            timeoutId = setTimeout(() => reject(new Error('Client discovery timeout')), 10000);
        });
        const refresh = Promise.race([accountService.getActiveClientsStatus(), timeout])
            .then(activeClients => {
                if (generation === activeClientsGeneration) set({ activeClients });
            })
            .catch(error => {
                // Keep the last known clients when discovery temporarily fails.
                console.error('[Store] Fetch active clients failed:', error);
            })
            .finally(() => {
                if (timeoutId !== undefined) clearTimeout(timeoutId);
                if (activeClientsRefresh === refresh) activeClientsRefresh = null;
            });
        activeClientsRefresh = refresh;
        return refresh;
    },

    addAccount: async (email: string, refreshToken: string) => {
        set({ loading: true, error: null });
        try {
            await accountService.addAccount(email, refreshToken);
            const refreshCurrentAccount = invalidateAccountSnapshots();
            invalidateActiveClientsRefresh();
            await Promise.all([
                get().fetchAccounts(),
                refreshCurrentAccount ? get().fetchCurrentAccount() : Promise.resolve(),
            ]);
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
            invalidateAccountSnapshots();
            invalidateActiveClientsRefresh();
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
            invalidateAccountSnapshots();
            invalidateActiveClientsRefresh();
            await Promise.all([
                get().fetchAccounts(),
                get().fetchCurrentAccount()
            ]);
            set({ loading: false });
        } catch (error) {
            // The index may already be committed when credential cleanup fails.
            // Refresh the durable state while preserving the original error.
            invalidateAccountSnapshots();
            invalidateActiveClientsRefresh();
            await Promise.all([get().fetchAccounts(), get().fetchCurrentAccount()]);
            set({ error: String(error), loading: false });
            throw error;
        }
    },

    switchAccount: async (accountId: string, targetIde?: string) => {
        set({ loading: true, error: null });
        try {
            await accountService.switchAccount(accountId, targetIde);
            invalidateAccountSnapshots();
            // An earlier discovery may describe the client before the switch.
            // Do not let its eventual completion replace the new client state.
            invalidateActiveClientsRefresh();
            await Promise.all([
                get().fetchCurrentAccount(),
                get().fetchAccounts(),
            ]);

            set({ loading: false });

            // Asynchronously refresh quota to update the dashboard, do not block the UI
            // NOTE: We call the service directly instead of get().refreshQuota() to avoid
            // re-setting loading=true after we just set loading=false.
            accountService.fetchAccountQuota(accountId, true)
                .then(() => {
                    invalidateActiveClientsRefresh();
                    return Promise.all([
                        get().fetchAccounts(),
                        get().fetchCurrentAccount(),
                    ]);
                })
                .catch(e => {
                    console.error('[Store] Background quota refresh failed after switch:', e);
                });
        } catch (error) {
            set({ error: String(error), loading: false });
            throw error;
        }
    },

    refreshQuota: async (accountId: string, automatic = false) => {
        set({ loading: true, error: null });
        try {
            await accountService.fetchAccountQuota(accountId, automatic);
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

    refreshAllQuotas: async (automatic = false) => {
        set({ loading: true, error: null });
        try {
            const stats = await accountService.refreshAllQuotas(automatic);
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
        const generation = ++accountsGeneration;
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
        set({ accounts: finalAccounts, loading: false });

        try {
            await accountService.reorderAccounts(accountIds);
        } catch (error) {
            // Roll back to the original order if the backend call fails
            console.error('[AccountStore] Reorder accounts failed:', error);
            if (generation === accountsGeneration) set({ accounts });
            throw error;
        }
    },

    startOAuthLogin: async (oauthClientKey?: string) => {
        set({ loading: true, error: null });
        try {
            await accountService.startOAuthLogin(oauthClientKey);
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

    batchImportAccounts: async (entries: accountService.ImportAccountEntry[]) => {
        set({ loading: true, error: null });
        try {
            const res = await accountService.batchImportAccounts(entries);
            await get().fetchAccounts();
            set({ loading: false });
            return res;
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
        const generation = ++currentAccountGeneration;
        try {
            const syncedAccount = await accountService.syncAccountFromDb();
            if (syncedAccount) {
                console.log('[AccountStore] Account synced from DB:', syncedAccount.email);
                await get().fetchAccounts();
                if (generation === currentAccountGeneration) set({ currentAccount: syncedAccount, loading: false });
            }
        } catch (error) {
            console.error('[AccountStore] Sync from DB failed:', error);
        } finally {
            if (generation === currentAccountGeneration) set({ loading: false });
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

    updateAccountPriority: async (accountId, priority, modelPriorities) => {
        await accountService.updateAccountPriority(accountId, priority, modelPriorities);
        const refreshCurrentAccount = invalidateAccountSnapshots();
        const cleanModelPriorities =
            modelPriorities !== undefined
                ? Object.fromEntries(
                      Object.entries(modelPriorities).filter(
                          ([, v]) => typeof v === 'number' && v >= 1 && v <= 100
                      )
                  )
                : undefined;
        set(state => ({
            loading: false,
            accounts: state.accounts.map(account =>
                account.id === accountId
                    ? {
                          ...account,
                          priority,
                          ...(cleanModelPriorities !== undefined
                              ? {
                                    model_priorities:
                                        Object.keys(cleanModelPriorities).length > 0
                                            ? cleanModelPriorities
                                            : undefined,
                                }
                              : {}),
                      }
                    : account
            ),
            currentAccount:
                state.currentAccount?.id === accountId
                    ? {
                          ...state.currentAccount,
                          priority,
                          ...(cleanModelPriorities !== undefined
                              ? {
                                    model_priorities:
                                        Object.keys(cleanModelPriorities).length > 0
                                            ? cleanModelPriorities
                                            : undefined,
                                }
                              : {}),
                      }
                    : state.currentAccount,
        }));
        // An invalidated read may describe a concurrent account switch. Replace
        // it so a field edit cannot leave the previous account identity visible.
        if (refreshCurrentAccount) void get().fetchCurrentAccount();
    },

    updateAccountProjectId: async (accountId, projectId) => {
        await accountService.updateAccountProjectId(accountId, projectId);
        const refreshCurrentAccount = invalidateAccountSnapshots();
        set(state => ({
            loading: false,
            accounts: state.accounts.map(account =>
                account.id === accountId
                    ? { ...account, token: { ...account.token, project_id: projectId.trim() || undefined } }
                    : account
            ),
            currentAccount:
                state.currentAccount?.id === accountId
                    ? { ...state.currentAccount, token: { ...state.currentAccount.token, project_id: projectId.trim() || undefined } }
                    : state.currentAccount,
        }));
        if (refreshCurrentAccount) void get().fetchCurrentAccount();
    },

    autoFetchAccountProjectId: async (accountId) => {
        const projectId = await accountService.autoFetchAccountProjectId(accountId);
        const refreshCurrentAccount = invalidateAccountSnapshots();
        set(state => ({
            loading: false,
            accounts: state.accounts.map(account =>
                account.id === accountId
                    ? { ...account, token: { ...account.token, project_id: projectId.trim() || undefined } }
                    : account
            ),
            currentAccount:
                state.currentAccount?.id === accountId
                    ? { ...state.currentAccount, token: { ...state.currentAccount.token, project_id: projectId.trim() || undefined } }
                    : state.currentAccount,
        }));
        if (refreshCurrentAccount) void get().fetchCurrentAccount();
        return projectId;
    },

    updateAccountLabel: async (accountId: string, label: string) => {
        try {
            await accountService.updateAccountLabel(accountId, label);
            accountsGeneration += 1;
            // Optimistically update local state
            const { accounts } = get();
            const updatedAccounts = accounts.map(acc =>
                acc.id === accountId ? { ...acc, custom_label: label || undefined } : acc
            );
            set({ accounts: updatedAccounts, loading: false });
        } catch (error) {
            console.error('[AccountStore] Update label failed:', error);
            throw error;
        }
    },
}));
