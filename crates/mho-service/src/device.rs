//! The device actor: one thread owns the scope, everything else sends it jobs.

use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use mho_driver::{Connection, DriverError, Idn, Scope};
use tokio::sync::{mpsc, oneshot};
use tracing::{info, warn};

use crate::api::{ApiError, ApiResult, ErrorKind};

type Job = Box<dyn FnOnce(&mut Slot) + Send>;

/// Connection state visible without going through the job queue.
#[derive(Debug, Clone, Default)]
pub struct DeviceState {
    pub connected: bool,
    pub idn: Option<Idn>,
    pub last_error: Option<String>,
}

struct Slot {
    conn: Connection,
    scope: Option<Scope>,
    last_attempt: Option<Instant>,
    last_open_error: Option<String>,
    consecutive_timeouts: u32,
    shared: Arc<Mutex<DeviceState>>,
}

const RECONNECT_BACKOFF: Duration = Duration::from_secs(2);
const MAX_CONSECUTIVE_TIMEOUTS: u32 = 3;

impl Slot {
    fn ensure_open(&mut self) -> ApiResult<&mut Scope> {
        if self.scope.is_none() {
            if let Some(t) = self.last_attempt
                && t.elapsed() < RECONNECT_BACKOFF
            {
                let msg = self.last_open_error.clone().unwrap_or_else(|| "not connected".into());
                return Err(ApiError::unavailable(msg));
            }
            self.last_attempt = Some(Instant::now());
            match Scope::open(&self.conn) {
                Ok(scope) => {
                    let mut st = self.shared.lock().unwrap();
                    st.connected = true;
                    st.idn = Some(scope.idn().clone());
                    st.last_error = None;
                    self.last_open_error = None;
                    self.consecutive_timeouts = 0;
                    self.scope = Some(scope);
                }
                Err(e) => {
                    let msg = format!("cannot open {}: {e}", self.conn);
                    warn!("{msg}");
                    self.last_open_error = Some(msg.clone());
                    let mut st = self.shared.lock().unwrap();
                    st.connected = false;
                    st.last_error = Some(msg.clone());
                    return Err(ApiError::unavailable(msg));
                }
            }
        }
        Ok(self.scope.as_mut().expect("scope opened above"))
    }

    fn run<R>(&mut self, f: impl FnOnce(&mut Scope) -> mho_driver::Result<R>) -> ApiResult<R> {
        let scope = self.ensure_open()?;
        match f(scope) {
            Ok(v) => {
                self.consecutive_timeouts = 0;
                Ok(v)
            }
            Err(e) => {
                let disconnect = match &e {
                    DriverError::Scpi(s) if s.is_timeout() => {
                        self.consecutive_timeouts += 1;
                        self.consecutive_timeouts >= MAX_CONSECUTIVE_TIMEOUTS
                    }
                    // ENODEV/EPIPE etc.: device vanished (USB unplugged, USB/IP detached).
                    DriverError::Scpi(mho_scpi::ScpiError::Io(_)) => true,
                    _ => false,
                };
                let err = ApiError::from(e);
                if disconnect {
                    warn!("dropping connection after error: {err}");
                    self.scope = None;
                    let mut st = self.shared.lock().unwrap();
                    st.connected = false;
                    st.last_error = Some(err.message.clone());
                }
                Err(err)
            }
        }
    }
}

/// Handle to the device thread. Cheap to clone.
#[derive(Clone)]
pub struct Device {
    tx: mpsc::UnboundedSender<Job>,
    shared: Arc<Mutex<DeviceState>>,
    epoch: Arc<AtomicU64>,
    connection: Connection,
}

impl Device {
    /// Starts the device thread. The scope is opened lazily on the first job and reopened
    /// automatically after it disappears.
    pub fn spawn(connection: Connection) -> Device {
        let (tx, mut rx) = mpsc::unbounded_channel::<Job>();
        let shared = Arc::new(Mutex::new(DeviceState::default()));
        let mut slot = Slot {
            conn: connection.clone(),
            scope: None,
            last_attempt: None,
            last_open_error: None,
            consecutive_timeouts: 0,
            shared: shared.clone(),
        };
        std::thread::Builder::new()
            .name("mho-device".into())
            .spawn(move || {
                while let Some(job) = rx.blocking_recv() {
                    job(&mut slot);
                }
                info!("device thread stopped");
            })
            .expect("spawn device thread");
        Device { tx, shared, epoch: Arc::new(AtomicU64::new(0)), connection }
    }

    /// Runs `f` on the device thread.
    pub async fn call<R, F>(&self, f: F) -> ApiResult<R>
    where
        R: Send + 'static,
        F: FnOnce(&mut Scope) -> mho_driver::Result<R> + Send + 'static,
    {
        let (tx, rx) = oneshot::channel();
        let job: Job = Box::new(move |slot| {
            let _ = tx.send(slot.run(f));
        });
        self.tx.send(job).map_err(|_| ApiError::new(ErrorKind::Internal, "device thread stopped"))?;
        rx.await.map_err(|_| ApiError::new(ErrorKind::Internal, "device job dropped"))?
    }

    /// Like [`Self::call`] for operations that change scope settings: bumps the settings
    /// epoch so the live loop refreshes cached preambles.
    pub async fn call_mut<R, F>(&self, f: F) -> ApiResult<R>
    where
        R: Send + 'static,
        F: FnOnce(&mut Scope) -> mho_driver::Result<R> + Send + 'static,
    {
        let r = self.call(f).await;
        self.epoch.fetch_add(1, Ordering::Relaxed);
        r
    }

    pub fn state(&self) -> DeviceState {
        self.shared.lock().unwrap().clone()
    }

    pub fn connection(&self) -> &Connection {
        &self.connection
    }

    pub fn settings_epoch(&self) -> u64 {
        self.epoch.load(Ordering::Relaxed)
    }
}
