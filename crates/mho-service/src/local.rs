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
    /// `:SYST:MOD?` is constant for a scope; cached after the first query.
    modules: std::sync::Arc<std::sync::Mutex<Option<Modules>>>,
}

impl LocalBackend {
    /// Spawns the device thread and the live acquisition task (needs a Tokio runtime).
    pub fn start(connection: Connection, allow_raw_scpi: bool, live: LiveConfig) -> Self {
        Self::with_device(Device::spawn(connection), allow_raw_scpi, live)
    }

    /// Backend on an existing device thread (see [`Device::spawn_with`]).
    pub fn with_device(device: Device, allow_raw_scpi: bool, live: LiveConfig) -> Self {
        let hub = LiveHub::new(live);
        let live_stop = tokio_util::sync::CancellationToken::new();
        tokio::spawn(run_loop(device.clone(), hub.clone(), live_stop.clone()));
        LocalBackend {
            device,
            hub,
            allow_raw_scpi,
            live_stop,
            modules: Default::default(),
        }
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
        self.device
            .call(move |s| s.raw_waveform(&source, &mut |a, b| progress(a, b)))
            .await
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
        self.device
            .call_mut(move |s| s.set_channel(n, &update))
            .await
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
                req.items
                    .iter()
                    .map(|i| s.measure(i, &req.source, req.source2.as_deref()))
                    .collect()
            })
            .await
    }

    async fn waveform(&self, source: String, mode: WaveMode) -> ApiResult<Waveform> {
        match mode {
            WaveMode::Raw => self.raw_waveform(source, Arc::new(|_, _| {})).await,
            _ => {
                self.device
                    .call(move |s| s.screen_waveform(&source, None))
                    .await
            }
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
        let response = self
            .device
            .call_mut(move |s| s.raw(&req.command, timeout))
            .await?;
        Ok(ScpiResponse { response })
    }

    async fn measure_stats(&self, r: StatsRequest) -> ApiResult<MeasureStats> {
        self.device
            .call(move |s| s.measure_stats(&r.item, &r.source, r.source2.as_deref()))
            .await
    }

    async fn measure_action(&self, action: MeasureAction) -> ApiResult<()> {
        self.device
            .call_mut(move |s| s.measure_action(action))
            .await
    }

    async fn counter_value(&self) -> ApiResult<Option<f64>> {
        self.device.call(|s| s.counter_value()).await
    }

    async fn dvm_value(&self) -> ApiResult<Option<f64>> {
        self.device.call(|s| s.dvm_value()).await
    }

    async fn decode_bus(&self, bus: u8) -> ApiResult<mho_dsp::DecodeTable> {
        self.device.call(move |s| s.bus_data(bus)).await
    }

    async fn fft_peaks(&self, math: u8) -> ApiResult<Vec<mho_dsp::ScopePeak>> {
        self.device.call(move |s| s.fft_peaks(math)).await
    }

    async fn readouts(&self) -> ApiResult<Vec<mho_driver::ops::ReadoutInfo>> {
        let trusted = self
            .device
            .call(|s| Ok(s.trusts_unverified()))
            .await
            .unwrap_or(false);
        Ok(mho_driver::ops::READOUTS
            .iter()
            .map(|r| mho_driver::ops::describe_readout(r, trusted))
            .collect())
    }

    async fn readout(&self, id: String) -> ApiResult<mho_driver::params::ParamMap> {
        self.device.call(move |s| s.readout(&id)).await
    }

    async fn commands(&self) -> ApiResult<Vec<mho_driver::ops::CommandInfo>> {
        Ok(mho_driver::ops::COMMANDS
            .iter()
            .map(mho_driver::ops::describe_command)
            .collect())
    }

    async fn run_command(&self, id: String, request: CommandRequest) -> ApiResult<()> {
        self.device
            .call_mut(move |s| s.run_command(&id, request.path.as_deref(), request.confirm))
            .await
    }

    async fn modules(&self) -> ApiResult<Modules> {
        if let Some(m) = *self.modules.lock().unwrap() {
            return Ok(m);
        }
        let m = self.device.call(|s| s.modules()).await?;
        *self.modules.lock().unwrap() = Some(m);
        Ok(m)
    }

    async fn groups(&self) -> ApiResult<Vec<mho_driver::params::GroupInfo>> {
        let m = self.modules().await.unwrap_or_default();
        let trusted = self
            .device
            .call(|s| Ok(s.trusts_unverified()))
            .await
            .unwrap_or(false);
        Ok(mho_driver::params::GROUPS
            .iter()
            .map(|g| {
                let available = match g.requires {
                    mho_driver::params::Requires::Nothing => true,
                    mho_driver::params::Requires::LogicAnalyzer => m.logic_analyzer,
                    mho_driver::params::Requires::FunctionGenerator => m.function_generator,
                };
                let mut info = mho_driver::params::describe(g, available);
                info.verified |= trusted;
                info
            })
            .collect())
    }

    async fn group_get(
        &self,
        id: String,
        instance: Option<u8>,
    ) -> ApiResult<mho_driver::params::ParamMap> {
        self.device
            .call(move |s| s.group_get(&id, instance, false))
            .await
    }

    async fn group_set(
        &self,
        id: String,
        instance: Option<u8>,
        values: mho_driver::params::ParamMap,
        activate: bool,
    ) -> ApiResult<Option<mho_driver::params::ParamMap>> {
        self.device
            .call_mut(move |s| s.group_set(&id, instance, &values, activate))
            .await
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
        config.validate().map_err(ApiError::invalid)?;
        self.hub.set_config(config);
        Ok(self.hub.config())
    }
}
