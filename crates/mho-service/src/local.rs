use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use mho_driver::*;

use crate::api::*;
use crate::device::Device;
use crate::live::{LiveConfig, LiveHub, run_loop};

/// In-process backend: owns the device thread and the live loop.
#[derive(Clone)]
pub struct LocalBackend {
    device: Device,
    hub: LiveHub,
    allow_raw_scpi: bool,
    live_stop: tokio_util::sync::CancellationToken,
}

impl LocalBackend {
    /// Spawns the device thread and the live acquisition task (needs a Tokio runtime).
    pub fn start(connection: Connection, allow_raw_scpi: bool, live: LiveConfig) -> Self {
        let device = Device::spawn(connection);
        let hub = LiveHub::new(live);
        let live_stop = tokio_util::sync::CancellationToken::new();
        tokio::spawn(run_loop(device.clone(), hub.clone(), live_stop.clone()));
        LocalBackend { device, hub, allow_raw_scpi, live_stop }
    }

    /// Stops the live loop and closes the scope connection cleanly (see [`Device::shutdown`]).
    pub async fn shutdown(&self) {
        self.live_stop.cancel();
        self.device.shutdown().await;
    }

    pub fn device(&self) -> &Device {
        &self.device
    }

    pub fn hub(&self) -> &LiveHub {
        &self.hub
    }

    pub fn raw_scpi_allowed(&self) -> bool {
        self.allow_raw_scpi
    }

    /// RAW memory download with progress `(points_done, points_total)`.
    pub async fn raw_waveform(
        &self,
        source: String,
        progress: Arc<dyn Fn(usize, usize) + Send + Sync>,
    ) -> ApiResult<Waveform> {
        self.device.call(move |s| s.raw_waveform(&source, &mut |a, b| progress(a, b))).await
    }
}

#[async_trait]
impl ScopeApi for LocalBackend {
    async fn info(&self) -> ApiResult<ServiceInfo> {
        // Touch the device so a disconnected scope gets a reconnect attempt.
        let _ = self.device.call(|s| Ok(s.idn().clone())).await;
        let st = self.device.state();
        Ok(ServiceInfo {
            idn: st.idn,
            connection: self.device.connection().to_string(),
            connected: st.connected,
            raw_scpi_allowed: self.allow_raw_scpi,
            version: env!("CARGO_PKG_VERSION").to_string(),
        })
    }

    async fn status(&self) -> ApiResult<ScopeStatus> {
        self.device.call(|s| s.status()).await
    }

    async fn channel(&self, n: u8) -> ApiResult<ChannelState> {
        self.device.call(move |s| s.channel(n)).await
    }

    async fn set_channel(&self, n: u8, update: ChannelUpdate) -> ApiResult<ChannelState> {
        self.device.call_mut(move |s| s.set_channel(n, &update)).await
    }

    async fn timebase(&self) -> ApiResult<TimebaseState> {
        self.device.call(|s| s.timebase()).await
    }

    async fn set_timebase(&self, update: TimebaseUpdate) -> ApiResult<TimebaseState> {
        self.device.call_mut(move |s| s.set_timebase(&update)).await
    }

    async fn trigger(&self) -> ApiResult<TriggerState> {
        self.device.call(|s| s.trigger()).await
    }

    async fn set_trigger(&self, update: TriggerUpdate) -> ApiResult<TriggerState> {
        self.device.call_mut(move |s| s.set_trigger(&update)).await
    }

    async fn acquire(&self) -> ApiResult<AcquireState> {
        self.device.call(|s| s.acquire()).await
    }

    async fn set_acquire(&self, update: AcquireUpdate) -> ApiResult<AcquireState> {
        self.device.call_mut(move |s| s.set_acquire(&update)).await
    }

    async fn action(&self, action: Action) -> ApiResult<()> {
        self.device.call_mut(move |s| s.action(action)).await
    }

    async fn measure(&self, req: MeasureRequest) -> ApiResult<Vec<Measurement>> {
        if req.items.is_empty() {
            return Err(ApiError::invalid("no measurement items given"));
        }
        self.device
            .call(move |s| {
                req.items.iter().map(|i| s.measure(i, &req.source, req.source2.as_deref())).collect()
            })
            .await
    }

    async fn waveform(&self, source: String, mode: WaveMode) -> ApiResult<Waveform> {
        match mode {
            WaveMode::Raw => self.raw_waveform(source, Arc::new(|_, _| {})).await,
            _ => self.device.call(move |s| s.screen_waveform(&source, None)).await,
        }
    }

    async fn screenshot(&self) -> ApiResult<Vec<u8>> {
        self.device.call(|s| s.screenshot()).await
    }

    async fn options(&self) -> ApiResult<Vec<OptionInfo>> {
        self.device.call(|s| s.options()).await
    }

    async fn install_option(&self, key: String) -> ApiResult<()> {
        self.device.call_mut(move |s| s.install_option(&key)).await
    }

    async fn setup_save(&self) -> ApiResult<Vec<u8>> {
        self.device.call(|s| s.setup_save()).await
    }

    async fn setup_load(&self, blob: Vec<u8>) -> ApiResult<()> {
        self.device.call_mut(move |s| s.setup_load(&blob)).await
    }

    async fn scpi(&self, req: ScpiRequest) -> ApiResult<ScpiResponse> {
        if !self.allow_raw_scpi {
            return Err(ApiError::new(
                ErrorKind::Forbidden,
                "raw SCPI is disabled; start the server with --allow-raw-scpi",
            ));
        }
        let timeout = Duration::from_millis(req.timeout_ms.unwrap_or(2000).clamp(100, 60_000));
        let response = self.device.call_mut(move |s| s.raw(&req.command, timeout)).await?;
        Ok(ScpiResponse { response })
    }

    async fn network(&self) -> ApiResult<NetworkState> {
        self.device.call(|s| s.network()).await
    }

    async fn set_network(&self, update: NetworkUpdate) -> ApiResult<NetworkState> {
        self.device.call(move |s| s.set_network(&update)).await
    }

    async fn live_config(&self) -> ApiResult<LiveConfig> {
        Ok(self.hub.config())
    }

    async fn set_live_config(&self, config: LiveConfig) -> ApiResult<LiveConfig> {
        if !(config.max_fps > 0.0 && config.stopped_fps > 0.0) {
            return Err(ApiError::invalid("frame rates must be > 0"));
        }
        self.hub.set_config(config);
        Ok(self.hub.config())
    }
}
