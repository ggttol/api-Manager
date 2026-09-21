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
    custom_label?: string;  // 用户自定义标签
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
    subscription_tier?: string;  // 订阅类型: FREE/PRO/ULTRA
    model_forwarding_rules?: Record<string, string>; // 废弃模型转发表
    quota_groups?: QuotaGroup[]; // 按模型组的配额摘要 (weekly + 5h 双窗口)
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

/** 单个配额桶 (weekly / 5h) */
export interface QuotaBucket {
    bucket_id: string;
    window: string;  // "weekly" | "5h"
    remaining_fraction: number;
    reset_time: string;
    display_name?: string;
    description?: string;
}

/** 模型组配额 (如 Gemini Models / Claude and GPT models) */
export interface QuotaGroup {
    display_name?: string;
    description?: string;
    buckets?: QuotaBucket[];
}

export type AccountTier = 'ultra' | 'pro' | 'free';

/**
 * Normalize the authoritative subscription tier returned by Code Assist.
 * Model availability is deliberately not used as a fallback: the model catalog
 * is shared by free and paid accounts and cannot identify a subscription.
 */
export function getAccountTier(account: { quota?: QuotaData | null }): AccountTier {
    const rawTier = account.quota?.subscription_tier?.trim().toLowerCase();
    if (!rawTier) return 'free';

    // Canonical values plus historical/internal aliases.
    if (rawTier.includes('ultra') || rawTier.includes('helium')) return 'ultra';
    if (rawTier.includes('free') || rawTier.includes('starter')) return 'free';
    if (
        rawTier.includes('pro') ||
        rawTier.includes('premium') ||
        rawTier.includes('advanced')
    ) {
        return 'pro';
    }
    return 'free';
}

export function getTierLabel(tier: AccountTier): string {
    return tier.toUpperCase();
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
