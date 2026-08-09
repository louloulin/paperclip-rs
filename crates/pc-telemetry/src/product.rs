use std::{
    collections::{BTreeMap, HashMap, VecDeque},
    fs,
    path::{Path, PathBuf},
    sync::Arc,
    time::{Duration, Instant},
};

use crate::{RetryBackoff, RetryQueue};
use anyhow::Context;
use chrono::Utc;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use sha2::{Digest, Sha256};
use tokio::sync::{Mutex, Notify};
use uuid::Uuid;

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TelemetryState {
    pub install_id: String,
    pub salt: String,
    pub created_at: String,
    pub first_seen_version: String,
}

pub fn load_or_create_state(dir: &Path, version: &str) -> anyhow::Result<TelemetryState> {
    let path = dir.join("state.json");
    if let Ok(raw) = fs::read_to_string(&path) {
        if let Ok(state) = serde_json::from_str::<TelemetryState>(&raw) {
            if !state.install_id.is_empty() && !state.salt.is_empty() {
                return Ok(state);
            }
        }
    }
    let state = TelemetryState {
        install_id: Uuid::new_v4().to_string(),
        salt: format!("{:x}", Sha256::digest(Uuid::new_v4().as_bytes())),
        created_at: Utc::now().to_rfc3339(),
        first_seen_version: version.to_owned(),
    };
    fs::create_dir_all(dir).context("create telemetry state directory")?;
    fs::write(path, format!("{}\n", serde_json::to_string_pretty(&state)?))
        .context("write telemetry state")?;
    Ok(state)
}

#[derive(Debug, Clone)]
pub struct ProductTelemetryConfig {
    pub enabled: bool,
    pub endpoint: Option<String>,
    pub fallback_endpoints: Vec<String>,
    pub app: String,
    pub schema_version: String,
    pub max_events_per_batch: usize,
    pub max_body_bytes: usize,
    pub retry_base_delay: Duration,
    pub retry_max_delay: Duration,
    pub max_attempts: u32,
    pub jitter_ratio: f64,
    pub max_pending_batches: usize,
}

impl Default for ProductTelemetryConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            endpoint: None,
            fallback_endpoints: Vec::new(),
            app: "paperclip".into(),
            schema_version: "1".into(),
            max_events_per_batch: 50,
            max_body_bytes: 512 * 1024,
            retry_base_delay: Duration::from_secs(1),
            retry_max_delay: Duration::from_secs(30),
            max_attempts: 5,
            jitter_ratio: 0.25,
            max_pending_batches: 20,
        }
    }
}

impl ProductTelemetryConfig {
    pub fn from_env() -> Self {
        Self::resolve_with(|key| std::env::var(key).ok())
    }

    pub fn resolve_with(lookup: impl Fn(&str) -> Option<String>) -> Self {
        let mut config = Self::default();
        config.enabled = !matches!(lookup("PAPERCLIP_TELEMETRY_DISABLED").as_deref(), Some("1"))
            && !matches!(lookup("DO_NOT_TRACK").as_deref(), Some("1"))
            && ![
                "CI",
                "CONTINUOUS_INTEGRATION",
                "GITHUB_ACTIONS",
                "GITLAB_CI",
            ]
            .iter()
            .any(|key| matches!(lookup(key).as_deref(), Some("1" | "true")));
        config.endpoint =
            lookup("PAPERCLIP_TELEMETRY_ENDPOINT").filter(|value| !value.trim().is_empty());
        config
    }
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Event {
    pub name: String,
    pub occurred_at: String,
    pub dimensions: BTreeMap<String, Value>,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct Envelope<'a> {
    app: &'a str,
    schema_version: &'a str,
    install_id: &'a str,
    version: &'a str,
    events: &'a [Event],
    batch_id: &'a str,
}

#[derive(Debug, Clone)]
pub struct PendingBatch {
    batch_id: String,
    body: Vec<u8>,
    next_attempt: u32,
}

impl PendingBatch {
    pub fn for_events(client: &ProductTelemetryClient, events: &[Event], next_attempt: u32) -> anyhow::Result<Self> {
        let body = client.build_body(events)?;
        let batch_id = {
            let mut hasher = <sha2::Sha256 as sha2::Digest>::new();
            sha2::Digest::update(&mut hasher, client.state.install_id.as_bytes());
            sha2::Digest::update(&mut hasher, &body);
            format!("{:x}", sha2::Digest::finalize(hasher))[..32].to_owned()
        };
        Ok(Self { batch_id, body, next_attempt })
    }
}

pub struct ProductTelemetryClient {
    config: ProductTelemetryConfig,
    state: TelemetryState,
    version: String,
    queue: Arc<Mutex<Vec<Event>>>,
    http: reqwest::Client,
    pending: Arc<Mutex<RetryQueue<PendingBatch>>>,
    next_attempt: Arc<Mutex<HashMap<String, u32>>>,
    actor_signal: Arc<Notify>,
    _state_dir: PathBuf,
}

impl ProductTelemetryClient {
    pub fn new(
        config: ProductTelemetryConfig,
        state_dir: &Path,
        version: &str,
    ) -> anyhow::Result<Self> {
        let state = load_or_create_state(state_dir, version)?;
        let pending = Arc::new(Mutex::new(RetryQueue::new(config.max_pending_batches.max(1))));
        let next_attempt = Arc::new(Mutex::new(HashMap::new()));
        let actor_signal = Arc::new(Notify::new());
        Ok(Self {
            config,
            state,
            version: version.into(),
            queue: Arc::new(Mutex::new(Vec::new())),
            http: reqwest::Client::new(),
            pending,
            next_attempt,
            actor_signal,
            _state_dir: state_dir.into(),
        })
    }

    pub async fn track(&self, name: impl Into<String>, dimensions: BTreeMap<String, Value>) {
        if !self.config.enabled {
            return;
        }
        self.queue.lock().await.push(Event {
            name: name.into(),
            occurred_at: Utc::now().to_rfc3339(),
            dimensions,
        });
    }

    pub async fn flush(&self) -> anyhow::Result<()> {
        if !self.config.enabled {
            return Ok(());
        }
        let endpoint = self.primary_endpoint();
        loop {
            let events = {
                let mut queue = self.queue.lock().await;
                let count = queue.len().min(self.config.max_events_per_batch);
                queue.drain(..count).collect::<Vec<_>>()
            };
            if events.is_empty() {
                return Ok(());
            }
            for batch in self.split_by_body_bytes(events)? {
                if let Err(error) = self.send_blocking(&endpoint, &batch).await {
                    self.queue.lock().await.splice(0..0, batch);
                    return Err(error);
                }
            }
        }
    }

    pub async fn final_flush(&self) -> anyhow::Result<()> {
        if !self.config.enabled {
            return Ok(());
        }
        self.flush().await?;
        if !self.pending.lock().await.is_empty() {
            anyhow::bail!("pending batches remain after final flush");
        }
        Ok(())
    }

    pub fn hash_private_ref(&self, value: &str) -> String {
        format!("{:x}", Sha256::digest(format!("{}{value}", self.state.salt)))[..16].to_owned()
    }

    fn primary_endpoint(&self) -> String {
        self.config
            .endpoint
            .clone()
            .unwrap_or_else(|| "https://telemetry.paperclip.ing/ingest".into())
    }

    fn compute_backoff(&self, attempt: u32) -> Duration {
        RetryBackoff {
            base: self.config.retry_base_delay,
            max: self.config.retry_max_delay,
            jitter_ratio: self.config.jitter_ratio,
        }
        .delay(attempt, jitter_sample())
    }

    fn build_body(&self, events: &[Event]) -> anyhow::Result<Vec<u8>> {
        let basis = serde_json::to_vec(&json_basis(&self.state.install_id, events))?;
        let digest = format!("{:x}", Sha256::digest(basis));
        Ok(serde_json::to_vec(&Envelope {
            app: &self.config.app,
            schema_version: &self.config.schema_version,
            install_id: &self.state.install_id,
            version: &self.version,
            events,
            batch_id: &digest[..32],
        })?)
    }

    fn split_by_body_bytes(&self, events: Vec<Event>) -> anyhow::Result<Vec<Vec<Event>>> {
        let mut pending = VecDeque::from([events]);
        let mut batches = Vec::new();
        while let Some(batch) = pending.pop_front() {
            if self.build_body(&batch)?.len() <= self.config.max_body_bytes {
                batches.push(batch);
            } else if batch.len() > 1 {
                let middle = batch.len().div_ceil(2);
                pending.push_front(batch[middle..].to_vec());
                pending.push_front(batch[..middle].to_vec());
            } else {
                tracing::warn!(event = %batch[0].name, "dropping oversized telemetry event");
            }
        }
        Ok(batches)
    }

    pub async fn send_blocking(&self, primary: &str, events: &[Event]) -> anyhow::Result<()> {
        let body = self.build_body(events)?;
        for attempt in 1..=self.config.max_attempts {
            match self.post_with_fallback(primary, body.clone()).await? {
                SendOutcome::Ok => return Ok(()),
                SendOutcome::Terminal(status) => anyhow::bail!("terminal telemetry status {status}"),
                SendOutcome::Retry(delay) if attempt < self.config.max_attempts => {
                    tokio::time::sleep(delay.unwrap_or(self.compute_backoff(attempt)).min(self.config.retry_max_delay)).await;
                }
                SendOutcome::Retry(_) => anyhow::bail!("telemetry retry attempts exhausted"),
            }
        }
        unreachable!()
    }

    async fn post_with_fallback(&self, primary: &str, body: Vec<u8>) -> anyhow::Result<SendOutcome> {
        let endpoints = std::iter::once(primary).chain(self.config.fallback_endpoints.iter().map(String::as_str));
        let mut last_error = None;
        for endpoint in endpoints {
            match self
                .http
                .post(endpoint)
                .header("content-type", "application/json")
                .body(body.clone())
                .send()
                .await
            {
                Ok(response) if response.status().is_success() => return Ok(SendOutcome::Ok),
                Ok(response) if response.status().as_u16() == 429 => {
                    let delay = response
                        .headers()
                        .get("retry-after")
                        .and_then(|v| v.to_str().ok())
                        .and_then(|v| v.parse::<f64>().ok())
                        .filter(|v| *v >= 0.0)
                        .map(Duration::from_secs_f64);
                    return Ok(SendOutcome::Retry(delay));
                }
                Ok(response) if matches!(response.status().as_u16(), 502 | 503 | 504) => {
                    last_error = Some(anyhow::anyhow!("transient telemetry status {}", response.status()));
                }
                Ok(response) => return Ok(SendOutcome::Terminal(response.status().as_u16())),
                Err(error) => last_error = Some(error.into()),
            }
        }
        if last_error.is_some() {
            Ok(SendOutcome::Retry(None))
        } else {
            anyhow::bail!("no telemetry endpoint configured")
        }
    }

    pub async fn enqueue_retry(&self, batch: PendingBatch, due_at: Instant) {
        let mut pending = self.pending.lock().await;
        if let Some(evicted) = pending.push(batch.clone(), batch.next_attempt, due_at) {
            tracing::warn!(batch_id = %evicted.batch_id, "dropping evicted retry batch");
            self.next_attempt.lock().await.remove(&evicted.batch_id);
        }
        self.next_attempt.lock().await.insert(batch.batch_id.clone(), batch.next_attempt);
        self.actor_signal.notify_one();
    }

    pub async fn drain_due(&self, now: Instant) -> Vec<PendingBatch> {
        self.pending.lock().await.drain_due(now)
    }

    pub fn start_background_retry_actor(self: Arc<Self>) -> RetryActorHandle {
        let signal_for_task = Arc::clone(&self.actor_signal);
        let signal = Arc::clone(&self.actor_signal);
        let task = tokio::spawn(async move {
            loop {
                let due = self.drain_due(Instant::now()).await;
                if !due.is_empty() {
                    let client = self.clone();
                    for batch in due {
                        let client = client.clone();
                        tokio::spawn(async move { client.retry_one(batch).await });
                    }
                }
                tokio::select! {
                    _ = signal_for_task.notified() => continue,
                    _ = tokio::time::sleep(Duration::from_millis(100)) => {}
                }
            }
        });
        RetryActorHandle { signal, task }
    }

    async fn retry_one(&self, batch: PendingBatch) {
        match self.post_with_fallback(&self.primary_endpoint(), batch.body.clone()).await {
            Ok(SendOutcome::Ok) | Ok(SendOutcome::Terminal(_)) => {
                self.next_attempt.lock().await.remove(&batch.batch_id);
            }
            Ok(SendOutcome::Retry(delay)) => {
                let next_attempt = batch.next_attempt + 1;
                if next_attempt > self.config.max_attempts {
                    tracing::warn!(batch_id = %batch.batch_id, "dropping batch after max attempts");
                    self.next_attempt.lock().await.remove(&batch.batch_id);
                    return;
                }
                let backoff = self.compute_backoff(next_attempt);
                let due = Instant::now() + delay.unwrap_or(backoff).min(self.config.retry_max_delay);
                self.enqueue_retry(
                    PendingBatch { batch_id: batch.batch_id, body: batch.body, next_attempt },
                    due,
                )
                .await;
            }
            Err(error) => tracing::warn!(error = %error, "background retry transport error"),
        }
    }

    pub fn start_periodic_flush(self: Arc<Self>, interval: Duration) -> PeriodicFlushHandle {
        let task = tokio::spawn(async move {
            let mut ticker = tokio::time::interval(interval);
            ticker.tick().await;
            loop {
                ticker.tick().await;
                if let Err(error) = self.flush().await {
                    tracing::warn!(error = %error, "periodic product telemetry flush failed");
                }
            }
        });
        PeriodicFlushHandle { task }
    }
}

pub struct RetryActorHandle {
    signal: Arc<Notify>,
    task: tokio::task::JoinHandle<()>,
}

impl RetryActorHandle {
    pub async fn stop(self) {
        self.signal.notify_waiters();
        self.task.abort();
        let _ = self.task.await;
    }
}

pub struct PeriodicFlushHandle {
    task: tokio::task::JoinHandle<()>,
}

impl PeriodicFlushHandle {
    pub async fn stop(self) {
        self.task.abort();
        let _ = self.task.await;
    }
}

enum SendOutcome {
    Ok,
    Retry(Option<Duration>),
    Terminal(u16),
}

fn jitter_sample() -> f64 {
    use std::time::{SystemTime, UNIX_EPOCH};
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |value| value.subsec_nanos());
    f64::from(nanos) / f64::from(u32::MAX)
}

fn json_basis(install_id: &str, events: &[Event]) -> Value {
    serde_json::json!({ "events": events, "installId": install_id })
}
