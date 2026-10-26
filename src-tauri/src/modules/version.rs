use crate::modules::process;
#[cfg(any(target_os = "macos", target_os = "linux"))]
use std::fs;
use std::path::PathBuf;

/// Antigravity 版本信息
#[derive(Debug, Clone)]
pub struct AntigravityVersion {
    pub short_version: String,
    #[allow(dead_code)] // 预留给构建/诊断输出
    pub bundle_version: String,
}

/// 从任意字符串中提取第一个语义化版本号 (X.Y.Z)
fn extract_semver(raw: &str) -> Option<String> {
    for token in raw.split(|c: char| c.is_whitespace() || c == ',' || c == ';') {
        let t = token.trim_matches(|c: char| c == '"' || c == '\'' || c == '(' || c == ')');
        if t.is_empty() {
            continue;
        }
        let mut parts = t.split('.');
        let p1 = parts.next();
        let p2 = parts.next();
        let p3 = parts.next();
        if p1.is_some()
            && p2.is_some()
            && p3.is_some()
            && [p1.unwrap(), p2.unwrap(), p3.unwrap()]
                .iter()
                .all(|p| !p.is_empty() && p.chars().all(|c| c.is_ascii_digit()))
        {
            return Some(t.to_string());
        }
    }
    None
}

/// 检测 Antigravity 版本（跨平台，支持预快照路径优先）
pub fn get_antigravity_version_with_path(
    target_ide: Option<&str>,
    preferred_path: Option<&std::path::Path>,
) -> Result<AntigravityVersion, String> {
    // 1. 优先使用预捕获路径，若无则探查 Antigravity 可执行文件路径
    let exe_path = preferred_path
        .map(|p| p.to_path_buf())
        .or_else(|| process::get_antigravity_executable_path(target_ide))
        .ok_or("Unable to locate Antigravity executable")?;

    // 2. 根据平台读取版本信息
    #[cfg(target_os = "macos")]
    {
        get_version_macos(&exe_path)
    }

    #[cfg(target_os = "windows")]
    {
        get_version_windows(&exe_path)
    }

    #[cfg(target_os = "linux")]
    {
        get_version_linux(&exe_path)
    }
}

/// 检测 Antigravity 版本（跨平台）
pub fn get_antigravity_version(target_ide: Option<&str>) -> Result<AntigravityVersion, String> {
    get_antigravity_version_with_path(target_ide, None)
}

/// macOS: 从 Info.plist 读取版本
#[cfg(target_os = "macos")]
fn get_version_macos(exe_path: &PathBuf) -> Result<AntigravityVersion, String> {
    use plist::Value;

    // exe_path 可能是 /Applications/Antigravity.app 或内部可执行文件
    // 需要找到 .app 目录
    let path_str = exe_path.to_string_lossy();
    let app_path = if let Some(idx) = path_str.find(".app") {
        PathBuf::from(&path_str[..idx + 4])
    } else {
        exe_path.clone()
    };

    let info_plist_path = app_path.join("Contents/Info.plist");
    if !info_plist_path.exists() {
        return Err(format!("Info.plist not found: {:?}", info_plist_path));
    }

    let content =
        fs::read(&info_plist_path).map_err(|e| format!("Failed to read Info.plist: {}", e))?;

    let plist: Value =
        plist::from_bytes(&content).map_err(|e| format!("Failed to parse Info.plist: {}", e))?;

    let dict = plist
        .as_dictionary()
        .ok_or("Info.plist is not a dictionary")?;

    let short_version = dict
        .get("CFBundleShortVersionString")
        .and_then(|v| v.as_string())
        .ok_or("CFBundleShortVersionString not found")?;

    let bundle_version = dict
        .get("CFBundleVersion")
        .and_then(|v| v.as_string())
        .unwrap_or(short_version);

    Ok(AntigravityVersion {
        short_version: short_version.to_string(),
        bundle_version: bundle_version.to_string(),
    })
}

/// Windows: 从可执行文件元数据读取版本（纯原生 Win32 API，不调用 powershell）
#[cfg(target_os = "windows")]
fn get_version_windows(exe_path: &PathBuf) -> Result<AntigravityVersion, String> {
    use std::ffi::OsStr;
    use std::os::windows::ffi::OsStrExt;

    #[link(name = "version")]
    extern "system" {
        fn GetFileVersionInfoSizeW(lptstrFilename: *const u16, lpdwHandle: *mut u32) -> u32;
        fn GetFileVersionInfoW(
            lptstrFilename: *const u16,
            dwHandle: u32,
            dwLen: u32,
            lpData: *mut std::ffi::c_void,
        ) -> i32;
        fn VerQueryValueW(
            pBlock: *const std::ffi::c_void,
            lpSubBlock: *const u16,
            lplpBuffer: *mut *mut std::ffi::c_void,
            puLen: *mut u32,
        ) -> i32;
    }

    let mut path_u16: Vec<u16> = exe_path.as_os_str().encode_wide().collect();
    path_u16.push(0);

    let mut dummy: u32 = 0;
    let size = unsafe { GetFileVersionInfoSizeW(path_u16.as_ptr(), &mut dummy) };
    if size == 0 {
        return Err("Failed to get version info size from executable".to_string());
    }

    let mut data = vec![0u8; size as usize];
    let ok =
        unsafe { GetFileVersionInfoW(path_u16.as_ptr(), 0, size, data.as_mut_ptr() as *mut _) };
    if ok == 0 {
        return Err("Failed to read version info from executable".to_string());
    }

    let subblock_root: Vec<u16> = OsStr::new(r"\").encode_wide().chain(Some(0)).collect();
    let mut root_buf: *mut std::ffi::c_void = std::ptr::null_mut();
    let mut root_len: u32 = 0;

    #[repr(C)]
    struct VsFixedFileInfo {
        dw_signature: u32,
        dw_struc_version: u32,
        dw_file_version_ms: u32,
        dw_file_version_ls: u32,
        dw_product_version_ms: u32,
        dw_product_version_ls: u32,
        dw_file_flags_mask: u32,
        dw_file_flags: u32,
        dw_file_os: u32,
        dw_file_type: u32,
        dw_file_subtype: u32,
        dw_file_date_ms: u32,
        dw_file_date_ls: u32,
    }

    if unsafe {
        VerQueryValueW(
            data.as_ptr() as *const _,
            subblock_root.as_ptr(),
            &mut root_buf,
            &mut root_len,
        )
    } != 0
        && !root_buf.is_null()
        && (root_len as usize) >= std::mem::size_of::<VsFixedFileInfo>()
    {
        let ffi = unsafe { &*(root_buf as *const VsFixedFileInfo) };
        let major = (ffi.dw_file_version_ms >> 16) & 0xffff;
        let minor = ffi.dw_file_version_ms & 0xffff;
        let patch = (ffi.dw_file_version_ls >> 16) & 0xffff;
        let build = ffi.dw_file_version_ls & 0xffff;
        let version = if build > 0 {
            format!("{}.{}.{}.{}", major, minor, patch, build)
        } else {
            format!("{}.{}.{}", major, minor, patch)
        };
        return Ok(AntigravityVersion {
            short_version: version.clone(),
            bundle_version: version,
        });
    }

    Err("Version information not found in executable".to_string())
}

/// Read only installation metadata. Executing an Electron GUI with --version can
/// launch it or focus the existing window through single-instance IPC.
#[cfg(any(target_os = "linux", test))]
fn get_version_linux(exe_path: &PathBuf) -> Result<AntigravityVersion, String> {
    let resolved = std::fs::canonicalize(exe_path).unwrap_or_else(|_| exe_path.clone());
    let mut roots = Vec::new();
    for path in [&resolved, exe_path] {
        if let Some(parent) = path.parent() {
            if !roots.contains(&parent.to_path_buf()) {
                roots.push(parent.to_path_buf());
            }
            if parent.file_name().is_some_and(|name| name == "bin") {
                if let Some(root) = parent.parent() {
                    roots.push(root.to_path_buf());
                }
            }
        }
    }
    // Stay anchored to the selected installation rather than reporting a
    // different application's version from an unrelated global directory.
    for root in &roots {
        for relative in [
            "resources/app/package.json",
            "resources/app/product.json",
            "package.json",
            "product.json",
        ] {
            if let Ok(file) = std::fs::File::open(root.join(relative)) {
                use std::io::Read;
                let mut raw = String::new();
                if file.take(1024 * 1024).read_to_string(&mut raw).is_ok() {
                    if let Some(version) = version_from_package_json(raw.as_bytes()) {
                        return Ok(version);
                    }
                }
            }
        }
    }
    for root in roots {
        if let Some(version) = version_from_asar(&root.join("resources/app.asar")) {
            return Ok(version);
        }
    }
    Err("Unable to statically determine Antigravity version on Linux".to_string())
}

#[cfg(any(target_os = "linux", test))]
fn version_from_package_json(bytes: &[u8]) -> Option<AntigravityVersion> {
    let json: serde_json::Value = serde_json::from_slice(bytes).ok()?;
    let version = json.get("version")?.as_str()?.trim();
    let numeric_core = version.split(['-', '+']).next()?;
    let components: Vec<_> = numeric_core.split('.').collect();
    if components.len() < 3
        || components
            .iter()
            .any(|part| part.is_empty() || !part.bytes().all(|byte| byte.is_ascii_digit()))
        || !version
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || b".-+".contains(&byte))
    {
        return None;
    }
    Some(AntigravityVersion {
        short_version: numeric_core.to_string(),
        bundle_version: version.to_string(),
    })
}

/// Electron ASAR stores package.json at an offset relative to the end of its
/// Pickle header. Parse that entry, never an arbitrary dependency's version.
#[cfg(any(target_os = "linux", test))]
fn version_from_asar(path: &std::path::Path) -> Option<AntigravityVersion> {
    use std::io::{Read, Seek, SeekFrom};
    let mut file = std::fs::File::open(path).ok()?;
    let mut prefix = [0u8; 16];
    file.read_exact(&mut prefix).ok()?;
    if u32::from_le_bytes(prefix[..4].try_into().ok()?) != 4 {
        return None;
    }
    let header_size = u32::from_le_bytes(prefix[4..8].try_into().ok()?) as u64;
    let json_size = u32::from_le_bytes(prefix[12..16].try_into().ok()?) as u64;
    if header_size > 4 * 1024 * 1024 || json_size > header_size.checked_sub(8)? {
        return None;
    }
    let mut header = vec![0; json_size as usize];
    file.read_exact(&mut header).ok()?;
    let header: serde_json::Value = serde_json::from_slice(&header).ok()?;
    let package = header.get("files")?.get("package.json")?;
    if package.get("unpacked").and_then(|v| v.as_bool()) == Some(true) {
        return None;
    }
    let offset = package.get("offset")?.as_str()?.parse::<u64>().ok()?;
    let size = package.get("size")?.as_u64()?;
    if size > 1024 * 1024 {
        return None;
    }
    let start = 8u64.checked_add(header_size)?.checked_add(offset)?;
    if start.checked_add(size)? > file.metadata().ok()?.len() {
        return None;
    }
    file.seek(SeekFrom::Start(start)).ok()?;
    let mut bytes = vec![0; size as usize];
    file.read_exact(&mut bytes).ok()?;
    version_from_package_json(&bytes)
}

/// 判断是否为新版本 (>= 1.16.5)
pub fn is_new_version(version: &AntigravityVersion) -> bool {
    compare_version(&version.short_version, "1.16.5") >= std::cmp::Ordering::Equal
}

/// 比较版本号
pub fn compare_version(v1: &str, v2: &str) -> std::cmp::Ordering {
    let parts1: Vec<u32> = v1.split('.').filter_map(|s| s.parse().ok()).collect();
    let parts2: Vec<u32> = v2.split('.').filter_map(|s| s.parse().ok()).collect();

    for i in 0..parts1.len().max(parts2.len()) {
        let p1 = parts1.get(i).unwrap_or(&0);
        let p2 = parts2.get(i).unwrap_or(&0);
        match p1.cmp(p2) {
            std::cmp::Ordering::Equal => continue,
            other => return other,
        }
    }
    std::cmp::Ordering::Equal
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn linux_version_reads_selected_installation_without_launching_it() {
        let dir = tempfile::tempdir().unwrap();
        let executable = dir.path().join("antigravity");
        std::fs::write(&executable, b"not an executable").unwrap();
        assert!(get_version_linux(&executable).is_err());
        let app = dir.path().join("resources/app");
        std::fs::create_dir_all(&app).unwrap();
        std::fs::write(app.join("package.json"), br#"{"version":"2.24.0"}"#).unwrap();
        assert_eq!(
            get_version_linux(&executable).unwrap().short_version,
            "2.24.0"
        );
        std::fs::write(app.join("package.json"), b"broken").unwrap();
        std::fs::write(app.join("product.json"), br#"{"version":"2.25.0"}"#).unwrap();
        assert_eq!(
            get_version_linux(&executable).unwrap().short_version,
            "2.25.0"
        );
    }

    #[test]
    fn asar_version_comes_from_root_package_and_bounds_archive_reads() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("app.asar");
        let package = br#"{"version":"2.24.0"}"#;
        let make_archive = |offset: &str, size: u64| {
            let header = serde_json::json!({"files": {
                "dependency": {"version": "99.99.99"},
                "package.json": {"offset":offset,"size":size}
            }})
            .to_string();
            let padded = (header.len() + 3) & !3;
            let header_size = 8 + padded;
            let mut archive = Vec::new();
            for value in [
                4,
                header_size as u32,
                header_size as u32 - 4,
                header.len() as u32,
            ] {
                archive.extend_from_slice(&value.to_le_bytes());
            }
            archive.extend_from_slice(header.as_bytes());
            archive.resize(8 + header_size, 0);
            archive.extend_from_slice(package);
            archive
        };
        std::fs::write(&path, make_archive("0", package.len() as u64)).unwrap();
        assert_eq!(version_from_asar(&path).unwrap().short_version, "2.24.0");
        std::fs::write(&path, make_archive("18446744073709551615", 20)).unwrap();
        assert!(version_from_asar(&path).is_none());
        std::fs::write(&path, make_archive("0", 2 * 1024 * 1024)).unwrap();
        assert!(version_from_asar(&path).is_none());
    }

    #[test]
    fn static_version_metadata_preserves_prerelease_and_build_suffixes() {
        for value in ["2.24.0-beta.1", "2.24.0-dev", "2.24.0+build.7"] {
            let raw = serde_json::json!({"version":value}).to_string();
            let parsed = version_from_package_json(raw.as_bytes()).unwrap();
            assert_eq!(parsed.short_version, "2.24.0");
            assert_eq!(parsed.bundle_version, value);
        }
        assert!(version_from_package_json(br#"{"version":"2.24.nope"}"#).is_none());
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn linux_version_follows_symlink_and_never_executes_missing_metadata() {
        use std::os::unix::fs::{symlink, PermissionsExt};
        let dir = tempfile::tempdir().unwrap();
        let install = dir.path().join("installation");
        std::fs::create_dir(&install).unwrap();
        let executable = install.join("antigravity");
        let marker = install.join("launched");
        std::fs::write(
            &executable,
            format!("#!/bin/sh\ntouch '{}'\necho 2.24.0\n", marker.display()),
        )
        .unwrap();
        std::fs::set_permissions(&executable, std::fs::Permissions::from_mode(0o700)).unwrap();
        let link = dir.path().join("launcher");
        symlink(&executable, &link).unwrap();
        assert!(get_version_linux(&link).is_err());
        assert!(!marker.exists());
        std::fs::write(install.join("package.json"), br#"{"version":"2.24.0"}"#).unwrap();
        assert_eq!(get_version_linux(&link).unwrap().short_version, "2.24.0");
        assert!(!marker.exists());
    }

    #[test]
    fn test_version_comparison() {
        assert_eq!(
            compare_version("1.16.5", "1.16.4"),
            std::cmp::Ordering::Greater
        );
        assert_eq!(
            compare_version("1.16.5", "1.16.5"),
            std::cmp::Ordering::Equal
        );
        assert_eq!(
            compare_version("1.16.4", "1.16.5"),
            std::cmp::Ordering::Less
        );
        assert_eq!(
            compare_version("1.17.0", "1.16.5"),
            std::cmp::Ordering::Greater
        );
        assert_eq!(
            compare_version("2.0.0", "1.16.5"),
            std::cmp::Ordering::Greater
        );
    }

    #[test]
    fn test_is_new_version() {
        let old = AntigravityVersion {
            short_version: "1.16.4".to_string(),
            bundle_version: "1.16.4".to_string(),
        };
        assert!(!is_new_version(&old));

        let new = AntigravityVersion {
            short_version: "1.16.5".to_string(),
            bundle_version: "1.16.5".to_string(),
        };
        assert!(is_new_version(&new));

        let newer = AntigravityVersion {
            short_version: "1.17.0".to_string(),
            bundle_version: "1.17.0".to_string(),
        };
        assert!(is_new_version(&newer));
    }

    #[test]
    fn test_extract_semver_from_messy_output() {
        let raw = "1.107.0\n1504c8cc4b34dbfbb4a97ebe954b3da2b5634516\nx64";
        assert_eq!(extract_semver(raw), Some("1.107.0".to_string()));
    }
}
