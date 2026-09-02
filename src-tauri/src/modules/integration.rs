use crate::models::Account;
use crate::modules::{db, device, process, version};
use std::fs;
pub trait SystemIntegration: Send + Sync {
    /// System-level operations executed when switching accounts (e.g. killing processes, writing files, injecting into the database)
    async fn on_account_switch(
        &self,
        account: &crate::models::Account,
        target_ide: Option<&str>,
    ) -> Result<(), String>;

    /// Update the system tray (if applicable)
    fn update_tray(&self);

    /// Send a system notification
    fn show_notification(&self, title: &str, body: &str);
}

/// Desktop implementation: includes full process control and UI sync
pub struct DesktopIntegration {
    pub app_handle: tauri::AppHandle,
}

impl SystemIntegration for DesktopIntegration {
    async fn on_account_switch(
        &self,
        account: &crate::models::Account,
        target_ide: Option<&str>,
    ) -> Result<(), String> {
        crate::modules::logger::log_info(&format!(
            "[Desktop] Executing system switch for: {} (target_ide: {:?})",
            account.email, target_ide
        ));

        if target_ide == Some("agy") {
            write_to_system_keyring(account)?;

            if let Ok(storage_path) = device::get_storage_path(target_ide) {
                if let Some(ref profile) = account.device_profile {
                    let _ = device::write_profile(&storage_path, profile);
                }
            }

            let is_running = process::is_process_running_by_name("agy");
            let msg = if is_running {
                format!(
                    "Account {} activated. Agy is running, token will be picked up automatically.",
                    account.email
                )
            } else {
                format!(
                    "Account {} activated. Token is ready for your next CLI command.",
                    account.email
                )
            };
            self.show_notification("Antigravity CLI", &msg);
            self.update_tray();

            return Ok(());
        }

        // 1. First close any externally running process (whether native or IDE, close it safely first to avoid file or credential conflicts)
        if process::is_antigravity_running(target_ide) {
            process::close_antigravity(20, target_ide)?;
        }

        // 2. Smart decision: whether to store the Token via the latest system Keychain credential manager approach
        let mut is_ide = target_ide == Some("ide");

        // Auto-detect IDE: if the located executable is the IDE, treat as IDE mode
        if !is_ide {
            if let Some(exe_path) = process::get_antigravity_executable_path(target_ide) {
                let path_lower = exe_path.to_string_lossy().to_lowercase();
                if path_lower.contains("antigravity ide") || path_lower.contains("antigravity-ide")
                {
                    is_ide = true;
                    crate::modules::logger::log_info(
                        "[Desktop] Auto-detected Antigravity IDE executable, using IDE account switch logic.",
                    );
                }
            }
        }

        let mut use_keyring = false;

        if !is_ide {
            // Classic native version: auto-detect the version number
            match version::get_antigravity_version(target_ide) {
                Ok(ver) => {
                    // If the version number >= 2.0.0
                    if version::compare_version(&ver.short_version, "2.0.0")
                        != std::cmp::Ordering::Less
                    {
                        use_keyring = true;
                        crate::modules::logger::log_info(&format!(
                            "[Desktop] Detected Antigravity version {} >= 2.0.0, using system Keyring.",
                            ver.short_version
                        ));
                    } else {
                        crate::modules::logger::log_info(&format!(
                            "[Desktop] Detected Antigravity version {} < 2.0.0, falling back to legacy SQLite injection.",
                            ver.short_version
                        ));
                    }
                }
                Err(e) => {
                    // If detection fails, default to injecting as a new credential to prevent the latest version from being blocked by an error due to missing storage.json
                    use_keyring = true;
                    crate::modules::logger::log_warn(&format!(
                        "[Desktop] Failed to detect Antigravity version ({}), defaulting to system Keyring for robustness.",
                        e
                    ));
                }
            }
        }

        if use_keyring {
            // ================== Latest Antigravity native app logic (>= 2.0.0) ==================
            // 2.1 Write to the system Keychain/Keyring
            write_to_system_keyring(account)?;

            // 2.2 The native app may not have storage.json, but if it does, we can also try to safely write the device Profile, to remain compatible with fingerprint information
            if let Ok(storage_path) = device::get_storage_path(target_ide) {
                if let Some(ref profile) = account.device_profile {
                    let _ = device::write_profile(&storage_path, profile);
                }
            }
        } else {
            // ================== Legacy Antigravity old version or custom IDE logic (< 2.0.0) ==================
            // 2.1 Get the storage path
            let storage_path = device::get_storage_path(target_ide)?;

            // 2.2 Write the device Profile
            if let Some(ref profile) = account.device_profile {
                device::write_profile(&storage_path, profile)?;
            }

            // 2.3 Database handling and Token injection
            let db_path = db::get_db_path(target_ide)?;
            if db_path.exists() {
                let backup_path = db_path.with_extension("vscdb.backup");
                let _ = fs::copy(&db_path, &backup_path);
            }

            db::inject_token(
                &db_path,
                &account.token.access_token,
                &account.token.refresh_token,
                account.token.expiry_timestamp,
                &account.email,
                account.token.is_gcp_tos,
                account.token.project_id.as_deref(),
                account.token.id_token.as_deref(),
                account.token.oauth_client_key.as_deref(),
                target_ide,
            )?;

            // 2.4 Sync the Service Machine ID to the database
            if let Some(ref profile) = account.device_profile {
                let _ = db::write_service_machine_id(&db_path, &profile.mac_machine_id);
            }
        }

        // 3. Restart the external process
        process::start_antigravity(target_ide)?;

        // 4. Update the tray
        let _ = crate::modules::tray::update_tray_menus(&self.app_handle);

        Ok(())
    }

    fn update_tray(&self) {
        let _ = crate::modules::tray::update_tray_menus(&self.app_handle);
    }

    fn show_notification(&self, title: &str, body: &str) {
        // Uses tauri-plugin-dialog or native notifications (simplified here)
        crate::modules::logger::log_info(&format!("[Notification] {}: {}", title, body));
    }
}

/// Helper method: write the Token to the host operating system's Keychain/Credentials Manager
fn write_to_system_keyring(account: &crate::models::Account) -> Result<(), String> {
    // 1. Build the Token's JSON Payload, and format the expiry timestamp to an RFC3339-compliant format with microseconds
    let expiry_datetime = chrono::DateTime::from_timestamp(account.token.expiry_timestamp, 0)
        .unwrap_or_else(|| chrono::Utc::now());
    let expiry_str = expiry_datetime.to_rfc3339_opts(chrono::SecondsFormat::Micros, true);

    #[derive(serde::Serialize)]
    struct KeyringTokenDetails {
        access_token: String,
        token_type: String,
        refresh_token: String,
        expiry: String,
    }

    #[derive(serde::Serialize)]
    struct KeyringPayload {
        token: KeyringTokenDetails,
        auth_method: String,
    }

    let payload_json = serde_json::to_string(&KeyringPayload {
        token: KeyringTokenDetails {
            access_token: account.token.access_token.clone(),
            token_type: "Bearer".to_string(),
            refresh_token: account.token.refresh_token.clone(),
            expiry: expiry_str,
        },
        auth_method: "consumer".to_string(),
    })
    .map_err(|e| format!("Failed to serialize keyring JSON: {}", e))?;

    crate::modules::logger::log_info(&format!(
        "[Desktop] Writing token to system credential store for: {}",
        account.email
    ));

    // 2. Cross-platform credential injection
    #[cfg(target_os = "macos")]
    {
        use base64::{engine::general_purpose::STANDARD, Engine as _};
        use std::process::Command;
        let encoded_payload = STANDARD.encode(&payload_json);
        let full_keyring_value = format!("go-keyring-base64:{}", encoded_payload);

        // 2.1 macOS Keychain Access
        // Delete the old one
        let _ = Command::new("security")
            .args([
                "delete-generic-password",
                "-s",
                "gemini",
                "-a",
                "antigravity",
            ])
            .output();

        // Write the new one (the -A flag allows all local apps to read the credential directly without a password prompt)
        let output = Command::new("security")
            .args([
                "add-generic-password",
                "-s",
                "gemini",
                "-a",
                "antigravity",
                "-w",
                &full_keyring_value,
                "-A",
            ])
            .output()
            .map_err(|e| format!("Failed to execute security command: {}", e))?;

        if !output.status.success() {
            let err_msg = String::from_utf8_lossy(&output.stderr);
            return Err(format!("macOS security command failed: {}", err_msg.trim()));
        }
    }

    #[cfg(target_os = "windows")]
    {
        // 2.2 Windows Credential Manager direct Win32 API calls to write raw UTF-8 bytes
        use std::os::windows::ffi::OsStrExt;
        use std::ptr;

        #[repr(C)]
        struct FILETIME {
            dw_low_date_time: u32,
            dw_high_date_time: u32,
        }

        #[repr(C)]
        struct CREDENTIALW {
            flags: u32,
            cred_type: u32,
            target_name: *const u16,
            comment: *const u16,
            last_written: FILETIME,
            credential_blob_size: u32,
            credential_blob: *const u8,
            persist: u32,
            attribute_count: u32,
            attributes: *const std::ffi::c_void,
            target_alias: *const u16,
            user_name: *const u16,
        }

        #[link(name = "advapi32")]
        extern "system" {
            fn CredWriteW(credential: *const CREDENTIALW, flags: u32) -> i32;
            fn CredDeleteW(target_name: *const u16, type_: u32, flags: u32) -> i32;
        }

        let target = "gemini:antigravity";
        let user = "antigravity";
        let secret = payload_json.as_bytes();

        let target_wide: Vec<u16> = std::ffi::OsStr::new(target)
            .encode_wide()
            .chain(std::iter::once(0))
            .collect();

        let user_wide: Vec<u16> = std::ffi::OsStr::new(user)
            .encode_wide()
            .chain(std::iter::once(0))
            .collect();

        let cred = CREDENTIALW {
            flags: 0,
            cred_type: 1, // CRED_TYPE_GENERIC
            target_name: target_wide.as_ptr(),
            comment: ptr::null(),
            last_written: FILETIME {
                dw_low_date_time: 0,
                dw_high_date_time: 0,
            },
            credential_blob_size: secret.len() as u32,
            credential_blob: secret.as_ptr(),
            persist: 2, // CRED_PERSIST_LOCAL_MACHINE
            attribute_count: 0,
            attributes: ptr::null(),
            target_alias: ptr::null(),
            user_name: user_wide.as_ptr(),
        };

        unsafe {
            // Delete first to ensure we write clean
            let _ = CredDeleteW(target_wide.as_ptr(), 1, 0);

            let res = CredWriteW(&cred, 0);
            if res == 0 {
                let err = std::io::Error::last_os_error();
                return Err(format!("Windows CredWriteW failed: {}", err));
            }
        }
    }

    #[cfg(target_os = "linux")]
    {
        // 2.3 Linux Secret Service API
        use std::io::Write;
        use std::process::Command;
        use std::sync::mpsc;

        let mut child = Command::new("secret-tool")
            .args([
                "store",
                "--label=gemini",
                "service",
                "gemini",
                "username",
                "antigravity",
            ])
            .stdin(std::process::Stdio::piped())
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped())
            .spawn()
            .map_err(|e| format!("Failed to spawn secret-tool: {}", e))?;

        if let Some(mut stdin) = child.stdin.take() {
            stdin
                .write_all(payload_json.as_bytes())
                .map_err(|e| format!("Failed to write to secret-tool stdin: {}", e))?;
            // stdin is dropped here, closing the pipe and signalling EOF to secret-tool
        }

        // Protect against indefinite blocking when D-Bus is unavailable (common on Wayland
        // sessions where DBUS_SESSION_BUS_ADDRESS is not inherited by the Tauri process).
        let child_pid = child.id();
        let (tx, rx) = mpsc::channel::<Result<std::process::Output, std::io::Error>>();
        std::thread::spawn(move || {
            let _ = tx.send(child.wait_with_output());
        });

        let output = match rx.recv_timeout(std::time::Duration::from_secs(10)) {
            Ok(result) => result.map_err(|e| format!("Failed to wait for secret-tool: {}", e))?,
            Err(_) => {
                // secret-tool has been blocked for more than 10 seconds.
                // Most likely cause: D-Bus session bus is not reachable from the Tauri process
                // (typical on Wayland without proper DBUS_SESSION_BUS_ADDRESS propagation).
                // Kill the hung subprocess before returning the error.
                let _ = Command::new("kill")
                    .args(["-9", &child_pid.to_string()])
                    .output();
                crate::modules::logger::log_error(
                    "[Desktop] secret-tool store blocked for >10s — D-Bus session bus unreachable. \
                     Ensure gnome-keyring/kwallet is running and DBUS_SESSION_BUS_ADDRESS is exported.",
                );
                return Err(
                    "Keyring write timed out (10s). The D-Bus session bus is not reachable from this process, \
                     which is common on Wayland without proper session setup. \
                     Please ensure gnome-keyring or kwallet is running."
                        .to_string(),
                );
            }
        };

        if !output.status.success() {
            let err_msg = String::from_utf8_lossy(&output.stderr);
            return Err(format!("Linux secret-tool failed: {}", err_msg.trim()));
        }
    }

    crate::modules::logger::log_info(
        "[Desktop] Successfully wrote token to system credential store.",
    );

    // Also write file-based credentials under ~/.gemini/, for compatibility with SSH sessions, container environments, and scenarios without Keyring/D-Bus
    if let Err(e) = write_to_file_credentials(account) {
        crate::modules::logger::log_warn(&format!(
            "[Desktop] File credential sync warning: {}",
            e
        ));
    }

    Ok(())
}

/// Helper method: write local file-based credentials (~/.gemini/oauth_creds.json and ~/.gemini/google_accounts.json)
/// Used to ensure CLI/tool credential compatibility in SSH sessions, container environments, or scenarios without a system Keyring / D-Bus
fn write_to_file_credentials(account: &crate::models::Account) -> Result<(), String> {
    let home = match dirs::home_dir() {
        Some(h) => h,
        None => return Err("Failed to resolve user home directory".to_string()),
    };
    let gemini_dir = home.join(".gemini");

    if !gemini_dir.exists() {
        if let Err(e) = std::fs::create_dir_all(&gemini_dir) {
            crate::modules::logger::log_warn(&format!(
                "[Desktop] Failed to create .gemini directory: {}",
                e
            ));
            return Err(format!("Failed to create .gemini directory: {}", e));
        }
    }

    let expiry_ms = if account.token.expiry_timestamp > 10_000_000_000 {
        account.token.expiry_timestamp
    } else {
        account.token.expiry_timestamp * 1000
    };

    #[derive(serde::Serialize)]
    struct OAuthCredsFile {
        access_token: String,
        refresh_token: String,
        token_type: String,
        expiry_date: i64,
        #[serde(skip_serializing_if = "Option::is_none")]
        id_token: Option<String>,
        scope: String,
    }

    let creds = OAuthCredsFile {
        access_token: account.token.access_token.clone(),
        refresh_token: account.token.refresh_token.clone(),
        token_type: "Bearer".to_string(),
        expiry_date: expiry_ms,
        id_token: account.token.id_token.clone(),
        scope: "https://www.googleapis.com/auth/userinfo.email openid https://www.googleapis.com/auth/cloud-platform https://www.googleapis.com/auth/userinfo.profile".to_string(),
    };

    let creds_path = gemini_dir.join("oauth_creds.json");
    let json_str = serde_json::to_string_pretty(&creds)
        .map_err(|e| format!("Failed to serialize oauth_creds JSON: {}", e))?;

    if let Err(e) = std::fs::write(&creds_path, json_str) {
        crate::modules::logger::log_warn(&format!(
            "[Desktop] Failed to write oauth_creds.json: {}",
            e
        ));
        return Err(format!("Failed to write oauth_creds.json: {}", e));
    }

    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let _ = std::fs::set_permissions(&creds_path, std::fs::Permissions::from_mode(0o600));
    }

    #[derive(serde::Serialize)]
    struct GoogleAccountsFile {
        active: String,
        old: Vec<String>,
    }

    let accounts_info = GoogleAccountsFile {
        active: account.email.clone(),
        old: vec![],
    };

    let accounts_path = gemini_dir.join("google_accounts.json");
    if let Ok(accounts_json_str) = serde_json::to_string_pretty(&accounts_info) {
        let _ = std::fs::write(&accounts_path, accounts_json_str);
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let _ = std::fs::set_permissions(&accounts_path, std::fs::Permissions::from_mode(0o600));
        }
    }

    crate::modules::logger::log_info(&format!(
        "[Desktop] Successfully synced file-based credentials to ~/.gemini/oauth_creds.json for: {}",
        account.email
    ));

    Ok(())
}

/// Helper method: read the Token from the host operating system's Keychain/Credentials Manager
pub fn read_from_system_keyring() -> Result<crate::modules::migration::ImportedOAuthState, String> {
    #[cfg(target_os = "macos")]
    {
        use base64::{engine::general_purpose::STANDARD, Engine as _};
        let output = Command::new("security")
            .args([
                "find-generic-password",
                "-s",
                "gemini",
                "-a",
                "antigravity",
                "-w",
            ])
            .output()
            .map_err(|e| format!("Failed to execute security command: {}", e))?;

        if !output.status.success() {
            return Err("No credential found in macOS Keychain".to_string());
        }

        let secret_str = String::from_utf8_lossy(&output.stdout).trim().to_string();
        let payload_str = if secret_str.starts_with("go-keyring-base64:") {
            let b64_part = &secret_str["go-keyring-base64:".len()..];
            let decoded = STANDARD.decode(b64_part).map_err(|e| format!("Base64 decode failed: {}", e))?;
            String::from_utf8(decoded).map_err(|e| format!("UTF-8 decode failed: {}", e))?
        } else {
            secret_str
        };

        return parse_keyring_payload(&payload_str);
    }

    #[cfg(target_os = "windows")]
    {
        use std::os::windows::ffi::OsStrExt;
        use std::ptr;

        #[repr(C)]
        struct FILETIME {
            dw_low_date_time: u32,
            dw_high_date_time: u32,
        }

        #[repr(C)]
        struct CREDENTIALW {
            flags: u32,
            cred_type: u32,
            target_name: *const u16,
            comment: *const u16,
            last_written: FILETIME,
            credential_blob_size: u32,
            credential_blob: *mut u8,
            persist: u32,
            attribute_count: u32,
            attributes: *const std::ffi::c_void,
            target_alias: *const u16,
            user_name: *const u16,
        }

        #[link(name = "advapi32")]
        extern "system" {
            fn CredReadW(
                target_name: *const u16,
                type_: u32,
                flags: u32,
                credential: *mut *mut CREDENTIALW,
            ) -> i32;
            fn CredFree(buffer: *mut std::ffi::c_void);
        }

        let target = "gemini:antigravity";
        let target_wide: Vec<u16> = std::ffi::OsStr::new(target)
            .encode_wide()
            .chain(std::iter::once(0))
            .collect();

        let mut cred_ptr: *mut CREDENTIALW = ptr::null_mut();
        unsafe {
            let res = CredReadW(target_wide.as_ptr(), 1, 0, &mut cred_ptr);
            if res == 0 || cred_ptr.is_null() {
                return Err("No credential found in Windows Credential Manager".to_string());
            }

            let cred = &*cred_ptr;
            let blob = std::slice::from_raw_parts(
                cred.credential_blob,
                cred.credential_blob_size as usize,
            );
            let payload_str = String::from_utf8_lossy(blob).to_string();
            CredFree(cred_ptr as *mut std::ffi::c_void);

            return parse_keyring_payload(&payload_str);
        }
    }

    #[cfg(target_os = "linux")]
    {
        let output = Command::new("secret-tool")
            .args([
                "lookup",
                "service",
                "gemini",
                "username",
                "antigravity",
            ])
            .output()
            .map_err(|e| format!("Failed to execute secret-tool: {}", e))?;

        if !output.status.success() {
            return Err("No credential found in Linux secret-tool".to_string());
        }

        let payload_str = String::from_utf8_lossy(&output.stdout).trim().to_string();
        return parse_keyring_payload(&payload_str);
    }

    #[cfg(not(any(target_os = "macos", target_os = "windows", target_os = "linux")))]
    {
        Err("Keyring not supported on this operating system".to_string())
    }
}

fn parse_keyring_payload(payload_str: &str) -> Result<crate::modules::migration::ImportedOAuthState, String> {
    let json: serde_json::Value = serde_json::from_str(payload_str)
        .map_err(|e| format!("Failed to parse keyring payload JSON: {}", e))?;

    let refresh_token = json
        .get("token")
        .and_then(|t| t.get("refresh_token"))
        .and_then(|v| v.as_str())
        .or_else(|| json.get("refresh_token").and_then(|v| v.as_str()))
        .ok_or_else(|| "Refresh Token not found in keyring payload".to_string())?
        .to_string();

    Ok(crate::modules::migration::ImportedOAuthState {
        refresh_token,
        is_gcp_tos: true,
        project_id: None,
    })
}

/// Headless/Docker implementation: only performs data-layer operations, ignores UI and process control
pub struct HeadlessIntegration;

impl SystemIntegration for HeadlessIntegration {
    async fn on_account_switch(
        &self,
        account: &crate::models::Account,
        _target_ide: Option<&str>,
    ) -> Result<(), String> {
        if _target_ide == Some("agy") {
            return Err(
                "Switching to the agy CLI is not supported in headless mode (no host keyring access)."
                    .to_string(),
            );
        }

        crate::modules::logger::log_info(&format!(
            "[Headless] Account switched in memory: {}",
            account.email
        ));
        // In Docker mode, we typically don't directly control the host's VS Code process
        // If configuration needs to be synced to some volume, logic can be added here
        Ok(())
    }

    fn update_tray(&self) {
        // No-op
    }

    fn show_notification(&self, title: &str, body: &str) {
        crate::modules::logger::log_info(&format!("[Log Notification] {}: {}", title, body));
    }
}

/// System integration manager: replaces Arc<dyn SystemIntegration> to resolve async trait dyn compatibility issues
#[derive(Clone)]
pub enum SystemManager {
    Desktop(tauri::AppHandle),
    Headless,
}

impl SystemManager {
    pub async fn on_account_switch(
        &self,
        account: &Account,
        target_ide: Option<&str>,
    ) -> Result<(), String> {
        match self {
            SystemManager::Desktop(handle) => {
                let integration = DesktopIntegration {
                    app_handle: handle.clone(),
                };
                integration.on_account_switch(account, target_ide).await
            }
            SystemManager::Headless => {
                let integration = HeadlessIntegration;
                integration.on_account_switch(account, target_ide).await
            }
        }
    }

    pub fn update_tray(&self) {
        if let SystemManager::Desktop(handle) = self {
            let integration = DesktopIntegration {
                app_handle: handle.clone(),
            };
            integration.update_tray();
        }
    }

    pub fn show_notification(&self, title: &str, body: &str) {
        match self {
            SystemManager::Desktop(handle) => {
                let integration = DesktopIntegration {
                    app_handle: handle.clone(),
                };
                integration.show_notification(title, body);
            }
            SystemManager::Headless => {
                let integration = HeadlessIntegration;
                integration.show_notification(title, body);
            }
        }
    }
}

impl SystemIntegration for SystemManager {
    async fn on_account_switch(
        &self,
        account: &crate::models::Account,
        target_ide: Option<&str>,
    ) -> Result<(), String> {
        match self {
            SystemManager::Desktop(handle) => {
                let integration = DesktopIntegration {
                    app_handle: handle.clone(),
                };
                integration.on_account_switch(account, target_ide).await
            }
            SystemManager::Headless => {
                let integration = HeadlessIntegration;
                integration.on_account_switch(account, target_ide).await
            }
        }
    }

    fn update_tray(&self) {
        self.update_tray();
    }

    fn show_notification(&self, title: &str, body: &str) {
        self.show_notification(title, body);
    }
}
