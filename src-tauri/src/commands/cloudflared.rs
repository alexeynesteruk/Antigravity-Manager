use crate::modules::cloudflared::{CloudflaredConfig, CloudflaredManager, CloudflaredStatus};
use std::sync::Arc;
use tauri::State;
use tokio::sync::RwLock;

/// Cloudflared service state management
#[derive(Clone)]
pub struct CloudflaredState {
    pub manager: Arc<RwLock<Option<CloudflaredManager>>>,
}

impl CloudflaredState {
    pub fn new() -> Self {
        Self {
            manager: Arc::new(RwLock::new(None)),
        }
    }

    /// Ensure the manager is initialized
    pub async fn ensure_manager(&self) -> Result<(), String> {
        let mut lock = self.manager.write().await;
        if lock.is_none() {
            let data_dir = crate::modules::account::get_data_dir()?;
            *lock = Some(CloudflaredManager::new(&data_dir));
        }
        Ok(())
    }
}

/// Check whether cloudflared is installed
#[tauri::command]
pub async fn cloudflared_check(
    state: State<'_, CloudflaredState>,
) -> Result<CloudflaredStatus, String> {
    state.ensure_manager().await?;

    let lock = state.manager.read().await;
    if let Some(manager) = lock.as_ref() {
        let (installed, version) = manager.check_installed().await;
        Ok(CloudflaredStatus {
            installed,
            version,
            running: false,
            url: None,
            error: None,
        })
    } else {
        Err("Manager not initialized".to_string())
    }
}

/// Install cloudflared
#[tauri::command]
pub async fn cloudflared_install(
    state: State<'_, CloudflaredState>,
) -> Result<CloudflaredStatus, String> {
    state.ensure_manager().await?;

    let lock = state.manager.read().await;
    if let Some(manager) = lock.as_ref() {
        manager.install().await
    } else {
        Err("Manager not initialized".to_string())
    }
}

/// Start the cloudflared tunnel
#[tauri::command]
pub async fn cloudflared_start(
    state: State<'_, CloudflaredState>,
    config: CloudflaredConfig,
) -> Result<CloudflaredStatus, String> {
    state.ensure_manager().await?;

    let lock = state.manager.read().await;
    if let Some(manager) = lock.as_ref() {
        manager.start(config).await
    } else {
        Err("Manager not initialized".to_string())
    }
}

/// Stop the cloudflared tunnel
#[tauri::command]
pub async fn cloudflared_stop(
    state: State<'_, CloudflaredState>,
) -> Result<CloudflaredStatus, String> {
    state.ensure_manager().await?;

    let lock = state.manager.read().await;
    if let Some(manager) = lock.as_ref() {
        manager.stop().await
    } else {
        Err("Manager not initialized".to_string())
    }
}

/// Get the cloudflared status
#[tauri::command]
pub async fn cloudflared_get_status(
    state: State<'_, CloudflaredState>,
) -> Result<CloudflaredStatus, String> {
    state.ensure_manager().await?;

    let lock = state.manager.read().await;
    if let Some(manager) = lock.as_ref() {
        let (installed, version) = manager.check_installed().await;
        let mut status = manager.get_status().await;
        status.installed = installed;
        status.version = version;
        if !installed {
            status.running = false;
            status.url = None;
        }
        Ok(status)
    } else {
        Ok(CloudflaredStatus::default())
    }
}
