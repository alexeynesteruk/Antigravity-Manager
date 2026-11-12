import { useEffect, useState } from 'react';
import { useTranslation } from 'react-i18next';
import { AccountSafetyConfig } from '../../types/config';
import { request } from '../../utils/request';

interface Status { halted: boolean; generation: number; reason?: string; upstream_requests_today: number; config: AccountSafetyConfig }
interface Props { config?: AccountSafetyConfig; onChange: (config: AccountSafetyConfig) => Promise<void> | void }
const defaults: AccountSafetyConfig = { halt_on_account_restriction: true, halt_on_authentication_challenge: true, client_requests_per_minute: 10, client_requests_per_day: 0, upstream_requests_per_minute: 20, upstream_requests_per_day: 500 };

export default function AccountSafetySettings({ config, onChange }: Props) {
    const { t } = useTranslation();
    const [status, setStatus] = useState<Status>();
    const [error, setError] = useState('');
    const [acknowledged, setAcknowledged] = useState(false);
    const [busy, setBusy] = useState(false);
    const current = { ...defaults, ...config };
    const refresh = async () => { try { setStatus(await request<Status>('get_account_safety_status')); setError(''); } catch (error) { setError(String(error)); } };
    useEffect(() => { let active = true; const poll = async () => { try { const next = await request<Status>('get_account_safety_status'); if (active) { setStatus(next); setError(''); } } catch (error) { if (active) setError(String(error)); } }; void poll(); const timer = setInterval(() => { void poll(); }, 5000); return () => { active = false; clearInterval(timer); }; }, []);
    useEffect(() => { setAcknowledged(false); }, [status?.generation]);
    const action = async (resume: boolean) => {
        setBusy(true);
        try {
            if (resume && status) await request('resume_account_safety', { generation: status.generation, acknowledged });
            else await request('halt_account_safety');
            setAcknowledged(false);
            await refresh();
        } catch (error) { setError(String(error)); } finally { setBusy(false); }
    };
    const update = async (changes: Partial<AccountSafetyConfig>) => { try { await onChange({ ...current, ...changes }); await refresh(); } catch (error) { setError(String(error)); } };
    const fields: [keyof Pick<AccountSafetyConfig, 'client_requests_per_minute' | 'client_requests_per_day' | 'upstream_requests_per_minute' | 'upstream_requests_per_day'>, string][] = [
        ['client_requests_per_minute', t('proxy.safety.client_minute', { defaultValue: 'Incoming requests per device / minute' })],
        ['client_requests_per_day', t('proxy.safety.client_day', { defaultValue: 'Incoming requests per device / UTC day' })],
        ['upstream_requests_per_minute', t('proxy.safety.upstream_minute', { defaultValue: 'All outbound model attempts / minute' })],
        ['upstream_requests_per_day', t('proxy.safety.upstream_day', { defaultValue: 'All outbound model attempts / UTC day' })],
    ];
    return <div className="space-y-3 text-sm">
        <div className="flex flex-wrap items-center justify-between gap-2">
            <span role="status" className={status?.halted ? 'badge badge-error' : 'badge badge-outline'}>{!status ? t('proxy.safety.unknown', { defaultValue: 'Safety status unavailable' }) : status.halted ? t('proxy.safety.paused', { defaultValue: 'Dispatch paused — attention required' }) : t('proxy.safety.active', { defaultValue: 'Dispatch enabled' })}</span>
            <button className="btn btn-sm btn-outline" disabled={busy || !status || status.halted} onClick={() => { void action(false); }}>{t('proxy.safety.stop', { defaultValue: 'Stop all dispatch' })}</button>
        </div>
        {status && <p className="text-xs text-gray-500">{t('proxy.safety.usage', { defaultValue: 'Outbound model attempts today' })}: {status.upstream_requests_today} {status.reason ? `· ${status.reason}` : ''}</p>}
        {status?.halted && <div className="space-y-2 rounded-lg border border-warning/40 p-3">
            <p>{t('proxy.safety.recovery', { defaultValue: 'Review the provider challenge or restriction in Accounts before resuming. Resume does not recover paused accounts or reset request counters. Requests already sent cannot be recalled.' })}</p>
            <label className="flex items-center gap-2"><input type="checkbox" className="checkbox checkbox-sm" checked={acknowledged} onChange={event => setAcknowledged(event.target.checked)} />{t('proxy.safety.ack', { defaultValue: 'I reviewed this stop and acknowledge resuming dispatch.' })}</label>
            <button className="btn btn-sm btn-warning" disabled={!acknowledged || busy} onClick={() => { void action(true); }}>{t('proxy.safety.resume', { defaultValue: 'Acknowledge and resume' })}</button>
        </div>}
        {error && <p role="alert" className="text-error text-xs">{error}</p>}
        <p className="text-xs text-gray-500">{t('proxy.safety.policy_hint', { defaultValue: 'Local operator limits, not a guarantee of provider-safe usage. Each retry, image and continuation counts toward the global outbound budget. Zero disables an individual limit; environment overrides take precedence. Master-key requests share one device budget.' })}</p>
        <button className="btn btn-xs btn-outline" onClick={() => { void update({ client_requests_per_minute: 30, client_requests_per_day: 0, upstream_requests_per_minute: 60, upstream_requests_per_day: 2000 }); }}>{t('proxy.safety.moderate', { defaultValue: 'Moderate preset: 30 / 60 / 2,000 per day' })}</button>
        <div className="grid gap-2 sm:grid-cols-2">{fields.map(([key, label]) => <label key={key} className="flex items-center justify-between gap-3 rounded-lg bg-base-200/50 p-2"><span className="text-xs">{label}</span><input type="number" min={0} max={4294967295} step={1} className="input input-bordered input-sm w-24" value={current[key]} onChange={event => { void update({ [key]: Math.max(0, Math.min(4294967295, Math.trunc(Number(event.target.value) || 0))) }); }} /></label>)}</div>
        {(['halt_on_account_restriction', 'halt_on_authentication_challenge'] as const).map(key => <label key={key} className="flex items-center justify-between gap-3"><span>{key === 'halt_on_account_restriction' ? t('proxy.safety.halt_restriction', { defaultValue: 'Stop all dispatch on a confirmed account restriction' }) : t('proxy.safety.halt_challenge', { defaultValue: 'Stop all dispatch on an authentication challenge' })}</span><input type="checkbox" className="toggle toggle-sm" checked={current[key]} onChange={event => { void update({ [key]: event.target.checked }); }} /></label>)}
        {status && <p className="text-[10px] text-gray-500">{t('proxy.safety.effective', { defaultValue: 'Effective limits' })}: {status.config.client_requests_per_minute} / {status.config.client_requests_per_day} · {status.config.upstream_requests_per_minute} / {status.config.upstream_requests_per_day}</p>}
    </div>;
}
