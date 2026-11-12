//! Local request budgets and the durable, explicitly acknowledged emergency stop.
//! These limits are operator policy, not estimates of provider-safe usage or billing.
use axum::{
    http::StatusCode,
    response::{IntoResponse, Response},
    Json,
};
use once_cell::sync::Lazy;
use rusqlite::{params, Connection, OptionalExtension, TransactionBehavior};
use serde::{Deserialize, Serialize};
use std::{
    collections::HashMap,
    path::{Path, PathBuf},
    sync::{
        atomic::{AtomicBool, Ordering},
        Arc, Mutex,
    },
};

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct AccountSafetyConfig {
    pub halt_on_account_restriction: bool,
    pub halt_on_authentication_challenge: bool,
    pub client_requests_per_minute: u32,
    pub client_requests_per_day: u32,
    pub upstream_requests_per_minute: u32,
    pub upstream_requests_per_day: u32,
}

impl Default for AccountSafetyConfig {
    fn default() -> Self {
        Self {
            halt_on_account_restriction: true,
            halt_on_authentication_challenge: true,
            client_requests_per_minute: 10,
            client_requests_per_day: 0,
            upstream_requests_per_minute: 20,
            upstream_requests_per_day: 500,
        }
    }
}

impl AccountSafetyConfig {
    pub fn effective(mut self) -> Self {
        for (name, target) in [
            (
                "ANTIGRAVITY_CLIENT_REQUESTS_PER_MINUTE",
                &mut self.client_requests_per_minute,
            ),
            (
                "ANTIGRAVITY_CLIENT_REQUESTS_PER_DAY",
                &mut self.client_requests_per_day,
            ),
            (
                "ANTIGRAVITY_UPSTREAM_REQUESTS_PER_MINUTE",
                &mut self.upstream_requests_per_minute,
            ),
            (
                "ANTIGRAVITY_UPSTREAM_REQUESTS_PER_DAY",
                &mut self.upstream_requests_per_day,
            ),
        ] {
            if let Ok(value) = std::env::var(name) {
                if let Ok(value) = value.parse() {
                    *target = value;
                }
            }
        }
        for (name, target) in [
            (
                "ANTIGRAVITY_HALT_ON_ACCOUNT_RESTRICTION",
                &mut self.halt_on_account_restriction,
            ),
            (
                "ANTIGRAVITY_HALT_ON_AUTHENTICATION_CHALLENGE",
                &mut self.halt_on_authentication_challenge,
            ),
        ] {
            if let Ok(value) = std::env::var(name) {
                match value.as_str() {
                    "true" | "1" => *target = true,
                    "false" | "0" => *target = false,
                    _ => {}
                }
            }
        }
        self
    }
}

#[derive(Debug, Clone, Serialize)]
pub struct SafetyStatus {
    pub halted: bool,
    pub generation: u64,
    pub reason: Option<String>,
    pub halted_at: Option<i64>,
    pub upstream_requests_today: u64,
    pub config: AccountSafetyConfig,
}

#[derive(Debug, Clone)]
pub struct AdmissionError {
    pub code: &'static str,
    pub retry_after: u64,
}
impl AdmissionError {
    pub fn unavailable() -> Self {
        Self {
            code: "SAFETY_STATE_UNAVAILABLE",
            retry_after: 0,
        }
    }
    fn halted() -> Self {
        Self {
            code: "GATEWAY_EMERGENCY_STOP",
            retry_after: 0,
        }
    }
    pub fn body(&self) -> serde_json::Value {
        serde_json::json!({"error":{"type": if self.retry_after > 0 {"gateway_request_limit"} else {"account_attention_required"}, "code": self.code,
            "message": if self.retry_after > 0 {"The local gateway request budget is exhausted. Retry after the indicated delay."} else {"Gateway dispatch is paused. Review Account safety and explicitly acknowledge the stop before resuming."},
            "retry_after": self.retry_after, "manual_recovery_required": self.retry_after == 0}})
    }
    pub fn response(&self) -> Response {
        let mut response = (
            if self.retry_after > 0 {
                StatusCode::TOO_MANY_REQUESTS
            } else {
                StatusCode::BAD_REQUEST
            },
            Json(self.body()),
        )
            .into_response();
        response
            .headers_mut()
            .insert("Cache-Control", "no-store".parse().unwrap());
        if self.retry_after > 0 {
            response
                .headers_mut()
                .insert("Retry-After", self.retry_after.to_string().parse().unwrap());
        }
        response
    }
    pub fn encoded(&self) -> String {
        self.body().to_string()
    }
}

pub struct SafetyController {
    db: Mutex<Connection>,
    control: Mutex<()>,
    config: std::sync::RwLock<AccountSafetyConfig>,
    // Raised before waiting for durable storage. A failed write must never reopen dispatch.
    memory_stop: AtomicBool,
    persisted_stop: AtomicBool,
}

pub struct SafetyPermit {
    controller: Arc<SafetyController>,
    generation: u64,
}
impl SafetyPermit {
    pub fn check(&self) -> Result<(), AdmissionError> {
        self.controller.check(Some(self.generation))
    }
    pub fn outbound(&self) -> Result<(), AdmissionError> {
        self.controller
            .outbound(Some(self.generation), chrono::Utc::now().timestamp())
    }
}

static CONTROLLERS: Lazy<Mutex<HashMap<PathBuf, Arc<SafetyController>>>> =
    Lazy::new(|| Mutex::new(HashMap::new()));

impl SafetyController {
    pub fn permit(self: &Arc<Self>) -> Result<Arc<SafetyPermit>, AdmissionError> {
        self.check(None)?;
        let status = self.status().map_err(|_| AdmissionError::unavailable())?;
        if status.halted {
            return Err(AdmissionError::halted());
        }
        Ok(Arc::new(SafetyPermit {
            controller: self.clone(),
            generation: status.generation,
        }))
    }
    pub fn for_dir(dir: &Path) -> Result<Arc<Self>, String> {
        let mut registry = CONTROLLERS
            .lock()
            .map_err(|_| "Safety controller registry unavailable")?;
        registry.retain(|_, controller| {
            Arc::strong_count(controller) > 1
                || (controller.memory_stop.load(Ordering::Acquire)
                    && !controller.persisted_stop.load(Ordering::Acquire))
        });
        if let Some(controller) = registry.get(dir) {
            return Ok(controller.clone());
        }
        if registry.len() >= 64 {
            return Err("Safety controller capacity reached; dispatch remains disabled".into());
        }
        std::fs::create_dir_all(dir).map_err(|_| "Cannot create safety state directory")?;
        let controller = Arc::new(Self::open(&dir.join("gateway_safety.db"))?);
        registry.insert(dir.to_path_buf(), controller.clone());
        Ok(controller)
    }
    fn open(path: &Path) -> Result<Self, String> {
        let db = Connection::open(path).map_err(|_| "Cannot open gateway safety state")?;
        db.busy_timeout(std::time::Duration::from_secs(2))
            .map_err(|_| "Cannot configure safety storage")?;
        db.execute_batch("PRAGMA journal_mode=WAL; PRAGMA synchronous=FULL;
            CREATE TABLE IF NOT EXISTS safety (id INTEGER PRIMARY KEY CHECK(id=1), halted INTEGER NOT NULL, generation INTEGER NOT NULL, reason TEXT, halted_at INTEGER, clock INTEGER NOT NULL);
            INSERT OR IGNORE INTO safety VALUES(1,0,0,NULL,NULL,0);
            CREATE TABLE IF NOT EXISTS daily (scope TEXT PRIMARY KEY, day INTEGER NOT NULL, count INTEGER NOT NULL);
            CREATE TABLE IF NOT EXISTS rate (scope TEXT NOT NULL, second INTEGER NOT NULL, count INTEGER NOT NULL, PRIMARY KEY(scope,second));").map_err(|_| "Cannot initialize gateway safety state")?;
        Ok(Self {
            db: Mutex::new(db),
            control: Mutex::new(()),
            config: std::sync::RwLock::new(AccountSafetyConfig::default().effective()),
            memory_stop: AtomicBool::new(false),
            persisted_stop: AtomicBool::new(false),
        })
    }
    pub fn configure(&self, config: AccountSafetyConfig) {
        if let Ok(mut current) = self.config.write() {
            *current = config.effective();
        }
    }
    pub fn config(&self) -> Result<AccountSafetyConfig, AdmissionError> {
        self.config
            .read()
            .map(|config| config.clone())
            .map_err(|_| AdmissionError::unavailable())
    }
    pub fn status(&self) -> Result<SafetyStatus, String> {
        let db = self.db.lock().map_err(|_| "Safety state unavailable")?;
        let (halted, generation, reason, halted_at, clock): (
            bool,
            u64,
            Option<String>,
            Option<i64>,
            i64,
        ) = db
            .query_row(
                "SELECT halted,generation,reason,halted_at,clock FROM safety WHERE id=1",
                [],
                |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?, r.get(4)?)),
            )
            .map_err(|_| "Cannot read safety state")?;
        let today = chrono::Utc::now().timestamp().max(clock).div_euclid(86400);
        let count = db
            .query_row(
                "SELECT count FROM daily WHERE scope='global' AND day=?1",
                [today],
                |r| r.get::<_, u64>(0),
            )
            .optional()
            .map_err(|_| "Cannot read safety counters")?
            .unwrap_or(0);
        Ok(SafetyStatus {
            halted: halted || self.memory_stop.load(Ordering::Acquire),
            generation,
            reason,
            halted_at,
            upstream_requests_today: count,
            config: self.config().map_err(|_| "Safety config unavailable")?,
        })
    }
    pub fn halt(&self, reason: &'static str) -> Result<(), String> {
        let _control = self
            .control
            .lock()
            .map_err(|_| "Safety controls unavailable")?;
        self.memory_stop.store(true, Ordering::Release);
        self.persisted_stop.store(false, Ordering::Release);
        let db = self
            .db
            .lock()
            .map_err(|_| "Safety state unavailable; dispatch remains stopped")?;
        db.execute(
            "UPDATE safety SET halted=1,generation=generation+1,reason=?1,halted_at=?2 WHERE id=1",
            params![reason, chrono::Utc::now().timestamp()],
        )
        .map_err(|_| "Cannot persist emergency stop; dispatch remains stopped")?;
        self.persisted_stop.store(true, Ordering::Release);
        Ok(())
    }
    pub fn observe_attention(
        &self,
        attention: super::account_safety::AccountAttention,
    ) -> Result<(), String> {
        let config = self.config().map_err(|_| "Safety config unavailable")?;
        use super::account_safety::AccountAttention::*;
        if matches!(attention, AccountRestricted) && config.halt_on_account_restriction
            || matches!(attention, AuthenticationChallenge)
                && config.halt_on_authentication_challenge
        {
            self.halt(attention.code())?;
        }
        Ok(())
    }
    pub fn resume(
        &self,
        expected_generation: u64,
        acknowledged: bool,
    ) -> Result<SafetyStatus, String> {
        if !acknowledged {
            return Err("Explicit acknowledgement is required".into());
        }
        let _control = self
            .control
            .lock()
            .map_err(|_| "Safety controls unavailable")?;
        {
            let db = self.db.lock().map_err(|_| "Safety state unavailable")?;
            let changed = db.execute("UPDATE safety SET halted=0,generation=generation+1,reason=NULL,halted_at=NULL WHERE id=1 AND generation=?1", [expected_generation]).map_err(|_| "Cannot persist explicit resume")?;
            if changed != 1 {
                return Err(
                    "A newer safety event occurred; review current status before resuming".into(),
                );
            }
            // Under the same lock as halt's write, so an older acknowledgement cannot clear a new halt.
            self.memory_stop.store(false, Ordering::Release);
        }
        self.status()
    }
    pub fn check(&self, generation: Option<u64>) -> Result<(), AdmissionError> {
        if self.memory_stop.load(Ordering::Acquire) {
            return Err(AdmissionError::halted());
        }
        let db = self.db.lock().map_err(|_| AdmissionError::unavailable())?;
        let (halted, current): (bool, u64) = db
            .query_row("SELECT halted,generation FROM safety WHERE id=1", [], |r| {
                Ok((r.get(0)?, r.get(1)?))
            })
            .map_err(|_| AdmissionError::unavailable())?;
        if halted || generation.is_some_and(|generation| generation != current) {
            return Err(AdmissionError::halted());
        }
        Ok(())
    }
    pub fn admit(
        self: &Arc<Self>,
        client_id: &str,
        now: i64,
    ) -> Result<Arc<SafetyPermit>, AdmissionError> {
        let config = self.config()?;
        let generation = self.charge(
            &format!("client:{client_id}"),
            config.client_requests_per_minute,
            config.client_requests_per_day,
            None,
            now,
        )?;
        Ok(Arc::new(SafetyPermit {
            controller: self.clone(),
            generation,
        }))
    }
    pub fn outbound(&self, generation: Option<u64>, now: i64) -> Result<(), AdmissionError> {
        let config = self.config()?;
        self.charge(
            "global",
            config.upstream_requests_per_minute,
            config.upstream_requests_per_day,
            generation,
            now,
        )
        .map(|_| ())
    }
    fn charge(
        &self,
        scope: &str,
        minute_limit: u32,
        daily_limit: u32,
        generation: Option<u64>,
        now: i64,
    ) -> Result<u64, AdmissionError> {
        if self.memory_stop.load(Ordering::Acquire) {
            return Err(AdmissionError::halted());
        }
        let mut db = self.db.lock().map_err(|_| AdmissionError::unavailable())?;
        let tx = db
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .map_err(|_| AdmissionError::unavailable())?;
        let (halted, current, clock): (bool, u64, i64) = tx
            .query_row(
                "SELECT halted,generation,clock FROM safety WHERE id=1",
                [],
                |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
            )
            .map_err(|_| AdmissionError::unavailable())?;
        if halted
            || generation.is_some_and(|generation| generation != current)
            || self.memory_stop.load(Ordering::Acquire)
        {
            return Err(AdmissionError::halted());
        }
        let now = now.max(clock);
        let day = now.div_euclid(86400);
        tx.execute("DELETE FROM rate WHERE second<=?1", [now - 60])
            .map_err(|_| AdmissionError::unavailable())?;
        tx.execute("DELETE FROM daily WHERE day<?1", [day])
            .map_err(|_| AdmissionError::unavailable())?;
        let count = tx
            .query_row(
                "SELECT count FROM daily WHERE scope=?1 AND day=?2",
                params![scope, day],
                |r| r.get::<_, u64>(0),
            )
            .optional()
            .map_err(|_| AdmissionError::unavailable())?
            .unwrap_or(0);
        if daily_limit > 0 && count >= daily_limit as u64 {
            return Err(AdmissionError {
                code: "DAILY_REQUEST_BUDGET",
                retry_after: ((day + 1) * 86400 - now).max(1) as u64,
            });
        }
        let (rate, oldest): (u64, Option<i64>) = tx
            .query_row(
                "SELECT COALESCE(SUM(count),0),MIN(second) FROM rate WHERE scope=?1 AND second>?2",
                params![scope, now - 60],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .map_err(|_| AdmissionError::unavailable())?;
        if minute_limit > 0 && rate >= minute_limit as u64 {
            return Err(AdmissionError {
                code: "REQUEST_RATE_LIMIT",
                retry_after: (oldest.unwrap_or(now) + 60 - now).max(1) as u64,
            });
        }
        // Bound retention even if an operator disables both request limits.
        if count == 0 {
            let scopes: u64 = tx
                .query_row("SELECT COUNT(*) FROM daily", [], |r| r.get(0))
                .map_err(|_| AdmissionError::unavailable())?;
            if scopes >= 10000 {
                return Err(AdmissionError {
                    code: "CLIENT_CAPACITY_LIMIT",
                    retry_after: ((day + 1) * 86400 - now).max(1) as u64,
                });
            }
        }
        tx.execute("INSERT INTO daily VALUES(?1,?2,1) ON CONFLICT(scope) DO UPDATE SET day=excluded.day,count=CASE WHEN daily.day=excluded.day THEN daily.count+1 ELSE 1 END", params![scope,day]).map_err(|_| AdmissionError::unavailable())?;
        tx.execute("INSERT INTO rate VALUES(?1,?2,1) ON CONFLICT(scope,second) DO UPDATE SET count=count+1", params![scope,now]).map_err(|_| AdmissionError::unavailable())?;
        tx.execute("UPDATE safety SET clock=?1 WHERE id=1", [now])
            .map_err(|_| AdmissionError::unavailable())?;
        tx.commit().map_err(|_| AdmissionError::unavailable())?;
        Ok(current)
    }
}

pub fn is_model_call(method: &str) -> bool {
    matches!(method, "generateContent" | "streamGenerateContent")
}

pub fn error_from_encoded(error: &str) -> Option<AdmissionError> {
    let value: serde_json::Value = serde_json::from_str(error).ok()?;
    let code = match value.pointer("/error/code")?.as_str()? {
        "GATEWAY_EMERGENCY_STOP" => "GATEWAY_EMERGENCY_STOP",
        "SAFETY_STATE_UNAVAILABLE" => "SAFETY_STATE_UNAVAILABLE",
        "REQUEST_RATE_LIMIT" => "REQUEST_RATE_LIMIT",
        "DAILY_REQUEST_BUDGET" => "DAILY_REQUEST_BUDGET",
        "CLIENT_CAPACITY_LIMIT" => "CLIENT_CAPACITY_LIMIT",
        _ => return None,
    };
    Some(AdmissionError {
        code,
        retry_after: value
            .pointer("/error/retry_after")
            .and_then(serde_json::Value::as_u64)
            .unwrap_or(0),
    })
}

pub fn response_from_encoded(error: &str) -> Option<Response> {
    error_from_encoded(error).map(|error| error.response())
}

pub fn global_controller() -> Result<Arc<SafetyController>, String> {
    let dir = crate::modules::account::get_data_dir()?;
    let controller = SafetyController::for_dir(&dir)?;
    controller.configure(load_saved_policy(&dir)?);
    Ok(controller)
}

/// Read only nonsecret safety policy; status and emergency controls must not
/// depend on hydrating unrelated API keys from an unlocked native store.
fn load_saved_policy(dir: &Path) -> Result<AccountSafetyConfig, String> {
    #[derive(Deserialize, Default)]
    struct ProxyPolicy {
        #[serde(default)]
        account_safety: AccountSafetyConfig,
    }
    #[derive(Deserialize)]
    struct Document {
        #[serde(default)]
        proxy: ProxyPolicy,
    }
    let file = match std::fs::File::open(dir.join("gui_config.json")) {
        Ok(file) => file,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            return Ok(AccountSafetyConfig::default())
        }
        Err(_) => return Err("Cannot read gateway safety policy".into()),
    };
    serde_json::from_reader::<_, Document>(std::io::BufReader::new(file))
        .map(|document| document.proxy.account_safety)
        .map_err(|_| "Invalid gateway safety policy".into())
}

/// Background callers fail closed; explicit admin recovery uses separate entrypoints.
pub fn automatic_activity_allowed() -> bool {
    global_controller().is_ok_and(|controller| controller.check(None).is_ok())
}

pub fn automatic_activity_permit() -> Result<Arc<SafetyPermit>, String> {
    global_controller()?
        .permit()
        .map_err(|error| error.encoded())
}

pub fn observe_global_attention(
    attention: super::account_safety::AccountAttention,
) -> Result<(), String> {
    global_controller()?.observe_attention(attention)
}

pub fn check_current_activity() -> Result<(), String> {
    if let Some(permit) = super::budget::current().and_then(|context| context.safety()) {
        permit.check().map_err(|error| error.encoded())?;
    }
    Ok(())
}

pub async fn automatic_context() -> Result<Arc<super::budget::RequestContext>, String> {
    if let Some(context) = super::budget::current().filter(|context| context.safety().is_some()) {
        check_current_activity()?;
        return Ok(context);
    }
    let permit = tokio::task::spawn_blocking(automatic_activity_permit)
        .await
        .map_err(|_| AdmissionError::unavailable().encoded())??;
    let context = super::budget::RequestContext::new(super::budget::deadline_now());
    context.set_safety(permit);
    Ok(context)
}

pub async fn run_automatic<T, F>(future: F) -> Result<T, String>
where
    F: std::future::Future<Output = Result<T, String>>,
{
    let context = automatic_context().await?;
    super::budget::scope(context, future).await
}

pub fn spawn_scoped<F>(future: F) -> tokio::task::JoinHandle<F::Output>
where
    F: std::future::Future + Send + 'static,
    F::Output: Send + 'static,
{
    tokio::spawn(super::budget::scope_optional(
        super::budget::current(),
        future,
    ))
}

#[cfg(test)]
mod tests {
    use super::*;
    fn controller() -> (tempfile::TempDir, Arc<SafetyController>) {
        let dir = tempfile::tempdir().unwrap();
        let controller =
            Arc::new(SafetyController::open(&dir.path().join("gateway_safety.db")).unwrap());
        *controller.config.write().unwrap() = AccountSafetyConfig {
            client_requests_per_minute: 2,
            client_requests_per_day: 3,
            upstream_requests_per_minute: 2,
            upstream_requests_per_day: 4,
            ..Default::default()
        };
        (dir, controller)
    }

    #[test]
    fn policy_read_ignores_unavailable_secret_references_and_rejects_malformed_policy() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("gui_config.json"),serde_json::to_vec(&serde_json::json!({"proxy":{"api_key":"","secret_store":{"provider":"unavailable-test-only","entry":"missing"},"account_safety":{"client_requests_per_minute":30,"client_requests_per_day":0,"upstream_requests_per_minute":60,"upstream_requests_per_day":2000}}})).unwrap()).unwrap();
        let policy = load_saved_policy(dir.path()).unwrap();
        assert_eq!(policy.client_requests_per_minute, 30);
        assert_eq!(policy.upstream_requests_per_day, 2000);
        std::fs::write(
            dir.path().join("gui_config.json"),
            b"{\"proxy\":{\"account_safety\":{\"upstream_requests_per_day\":\"bad\"}}}",
        )
        .unwrap();
        assert!(load_saved_policy(dir.path()).is_err());
    }
    #[test]
    fn device_budgets_are_separate_from_actual_outbound_attempts() {
        let (_dir, safety) = controller();
        let now = 86400 + 5;
        let permit = safety.admit("device-a", now).unwrap();
        safety.admit("device-a", now).unwrap();
        assert_eq!(safety.admit("device-a", now).err().unwrap().retry_after, 60);
        safety.admit("device-b", now).unwrap();
        // One incoming image request may fan out; every physical call is charged.
        safety.outbound(Some(permit.generation), now).unwrap();
        safety.outbound(Some(permit.generation), now).unwrap();
        assert_eq!(
            safety
                .outbound(Some(permit.generation), now)
                .err()
                .unwrap()
                .code,
            "REQUEST_RATE_LIMIT"
        );
        safety.outbound(Some(permit.generation), now + 60).unwrap();
        safety.outbound(Some(permit.generation), now + 60).unwrap();
        assert_eq!(
            safety
                .outbound(Some(permit.generation), now + 120)
                .err()
                .unwrap()
                .code,
            "DAILY_REQUEST_BUDGET"
        );
        safety.admit("device-a", now + 60).unwrap();
        assert_eq!(
            safety.admit("device-a", now + 120).err().unwrap().code,
            "DAILY_REQUEST_BUDGET"
        );
        safety.outbound(None, 2 * 86400).unwrap();
        assert!(is_model_call("generateContent") && is_model_call("streamGenerateContent"));
        assert!(
            !is_model_call("fetchAvailableModels")
                && !is_model_call("countTokens")
                && !is_model_call("loadCodeAssist")
        );
    }
    #[test]
    fn simultaneous_fanout_cannot_overspend_daily_budget() {
        let (_dir, safety) = controller();
        {
            let mut config = safety.config.write().unwrap();
            config.upstream_requests_per_day = 7;
            config.upstream_requests_per_minute = 0;
        }
        let barrier = Arc::new(std::sync::Barrier::new(32));
        let tasks: Vec<_> = (0..32)
            .map(|_| {
                let safety = safety.clone();
                let barrier = barrier.clone();
                std::thread::spawn(move || {
                    barrier.wait();
                    safety.outbound(None, 100000).is_ok()
                })
            })
            .collect();
        assert_eq!(
            tasks
                .into_iter()
                .map(|task| task.join().unwrap())
                .filter(|admitted| *admitted)
                .count(),
            7
        );
    }
    #[test]
    fn persistence_survives_restart_and_clock_rollback() {
        let (dir, safety) = controller();
        safety.outbound(None, 100000).unwrap();
        safety.outbound(None, 100000).unwrap();
        drop(safety);
        let restarted = SafetyController::open(&dir.path().join("gateway_safety.db")).unwrap();
        restarted
            .config
            .write()
            .unwrap()
            .upstream_requests_per_minute = 2;
        assert_eq!(
            restarted.outbound(None, 1).err().unwrap().code,
            "REQUEST_RATE_LIMIT"
        );
        assert_eq!(
            restarted.outbound(None, 100059).err().unwrap().retry_after,
            1
        );
        assert!(restarted.outbound(None, 100060).is_ok());
    }
    #[test]
    fn halt_resume_never_revives_old_work_or_accepts_stale_acknowledgement() {
        use super::super::account_safety::AccountAttention::*;
        let (dir, safety) = controller();
        let permit = safety.admit("shared-gateway", 100000).unwrap();
        safety.observe_attention(AccountRestricted).unwrap();
        let generation = safety.status().unwrap().generation;
        assert!(permit.check().is_err());
        assert!(safety.resume(generation, false).is_err());
        safety.observe_attention(AuthenticationChallenge).unwrap();
        assert!(safety.resume(generation, true).is_err());
        let current = safety.status().unwrap().generation;
        safety.resume(current, true).unwrap();
        assert!(permit.check().is_err());
        assert!(safety.permit().unwrap().check().is_ok());
        safety.halt("OPERATOR_STOP").unwrap();
        drop(permit);
        drop(safety);
        let restarted = SafetyController::open(&dir.path().join("gateway_safety.db")).unwrap();
        assert!(restarted.check(None).is_err());
        assert!(restarted.status().unwrap().halted);
    }
    #[test]
    fn failed_halt_write_stays_denied_until_explicit_resume_and_invalidates_old_work() {
        let (_dir, safety) = controller();
        let permit = safety.permit().unwrap();
        safety.db.lock().unwrap().execute_batch("CREATE TRIGGER fail_halt BEFORE UPDATE OF halted ON safety WHEN NEW.halted=1 BEGIN SELECT RAISE(FAIL,'synthetic failure'); END;").unwrap();
        assert!(safety.halt("OPERATOR_STOP").is_err());
        assert!(safety.check(None).is_err());
        assert!(safety.status().unwrap().halted);
        safety
            .db
            .lock()
            .unwrap()
            .execute_batch("DROP TRIGGER fail_halt;")
            .unwrap();
        safety
            .resume(safety.status().unwrap().generation, true)
            .unwrap();
        assert!(permit.check().is_err());
    }

    #[test]
    fn failed_halt_latch_survives_last_request_and_controller_drop() {
        let dir = tempfile::tempdir().unwrap();
        let safety = SafetyController::for_dir(dir.path()).unwrap();
        let permit = safety.permit().unwrap();
        safety.db.lock().unwrap().execute_batch("CREATE TRIGGER fail_halt BEFORE UPDATE OF halted ON safety WHEN NEW.halted=1 BEGIN SELECT RAISE(FAIL,'synthetic failure'); END;").unwrap();
        assert!(safety.halt("OPERATOR_STOP").is_err());
        drop(permit);
        drop(safety);
        let reopened = SafetyController::for_dir(dir.path()).unwrap();
        assert!(reopened.check(None).is_err());
        assert!(reopened.admit("shared-gateway", 100000).is_err());
        reopened
            .db
            .lock()
            .unwrap()
            .execute_batch("DROP TRIGGER fail_halt;")
            .unwrap();
        reopened
            .resume(reopened.status().unwrap().generation, true)
            .unwrap();
        assert!(reopened.permit().unwrap().check().is_ok());
    }
    #[test]
    fn policy_changes_and_ordinary_rate_limits_do_not_resume_a_halt() {
        let (_dir, safety) = controller();
        safety.outbound(None, 100000).unwrap();
        safety.outbound(None, 100000).unwrap();
        assert!(!safety.status().unwrap().halted);
        assert!(safety.outbound(None, 100000).is_err());
        safety.halt("OPERATOR_STOP").unwrap();
        safety.configure(AccountSafetyConfig {
            halt_on_account_restriction: false,
            halt_on_authentication_challenge: false,
            ..Default::default()
        });
        assert!(safety.check(None).is_err());
    }
    #[test]
    fn independent_connections_share_atomic_counters() {
        let (dir, safety) = controller();
        let second = SafetyController::open(&dir.path().join("gateway_safety.db")).unwrap();
        second.config.write().unwrap().upstream_requests_per_minute = 2;
        safety.outbound(None, 100000).unwrap();
        second.outbound(None, 100000).unwrap();
        assert!(safety.outbound(None, 100000).is_err());
        assert!(second.outbound(None, 100000).is_err());
    }
    #[tokio::test]
    async fn local_limit_is_429_with_retry_after_and_terminal_in_every_schedule() {
        let error = AdmissionError {
            code: "DAILY_REQUEST_BUDGET",
            retry_after: 321,
        };
        let response = error.response();
        assert_eq!(response.status(), StatusCode::TOO_MANY_REQUESTS);
        assert_eq!(response.headers()["retry-after"], "321");
        let terminal = super::super::terminal::classify(&error.encoded()).unwrap();
        for mode in [
            crate::proxy::sticky_config::SchedulingMode::CacheFirst,
            crate::proxy::sticky_config::SchedulingMode::Balance,
            crate::proxy::sticky_config::SchedulingMode::PerformanceFirst,
        ] {
            let disposition = terminal.disposition();
            assert!(!disposition.penalize_account && !disposition.cross_account_replay_allowed);
            assert_eq!(
                disposition.plan(mode, true, true).next,
                super::super::terminal::TerminalNextStep::Stop
            );
        }
    }

    #[tokio::test]
    async fn image_worker_scope_preserves_stop_before_oauth_or_token_acquisition() {
        let (_dir, safety) = controller();
        let parent =
            super::super::budget::RequestContext::new(super::super::budget::deadline_now());
        parent.set_safety(safety.permit().unwrap());
        let child = parent.fork();
        let token = crate::models::TokenData::new(
            "synthetic-valid-access".into(),
            "synthetic-refresh".into(),
            3600,
            None,
            None,
            None,
            false,
            None,
        );
        // A fresh synthetic token cannot cause a network request even if a
        // regression accidentally loses the scope; it would wrongly return Ok.
        assert!(crate::modules::oauth::ensure_fresh_token(&token, None)
            .await
            .is_ok());
        safety.halt("OPERATOR_STOP").unwrap();
        safety
            .resume(safety.status().unwrap().generation, true)
            .unwrap();
        let error = tokio::spawn(super::super::budget::scope_optional(
            Some(child),
            async move {
                assert!(super::super::budget::current()
                    .and_then(|context| context.safety())
                    .is_some());
                crate::modules::oauth::ensure_fresh_token(&token, None)
                    .await
                    .expect_err("old queued image work cannot use even a fresh token")
            },
        ))
        .await
        .unwrap();
        assert_eq!(
            error_from_encoded(&error).unwrap().code,
            "GATEWAY_EMERGENCY_STOP"
        );
    }

    #[tokio::test]
    async fn continuation_limits_survive_real_protocol_mappers_and_collectors() {
        use bytes::Bytes;
        use futures::StreamExt;
        let encoded = AdmissionError {
            code: "DAILY_REQUEST_BUDGET",
            retry_after: 321,
        }
        .encoded();
        for protocol in ["claude", "chat", "responses"] {
            let upstream=Box::pin(futures::stream::iter(vec![
                Ok::<_,String>(Bytes::from_static(b"data: {\"candidates\":[{\"content\":{\"role\":\"model\",\"parts\":[{\"text\":\"Partial content\"}]}}]}\n\n")),
                Err(encoded.clone()),
            ]));
            let converted = match protocol {
                "claude" => crate::proxy::mappers::claude::create_claude_sse_stream(
                    upstream,
                    "limit-test".into(),
                    "account@example.invalid".into(),
                    None,
                    false,
                    200000,
                    Some(10),
                    0,
                    None,
                    vec![],
                ),
                "chat" => {
                    crate::proxy::mappers::openai::streaming::create_openai_sse_stream_with_anchor(
                        upstream,
                        "test-model".into(),
                        "limit-test".into(),
                        0,
                        None,
                        false,
                        None,
                    )
                }
                _ => crate::proxy::mappers::openai::streaming::create_codex_sse_stream(
                    upstream,
                    "test-model".into(),
                    "limit-test".into(),
                    0,
                    0,
                    "resp-limit-test".into(),
                    None,
                    false,
                    false,
                ),
            };
            if protocol == "responses" {
                let wire = converted
                    .map(|item| item.unwrap())
                    .collect::<Vec<_>>()
                    .await
                    .concat();
                let text = String::from_utf8(wire).unwrap();
                assert!(text.contains("response.failed") && !text.contains("response.completed"));
                let response = super::super::account_safety::account_attention_stream_response(
                    text.as_bytes(),
                )
                .expect("typed limit survives Responses envelope");
                assert_eq!(response.status(), StatusCode::TOO_MANY_REQUESTS);
                assert_eq!(response.headers()["retry-after"], "321");
            } else {
                let error = if protocol == "claude" {
                    crate::proxy::mappers::claude::collect_stream_to_json(
                        converted.map(|item| item.map_err(std::io::Error::other)),
                    )
                    .await
                    .map(|_| ())
                    .expect_err("must not return partial success")
                } else {
                    crate::proxy::mappers::openai::collector::collect_stream_to_json(converted)
                        .await
                        .map(|_| ())
                        .expect_err("must not return partial success")
                };
                let response =
                    super::super::account_safety::account_attention_error_response(&error).unwrap();
                assert_eq!(
                    response.status(),
                    StatusCode::TOO_MANY_REQUESTS,
                    "{protocol}: {error}"
                );
                assert_eq!(response.headers()["retry-after"], "321");
            }
        }
    }
}
