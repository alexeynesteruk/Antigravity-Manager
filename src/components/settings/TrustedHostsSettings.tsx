import { useState } from 'react';
import { useTranslation } from 'react-i18next';
import { AppConfig, ProxyConfig } from '../../types/config';
import { useConfigStore } from '../../stores/useConfigStore';
import { reloadSavedConfigIfUnchanged } from '../../utils/configUpdates';

interface Props {
    proxy: ProxyConfig;
    onChange: (updates: Partial<ProxyConfig>) => void | Promise<void>;
}

export default function TrustedHostsSettings({ proxy, onChange }: Props) {
    const { t } = useTranslation();
    const [error, setError] = useState('');
    const hosts = proxy.security_monitor?.trusted_hosts ?? [];
    const save = async (raw: string) => {
        const trusted_hosts = [...new Set(raw.split(/[\n,]+/).map(value => value.trim()).filter(Boolean))];
        if (JSON.stringify(trusted_hosts) === JSON.stringify(hosts)) return;
        const latest = useConfigStore.getState().config?.proxy ?? proxy;
        let optimistic: AppConfig | null = null;
        try {
            const saving = onChange({ security_monitor: {
                blacklist: { enabled: false, block_message: 'Access denied' },
                whitelist: { enabled: false, whitelist_priority: true },
                ...latest.security_monitor,
                trusted_hosts,
            } });
            optimistic = useConfigStore.getState().config;
            await saving;
            setError('');
        } catch {
            if (optimistic) await reloadSavedConfigIfUnchanged(optimistic);
            setError(t('proxy.hosts.save_failed', { defaultValue: 'Could not save trusted hosts. Use exact hostnames or IP addresses with an optional port, without URLs or wildcards.' }));
        }
    };
    return <details className="rounded-lg border border-gray-200 dark:border-base-300 p-3">
        <summary className="cursor-pointer text-sm font-medium">{t('proxy.hosts.title', { defaultValue: 'Additional trusted hosts' })}</summary>
        <div className="space-y-2 pt-3">
            <p className="text-xs text-gray-500 dark:text-gray-400">{t('proxy.hosts.hint', { defaultValue: 'Localhost, the listener IP and the configured HTTPS hostname are accepted automatically. Add an exact hostname and port only for your own reverse proxy or another local hostname. Include :8045 or :8443 when used; entries without a port allow only 80 and 443. This does not open network access or replace authentication.' })}</p>
            <textarea key={hosts.join('\n')} defaultValue={hosts.join('\n')} rows={3}
                aria-label={t('proxy.hosts.title', { defaultValue: 'Additional trusted hosts' })}
                placeholder="gateway.home:8443"
                className="textarea textarea-bordered w-full font-mono text-xs"
                onBlur={event => { void save(event.currentTarget.value); }} />
            {error && <p role="alert" className="text-xs text-error">{error}</p>}
        </div>
    </details>;
}
