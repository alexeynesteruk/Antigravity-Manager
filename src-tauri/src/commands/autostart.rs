// Autostart commands
use tauri_plugin_autostart::ManagerExt;

#[tauri::command]
pub async fn toggle_auto_launch(app: tauri::AppHandle, enable: bool) -> Result<(), String> {
    let manager = app.autolaunch();

    if enable {
        manager
            .enable()
            .map_err(|e| format!("Failed to enable autostart: {}", e))?;
        crate::modules::logger::log_info("Autostart on login enabled");
    } else {
        match manager.disable() {
            Ok(_) => {
                crate::modules::logger::log_info("Autostart on login disabled");
            }
            Err(e) => {
                let err_msg = e.to_string();
                // On Windows, if the registry key doesn't exist, disable() returns "系统找不到指定的文件" (os error 2)
                // This case should be treated as success, since the goal (disabled) has already been achieved
                if err_msg.contains("os error 2") || err_msg.contains("找不到指定的文件") {
                    // NOTE: "找不到指定的文件" is a literal fragment of the Windows OS error message
                    // (part of "系统找不到指定的文件"). It must stay byte-identical in Chinese because
                    // it is matched against the actual OS-provided error text at runtime, which is not
                    // localized by this app.
                    crate::modules::logger::log_info("Autostart entry no longer exists, treating as disabled successfully");
                } else {
                    return Err(format!("Failed to disable autostart: {}", e));
                }
            }
        }
    }

    Ok(())
}

#[tauri::command]
pub async fn is_auto_launch_enabled(app: tauri::AppHandle) -> Result<bool, String> {
    let manager = app.autolaunch();
    manager.is_enabled().map_err(|e| e.to_string())
}
