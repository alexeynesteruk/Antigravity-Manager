//! Bounded image-output admission and lazy worker creation, shared by adapters.

use crate::proxy::config::ImageSchedulerConfig;
use serde::Deserialize;
use serde_json::Value;
use std::{future::Future, num::NonZeroUsize, path::Path};
use tokio::task::{JoinError, JoinSet};

pub const MAX_OUTPUTS_CEILING: usize = 100;
pub const MAX_PARALLEL_CEILING: usize = 16;

#[derive(Clone, Copy, Debug)]
pub struct ImageOutputPolicy {
    max_outputs: NonZeroUsize,
    parallel: NonZeroUsize,
}

impl ImageOutputPolicy {
    fn new(max_outputs: usize, parallel: usize) -> Result<Self, String> {
        if !(1..=MAX_OUTPUTS_CEILING).contains(&max_outputs) {
            return Err(format!("image_scheduler.max_outputs_per_request must be between 1 and {MAX_OUTPUTS_CEILING}"));
        }
        if !(1..=MAX_PARALLEL_CEILING).contains(&parallel) || parallel > max_outputs {
            return Err(format!("image_scheduler.max_parallel_outputs must be between 1 and {MAX_PARALLEL_CEILING} and no greater than max_outputs_per_request"));
        }
        Ok(Self {
            max_outputs: NonZeroUsize::new(max_outputs).unwrap(),
            parallel: NonZeroUsize::new(parallel).unwrap(),
        })
    }

    fn checked_count(self, count: u64) -> Result<usize, String> {
        let count = usize::try_from(count).map_err(|_| self.count_error())?;
        if count == 0 || count > self.max_outputs.get() {
            return Err(self.count_error());
        }
        Ok(count)
    }

    fn count_error(self) -> String {
        format!("'n' must be an integer between 1 and {}", self.max_outputs)
    }

    pub fn json_count(self, value: Option<&Value>) -> Result<usize, String> {
        match value {
            None => Ok(1),
            Some(value) => self.checked_count(value.as_u64().ok_or_else(|| self.count_error())?),
        }
    }

    pub fn multipart_count(self, value: &str) -> Result<usize, String> {
        let value = value.trim();
        if value.is_empty() || !value.bytes().all(|byte| byte.is_ascii_digit()) {
            return Err(self.count_error());
        }
        self.checked_count(value.parse().map_err(|_| self.count_error())?)
    }

    pub fn parallelism(self) -> NonZeroUsize {
        self.parallel
    }
}

pub fn validate_config(config: &ImageSchedulerConfig) -> Result<(), String> {
    ImageOutputPolicy::new(config.max_outputs_per_request, config.max_parallel_outputs).map(|_| ())
}

fn effective_policy(
    config: ImageSchedulerConfig,
    read_env: impl Fn(&str) -> Option<String>,
) -> Result<ImageOutputPolicy, String> {
    let read_limit = |name: &str, saved: usize| match read_env(name) {
        None => Ok(saved),
        Some(value) => value
            .parse::<usize>()
            .map_err(|_| format!("{name} must be a positive integer")),
    };
    ImageOutputPolicy::new(
        read_limit(
            "ABV_IMAGE_MAX_OUTPUTS_PER_REQUEST",
            config.max_outputs_per_request,
        )?,
        read_limit(
            "ABV_IMAGE_MAX_PARALLEL_OUTPUTS",
            config.max_parallel_outputs,
        )?,
    )
}

fn read_policy(
    path: &Path,
    read_env: impl Fn(&str) -> Option<String>,
) -> Result<ImageOutputPolicy, String> {
    #[derive(Default, Deserialize)]
    struct Proxy {
        #[serde(default)]
        image_scheduler: ImageSchedulerConfig,
    }
    #[derive(Deserialize)]
    struct Document {
        #[serde(default)]
        proxy: Proxy,
    }
    let config = match std::fs::File::open(path) {
        Ok(file) => {
            serde_json::from_reader::<_, Document>(std::io::BufReader::new(file))
                .map_err(|_| "Cannot read valid image-output settings")?
                .proxy
                .image_scheduler
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            ImageSchedulerConfig::default()
        }
        Err(_) => return Err("Cannot read image-output settings".into()),
    };
    effective_policy(config, read_env)
}

/// Snapshot only public scheduling metadata, without native-store hydration.
/// Read once per request so saved settings apply equally to GUI and headless.
pub async fn request_policy() -> Result<ImageOutputPolicy, String> {
    tokio::task::spawn_blocking(|| {
        let path = crate::modules::account::get_data_dir()?.join("gui_config.json");
        let override_value = |name| {
            std::env::var_os(name)
                .map(|value| {
                    value
                        .into_string()
                        .map_err(|_| format!("{name} must be a UTF-8 positive integer"))
                })
                .transpose()
        };
        let outputs = override_value("ABV_IMAGE_MAX_OUTPUTS_PER_REQUEST")?;
        let parallel = override_value("ABV_IMAGE_MAX_PARALLEL_OUTPUTS")?;
        read_policy(&path, |name| match name {
            "ABV_IMAGE_MAX_OUTPUTS_PER_REQUEST" => outputs.clone(),
            "ABV_IMAGE_MAX_PARALLEL_OUTPUTS" => parallel.clone(),
            _ => None,
        })
    })
    .await
    .map_err(|_| "Image-output settings worker failed")?
}

/// Factories run only when a worker slot is free. Completed results occupy a
/// slot until consumed, so neither queued futures nor results grow with count.
/// JoinSet aborts active tasks on drop, preserving request cancellation.
pub struct BoundedJoinSet<T, F> {
    tasks: JoinSet<T>,
    factory: F,
    next: usize,
    total: usize,
    parallel: NonZeroUsize,
}

impl<T: Send + 'static, F> BoundedJoinSet<T, F> {
    pub fn new(total: usize, parallel: NonZeroUsize, factory: F) -> Self {
        Self {
            tasks: JoinSet::new(),
            factory,
            next: 0,
            total,
            parallel,
        }
    }

    pub async fn join_next<Fut>(&mut self) -> Option<Result<T, JoinError>>
    where
        F: FnMut(usize) -> Fut,
        Fut: Future<Output = T> + Send + 'static,
    {
        while self.next < self.total && self.tasks.len() < self.parallel.get() {
            let index = self.next;
            self.next += 1;
            self.tasks.spawn((self.factory)(index));
        }
        self.tasks.join_next().await
    }

    pub fn abort_all(&mut self) {
        self.next = self.total;
        self.tasks.abort_all();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    use std::sync::{
        atomic::{AtomicUsize, Ordering},
        Arc,
    };

    #[test]
    fn json_and_multipart_counts_reject_invalid_and_overflowing_values() {
        let policy = ImageOutputPolicy::new(10, 1).unwrap();
        assert_eq!(policy.json_count(None).unwrap(), 1);
        for count in [1, 10] {
            assert_eq!(policy.json_count(Some(&json!(count))).unwrap(), count);
            assert_eq!(policy.multipart_count(&count.to_string()).unwrap(), count);
        }
        for value in [
            json!(0),
            json!(-1),
            json!(11),
            json!(1.5),
            json!("2"),
            Value::Null,
            json!(u64::MAX),
        ] {
            assert!(policy.json_count(Some(&value)).is_err(), "{value}");
        }
        for value in [
            "",
            "0",
            "-1",
            "+1",
            "1.0",
            "1e1",
            "11",
            "184467440737095516160",
        ] {
            assert!(policy.multipart_count(value).is_err(), "{value}");
        }
    }

    #[test]
    fn settings_and_environment_have_explicit_safety_bounds() {
        for (outputs, parallel) in [(0, 1), (101, 1), (10, 0), (100, 17), (1, 2)] {
            assert!(ImageOutputPolicy::new(outputs, parallel).is_err());
        }
        assert!(ImageOutputPolicy::new(100, 16).is_ok());
        let policy = effective_policy(ImageSchedulerConfig::default(), |name| match name {
            "ABV_IMAGE_MAX_OUTPUTS_PER_REQUEST" => Some("4".into()),
            "ABV_IMAGE_MAX_PARALLEL_OUTPUTS" => Some("2".into()),
            _ => None,
        })
        .unwrap();
        assert_eq!(policy.parallelism().get(), 2);
        assert!(policy.json_count(Some(&json!(5))).is_err());
        assert!(
            effective_policy(ImageSchedulerConfig::default(), |_| Some("invalid".into())).is_err()
        );
    }

    #[test]
    fn settings_projection_ignores_secrets_but_rejects_invalid_files() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("gui_config.json");
        assert_eq!(read_policy(&path, |_| None).unwrap().parallelism().get(), 1);
        std::fs::write(&path, serde_json::to_vec(&json!({
            "credential_store":{"not_a_real_native_reference":true},
            "proxy":{"api_key":null,"image_scheduler":{"max_outputs_per_request":3,"max_parallel_outputs":2}}
        })).unwrap()).unwrap();
        let policy = read_policy(&path, |_| None).unwrap();
        assert_eq!(policy.multipart_count("3").unwrap(), 3);
        assert_eq!(policy.parallelism().get(), 2);
        std::fs::write(&path, "{malformed").unwrap();
        assert!(read_policy(&path, |_| None).is_err());
    }

    struct Active(Arc<AtomicUsize>);
    impl Drop for Active {
        fn drop(&mut self) {
            self.0.fetch_sub(1, Ordering::SeqCst);
        }
    }

    #[tokio::test]
    async fn workers_are_lazy_bounded_and_abort_never_creates_remaining_payloads() {
        let created = Arc::new(AtomicUsize::new(0));
        let active = Arc::new(AtomicUsize::new(0));
        let maximum = Arc::new(AtomicUsize::new(0));
        let permits = Arc::new(tokio::sync::Semaphore::new(0));
        let (started, mut starts) = tokio::sync::mpsc::unbounded_channel();
        let mut work = BoundedJoinSet::new(100, NonZeroUsize::new(2).unwrap(), {
            let created = created.clone();
            let active = active.clone();
            let maximum = maximum.clone();
            let permits = permits.clone();
            move |index| {
                // Represents expensive payload construction, before spawning.
                created.fetch_add(1, Ordering::SeqCst);
                let active = active.clone();
                let maximum = maximum.clone();
                let permits = permits.clone();
                let started = started.clone();
                async move {
                    let running = active.fetch_add(1, Ordering::SeqCst) + 1;
                    maximum.fetch_max(running, Ordering::SeqCst);
                    let _active = Active(active);
                    started.send(index).unwrap();
                    permits.acquire().await.unwrap().forget();
                    index
                }
            }
        });
        assert_eq!(created.load(Ordering::SeqCst), 0);
        {
            let mut next = Box::pin(work.join_next());
            assert!(futures::poll!(&mut next).is_pending());
            starts.recv().await.unwrap();
            starts.recv().await.unwrap();
            assert_eq!(created.load(Ordering::SeqCst), 2);
            permits.add_permits(1);
            next.await.unwrap().unwrap();
        }
        assert_eq!(
            created.load(Ordering::SeqCst),
            2,
            "refill waits for caller to inspect the result"
        );
        work.abort_all();
        while let Some(task) = work.join_next().await {
            assert!(task.unwrap_err().is_cancelled());
        }
        assert_eq!(created.load(Ordering::SeqCst), 2);
        assert_eq!(active.load(Ordering::SeqCst), 0);
        assert_eq!(maximum.load(Ordering::SeqCst), 2);
    }

    #[tokio::test]
    async fn dropping_request_aborts_workers_and_releases_owned_guards() {
        let (dropped, mut drops) = tokio::sync::mpsc::unbounded_channel();
        struct Guard(tokio::sync::mpsc::UnboundedSender<()>);
        impl Drop for Guard {
            fn drop(&mut self) {
                let _ = self.0.send(());
            }
        }
        let mut work = BoundedJoinSet::new(100, NonZeroUsize::new(1).unwrap(), move |_| {
            let guard = Guard(dropped.clone());
            async move {
                let _guard = guard;
                std::future::pending::<()>().await;
            }
        });
        {
            let mut next = Box::pin(work.join_next());
            assert!(futures::poll!(&mut next).is_pending());
        }
        drop(work);
        tokio::time::timeout(std::time::Duration::from_secs(1), drops.recv())
            .await
            .unwrap()
            .unwrap();
    }

    #[tokio::test]
    async fn bounded_workers_finish_all_outputs_with_the_captured_request_context() {
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(30);
        let context = super::super::budget::RequestContext::new(deadline);
        let created = Arc::new(AtomicUsize::new(0));
        let mut work = BoundedJoinSet::new(20, NonZeroUsize::new(2).unwrap(), {
            let created = created.clone();
            move |index| {
                created.fetch_add(1, Ordering::SeqCst);
                super::super::budget::scope_optional(Some(context.fork()), async move {
                    assert_eq!(super::super::budget::current_deadline(), Some(deadline));
                    tokio::task::yield_now().await;
                    index
                })
            }
        });
        let mut completed = Vec::new();
        while let Some(result) = work.join_next().await {
            completed.push(result.unwrap());
        }
        completed.sort_unstable();
        assert_eq!(completed, (0..20).collect::<Vec<_>>());
        assert_eq!(created.load(Ordering::SeqCst), 20);
    }

    #[tokio::test]
    async fn queued_work_keeps_its_safety_generation_after_halt_and_resume() {
        let directory = tempfile::tempdir().unwrap();
        let controller =
            super::super::admission::SafetyController::for_dir(directory.path()).unwrap();
        let context = super::super::budget::RequestContext::new(
            std::time::Instant::now() + std::time::Duration::from_secs(30),
        );
        context.set_safety(controller.permit().unwrap());
        let dispatches = Arc::new(AtomicUsize::new(0));
        let mut work = BoundedJoinSet::new(10, NonZeroUsize::new(1).unwrap(), {
            let dispatches = dispatches.clone();
            move |index| {
                let dispatches = dispatches.clone();
                super::super::budget::scope_optional(Some(context.fork()), async move {
                    super::super::admission::check_current_activity()?;
                    dispatches.fetch_add(1, Ordering::SeqCst);
                    Ok::<_, String>(index)
                })
            }
        });
        assert_eq!(work.join_next().await.unwrap().unwrap().unwrap(), 0);
        controller.halt("fixture-attention").unwrap();
        controller
            .resume(controller.status().unwrap().generation, true)
            .unwrap();
        let error = work.join_next().await.unwrap().unwrap().unwrap_err();
        assert!(error.contains("GATEWAY_EMERGENCY_STOP"));
        work.abort_all();
        assert!(work.join_next().await.is_none());
        assert_eq!(dispatches.load(Ordering::SeqCst), 1);
    }
}
