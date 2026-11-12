import { useTranslation } from 'react-i18next';
import { GatewayTlsConfig, ProxyConfig } from '../../types/config';
import { useConfigStore } from '../../stores/useConfigStore';
import { loadConfig } from '../../services/configService';

interface Props {
    proxy: ProxyConfig;
    onChange: (updates: Partial<ProxyConfig>) => Promise<void> | void;
    httpsBaseUrl?: string | null;
}

export default function ProxyTlsSettings({ proxy, onChange, httpsBaseUrl }: Props) {
    const { t } = useTranslation();
    const tls: GatewayTlsConfig = proxy.tls || { enabled: false, port: 8443, certificate_path: '' };
    const update = async (changes: Partial<GatewayTlsConfig>) => {
        const latest = useConfigStore.getState().config?.proxy.tls || tls;
        const saving = onChange({ tls: { ...latest, ...changes } });
        const published = useConfigStore.getState().config;
        await saving;
        // Reflect backend validation and normalization, including rejected edits.
        // A settings edit during either await owns the newer store snapshot.
        if (useConfigStore.getState().config !== published) return;
        try {
            const config = await loadConfig();
            if (useConfigStore.getState().config === published) useConfigStore.setState({ config });
        } catch (error) {
            if (useConfigStore.getState().config === published) useConfigStore.setState({ error: String(error) });
        }
    };
    const input = (field: 'certificate_path' | 'private_key_path' | 'advertised_host', label: string, placeholder: string) => (
        <label className="block text-xs text-gray-600 dark:text-gray-400">
            {label}
            <input key={`${field}:${tls[field] || ''}`} type="text" defaultValue={tls[field] || ''} placeholder={placeholder}
                className="input input-bordered input-sm w-full mt-1 font-mono"
                onBlur={(event) => {
                    const value = event.currentTarget.value.trim();
                    if (value !== (tls[field] || '')) void update({ [field]: value || undefined });
                }} onKeyDown={(event) => { if (event.key === 'Enter') event.currentTarget.blur(); }} />
        </label>
    );
    return (
        <details className="mt-3 rounded-lg border border-gray-200 dark:border-base-300 p-3">
            <summary className="text-xs font-semibold cursor-pointer">{t('proxy.tls.title', { defaultValue: 'HTTPS for LAN access' })}</summary>
            <div className="space-y-3 pt-3">
                <label className="flex items-center justify-between text-xs">
                    {t('proxy.tls.enabled', { defaultValue: 'Enable HTTPS' })}
                    <input type="checkbox" className="toggle toggle-sm" checked={tls.enabled} onChange={(e) => void update({ enabled: e.target.checked })} />
                </label>
                <p className="text-xs text-gray-500 dark:text-gray-400">{t('proxy.tls.hint', { defaultValue: 'When enabled, LAN clients use HTTPS. HTTP remains on loopback only for local clients. Restart the app after changes. Install your CA certificate on clients; certificate verification stays enabled.' })}</p>
                {input('certificate_path', t('proxy.tls.certificate', { defaultValue: 'Public certificate chain PEM (server path)' }), 'C:\\path\\gateway.pem / /etc/gateway.pem')}
                {tls.private_key_ref ? (
                    <p className="text-xs text-green-600 dark:text-green-400">{t('proxy.tls.native_key', { defaultValue: 'Private key is stored in the OS credential store. Use tls-import locally to replace the identity.' })}</p>
                ) : input('private_key_path', t('proxy.tls.key', { defaultValue: 'Protected private-key PEM (server path)' }), 'Use tls-import to keep the private key in native storage')}
                {input('advertised_host', t('proxy.tls.host', { defaultValue: 'Certificate hostname or LAN IP' }), '192.168.0.168')}
                <label className="block text-xs text-gray-600 dark:text-gray-400">
                    {t('proxy.tls.port', { defaultValue: 'HTTPS port' })}
                    <input key={tls.port} type="number" min={1} max={65535} defaultValue={tls.port} className="input input-bordered input-sm w-full mt-1"
                        onBlur={(event) => { const port = Number(event.currentTarget.value); if (port !== tls.port) void update({ port }); }} />
                </label>
                {httpsBaseUrl && <p className="text-xs font-mono break-all">{httpsBaseUrl}</p>}
            </div>
        </details>
    );
}
