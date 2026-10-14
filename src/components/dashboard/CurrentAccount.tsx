import { CheckCircle, Mail, Diamond, Gem, Circle, Tag } from 'lucide-react';
import { Account, getAccountTier } from '../../types/account';
import { useTranslation } from 'react-i18next';
import { getGroupQuotaSummaries, GroupQuotaSummary } from '../../utils/quotaDisplay';
import { GroupQuotaCard } from '../accounts/GroupQuotaCard';
import { getLiveLimitForModel } from '../../utils/liveLimit';

interface CurrentAccountProps {
    account: Account | null;
    quotaView?: string;
    onSwitch?: () => void;
}

function CurrentAccount({ account, onSwitch }: CurrentAccountProps) {
    const { t } = useTranslation();
    if (!account) {
        return (
            <div className="bg-white dark:bg-[#121316] rounded-xl p-4 shadow-sm border border-gray-100 dark:border-white/[0.08]">
                <h2 className="text-base font-semibold text-gray-900 dark:text-base-content mb-2 flex items-center gap-2">
                    <CheckCircle className="w-4 h-4 text-green-500" />
                    {t('dashboard.current_account')}
                </h2>
                <div className="text-center py-4 text-gray-400 dark:text-gray-500 text-sm">
                    {t('dashboard.no_active_account')}
                </div>
            </div>
        );
    }

    const tier = getAccountTier(account);
    const groupSummaries: GroupQuotaSummary[] = getGroupQuotaSummaries(
        account.quota?.quota_groups,
        account.quota?.models,
        tier
    );

    const isGroupProtected = (key: 'gemini' | 'claude') => {
        if (!account.protected_models || account.protected_models.length === 0) return false;
        if (key === 'gemini') {
            return account.protected_models.some(m => m.toLowerCase().includes('gemini'));
        }
        return account.protected_models.some(m => m.toLowerCase().includes('claude') || m.toLowerCase().includes('gpt'));
    };

    return (
        <div className="bg-white dark:bg-[#15161b] rounded-2xl p-5 shadow-sm dark:shadow-xl dark:shadow-black/40 border border-gray-100 dark:border-white/[0.10] h-full flex flex-col hover:shadow-md transition-shadow">
            <h2 className="text-base font-semibold text-gray-900 dark:text-base-content mb-3 flex items-center gap-2">
                <CheckCircle className="w-4 h-4 text-green-500" />
                {t('dashboard.current_account')}
            </h2>

            <div className="space-y-4 flex-1 flex flex-col justify-between">
                <div className="flex items-center gap-3 mb-1">
                    <div className="flex items-center gap-2 flex-1 min-w-0">
                        <Mail className="w-3.5 h-3.5 text-gray-400" />
                        <span className="text-sm font-medium text-gray-700 dark:text-gray-300 truncate">{account.email}</span>
                    </div>
                    {/* 订阅类型 */}
                    {(() => {
                        if (tier === 'ultra') {
                            return (
                                <span className="flex items-center gap-1 px-2 py-0.5 rounded-md bg-gradient-to-r from-purple-600 to-pink-600 text-white text-[10px] font-bold shadow-sm shrink-0">
                                    <Gem className="w-2.5 h-2.5 fill-current" />
                                    ULTRA
                                </span>
                            );
                        } else if (tier === 'pro') {
                            return (
                                <span className="flex items-center gap-1 px-2 py-0.5 rounded-md bg-gradient-to-r from-blue-600 to-indigo-600 text-white text-[10px] font-bold shadow-sm shrink-0">
                                    <Diamond className="w-2.5 h-2.5 fill-current" />
                                    PRO
                                </span>
                            );
                        } else {
                            return (
                                <span className="flex items-center gap-1 px-2 py-0.5 rounded-md bg-gray-100 dark:bg-white/10 text-gray-500 dark:text-gray-400 text-[10px] font-bold shadow-sm border border-gray-200 dark:border-white/10 shrink-0">
                                    <Circle className="w-2.5 h-2.5" />
                                    FREE
                                </span>
                            );
                        }
                    })()}
                    {/* 自定义标签 */}
                    {account.custom_label && (
                        <span className="flex items-center gap-1 px-2 py-0.5 rounded-md bg-orange-100 dark:bg-orange-900/30 text-orange-600 dark:text-orange-400 text-[10px] font-bold shadow-sm shrink-0">
                            <Tag className="w-2.5 h-2.5" />
                            {account.custom_label}
                        </span>
                    )}
                </div>

                {/* 统一分组展示: Gemini 与 Claude/第三方 */}
                <div className="grid grid-cols-1 sm:grid-cols-2 gap-2 my-auto">
                    {groupSummaries.map((summary) => (
                        <GroupQuotaCard
                            key={summary.key}
                            summary={summary}
                            isProtected={isGroupProtected(summary.key)}
                            liveLimit={summary.key === 'gemini' ? getLiveLimitForModel(account, 'gemini-pro') : getLiveLimitForModel(account, 'claude')}
                        />
                    ))}
                </div>

                {onSwitch && (
                    <div className="pt-2">
                        <button
                            className="w-full px-3 py-1.5 text-xs text-gray-700 dark:text-zinc-300 border border-gray-200 dark:border-white/[0.12] rounded-lg hover:bg-gray-50 dark:hover:bg-white/[0.06] active:scale-[0.98] transition-all cursor-pointer"
                            onClick={onSwitch}
                        >
                            {t('dashboard.switch_account')}
                        </button>
                    </div>
                )}
            </div>
        </div>
    );
}

export default CurrentAccount;
