import type { AppConfig } from '../types/config';
import { loadConfig } from '../services/configService';
import { useConfigStore } from '../stores/useConfigStore';

/** Restore authoritative settings after a rejected save without discarding a newer edit. */
export async function reloadSavedConfigIfUnchanged(expected: AppConfig): Promise<void> {
    if (useConfigStore.getState().config !== expected) return;
    try {
        const saved = await loadConfig();
        if (useConfigStore.getState().config === expected) {
            useConfigStore.setState({ config: saved });
        }
    } catch {
        // The calling control retains its save error when the server is unreachable.
    }
}
