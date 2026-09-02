export interface Account {
    id: string;
    email: string;
    name?: string;
    token: TokenData;
    device_profile?: DeviceProfile;
    device_history?: DeviceProfileVersion[];
    quota?: QuotaData;
    disabled?: boolean;
    disabled_reason?: string;
    disabled_at?: number;
    proxy_disabled?: boolean;
    proxy_disabled_reason?: string;
    proxy_disabled_at?: number;
    protected_models?: string[];
    live_limited_models?: Record<string, LiveLimitStatus>;
    custom_label?: string;  // User-defined custom label
    validation_blocked?: boolean;
    validation_blocked_until?: number;
    validation_blocked_reason?: string;
    validation_url?: string;
    created_at: number;
    last_used: number;
}

export interface LiveLimitStatus {
    model: string;
    status: number;
    reason: string;
    until: number;
    detected_at: number;
    message?: string;
}

export interface TokenData {
    access_token: string;
    refresh_token: string;
    expires_in: number;
    expiry_timestamp: number;
    token_type: string;
    email?: string;
}

export interface QuotaData {
    models: ModelQuota[];
    last_updated: number;
    is_forbidden?: boolean;
    forbidden_reason?: string;
    subscription_tier?: string;  // Subscription type: FREE/PRO/ULTRA
    model_forwarding_rules?: Record<string, string>; // Deprecated model forwarding table
    quota_groups?: QuotaGroup[]; // Quota summary grouped by model (weekly + 5h dual window)
}

export interface ModelQuota {
    name: string;
    percentage: number;
    reset_time: string;
    display_name?: string;
    supports_images?: boolean;
    supports_thinking?: boolean;
    thinking_budget?: number;
    recommended?: boolean;
    max_tokens?: number;
    max_output_tokens?: number;
    supported_mime_types?: Record<string, boolean>;
}

/** A single quota bucket (weekly / 5h) */
export interface QuotaBucket {
    bucket_id: string;
    window: string;  // "weekly" | "5h"
    remaining_fraction: number;
    reset_time: string;
    display_name?: string;
    description?: string;
}

/** Model group quota (e.g. Gemini Models / Claude and GPT models) */
export interface QuotaGroup {
    display_name: string;
    description?: string;
    buckets: QuotaBucket[];
}

export interface DeviceProfile {
    machine_id: string;
    mac_machine_id: string;
    dev_device_id: string;
    sqm_id: string;
}

export interface DeviceProfileVersion {
    id: string;
    created_at: number;
    label: string;
    profile: DeviceProfile;
    is_current?: boolean;
}
