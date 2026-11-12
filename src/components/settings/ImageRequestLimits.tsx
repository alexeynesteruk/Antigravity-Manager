import { useState } from 'react';
import { useTranslation } from 'react-i18next';
import { AppConfig, ImageSchedulerConfig, ProxyConfig } from '../../types/config';
import { useConfigStore } from '../../stores/useConfigStore';
import { reloadSavedConfigIfUnchanged } from '../../utils/configUpdates';

interface Props {
    proxy: ProxyConfig;
    onChange: (updates: Partial<ProxyConfig>) => void | Promise<void>;
}

export default function ImageRequestLimits({ proxy, onChange }: Props) {
    const { t } = useTranslation();
    const [error, setError] = useState('');
    const current: ImageSchedulerConfig = {
        per_account_concurrency: 4,
        max_outputs_per_request: 10,
        max_parallel_outputs: 1,
        ...proxy.image_scheduler,
    };
    const update = async (key: 'max_outputs_per_request' | 'max_parallel_outputs', raw: string) => {
        const value = Number(raw);
        if (!raw.trim() || !Number.isSafeInteger(value) || value < 1 || value > (key === 'max_outputs_per_request' ? 100 : 16)) {
            setError(t('proxy.images.invalid_limit', { defaultValue: 'Enter a positive whole number within the displayed limits.' }));
            return;
        }
        const latest = useConfigStore.getState().config?.proxy ?? proxy;
        const next: ImageSchedulerConfig = { ...current, ...latest.image_scheduler, [key]: value };
        if (key === 'max_outputs_per_request') {
            next.max_parallel_outputs = Math.min(next.max_parallel_outputs, value);
        }
        if (next.max_parallel_outputs > next.max_outputs_per_request) {
            setError(t('proxy.images.parallel_limit', { defaultValue: 'Parallel outputs cannot exceed the image count limit.' }));
            return;
        }
        let optimistic: AppConfig | null = null;
        try {
            const saving = onChange({ image_scheduler: next });
            optimistic = useConfigStore.getState().config;
            await saving;
            setError('');
        } catch {
            if (optimistic) await reloadSavedConfigIfUnchanged(optimistic);
            setError(t('proxy.images.save_failed', { defaultValue: 'Could not save image request limits.' }));
        }
    };
    return <details className="rounded-lg border border-gray-200 dark:border-base-300 p-3">
        <summary className="cursor-pointer text-sm font-medium">{t('proxy.images.limits', { defaultValue: 'Image request limits' })}</summary>
        <div className="space-y-3 pt-3">
            <p className="text-xs text-gray-500 dark:text-gray-400">{t('proxy.images.limits_hint', { defaultValue: 'Requests above the image count limit are rejected. Outputs are processed gradually within the parallel limit. Changes apply to new requests; server overrides take precedence.' })}</p>
            {([
                ['max_outputs_per_request', t('proxy.images.max_outputs', { defaultValue: 'Maximum images per request (1–100)' }), 100],
                ['max_parallel_outputs', t('proxy.images.parallel_outputs', { defaultValue: 'Parallel outputs per request (1–16)' }), Math.min(16, current.max_outputs_per_request)],
            ] as const).map(([key, label, max]) => <label key={key} className="flex items-center justify-between gap-3 text-xs">
                <span>{label}</span>
                <input key={`${key}:${current[key]}`} type="number" min={1} max={max} step={1} defaultValue={current[key]}
                    className="input input-bordered input-sm w-24"
                    onBlur={event => { if (event.currentTarget.value !== String(current[key])) void update(key, event.currentTarget.value); }}
                    onKeyDown={event => { if (event.key === 'Enter') event.currentTarget.blur(); }} />
            </label>)}
            {error && <p role="alert" className="text-xs text-error">{error}</p>}
        </div>
    </details>;
}
