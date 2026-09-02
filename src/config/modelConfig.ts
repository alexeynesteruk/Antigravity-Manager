import { Gemini, Claude, OpenAI } from '@lobehub/icons';

/**
 * Model configuration interface
 */
export interface ModelConfig {
    /** Full display name of the model (used as a fallback or default display) */
    label: string;
    /** Short label for the model (used in lists/cards) */
    shortLabel: string;
    /** Key name for the protected model */
    protectedKey: string;
    /** Model icon component */
    Icon: React.ComponentType<any>;
    /** i18n key (used for the dynamic name) */
    i18nKey: string;
    /** Description key (used for detailed explanation) */
    i18nDescKey: string;
    /** Series/group it belongs to */
    group: string;
    /** Optional tags (used for filtering) */
    tags?: string[];
}

/**
 * Model configuration map
 * Key is the model ID, value is the model configuration
 */
export const MODEL_CONFIG: Record<string, ModelConfig> = {
    // Gemini 3.x series
    // [Migrate] Gemini 3 Pro High/Low -> Gemini 3.1 Pro High/Low
    'gemini-3.1-pro-high': {
        label: 'Gemini 3.1 Pro High',
        shortLabel: 'G3.1 Pro',
        protectedKey: 'gemini-pro',
        Icon: Gemini.Color,
        i18nKey: 'proxy.model.pro_high',
        i18nDescKey: 'proxy.model.pro_high',
        group: 'Gemini 3',
        tags: ['pro', 'high'],
    },
    // Backward-compatible alias
    'gemini-3-pro-high': {
        label: 'Gemini 3.1 Pro High',
        shortLabel: 'G3.1 Pro',
        protectedKey: 'gemini-pro',
        Icon: Gemini.Color,
        i18nKey: 'proxy.model.pro_high',
        i18nDescKey: 'proxy.model.pro_high',
        group: 'Gemini 3',
        tags: ['pro', 'high'],
    },
    'gemini-3-flash': {
        label: 'Gemini 3 Flash',
        shortLabel: 'G3 Flash',
        protectedKey: 'gemini-flash',
        Icon: Gemini.Color,
        i18nKey: 'proxy.model.flash_preview',
        i18nDescKey: 'proxy.model.flash_preview',
        group: 'Gemini 3',
        tags: ['flash'],
    },
    'gemini-3.1-flash-image': {
        label: 'Gemini 3.1 Flash Image',
        shortLabel: 'G3.1 Image',
        protectedKey: 'gemini-3.1-flash-image',
        Icon: Gemini.Color,
        i18nKey: 'proxy.model.pro_image',
        i18nDescKey: 'proxy.model.pro_image_1_1',
        group: 'Gemini 3',
        tags: ['image', 'flash'],
    },
    'gemini-3-pro-image': {
        label: 'Gemini 3 Image',
        shortLabel: 'G3 Image',
        protectedKey: 'gemini-3-pro-image',
        Icon: Gemini.Color,
        i18nKey: 'proxy.model.pro_image',
        i18nDescKey: 'proxy.model.pro_image_1_1',
        group: 'Gemini 3',
        tags: ['image'],
    },
    'gemini-3.5-flash': {
        label: 'Gemini 3.5 Flash',
        shortLabel: 'G3.5 Flash',
        protectedKey: 'gemini-flash',
        Icon: Gemini.Color,
        i18nKey: 'proxy.model.flash_preview',
        i18nDescKey: 'proxy.model.flash_preview',
        group: 'Gemini 3',
        tags: ['flash'],
    },
    'gemini-3.7-flash': {
        label: 'Gemini 3.7 Flash',
        shortLabel: 'G3.7 Flash',
        protectedKey: 'gemini-flash',
        Icon: Gemini.Color,
        i18nKey: 'proxy.model.flash_preview',
        i18nDescKey: 'proxy.model.flash_preview',
        group: 'Gemini 3',
        tags: ['flash'],
    },
    'gemini-3.1-flash-lite': {
        label: 'Gemini 3.1 Flash Lite',
        shortLabel: 'G3.1 Lite',
        protectedKey: 'gemini-flash',
        Icon: Gemini.Color,
        i18nKey: 'proxy.model.flash_lite',
        i18nDescKey: 'proxy.model.flash_lite',
        group: 'Gemini 3',
        tags: ['flash', 'lite'],
    },
    'gemini-3.1-pro': {
        label: 'Gemini 3.1 Pro',
        shortLabel: 'G3.1 Pro',
        protectedKey: 'gemini-pro',
        Icon: Gemini.Color,
        i18nKey: 'proxy.model.pro_high',
        i18nDescKey: 'proxy.model.pro_high',
        group: 'Gemini 3',
        tags: ['pro'],
    },
    'gemini-3-flash-agent': {
        label: 'Gemini 3.5 Flash (High)',
        shortLabel: 'G3.5 Flash',
        protectedKey: 'gemini-flash',
        Icon: Gemini.Color,
        i18nKey: 'proxy.model.flash_preview',
        i18nDescKey: 'proxy.model.flash_preview',
        group: 'Gemini 3',
        tags: ['flash', 'high'],
    },
    'gemini-pro-agent': {
        label: 'Gemini 3.1 Pro (High)',
        shortLabel: 'G3.1 Pro',
        protectedKey: 'gemini-pro',
        Icon: Gemini.Color,
        i18nKey: 'proxy.model.pro_high',
        i18nDescKey: 'proxy.model.pro_high',
        group: 'Gemini 3',
        tags: ['pro', 'high'],
    },
    'gemini-3.1-pro-low': {
        label: 'Gemini 3.1 Pro Low',
        shortLabel: 'G3.1 Low',
        protectedKey: 'gemini-pro',
        Icon: Gemini.Color,
        i18nKey: 'proxy.model.pro_low',
        i18nDescKey: 'proxy.model.pro_low',
        group: 'Gemini 3',
        tags: ['pro', 'low'],
    },
    // Backward-compatible alias
    'gemini-3-pro-low': {
        label: 'Gemini 3.1 Pro Low',
        shortLabel: 'G3.1 Low',
        protectedKey: 'gemini-pro',
        Icon: Gemini.Color,
        i18nKey: 'proxy.model.pro_low',
        i18nDescKey: 'proxy.model.pro_low',
        group: 'Gemini 3',
        tags: ['pro', 'low'],
    },

    // Gemini 2.5 series
    'gemini-2.5-flash': {
        label: 'Gemini 2.5 Flash',
        shortLabel: 'G2.5 Flash',
        protectedKey: 'gemini-flash',
        Icon: Gemini.Color,
        i18nKey: 'proxy.model.gemini_2_5_flash',
        i18nDescKey: 'proxy.model.gemini_2_5_flash',
        group: 'Gemini 2.5',
        tags: ['flash'],
    },
    'gemini-2.5-flash-lite': {
        label: 'Gemini 2.5 Flash Lite',
        shortLabel: 'G2.5 Lite',
        protectedKey: 'gemini-flash',
        Icon: Gemini.Color,
        i18nKey: 'proxy.model.flash_lite',
        i18nDescKey: 'proxy.model.flash_lite',
        group: 'Gemini 2.5',
        tags: ['flash', 'lite'],
    },
    'gemini-2.5-flash-thinking': {
        label: 'Gemini 2.5 Flash Think',
        shortLabel: 'G2.5 Think',
        protectedKey: 'gemini-flash',
        Icon: Gemini.Color,
        i18nKey: 'proxy.model.flash_thinking',
        i18nDescKey: 'proxy.model.flash_thinking',
        group: 'Gemini 2.5',
        tags: ['flash', 'thinking'],
    },
    'gemini-2.5-pro': {
        label: 'Gemini 2.5 Pro',
        shortLabel: 'G2.5 Pro',
        protectedKey: 'gemini-pro',
        Icon: Gemini.Color,
        i18nKey: 'proxy.model.gemini_2_5_pro',
        i18nDescKey: 'proxy.model.gemini_2_5_pro',
        group: 'Gemini 2.5',
        tags: ['pro'],
    },

    // Claude series
    'claude-sonnet-4-6': {
        label: 'Claude 4.6',
        shortLabel: 'Claude 4.6',
        protectedKey: 'claude',
        Icon: Claude.Color,
        i18nKey: 'proxy.model.claude_sonnet',
        i18nDescKey: 'proxy.model.claude_sonnet',
        group: 'Claude',
        tags: ['sonnet'],
    },
    'claude-sonnet-4-6-thinking': {
        label: 'Claude 4.6 TK',
        shortLabel: 'Claude 4.6 TK',
        protectedKey: 'claude',
        Icon: Claude.Color,
        i18nKey: 'proxy.model.claude_sonnet_thinking',
        i18nDescKey: 'proxy.model.claude_sonnet_thinking',
        group: 'Claude',
        tags: ['sonnet', 'thinking'],
    },
    'claude-opus-4-6': {
        label: 'Claude Opus 4.6',
        shortLabel: 'Claude Opus 4.6',
        protectedKey: 'claude',
        Icon: Claude.Color,
        i18nKey: 'proxy.model.claude_opus',
        i18nDescKey: 'proxy.model.claude_opus',
        group: 'Claude',
        tags: ['opus'],
    },
    'claude-opus-4-6-thinking': {
        label: 'Claude Opus 4.6 TK',
        shortLabel: 'Claude Opus 4.6 TK',
        protectedKey: 'claude',
        Icon: Claude.Color,
        i18nKey: 'proxy.model.claude_opus_thinking',
        i18nDescKey: 'proxy.model.claude_opus_thinking',
        group: 'Claude',
        tags: ['opus', 'thinking'],
    },

    // OpenAI / Outros modelos
    'gpt-oss-120b-medium': {
        label: 'GPT-OSS 120B (Medium)',
        shortLabel: 'GPT-OSS',
        protectedKey: 'gpt-oss',
        Icon: OpenAI.Avatar,
        i18nKey: 'proxy.model.gpt_oss',
        i18nDescKey: 'proxy.model.gpt_oss',
        group: 'Other',
        tags: ['openai'],
    },
};

/**
 * Get the list of all model IDs
 */
export const getAllModelIds = (): string[] => Object.keys(MODEL_CONFIG);

/**
 * Get the configuration by model ID
 */
export const getModelConfig = (modelId: string): ModelConfig | undefined => {
    return MODEL_CONFIG[modelId.toLowerCase()];
};

/**
 * Model sort weight configuration
 * The smaller the number, the higher the priority
 */
const MODEL_SORT_WEIGHTS = {
    // Series weight (first priority)
    series: {
        'gemini-3': 100,
        'gemini-2.5': 200,
        'gemini-2': 300,
        'claude': 400,
    },
    // Performance tier weight (second priority)
    tier: {
        'pro': 10,
        'flash': 20,
        'lite': 30,
        'opus': 5,
        'sonnet': 10,
    },
    // Special suffix weight (third priority)
    suffix: {
        'thinking': 1,
        'image': 2,
        'high': 0,
        'low': 3,
    }
};

/**
 * Get the sort weight for a model
 */
function getModelSortWeight(modelId: string): number {
    const id = modelId.toLowerCase();
    let weight = 0;

    // 1. Series weight (x1000)
    if (id.startsWith('gemini-3')) {
        weight += MODEL_SORT_WEIGHTS.series['gemini-3'] * 1000;
    } else if (id.startsWith('gemini-2.5')) {
        weight += MODEL_SORT_WEIGHTS.series['gemini-2.5'] * 1000;
    } else if (id.startsWith('gemini-2')) {
        weight += MODEL_SORT_WEIGHTS.series['gemini-2'] * 1000;
    } else if (id.startsWith('claude')) {
        weight += MODEL_SORT_WEIGHTS.series['claude'] * 1000;
    }

    // 2. Performance tier weight (x100)
    if (id.includes('pro')) {
        weight += MODEL_SORT_WEIGHTS.tier['pro'] * 100;
    } else if (id.includes('flash')) {
        weight += MODEL_SORT_WEIGHTS.tier['flash'] * 100;
    } else if (id.includes('lite')) {
        weight += MODEL_SORT_WEIGHTS.tier['lite'] * 100;
    } else if (id.includes('opus')) {
        weight += MODEL_SORT_WEIGHTS.tier['opus'] * 100;
    } else if (id.includes('sonnet')) {
        weight += MODEL_SORT_WEIGHTS.tier['sonnet'] * 100;
    }

    // 3. Special suffix weight (x10)
    if (id.includes('thinking')) {
        weight += MODEL_SORT_WEIGHTS.suffix['thinking'] * 10;
    } else if (id.includes('image')) {
        weight += MODEL_SORT_WEIGHTS.suffix['image'] * 10;
    } else if (id.includes('high')) {
        weight += MODEL_SORT_WEIGHTS.suffix['high'] * 10;
    } else if (id.includes('low')) {
        weight += MODEL_SORT_WEIGHTS.suffix['low'] * 10;
    }

    return weight;
}

/**
 * Sort the model list
 * @param models the model list
 * @returns the sorted model list
 */
export function sortModels<T extends { id: string }>(models: T[]): T[] {
    return [...models].sort((a, b) => {
        const weightA = getModelSortWeight(a.id);
        const weightB = getModelSortWeight(b.id);

        // Sort ascending by weight
        if (weightA !== weightB) {
            return weightA - weightB;
        }

        // When weights are equal, sort alphabetically
        return a.id.localeCompare(b.id);
    });
}

// -- Model categorization and protection keys (implemented in src/utils/modelCategory.ts, only re-exported here) --

export {
    categorizeModel,
    getModelProtectionKey,
    getModelDisplayName,
    findQuotaModel,
    findImageQuotaModel,
    ensurePinnedImageSelector,
    DEFAULT_IMAGE_PIN_SELECTOR,
    resolveQuotaModels,
    type ModelCategory,
    type QuotaModelSelection,
} from '../utils/modelCategory';
