//! State shared between the UI thread and the background network tasks.

use std::collections::VecDeque;
use std::future::Future;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use eframe::egui;
use mho_driver::ScopeStatus;
use mho_service::{
    ApiResult, Frame, LiveConfig, LiveEvent, MeasureSnapshot, RemoteBackend, ScopeApi, ServiceInfo,
};

const LOG_LINES: usize = 8;
/// Full status costs ~40 queries (≈1 s of bus time), so it is refreshed rarely; changes made
/// through this GUI update it directly, and a changed set of displayed channels triggers a
/// refresh early.
const STATUS_INTERVAL: Duration = Duration::from_secs(15);

#[derive(Default)]
pub struct Shared {
    pub frame: Option<Arc<Frame>>,
    pub measurements: Option<Arc<MeasureSnapshot>>,
    pub status: Option<ScopeStatus>,
    pub info: Option<ServiceInfo>,
    pub live: Option<LiveConfig>,
    pub network: Option<mho_driver::NetworkState>,
    pub live_error: Option<String>,
    pub ws_connected: bool,
    pub busy: usize,
    /// `(message, is_error)`, newest last.
    pub log: VecDeque<(String, bool)>,
    /// Set to force an early status refresh.
    pub status_dirty: bool,
}

impl Shared {
    pub fn log(&mut self, msg: impl Into<String>, error: bool) {
        self.log.push_back((msg.into(), error));
        while self.log.len() > LOG_LINES {
            self.log.pop_front();
        }
    }
}

#[derive(Clone)]
pub struct Client {
    pub api: Arc<RemoteBackend>,
    pub shared: Arc<Mutex<Shared>>,
    pub rt: tokio::runtime::Handle,
    pub ctx: egui::Context,
}

impl Client {
    pub fn new(
        url: &str,
        token: Option<String>,
        rt: tokio::runtime::Handle,
        ctx: egui::Context,
    ) -> Self {
        let c = Client {
            api: Arc::new(RemoteBackend::with_token(url, token)),
            shared: Arc::new(Mutex::new(Shared {
                status_dirty: true,
                ..Default::default()
            })),
            rt,
            ctx,
        };
        c.spawn_stream();
        c.spawn_status_poll();
        c
    }

    pub fn lock(&self) -> std::sync::MutexGuard<'_, Shared> {
        self.shared.lock().unwrap()
    }

    /// Runs an API call in the background; `apply` stores the result.
    pub fn op<T, Fut>(
        &self,
        label: impl Into<String>,
        call: impl FnOnce(Arc<RemoteBackend>) -> Fut + Send + 'static,
        apply: impl FnOnce(&mut Shared, T) + Send + 'static,
    ) where
        T: Send + 'static,
        Fut: Future<Output = ApiResult<T>> + Send + 'static,
    {
        let label = label.into();
        let this = self.clone();
        self.lock().busy += 1;
        self.rt.spawn(async move {
            let r = call(this.api.clone()).await;
            {
                let mut sh = this.lock();
                sh.busy -= 1;
                match r {
                    Ok(v) => apply(&mut sh, v),
                    Err(e) => sh.log(format!("{label}: {e}"), true),
                }
            }
            this.ctx.request_repaint();
        });
    }

    fn spawn_stream(&self) {
        let this = self.clone();
        self.rt.spawn(async move {
            loop {
                match this.api.stream().await {
                    Ok(mut rx) => {
                        this.lock().ws_connected = true;
                        while let Some(ev) = rx.recv().await {
                            {
                                let mut sh = this.lock();
                                match ev {
                                    LiveEvent::Frame(f) => {
                                        let shown: Vec<&str> =
                                            f.traces.iter().map(|t| t.source.as_str()).collect();
                                        let known = sh.status.as_ref().map(|st| {
                                            st.channels
                                                .iter()
                                                .filter(|c| c.display)
                                                .map(|c| format!("CHAN{}", c.channel))
                                                .collect::<Vec<_>>()
                                        });
                                        if known.is_some_and(|k| k != shown) {
                                            // Channels switched on the front panel.
                                            sh.status_dirty = true;
                                        }
                                        sh.frame = Some(f);
                                        sh.live_error = None;
                                    }
                                    LiveEvent::Measurements(m) => sh.measurements = Some(m),
                                    LiveEvent::Error(e) => sh.live_error = Some(e),
                                }
                            }
                            this.ctx.request_repaint();
                        }
                        this.lock().ws_connected = false;
                    }
                    Err(e) => {
                        let mut sh = this.lock();
                        sh.ws_connected = false;
                        sh.live_error = Some(e.message);
                    }
                }
                this.ctx.request_repaint();
                tokio::time::sleep(Duration::from_secs(1)).await;
            }
        });
    }

    /// Full status is expensive (~50 queries), so it is refreshed rarely and after changes.
    fn spawn_status_poll(&self) {
        let this = self.clone();
        self.rt.spawn(async move {
            let mut last = tokio::time::Instant::now() - STATUS_INTERVAL;
            loop {
                let dirty = this.lock().status_dirty;
                if dirty || last.elapsed() >= STATUS_INTERVAL {
                    last = tokio::time::Instant::now();
                    this.lock().status_dirty = false;
                    let info = this.api.info().await;
                    let status = this.api.status().await;
                    let live = this.api.live_config().await;
                    {
                        let mut sh = this.lock();
                        if let Ok(i) = info {
                            sh.info = Some(i);
                        }
                        match status {
                            Ok(s) => sh.status = Some(s),
                            Err(e) => sh.live_error = Some(e.message),
                        }
                        if let Ok(l) = live {
                            sh.live = Some(l);
                        }
                    }
                    this.ctx.request_repaint();
                }
                tokio::time::sleep(Duration::from_millis(200)).await;
            }
        });
    }
}
