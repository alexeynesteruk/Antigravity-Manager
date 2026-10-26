// 官方 Antigravity Hub 客户端指纹探测与解析模块
//
// 提供官方语言服务枢纽 (Antigravity Hub) 版本号与构建提交点 (CL) 的动态检测能力：
// 1. 用户自定义优先 (custom_version / custom_cl)
// 2. 官方远程自动更新源 (antigravity-hub-auto-updater) 实时探测 (异步非阻塞)
// 3. 本地已安装 Antigravity Hub 探测 (macOS Info.plist / Windows PE / Linux)
// 4. 内置稳态基准兜底 (OFFICIAL_HUB_VERSION / OFFICIAL_HUB_CL)

use once_cell::sync::Lazy;
use serde::{Deserialize, Serialize};
use std::sync::RwLock;
use tracing::{debug, info, warn};

use crate::constants::{OFFICIAL_HUB_CL, OFFICIAL_HUB_VERSION};

/// 客户端指纹解析来源
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum HubFingerprintSource {
    /// 用户手动配置覆盖
    Custom,
    /// 官方远程更新源探测
    Remote,
    /// 本地已安装客户端探测
    Local,
    /// 内置稳态基准兜底
    Baseline,
}

impl std::fmt::Display for HubFingerprintSource {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Custom => write!(f, "custom"),
            Self::Remote => write!(f, "remote"),
            Self::Local => write!(f, "local"),
            Self::Baseline => write!(f, "baseline"),
        }
    }
}

/// 官方 Hub 客户端指纹结构体
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct HubFingerprint {
    /// 官方 Hub 版本号 (如 "2.23.1")
    pub version: String,
    /// 官方构建提交号 (Piper CL, 如 "998881661")
    pub cl: String,
    /// 完整出站 User-Agent 字符串
    pub user_agent: String,
    /// 指纹来源标识
    pub source: HubFingerprintSource,
}

/// 全局缓存的最新 Hub 指纹，初始状态使用纯内存稳态基线，零运行时或 I/O 异常风险
static CACHED_HUB_FINGERPRINT: Lazy<RwLock<HubFingerprint>> = Lazy::new(|| {
    RwLock::new(HubFingerprint {
        version: OFFICIAL_HUB_VERSION.to_string(),
        cl: OFFICIAL_HUB_CL.to_string(),
        user_agent: build_hub_user_agent(OFFICIAL_HUB_VERSION, OFFICIAL_HUB_CL),
        source: HubFingerprintSource::Baseline,
    })
});

/// 根据系统和架构获取官方更新清单的 URL
pub fn get_official_updater_manifest_url() -> &'static str {
    match std::env::consts::OS {
        "macos" => match std::env::consts::ARCH {
            "aarch64" => {
                "https://antigravity-hub-auto-updater-974169037036.us-central1.run.app/manifest/latest-arm64-mac.yml"
            }
            _ => {
                "https://antigravity-hub-auto-updater-974169037036.us-central1.run.app/manifest/latest-mac.yml"
            }
        },
        "windows" => {
            "https://antigravity-hub-auto-updater-974169037036.us-central1.run.app/manifest/latest.yml"
        }
        "linux" => {
            "https://antigravity-hub-auto-updater-974169037036.us-central1.run.app/manifest/latest-linux.yml"
        }
        _ => {
            "https://antigravity-hub-auto-updater-974169037036.us-central1.run.app/manifest/latest-arm64-mac.yml"
        }
    }
}

/// 构建官方标准的 Antigravity Hub User-Agent 字符串
pub fn build_hub_user_agent(version: &str, cl: &str) -> String {
    let os_type = match std::env::consts::OS {
        "windows" => "windows",
        "macos" => "darwin",
        "linux" => "linux",
        other => other,
    };
    let arch = match std::env::consts::ARCH {
        "x86_64" => "amd64",
        "aarch64" => "arm64",
        other => other,
    };
    format!(
        "antigravity/hub/{} (aidev_client; os_type={}; arch={}; cl={})",
        version, os_type, arch, cl
    )
}

/// 从更新清单 YAML 文本中解析 version 字段 (纯函数)
pub fn parse_hub_version_from_manifest(text: &str) -> Option<String> {
    for line in text.lines() {
        let trimmed = line.trim();
        if let Some(rest) = trimmed.strip_prefix("version:") {
            let ver = rest.trim().trim_matches(|c| c == '\'' || c == '"');
            if !ver.is_empty()
                && ver
                    .chars()
                    .all(|c| c.is_ascii_digit() || c == '.' || c == '-')
            {
                debug!(remote_version = %ver, "Successfully parsed remote Hub version");
                return Some(ver.to_string());
            }
        }
    }
    None
}

/// 尝试从官方远程更新清单中异步非阻塞探测最新 Hub 版本
pub async fn try_detect_remote_hub_version() -> Option<String> {
    let url = get_official_updater_manifest_url();
    let client = match reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(5))
        .build()
    {
        Ok(c) => c,
        Err(e) => {
            warn!(
                "Failed to build reqwest client for Hub version detection: {}",
                e
            );
            return None;
        }
    };

    let resp = match client.get(url).send().await {
        Ok(r) => r,
        Err(e) => {
            debug!("Remote Hub updater manifest unreachable: {}", e);
            return None;
        }
    };

    if !resp.status().is_success() {
        return None;
    }

    let text = resp.text().await.ok()?;
    parse_hub_version_from_manifest(&text)
}

/// 尝试从本地已安装的 Antigravity 客户端探测 Hub 版本
pub fn try_detect_local_hub_version() -> Option<String> {
    if let Ok(local_ver) = crate::modules::version::get_antigravity_version(None) {
        let ver = if !local_ver.short_version.is_empty() {
            local_ver.short_version
        } else {
            local_ver.bundle_version
        };
        if !ver.is_empty() {
            return Some(ver);
        }
    }
    None
}

/// Installed metadata supplies a version but no matching build number. Do not
/// combine an arbitrary local version with the compiled baseline's CL.
fn fingerprint_from_local_version(
    version: String,
    custom_cl: Option<&str>,
) -> Option<HubFingerprint> {
    let custom_cl = custom_cl.map(str::trim).filter(|value| !value.is_empty());
    if custom_cl.is_none() && version != OFFICIAL_HUB_VERSION {
        debug!(version = %version,
            "Local Hub version has no verified build number; using the known baseline pair");
        return None;
    }
    let cl = custom_cl.unwrap_or(OFFICIAL_HUB_CL).to_string();
    Some(HubFingerprint {
        user_agent: build_hub_user_agent(&version, &cl),
        version,
        cl,
        source: HubFingerprintSource::Local,
    })
}

/// 同步解析 Hub 客户端指纹（不发起任何外部网络 I/O，安全可靠）
/// 优先级: 用户手动自定义 > 本地安装探测 > 稳态基准
pub fn resolve_hub_fingerprint_sync(
    custom_version: Option<&str>,
    custom_cl: Option<&str>,
) -> HubFingerprint {
    let clean_ver = custom_version.map(|s| s.trim()).filter(|s| !s.is_empty());
    let clean_cl = custom_cl.map(|s| s.trim()).filter(|s| !s.is_empty());

    // 1. 如果用户自定义了版本与 CL
    if let (Some(v), Some(c)) = (clean_ver, clean_cl) {
        return HubFingerprint {
            version: v.to_string(),
            cl: c.to_string(),
            user_agent: build_hub_user_agent(v, c),
            source: HubFingerprintSource::Custom,
        };
    }

    let final_cl = clean_cl.unwrap_or(OFFICIAL_HUB_CL).to_string();

    // 2. 如果用户仅自定义了版本
    if let Some(v) = clean_ver {
        return HubFingerprint {
            version: v.to_string(),
            cl: final_cl.clone(),
            user_agent: build_hub_user_agent(v, &final_cl),
            source: HubFingerprintSource::Custom,
        };
    }

    // 3. 尝试本地已安装探测
    if let Some(local_ver) = try_detect_local_hub_version() {
        if let Some(fingerprint) = fingerprint_from_local_version(local_ver, clean_cl) {
            return fingerprint;
        }
    }

    // 4. 稳态基准兜底
    HubFingerprint {
        version: OFFICIAL_HUB_VERSION.to_string(),
        cl: final_cl.clone(),
        user_agent: build_hub_user_agent(OFFICIAL_HUB_VERSION, &final_cl),
        source: HubFingerprintSource::Baseline,
    }
}

/// 异步全流程解析 Hub 客户端指纹（包含官方远程源实时探测）
/// 优先级: 用户手动自定义 > 远程更新源探测 > 本地安装探测 > 稳态基准
pub async fn resolve_hub_fingerprint_async(
    custom_version: Option<&str>,
    custom_cl: Option<&str>,
) -> HubFingerprint {
    let clean_ver = custom_version.map(|s| s.trim()).filter(|s| !s.is_empty());
    let clean_cl = custom_cl.map(|s| s.trim()).filter(|s| !s.is_empty());

    // 1. 用户自定义版本与 CL
    if let (Some(v), Some(c)) = (clean_ver, clean_cl) {
        return HubFingerprint {
            version: v.to_string(),
            cl: c.to_string(),
            user_agent: build_hub_user_agent(v, c),
            source: HubFingerprintSource::Custom,
        };
    }

    let final_cl = clean_cl.unwrap_or(OFFICIAL_HUB_CL).to_string();

    // 2. 用户仅自定义了版本
    if let Some(v) = clean_ver {
        return HubFingerprint {
            version: v.to_string(),
            cl: final_cl.clone(),
            user_agent: build_hub_user_agent(v, &final_cl),
            source: HubFingerprintSource::Custom,
        };
    }

    // 3. 尝试远程动态探测 (异步非阻塞，超时 5 秒)
    if let Some(remote_ver) = try_detect_remote_hub_version().await {
        return HubFingerprint {
            version: remote_ver.clone(),
            cl: final_cl.clone(),
            user_agent: build_hub_user_agent(&remote_ver, &final_cl),
            source: HubFingerprintSource::Remote,
        };
    }

    // 4. 尝试本地已安装探测
    if let Some(local_ver) = try_detect_local_hub_version() {
        if let Some(fingerprint) = fingerprint_from_local_version(local_ver, clean_cl) {
            return fingerprint;
        }
    }

    // 5. 稳态基准兜底
    HubFingerprint {
        version: OFFICIAL_HUB_VERSION.to_string(),
        cl: final_cl.clone(),
        user_agent: build_hub_user_agent(OFFICIAL_HUB_VERSION, &final_cl),
        source: HubFingerprintSource::Baseline,
    }
}

/// 获取当前缓存的 Hub 客户端指纹（带毒化恢复保障）
pub fn get_current_hub_fingerprint() -> HubFingerprint {
    let guard = match CACHED_HUB_FINGERPRINT.read() {
        Ok(g) => g,
        Err(poisoned) => poisoned.into_inner(),
    };
    guard.clone()
}

/// 获取当前生效的 Hub User-Agent
pub fn get_current_hub_user_agent() -> String {
    get_current_hub_fingerprint().user_agent
}

/// 同步更新 Hub 客户端指纹（适用于启动配置或配置保存）
pub fn update_hub_fingerprint(
    custom_version: Option<&str>,
    custom_cl: Option<&str>,
) -> HubFingerprint {
    let resolved = resolve_hub_fingerprint_sync(custom_version, custom_cl);
    info!(
        version = %resolved.version,
        cl = %resolved.cl,
        source = %resolved.source,
        "Hub client fingerprint updated (sync)"
    );
    let mut lock = match CACHED_HUB_FINGERPRINT.write() {
        Ok(l) => l,
        Err(poisoned) => poisoned.into_inner(),
    };
    *lock = resolved.clone();
    resolved
}

/// 异步探测并更新 Hub 客户端指纹（适用于动态探测按键或后台刷新）
pub async fn detect_and_update_hub_fingerprint(
    custom_version: Option<&str>,
    custom_cl: Option<&str>,
) -> HubFingerprint {
    let resolved = resolve_hub_fingerprint_async(custom_version, custom_cl).await;
    info!(
        version = %resolved.version,
        cl = %resolved.cl,
        source = %resolved.source,
        "Hub client fingerprint detected and updated (async)"
    );
    let mut lock = match CACHED_HUB_FINGERPRINT.write() {
        Ok(l) => l,
        Err(poisoned) => poisoned.into_inner(),
    };
    *lock = resolved.clone();
    resolved
}

/// Startup discovery must not overwrite a setting saved while the remote
/// manifest was in flight. Keep the synchronous baseline until discovery ends.
pub async fn refresh_hub_fingerprint_if_unchanged(
    expected: HubFingerprint,
    custom_version: Option<String>,
    custom_cl: Option<String>,
) {
    let resolved =
        resolve_hub_fingerprint_async(custom_version.as_deref(), custom_cl.as_deref()).await;
    if !startup_pair_is_verified(&resolved, custom_cl.as_deref()) {
        warn!(version = %resolved.version,
            "Detected Hub version has no matching verified build number; retaining startup fingerprint");
        return;
    }
    let mut current = CACHED_HUB_FINGERPRINT
        .write()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    if publish_if_unchanged(&mut current, &expected, resolved) {
        info!(version = %current.version, source = %current.source,
            "Hub startup fingerprint discovery completed");
    }
}

fn startup_pair_is_verified(resolved: &HubFingerprint, custom_cl: Option<&str>) -> bool {
    // Automatic local/remote detection supplies a version, not its matching CL.
    // Explicit overrides remain authoritative; automatic results require the
    // known version/build pair before replacing the startup fingerprint.
    resolved.source == HubFingerprintSource::Custom
        || custom_cl.is_some_and(|value| !value.trim().is_empty())
        || (resolved.version == OFFICIAL_HUB_VERSION && resolved.cl == OFFICIAL_HUB_CL)
}

fn publish_if_unchanged(
    current: &mut HubFingerprint,
    expected: &HubFingerprint,
    resolved: HubFingerprint,
) -> bool {
    if current.version != expected.version
        || current.cl != expected.cl
        || current.source != expected.source
        || current.user_agent != expected.user_agent
    {
        return false;
    }
    *current = resolved;
    true
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn startup_discovery_requires_a_verified_remote_version_build_pair() {
        let mut remote = HubFingerprint {
            version: OFFICIAL_HUB_VERSION.into(),
            cl: OFFICIAL_HUB_CL.into(),
            user_agent: build_hub_user_agent(OFFICIAL_HUB_VERSION, OFFICIAL_HUB_CL),
            source: HubFingerprintSource::Remote,
        };
        assert!(startup_pair_is_verified(&remote, None));
        remote.version = "99.0.0".into();
        assert!(!startup_pair_is_verified(&remote, None));
        assert!(!startup_pair_is_verified(&remote, Some(" ")));
        assert!(startup_pair_is_verified(&remote, Some("12345")));
        remote.source = HubFingerprintSource::Local;
        assert!(!startup_pair_is_verified(&remote, None));
        assert!(startup_pair_is_verified(&remote, Some("12345")));
    }

    #[test]
    fn local_discovery_requires_its_matching_build_number() {
        let known = fingerprint_from_local_version(OFFICIAL_HUB_VERSION.into(), None).unwrap();
        assert_eq!(known.version, OFFICIAL_HUB_VERSION);
        assert_eq!(known.cl, OFFICIAL_HUB_CL);
        assert_eq!(known.source, HubFingerprintSource::Local);
        assert!(startup_pair_is_verified(&known, None));

        for version in ["2.23.1", "99.0.0"] {
            assert!(fingerprint_from_local_version(version.into(), None).is_none());
            assert!(fingerprint_from_local_version(version.into(), Some(" ")).is_none());
            let overridden =
                fingerprint_from_local_version(version.into(), Some(" 12345 ")).unwrap();
            assert_eq!(overridden.version, version);
            assert_eq!(overridden.cl, "12345");
            assert!(overridden.user_agent.contains("cl=12345"));
            assert!(startup_pair_is_verified(&overridden, Some("12345")));
        }
    }

    #[test]
    fn explicit_version_override_keeps_its_existing_semantics() {
        let custom = resolve_hub_fingerprint_sync(Some("3.0.0"), None);
        assert_eq!(custom.version, "3.0.0");
        assert_eq!(custom.cl, OFFICIAL_HUB_CL);
        assert_eq!(custom.source, HubFingerprintSource::Custom);
        assert!(startup_pair_is_verified(&custom, None));
    }

    #[test]
    fn startup_discovery_preserves_settings_saved_during_detection() {
        let baseline = HubFingerprint {
            version: "2.24.0".into(),
            cl: "1000335682".into(),
            user_agent: build_hub_user_agent("2.24.0", "1000335682"),
            source: HubFingerprintSource::Baseline,
        };
        let remote = HubFingerprint {
            version: "2.25.0".into(),
            source: HubFingerprintSource::Remote,
            ..baseline.clone()
        };
        let mut current = baseline.clone();
        assert!(publish_if_unchanged(
            &mut current,
            &baseline,
            remote.clone()
        ));
        let custom = resolve_hub_fingerprint_sync(Some("3.0.0"), Some("12345"));
        current = custom.clone();
        assert!(!publish_if_unchanged(&mut current, &baseline, remote));
        assert_eq!(current.version, custom.version);
        assert_eq!(current.cl, custom.cl);
        assert_eq!(current.source, HubFingerprintSource::Custom);
    }

    #[test]
    fn test_build_hub_user_agent() {
        let ua = build_hub_user_agent("2.23.1", "998881661");
        assert!(ua.starts_with("antigravity/hub/2.23.1"));
        assert!(ua.contains("aidev_client"));
        assert!(ua.contains("cl=998881661"));
    }

    #[test]
    fn test_custom_override_takes_precedence() {
        let fp = resolve_hub_fingerprint_sync(Some("3.0.0"), Some("12345678"));
        assert_eq!(fp.version, "3.0.0");
        assert_eq!(fp.cl, "12345678");
        assert_eq!(fp.source, HubFingerprintSource::Custom);
        assert!(fp.user_agent.contains("2.23.1") == false);
        assert!(fp.user_agent.contains("3.0.0"));
    }

    #[test]
    fn test_parse_hub_version_from_manifest() {
        let sample_yaml = r#"
version: 2.23.1
files:
  - url: Antigravity-2.23.1-arm64-mac.zip
    sha512: abcdef
"#;
        let ver = parse_hub_version_from_manifest(sample_yaml);
        assert_eq!(ver, Some("2.23.1".to_string()));
    }

    #[test]
    fn test_baseline_fallback() {
        let fp = resolve_hub_fingerprint_sync(None, None);
        // 如果本地没有安装 Antigravity，则必定回退到 baseline 或检测到 local
        assert!(!fp.version.is_empty());
        assert!(!fp.cl.is_empty());
    }
}
