import { useTranslation } from 'react-i18next';
import type { NominalCostTotals } from './usageMetrics';

export function NominalCostBreakdown({ costs, formatCurrency }: {
    costs: NominalCostTotals;
    formatCurrency: (value: number) => string;
}) {
    const { t } = useTranslation();
    const missingPrice = t('token_stats.price_not_configured', 'Price not configured');
    return (
        <div className="text-xs space-y-1">
            <div className="flex justify-between gap-2">
                <span>{t('token_stats.reported_cache_input_cost', 'Known-sample input')}</span>
                <span className="font-mono text-emerald-600 dark:text-emerald-400">
                    {costs.input === null
                        ? costs.pricedModels === 0 ? missingPrice : t('token_stats.cache_not_reported', 'Not reported')
                        : formatCurrency(costs.input)}
                </span>
            </div>
            <div className="flex justify-between gap-2">
                <span>{t('token_stats.reported_output_cost', 'Reported output')}</span>
                <span className="font-mono text-emerald-600 dark:text-emerald-400">
                    {costs.output === null ? costs.pricedModels === 0 ? missingPrice : t('token_stats.cache_not_reported', 'Not reported') : formatCurrency(costs.output)}
                </span>
            </div>
            <div className="text-[10px] text-gray-500 dark:text-gray-400">
                {t('token_stats.priced_model_coverage', 'Pricing: {{known}}/{{total}} models', {
                    known: costs.pricedModels, total: costs.totalModels,
                })}
            </div>
            <div className="text-[10px] text-gray-500 dark:text-gray-400">
                {t('token_stats.partial_nominal_cost_note', 'Nominal estimate from reported usage; not the full bill')}
            </div>
        </div>
    );
}
