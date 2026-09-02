use crate::models::{Account, TokenData};
use crate::modules;

/// Account service layer - fully decoupled from the Tauri runtime
pub struct AccountService {
    pub integration: crate::modules::integration::SystemManager,
}

impl AccountService {
    pub fn new(integration: crate::modules::integration::SystemManager) -> Self {
        Self { integration }
    }

    /// Add account logic
    pub async fn add_account(&self, refresh_token: &str) -> Result<Account, String> {
        // [FIX #1583] Generate a temporary UUID as the account context, to avoid passing None causing abnormal proxy selection
        let temp_account_id = uuid::Uuid::new_v4().to_string();

        // 1. Get the Token (use the temporary ID to ensure proxy selection has a clear context)
        let token_res =
            modules::oauth::refresh_access_token(refresh_token, Some(&temp_account_id)).await?;

        // 2. Get user information
        let user_info =
            modules::oauth::get_user_info(&token_res.access_token, Some(&temp_account_id)).await?;

        // 3. Get project ID (attempt)
        let project_id = crate::proxy::project_resolver::fetch_project_id(&token_res.access_token)
            .await
            .ok();

        // 4. Construct TokenData
        let token = TokenData::new(
            token_res.access_token.clone(),
            refresh_token.to_string(),
            token_res.expires_in,
            Some(user_info.email.clone()),
            project_id,
            None,
            false, // Personal accounts don't enable GCP TOS by default
            token_res.id_token.clone(),
        )
        .with_oauth_client_key(token_res.oauth_client_key.clone());

        // 5. Persist
        let mut account =
            modules::upsert_account(user_info.email.clone(), user_info.get_display_name(), token)?;

        // 6. [NEW] Automatically fetch quota information (used for sorting by refresh time)
        let email_for_log = account.email.clone();
        let access_token = token_res.access_token.clone();
        match modules::quota::fetch_quota(&access_token, &email_for_log, Some(&account.id)).await {
            Ok((quota_data, new_project_id)) => {
                account.quota = Some(quota_data);
                if let Some(pid) = new_project_id {
                    account.token.project_id = Some(pid);
                }
                // Save the updated account information
                if let Err(e) = modules::account::save_account(&account) {
                    modules::logger::log_warn(&format!(
                        "[Service] Failed to save quota for {}: {}",
                        email_for_log, e
                    ));
                } else {
                    modules::logger::log_info(&format!(
                        "[Service] Fetched quota for new account: {}",
                        email_for_log
                    ));
                }
            }
            Err(e) => {
                modules::logger::log_warn(&format!(
                    "[Service] Failed to fetch quota for {}: {}",
                    email_for_log, e
                ));
            }
        }

        modules::logger::log_info(&format!(
            "[Service] Added/Updated account: {}",
            account.email
        ));
        Ok(account)
    }

    /// Delete account logic
    pub fn delete_account(&self, account_id: &str) -> Result<(), String> {
        modules::delete_account(account_id)?;
        self.integration.update_tray();
        Ok(())
    }

    /// Switch account logic
    pub async fn switch_account(
        &self,
        account_id: &str,
        target_ide: Option<&str>,
    ) -> Result<(), String> {
        modules::account::switch_account(account_id, target_ide, &self.integration).await
    }

    /// Get the list
    pub fn list_accounts(&self) -> Result<Vec<Account>, String> {
        modules::list_accounts()
    }

    /// Get the current ID
    pub fn get_current_id(&self) -> Result<Option<String>, String> {
        modules::get_current_account_id()
    }

    // --- OAuth logic ---

    pub async fn prepare_oauth_url(
        &self,
        oauth_client_key: Option<String>,
    ) -> Result<String, String> {
        let handle = match &self.integration {
            modules::integration::SystemManager::Desktop(h) => Some(h.clone()),
            modules::integration::SystemManager::Headless => None,
        };
        modules::oauth_server::prepare_oauth_url(handle, oauth_client_key).await
    }

    pub async fn start_oauth_login(
        &self,
        oauth_client_key: Option<String>,
    ) -> Result<Account, String> {
        let handle = match &self.integration {
            modules::integration::SystemManager::Desktop(h) => Some(h.clone()),
            modules::integration::SystemManager::Headless => None,
        };
        let token_res = modules::oauth_server::start_oauth_flow(handle, oauth_client_key).await?;
        self.process_oauth_token(token_res).await
    }

    pub async fn complete_oauth_login(&self) -> Result<Account, String> {
        let handle = match &self.integration {
            modules::integration::SystemManager::Desktop(h) => Some(h.clone()),
            modules::integration::SystemManager::Headless => None,
        };
        let token_res = modules::oauth_server::complete_oauth_flow(handle).await?;
        self.process_oauth_token(token_res).await
    }

    pub fn cancel_oauth_login(&self) {
        modules::oauth_server::cancel_oauth_flow();
    }

    pub async fn submit_oauth_code(
        &self,
        code: String,
        state: Option<String>,
    ) -> Result<(), String> {
        modules::oauth_server::submit_oauth_code(code, state).await
    }

    async fn process_oauth_token(
        &self,
        token_res: modules::oauth::TokenResponse,
    ) -> Result<Account, String> {
        let refresh_token = token_res
            .refresh_token
            .ok_or_else(|| "Refresh Token not obtained. Please revoke permissions and try again.".to_string())?;

        // [FIX #1583] Generate a temporary UUID as the account context
        let temp_account_id = uuid::Uuid::new_v4().to_string();

        let user_info =
            modules::oauth::get_user_info(&token_res.access_token, Some(&temp_account_id)).await?;
        let project_id = crate::proxy::project_resolver::fetch_project_id(&token_res.access_token)
            .await
            .ok();

        let token_data = crate::models::TokenData::new(
            token_res.access_token,
            refresh_token,
            token_res.expires_in,
            Some(user_info.email.clone()),
            project_id,
            None,
            false, // Not enabled by default, adjusted by subsequent logic or the user manually
            token_res.id_token,
        )
        .with_oauth_client_key(token_res.oauth_client_key.clone());

        let account = modules::upsert_account(
            user_info.email.clone(),
            user_info.get_display_name(),
            token_data,
        )?;

        // Send UI update notification (via integration)
        self.integration.update_tray();

        Ok(account)
    }
}
