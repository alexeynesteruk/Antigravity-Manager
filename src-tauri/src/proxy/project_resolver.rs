use serde_json::Value;

/// Use Antigravity's loadCodeAssist API to get the project_id
/// This is the correct way to obtain cloudaicompanionProject
pub async fn fetch_project_id(access_token: &str) -> Result<String, String> {
    // Use the Sandbox environment to avoid 429 errors from the Prod environment
    let url = "https://daily-cloudcode-pa.sandbox.googleapis.com/v1internal:loadCodeAssist";

    let request_body = serde_json::json!({
        "metadata": {
            "ideType": "ANTIGRAVITY"
        }
    });

    let client = crate::utils::http::get_client();
    let response = client
        .post(url)
        .bearer_auth(access_token)
        // .header("Host", "cloudcode-pa.googleapis.com") // Host header removed since the domain has been switched
        .header("User-Agent", crate::constants::USER_AGENT.as_str())
        .header("Content-Type", "application/json")
        .json(&request_body)
        .send()
        .await
        .map_err(|e| format!("loadCodeAssist request failed: {}", e))?;

    if !response.status().is_success() {
        let status = response.status();
        let body = response.text().await.unwrap_or_default();
        return Err(format!("loadCodeAssist returned an error {}: {}", status, body));
    }

    let data: Value = response
        .json()
        .await
        .map_err(|e| format!("Failed to parse response: {}", e))?;

    // Extract cloudaicompanionProject
    if let Some(project_id) = data.get("cloudaicompanionProject").and_then(|v| v.as_str()) {
        return Ok(project_id.to_string());
    }

    // If project_id was not returned, the account is ineligible; return an error to trigger token_manager's stable fallback logic
    Err("Account is not eligible for the official cloudaicompanionProject".to_string())
}
