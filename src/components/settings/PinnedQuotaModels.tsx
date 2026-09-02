import { Pin, Check } from 'lucide-react';
import { useTranslation } from 'react-i18next';
import { PinnedQuotaModelsConfig } from '../../types/config';
import { MODEL_CONFIG } from '../../config/modelConfig';
import { useAccountStore } from '../../stores/useAccountStore';

interface PinnedQuotaModelsProps {
    config: PinnedQuotaModelsConfig;
    onChange: (config: PinnedQuotaModelsConfig) => void;
}

const PinnedQuotaModels = ({ config, onChange }: PinnedQuotaModelsProps) => {
    const { t } = useTranslation();

    const toggleModel = (model: string) => {
        const currentModels = config.models || [];
        let newModels: string[];

        if (currentModels.includes(model)) {
            // Keep at least one model
            if (currentModels.length <= 1) return;
            newModels = currentModels.filter(m => m !== model);
        } else {
            newModels = [...currentModels, model];
        }

        onChange({ ...config, models: newModels });
    };

    const { accounts } = useAccountStore();
    const uniqueIds = new Set<string>();

    // First collect the id and protectedKey of all known models, to prevent them from showing up as unknown "dynamically extracted models"
    Object.entries(MODEL_CONFIG).forEach(([id, cfg]) => {
        uniqueIds.add(id.toLowerCase());
        if (cfg.protectedKey) {
            uniqueIds.add(cfg.protectedKey.toLowerCase());
        }
    });

    const addedDisplayLabels = new Set<string>();

    // Base built-in configured models
    const baseModels = Object.entries(MODEL_CONFIG)
        .filter(([id, cfg]) => {
            // Hide thinking variants
            if (id.includes('thinking')) return false;

            const labelKey = (cfg.shortLabel || cfg.label).toLowerCase();
            // At this level, if the display labelKey has already been added, don't add it again to the exported options
            if (addedDisplayLabels.has(labelKey)) return false;
            addedDisplayLabels.add(labelKey);
            return true;
        })
        .map(([id, cfg]) => ({
            id,
            label: id,
            desc: cfg.shortLabel || cfg.label || t(cfg.i18nDescKey || cfg.i18nKey, cfg.label)
        }));

    // Extract the historical dynamic models across all accounts
    const dynamicModels = accounts.flatMap(a => a.quota?.models || [])
        .filter(m => {
            const id = m.name.toLowerCase();
            if (id.includes('thinking')) return false;
            // Dedupe: avoid models already included in the built-in list or duplicate id names
            if (uniqueIds.has(id)) return false;
            uniqueIds.add(id);
            return true;
        })
        .map(m => ({
            id: m.name.toLowerCase(),
            label: m.name.toLowerCase(),
            desc: m.display_name || t('settings.pinned_quota_models.dynamic', 'Dynamic Extracted Model')
        }));

    const modelOptions = [...baseModels, ...dynamicModels];

    // [FIX] Ensure previously pinned but unknown/hidden models are still rendered so users can un-pin them
    const currentChecked = config.models || [];
    currentChecked.forEach(modelId => {
        if (!modelOptions.some(m => m.id === modelId)) {
            // Try to find its real name in the historical quota data (to handle cases like a thinking model being hidden but still in the pinned list)
            const quotaModel = accounts.flatMap(a => a.quota?.models || []).find(m => m.name.toLowerCase() === modelId.toLowerCase());
            const cfg = MODEL_CONFIG[modelId.toLowerCase()];

            modelOptions.push({
                id: modelId,
                label: modelId,
                desc: quotaModel?.display_name || cfg?.shortLabel || cfg?.label || t('common.unknown', 'Unknown')
            });
        }
    });

    return (
        <div className="animate-in fade-in duration-500">
            <div className="flex items-center gap-4">
                {/* Icon section - uses a blue-purple tone */}
                <div className="w-10 h-10 rounded-xl bg-indigo-50 dark:bg-indigo-900/20 flex items-center justify-center text-indigo-500 group-hover:bg-indigo-500 group-hover:text-white transition-all duration-300">
                    <Pin size={20} />
                </div>
                <div>
                    <div className="font-bold text-gray-900 dark:text-gray-100">
                        {t('settings.pinned_quota_models.title')}
                    </div>
                    <p className="text-xs text-gray-500 dark:text-gray-400 mt-0.5">
                        {t('settings.pinned_quota_models.desc')}
                    </p>
                </div>
            </div>

            {/* Model selection area */}
            <div className="mt-5 pt-5 border-t border-gray-100 dark:border-base-200 space-y-4">
                <div className="grid grid-cols-4 gap-2">
                    {modelOptions.map((model) => {
                        const isSelected = config.models?.includes(model.id);
                        return (
                            <div
                                key={model.id}
                                onClick={() => toggleModel(model.id)}
                                className={`
                                    flex items-center justify-between p-2 rounded-lg border cursor-pointer transition-all duration-200
                                    ${isSelected
                                        ? 'bg-indigo-50 dark:bg-indigo-900/10 border-indigo-200 dark:border-indigo-800/50 text-indigo-700 dark:text-indigo-400'
                                        : 'bg-gray-50/50 dark:bg-base-200/50 border-gray-100 dark:border-base-300/50 text-gray-500 hover:border-gray-200 dark:hover:border-base-300'}
                                `}
                            >
                                <div className="flex flex-col min-w-0">
                                    <span className="text-[11px] font-bold truncate">
                                        {model.label}
                                    </span>
                                    <span className="text-[9px] text-gray-400 dark:text-gray-500 mt-0.5 truncate">
                                        {model.desc}
                                    </span>
                                </div>
                                <div className={`
                                    w-4 h-4 rounded-full flex items-center justify-center transition-all duration-300 flex-shrink-0 ml-1
                                    ${isSelected ? 'bg-indigo-500 text-white scale-100' : 'bg-gray-200 dark:bg-base-300 text-transparent scale-75 opacity-0'}
                                `}>
                                    <Check size={10} strokeWidth={4} />
                                </div>
                            </div>
                        );
                    })}
                </div>


            </div>
        </div>
    );
};

export default PinnedQuotaModels;
