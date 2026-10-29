//! 入站模型闸门（协议无关）
//!
//! 目标：在「选号 / 发往上游」之前，把**根本不存在的模型**与**全池无配额的模型**
//! 在网关层直接拒答，避免把无效模型发往 Google 触发 429 长冷却，污染整条号池链路。
//!
//! 判定顺序（严格遵循产品定义）：
//! 1. 请求未携带 model-id（空 / 缺失）→ 404 `model not exist`
//! 2. 应用用户自定义映射（精确 → 大小写 → 规范化 → 通配符）；无命中则保留原值
//! 3. 映射后的最终 model-id 必须存在于「下游 API 开放列表 ∪ agent 工具一键配置列表」，
//!    否则 → 404 `model not exist`
//! 4. 拥有该模型配额的账号全部为 0 配额 → 502 `no quota accounts`
//!
//! 设计约束：本模块不得依赖任何硬编码模型名，全部由运行时模型池 / 官方目录驱动。

use std::collections::{HashMap, HashSet};

use axum::http::StatusCode;

use crate::proxy::common::model_mapping;
use crate::proxy::token_manager::TokenManager;

/// 网关自答文案：模型不存在（404）
pub const MODEL_NOT_EXIST: &str = "model not exist";
/// 网关自答文案：全池无配额（502）
pub const NO_QUOTA_ACCOUNTS: &str = "no quota accounts";

/// 闸门裁决结果
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ModelGate {
    /// 放行，并给出最终用于上游的模型 ID
    Allow(String),
    /// 模型不存在（含请求缺失 model 字段）
    NotFound,
    /// 该模型在所有账号上都没有剩余配额
    NoQuota,
}

impl ModelGate {
    /// 拒绝时对应的 (HTTP 状态码, 账号列展示文案)
    pub fn rejection(&self) -> Option<(StatusCode, &'static str)> {
        match self {
            ModelGate::Allow(_) => None,
            ModelGate::NotFound => Some((StatusCode::NOT_FOUND, MODEL_NOT_EXIST)),
            ModelGate::NoQuota => Some((StatusCode::BAD_GATEWAY, NO_QUOTA_ACCOUNTS)),
        }
    }

    /// 放行时的最终模型 ID
    pub fn allowed_model(&self) -> Option<&str> {
        match self {
            ModelGate::Allow(model) => Some(model.as_str()),
            _ => None,
        }
    }
}

/// 仅应用「用户自定义映射」与官方淘汰重定向；无命中时保留原值，不做任何系统映射。
pub fn resolve_requested_model(
    requested: &str,
    custom_mapping: &HashMap<String, String>,
) -> String {
    model_mapping::resolve_custom_model_route(requested, custom_mapping)
        .unwrap_or_else(|| requested.to_string())
}

/// 生图模型族变体归一化（唯一实现位于 `model_mapping`，此处保留同名入口供门禁调用）。
pub fn strip_image_variant_suffix(id: &str) -> String {
    crate::proxy::common::model_mapping::strip_image_variant_suffix(id)
}

/// 生图模型“家族键”：忽略版本号与尺寸后缀，只区分 Pro 生图 / Flash 生图。
///
/// 官方与各客户端存在多套同义命名：`gemini-3-pro-image` / `gemini-3.1-pro-image`、
/// `gemini-3-flash-image` / `gemini-3.1-flash-image`，必须视为同一可用家族。
fn image_family_key(id: &str) -> Option<&'static str> {
    let lower = id.to_lowercase();
    if !(lower.contains("image") || lower.starts_with("imagen")) {
        return None;
    }
    if lower.contains("pro-image") {
        Some("pro-image")
    } else if lower.contains("flash") {
        Some("flash-image")
    } else {
        None
    }
}

/// 生图模型族宽松匹配：允许 `-preview` / `-2k` / `-4k` / `-16x9` 等尺寸与预览变体
/// 命中已开放的基底生图模型（否则用户常用的 `gemini-3-pro-image-preview` 会被误杀）。
fn is_image_family_allowed(requested: &str, allowed: &HashSet<String>) -> bool {
    let lower = requested.trim().to_lowercase();
    if !(lower.contains("image") || lower.starts_with("imagen")) {
        return false;
    }

    let base = strip_image_variant_suffix(&lower);
    if allowed.contains(&lower) || allowed.contains(&base) {
        return true;
    }

    // 双向前缀匹配：兼容目录里只登记了带后缀版本，而客户端请求基名的场景
    if allowed.iter().any(|candidate| {
        let c = candidate.to_lowercase();
        (c.contains("image") || c.starts_with("imagen"))
            && (c.starts_with(&format!("{base}-"))
                || base.starts_with(&format!("{c}-"))
                || strip_image_variant_suffix(&c) == base)
    }) {
        return true;
    }

    // 家族级兜底：忽略版本号差异（gemini-3.1-pro-image ↔ gemini-3-pro-image，
    // gemini-3-flash-image ↔ gemini-3.1-flash-image）
    match image_family_key(&lower) {
        Some(key) => allowed
            .iter()
            .any(|candidate| image_family_key(candidate) == Some(key)),
        None => false,
    }
}

/// 下游「看得见」的模型集合：
/// 1. API 向下游开放列表（`/v1/models`、`/v1beta/models`、opencode / hermes 一键配置同源目录）
/// 2. 官方基准目录
/// 3. agent 工具一键配置的旗舰模型列表
/// 4. 自定义映射的键与目标值（客户端可显式调用别名）
pub async fn downstream_exposed_models(
    custom_mapping: &tokio::sync::RwLock<HashMap<String, String>>,
    token_manager: &TokenManager,
    only_raw_quota_models: bool,
) -> HashSet<String> {
    let mut allowed: HashSet<String> = model_mapping::get_all_dynamic_models(
        custom_mapping,
        Some(token_manager),
        only_raw_quota_models,
    )
    .await
    .into_iter()
    .collect();

    for id in model_mapping::get_supported_models() {
        allowed.insert(id);
    }
    for core in crate::proxy::cli_sync::get_core_gateway_models() {
        allowed.insert(core.id.to_string());
    }

    {
        let mapping = custom_mapping.read().await;
        for (key, target) in mapping.iter() {
            allowed.insert(key.clone());
            allowed.insert(target.clone());
        }
    }

    allowed
}

/// 入站模型闸门主入口。
pub async fn gate_inbound_model(
    requested_model: Option<&str>,
    custom_mapping: &tokio::sync::RwLock<HashMap<String, String>>,
    token_manager: &TokenManager,
) -> ModelGate {
    let requested = requested_model.map(str::trim).unwrap_or("");
    if requested.is_empty() {
        return ModelGate::NotFound;
    }

    // Resolve explicit aliases before validating current catalog availability.
    let final_model = {
        let mapping = custom_mapping.read().await;
        resolve_requested_model(requested, &mapping)
    };

    let only_raw_quota_models = crate::modules::config::load_app_config()
        .map(|cfg| cfg.proxy.only_raw_quota_models)
        .unwrap_or(false);

    // 冷启动 fail-open：号池尚未同步到任何模型时，模型目录为空，
    // 此时做存在性拦截会把全部流量误杀（例如网关刚启动的几秒内），因此直接放行。
    if token_manager.get_all_collected_models().is_empty() {
        tracing::debug!(
            "[ModelGate] Account pool has no model metadata yet, skipping existence gate for '{}'",
            final_model
        );
        return ModelGate::Allow(final_model);
    }

    let allowed =
        downstream_exposed_models(custom_mapping, token_manager, only_raw_quota_models).await;
    if allowed.is_empty() || !allowed.contains(&final_model) {
        // 生图模型族允许尺寸/预览变体（-preview / -2k / -4k / -16x9 等）宽松命中基底模型
        if !is_image_family_allowed(&final_model, &allowed) {
            return ModelGate::NotFound;
        }
    }

    // 第二道：该模型在整池是否还有任何正配额账号
    if token_manager.accounts_with_positive_quota(&final_model) == 0 {
        return ModelGate::NoQuota;
    }

    ModelGate::Allow(final_model)
}

/// 解析 `gemini-<版本>-<后缀>` 的版本号（如 `gemini-3.8-flash-low` → (3, 8)）。
///
/// 版本段支持整数写法（`gemini-4-flash` → (4, 0)）与小数写法（`3.8` → (3, 8)）。
fn gemini_flash_version(model: &str, suffix: &str) -> Option<(u32, u32)> {
    let lower = model.to_lowercase();
    let rest = lower.strip_prefix("gemini-")?;
    let version = rest.strip_suffix(suffix)?;
    if version.is_empty() || version.contains("-flash") || version.contains('-') {
        return None;
    }
    model_mapping::parse_version_tuple(version)
}

/// 从给定模型集合中挑选「兜底简单任务模型」：
/// 1. 优先取最新版本的 `gemini-<x.x>-flash-low`；
/// 2. 无 low 档时退化为最新版本的无后缀衍生模型 `gemini-<x.x>-flash`；
/// 3. 两者皆无（模型池尚未同步）→ None，由调用方跳过简单任务，绝不回退到硬编码模型。
pub fn pick_background_model(models: &HashSet<String>) -> Option<String> {
    let newest = |suffix: &str| -> Option<String> {
        models
            .iter()
            .filter_map(|m| gemini_flash_version(m, suffix).map(|v| (v, m.clone())))
            .max_by(|a, b| a.0.cmp(&b.0).then_with(|| b.1.cmp(&a.1)))
            .map(|(_, model)| model)
    };

    newest("-flash-low").or_else(|| newest("-flash"))
}

/// 从当前号池的实时模型并集里挑选兜底简单任务模型（不读取任何硬编码常量）。
pub fn pick_background_model_from_pool(token_manager: &TokenManager) -> Option<String> {
    let models: HashSet<String> = token_manager
        .get_all_collected_models()
        .into_iter()
        .collect();
    pick_background_model(&models)
}

/// 网关自答（代答）错误文本：不来自任何账号。
///
/// 全局错误分类、限流解析、账号冷却与粘性解绑链路必须无条件豁免，
/// 否则一次“模型名写错”或“全池无配额”就会连坐惩罚无辜账号。
pub const GATEWAY_SELF_ANSWER_ERRORS: [&str; 2] = [MODEL_NOT_EXIST, NO_QUOTA_ACCOUNTS];

/// 判断错误文本是否为网关自答（非上游产生）。
pub fn is_gateway_self_answer(text: &str) -> bool {
    let lower = text.to_lowercase();
    GATEWAY_SELF_ANSWER_ERRORS
        .iter()
        .any(|sentinel| lower.contains(*sentinel))
}

/// 网关拒绝时写入「转出报文」面板的英文详细说明。
///
/// 日志列表的账号列只放一句短标语（`model not exist` / `no quota accounts`），
/// 点开详情后在 Forwarded 面板展示可操作的英文排查指引。
pub fn blocked_explanation(
    reason: &str,
    requested_model: &str,
    protocol: &str,
) -> serde_json::Value {
    match reason {
        MODEL_NOT_EXIST => serde_json::json!({
            "gateway_blocked": true,
            "gateway_error": "model_not_exist",
            "protocol": protocol,
            "requested_model": requested_model,
            "summary": MODEL_NOT_EXIST,
            "detail": "This model ID is not in the model list this gateway exposes to downstream clients, so the request was rejected at the gateway and never reached any upstream account.",
            "hint": "Use a model ID returned by /v1/models (or the agent tool model list), or add a custom model mapping that points this ID to a supported model."
        }),
        _ => serde_json::json!({
            "gateway_blocked": true,
            "gateway_error": "no_quota_accounts",
            "protocol": protocol,
            "requested_model": requested_model,
            "summary": NO_QUOTA_ACCOUNTS,
            "detail": "This model ID is recognized, but no account currently has remaining quota for it, so the request was rejected at the gateway instead of being sent upstream.",
            "hint": "Retry after the quota resets, switch to another model ID, or add accounts that carry quota for this model."
        }),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn explicitly_mapped_legacy_alias_is_resolved() {
        let mapping = HashMap::from([("gemini-2.5-pro".into(), "gemini-3.1-pro-high".into())]);
        assert_eq!(
            resolve_requested_model("gemini-2.5-pro", &mapping),
            "gemini-3.1-pro-high"
        );
    }

    fn set(items: &[&str]) -> HashSet<String> {
        items.iter().map(|s| s.to_string()).collect()
    }

    #[test]
    fn prefers_newest_flash_low() {
        let models = set(&[
            "gemini-3.8-flash-high",
            "gemini-3.8-flash-low",
            "gemini-4-flash-low",
            "gemini-3.8-flash",
            "claude-sonnet-4-6",
        ]);
        assert_eq!(
            pick_background_model(&models).as_deref(),
            Some("gemini-4-flash-low")
        );
    }

    #[test]
    fn falls_back_to_bare_flash_when_no_low() {
        let models = set(&[
            "gemini-3.8-flash-high",
            "gemini-3.8-flash",
            "gemini-4-flash",
            "gemini-4-argon-medium",
        ]);
        assert_eq!(
            pick_background_model(&models).as_deref(),
            Some("gemini-4-flash"),
            "整数版本号必须被识别为 4.0，且不得命中高/中档后缀"
        );
    }

    #[test]
    fn ignores_tiered_and_image_variants() {
        let models = set(&[
            "gemini-3.8-flash-tiered",
            "gemini-3.1-flash-image",
            "gemini-3.8-flash-medium",
        ]);
        assert_eq!(pick_background_model(&models), None);
    }

    #[test]
    fn custom_mapping_wins_and_passthrough_preserves_value() {
        let mut mapping = HashMap::new();
        mapping.insert("my-alias".to_string(), "gemini-3.8-flash-high".to_string());

        assert_eq!(
            resolve_requested_model("my-alias", &mapping),
            "gemini-3.8-flash-high"
        );
        assert_eq!(
            resolve_requested_model("gemini-3.8-flash-high", &mapping),
            "gemini-3.8-flash-high",
            "无自定义映射时必须原样保留请求 ID"
        );
    }

    #[test]
    fn image_variant_suffixes_hit_base_family() {
        let allowed = set(&["gemini-3-pro-image", "gemini-3.1-flash-image"]);
        for model in [
            "gemini-3-pro-image-preview",
            "gemini-3-pro-image-2k",
            "gemini-3-pro-image-4k-16x9",
            "gemini-3.1-flash-image-4k",
            "gemini-3.1-pro-image",
            "gemini-3-flash-image",
        ] {
            assert!(
                is_image_family_allowed(model, &allowed),
                "{model} 应命中基底生图模型"
            );
        }
        assert!(!is_image_family_allowed(
            "gemini-3-pro-image",
            &set(&["gemini-3.8-flash-high"])
        ));
        assert_eq!(
            strip_image_variant_suffix("gemini-3.1-flash-image-4k-16x9"),
            "gemini-3.1-flash-image"
        );
    }

    #[test]
    fn gateway_self_answers_are_exempt_from_penalties() {
        assert!(is_gateway_self_answer("model not exist"));
        assert!(is_gateway_self_answer(
            "{\"message\":\"no quota accounts\"}"
        ));
        assert!(!is_gateway_self_answer("quota exceeded for this model"));
    }

    #[test]
    fn rejection_status_mapping() {
        assert_eq!(
            ModelGate::NotFound.rejection(),
            Some((StatusCode::NOT_FOUND, MODEL_NOT_EXIST))
        );
        assert_eq!(
            ModelGate::NoQuota.rejection(),
            Some((StatusCode::BAD_GATEWAY, NO_QUOTA_ACCOUNTS))
        );
        assert!(ModelGate::Allow("m".into()).rejection().is_none());
    }
}
