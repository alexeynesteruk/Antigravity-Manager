use serde::{Deserialize, Serialize};
use std::path::PathBuf;
use std::process::Stdio;
use std::sync::Arc;
use tokio::io::{AsyncBufReadExt, AsyncRead, BufReader};
use tokio::process::{Child, Command};
use tokio::sync::RwLock;
use tracing::{debug, info};

#[cfg(target_os = "windows")]
const CREATE_NO_WINDOW: u32 = 0x08000000;
#[cfg(target_os = "windows")]
const CREATE_NEW_PROCESS_GROUP: u32 = 0x00000200;

/// Cloudflared tunnel mode
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "snake_case")]
pub enum TunnelMode {
    /// Quick tunnel (temporary URL)
    Quick,
    /// Authenticated tunnel (uses a Token)
    Auth,
}

impl Default for TunnelMode {
    fn default() -> Self {
        Self::Quick
    }
}

/// Cloudflared configuration
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CloudflaredConfig {
    #[serde(default)]
    pub enabled: bool,
    #[serde(default)]
    pub mode: TunnelMode,
    /// Local port being proxied
    pub port: u16,
    /// Token for authenticated mode
    #[serde(default)]
    pub token: Option<String>,
    /// Use the http2 protocol (more compatible)
    #[serde(default)]
    pub use_http2: bool,
}

impl Default for CloudflaredConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            mode: TunnelMode::Quick,
            port: 8045,
            token: None,
            use_http2: true, // Enable http2 by default, more stable
        }
    }
}

/// Cloudflared status
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CloudflaredStatus {
    pub installed: bool,
    pub version: Option<String>,
    pub running: bool,
    pub url: Option<String>,
    pub error: Option<String>,
}

impl Default for CloudflaredStatus {
    fn default() -> Self {
        Self {
            installed: false,
            version: None,
            running: false,
            url: None,
            error: None,
        }
    }
}

/// Cloudflared manager state
pub struct CloudflaredManager {
    process: Arc<RwLock<Option<Child>>>,
    status: Arc<RwLock<CloudflaredStatus>>,
    bin_path: PathBuf,
    /// Used to notify the process monitor task to stop
    shutdown_tx: RwLock<Option<tokio::sync::oneshot::Sender<()>>>,
}

impl CloudflaredManager {
    pub fn new(data_dir: &PathBuf) -> Self {
        let bin_name = if cfg!(target_os = "windows") {
            "cloudflared.exe"
        } else {
            "cloudflared"
        };
        let bin_path = data_dir.join("bin").join(bin_name);

        Self {
            process: Arc::new(RwLock::new(None)),
            status: Arc::new(RwLock::new(CloudflaredStatus::default())),
            bin_path,
            shutdown_tx: RwLock::new(None),
        }
    }

    /// Check whether it's already installed
    pub async fn check_installed(&self) -> (bool, Option<String>) {
        if !self.bin_path.exists() {
            return (false, None);
        }

        let mut cmd = Command::new(&self.bin_path);
        cmd.arg("--version");
        #[cfg(target_os = "windows")]
        cmd.creation_flags(CREATE_NO_WINDOW);

        match cmd.output().await {
            Ok(output) => {
                if output.status.success() {
                    let version = String::from_utf8_lossy(&output.stdout)
                        .lines()
                        .next()
                        .map(|s| s.trim().to_string());
                    (true, version)
                } else {
                    (false, None)
                }
            }
            Err(_) => (false, None),
        }
    }

    /// Get the current status
    pub async fn get_status(&self) -> CloudflaredStatus {
        self.status.read().await.clone()
    }

    /// Update the status
    async fn update_status(&self, f: impl FnOnce(&mut CloudflaredStatus)) {
        let mut status = self.status.write().await;
        f(&mut status);
    }

    /// Install cloudflared
    pub async fn install(&self) -> Result<CloudflaredStatus, String> {
        let bin_dir = self.bin_path.parent().unwrap();
        if !bin_dir.exists() {
            std::fs::create_dir_all(bin_dir)
                .map_err(|e| format!("Failed to create bin directory: {}", e))?;
        }

        let download_url = get_download_url()?;
        info!("[cloudflared] Downloading from: {}", download_url);

        let response = reqwest::get(&download_url)
            .await
            .map_err(|e| format!("Download failed: {}", e))?;

        if !response.status().is_success() {
            return Err(format!(
                "Download failed with status: {}",
                response.status()
            ));
        }

        let bytes = response
            .bytes()
            .await
            .map_err(|e| format!("Failed to read response: {}", e))?;

        let is_archive = download_url.ends_with(".tgz");
        if is_archive {
            let archive_path = self.bin_path.with_extension("tgz");
            std::fs::write(&archive_path, &bytes)
                .map_err(|e| format!("Failed to write archive: {}", e))?;

            let status = {
                let mut tar_cmd = Command::new("tar");
                tar_cmd
                    .arg("-xzf")
                    .arg(&archive_path)
                    .arg("-C")
                    .arg(bin_dir);
                #[cfg(target_os = "windows")]
                tar_cmd.creation_flags(CREATE_NO_WINDOW);
                tar_cmd.status().await
            }
            .map_err(|e| format!("Failed to extract archive: {}", e))?;

            if !status.success() {
                return Err("Failed to extract cloudflared archive".to_string());
            }

            let _ = std::fs::remove_file(&archive_path);
        } else {
            std::fs::write(&self.bin_path, &bytes)
                .map_err(|e| format!("Failed to write binary: {}", e))?;
        }

        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&self.bin_path, std::fs::Permissions::from_mode(0o755))
                .map_err(|e| format!("Failed to set permissions: {}", e))?;
        }

        let (installed, version) = self.check_installed().await;
        self.update_status(|s| {
            s.installed = installed;
            s.version = version.clone();
        })
        .await;

        info!(
            "[cloudflared] Installed successfully, version: {:?}",
            version
        );
        Ok(self.get_status().await)
    }

    /// Start the tunnel
    pub async fn start(&self, config: CloudflaredConfig) -> Result<CloudflaredStatus, String> {
        // Check whether it's already running
        {
            let proc = self.process.read().await;
            if proc.is_some() {
                return Ok(self.get_status().await);
            }
        }

        // Stop the previous monitor task
        if let Some(tx) = self.shutdown_tx.write().await.take() {
            let _ = tx.send(());
        }

        let (installed, version) = self.check_installed().await;
        if !installed {
            return Err("Cloudflared not installed".to_string());
        }

        let local_url = format!("http://localhost:{}", config.port);
        info!("[cloudflared] Starting tunnel to: {}", local_url);

        let mut cmd = Command::new(&self.bin_path);

        // Set the working directory
        if let Some(bin_dir) = self.bin_path.parent() {
            cmd.current_dir(bin_dir);
            debug!("[cloudflared] Working directory: {:?}", bin_dir);
        }

        match config.mode {
            TunnelMode::Quick => {
                cmd.arg("tunnel").arg("--url").arg(&local_url);

                // Note: the --no-autoupdate flag is no longer supported in newer versions of cloudflared, it causes the process to exit immediately
                // cmd.arg("--no-autoupdate");

                if config.use_http2 {
                    cmd.arg("--protocol").arg("http2");
                }

                // Note: the --loglevel flag also causes an Incorrect Usage error in this context, so it's removed to use the default value
                // cmd.arg("--loglevel").arg("info");

                info!("[cloudflared] Command args: tunnel --url {} ...", local_url);
            }
            TunnelMode::Auth => {
                if let Some(token) = &config.token {
                    cmd.arg("tunnel").arg("run").arg("--token").arg(token);

                    // Note: the --no-autoupdate flag is not supported
                    // cmd.arg("--no-autoupdate");

                    if config.use_http2 {
                        cmd.arg("--protocol").arg("http2");
                    }

                    // Note: the --loglevel flag is not supported
                    // cmd.arg("--loglevel").arg("info");

                    info!("[cloudflared] Command args: tunnel run --token [HIDDEN] ...");
                } else {
                    return Err("Token required for auth mode".to_string());
                }
            }
        }

        // Restore the pipes
        cmd.stdout(Stdio::piped()).stderr(Stdio::piped());

        // CREATE_NO_WINDOW supresses console window on Windows
        #[cfg(target_os = "windows")]
        cmd.creation_flags(CREATE_NO_WINDOW | CREATE_NEW_PROCESS_GROUP);

        let mut child = cmd.spawn().map_err(|e| format!("Failed to spawn: {}", e))?;

        let stdout = child.stdout.take();
        let stderr = child.stderr.take();

        let status_clone = self.status.clone();
        if let Some(stdout) = stdout {
            spawn_log_reader(stdout, status_clone.clone());
        }

        if let Some(stderr) = stderr {
            spawn_log_reader(stderr, status_clone.clone());
        }

        *self.process.write().await = Some(child);
        self.update_status(|s| {
            s.installed = installed.clone();
            s.version = version.clone();
            s.running = true;
            s.error = None;
        })
        .await;

        // Start the process monitor task
        let (shutdown_tx, shutdown_rx) = tokio::sync::oneshot::channel::<()>();
        *self.shutdown_tx.write().await = Some(shutdown_tx);

        let process_ref = self.process.clone();
        let status_ref = self.status.clone();

        tokio::spawn(async move {
            tokio::select! {
                _ = shutdown_rx => {
                    debug!("[cloudflared] Process monitor shutdown");
                }
                _ = async {
                    loop {
                        tokio::time::sleep(tokio::time::Duration::from_secs(3)).await;

                        let mut proc_lock = process_ref.write().await;
                        if let Some(ref mut child) = *proc_lock {
                            match child.try_wait() {
                                Ok(Some(exit_status)) => {
                                    // Process has exited
                                    info!("[cloudflared] Process exited with status: {:?}", exit_status);
                                    *proc_lock = None;
                                    drop(proc_lock);

                                    let mut s = status_ref.write().await;
                                    s.running = false;
                                    s.error = Some(format!("Tunnel process exited (status: {:?})", exit_status));
                                    break;
                                }
                                Ok(None) => {
                                    // Process is still running
                                }
                                Err(e) => {
                                    info!("[cloudflared] Error checking process: {}", e);
                                    *proc_lock = None;
                                    drop(proc_lock);

                                    let mut s = status_ref.write().await;
                                    s.running = false;
                                    s.error = Some(format!("Error checking tunnel: {}", e));
                                    break;
                                }
                            }
                        } else {
                            // Process does not exist
                            drop(proc_lock);
                            let mut s = status_ref.write().await;
                            if s.running {
                                s.running = false;
                                s.error = Some("Tunnel process not found".to_string());
                            }
                            break;
                        }
                    }
                } => {}
            }
        });

        Ok(self.get_status().await)
    }

    /// Stop the tunnel
    pub async fn stop(&self) -> Result<CloudflaredStatus, String> {
        let mut proc_lock = self.process.write().await;
        if let Some(mut child) = proc_lock.take() {
            let _ = child.kill().await;
            info!("[cloudflared] Tunnel stopped");
        }

        self.update_status(|s| {
            s.running = false;
            s.url = None;
            s.error = None;
        })
        .await;

        Ok(self.get_status().await)
    }
}

/// Get the download URL
fn get_download_url() -> Result<String, String> {
    let os = std::env::consts::OS;
    let arch = std::env::consts::ARCH;

    let (os_str, arch_str, ext) = match (os, arch) {
        ("macos", "aarch64") => ("darwin", "arm64", ".tgz"),
        ("macos", "x86_64") => ("darwin", "amd64", ".tgz"),
        ("linux", "x86_64") => ("linux", "amd64", ""),
        ("linux", "aarch64") => ("linux", "arm64", ""),
        ("windows", "x86_64") => ("windows", "amd64", ".exe"),
        _ => return Err(format!("Unsupported platform: {}-{}", os, arch)),
    };

    Ok(format!(
        "https://github.com/cloudflare/cloudflared/releases/latest/download/cloudflared-{}-{}{}",
        os_str, arch_str, ext
    ))
}

fn spawn_log_reader<R>(stream: R, status_ref: Arc<RwLock<CloudflaredStatus>>)
where
    R: AsyncRead + Unpin + Send + 'static,
{
    tokio::spawn(async move {
        let reader = BufReader::new(stream);
        let mut lines = reader.lines();
        while let Ok(Some(line)) = lines.next_line().await {
            // Keep the log level at debug, to avoid polluting production logs
            debug!("[cloudflared output] {}", line);
            if let Some(url) = extract_tunnel_url(&line) {
                info!("[cloudflared] Tunnel URL: {}", url);
                let mut s = status_ref.write().await;
                s.url = Some(url);
            }
        }
    });
}

/// Extract the tunnel URL from a log line
/// Supports two modes:
/// 1. Quick tunnel: directly extract the .trycloudflare.com URL
/// 2. Named tunnel: parse the hostname from the ingress configuration
fn extract_tunnel_url(line: &str) -> Option<String> {
    // Quick tunnel mode: directly look for the trycloudflare.com URL
    if let Some(url) = line
        .split_whitespace()
        .find(|s| s.starts_with("https://") && s.contains(".trycloudflare.com"))
    {
        return Some(url.to_string());
    }

    // Named tunnel mode: parse the hostname from the "Updated to new configuration" log line
    // Log format example: Updated to new configuration config="{\"ingress\":[{\"hostname\":\"api.example.com\", ...}]}"
    if line.contains("Updated to new configuration") && line.contains("ingress") {
        // Look for the hostname field
        if let Some(start) = line.find("\\\"hostname\\\":\\\"") {
            let after_key = &line[start + 15..]; // Skip \"hostname\":\" (15 characters total)
            if let Some(end) = after_key.find("\\\"") {
                let hostname = &after_key[..end];
                if !hostname.is_empty() {
                    return Some(format!("https://{}", hostname));
                }
            }
        }
    }

    None
}
