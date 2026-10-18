use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use mho_driver::*;

use crate::api::*;
use crate::device::Device;
use crate::live::{BodeLive, LiveConfig, LiveHub, run_loop};

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
            WaveMode::Raw => {
                let label = format!("RAW {source}");
                let r = self
                    .raw_waveform(source, self.hub.progress_reporter(&label))
                    .await;
                self.hub.progress_done(&label);
                r
            }
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

    async fn logic_capture(&self, bus: u8) -> ApiResult<mho_driver::LogicCapture> {
        self.device.call_mut(move |s| s.logic_capture(bus)).await
    }

    async fn bode_sweep(
        &self,
        request: mho_driver::BodeRequest,
    ) -> ApiResult<Vec<mho_driver::BodePoint>> {
        request.validate().map_err(ApiError::from)?;
        let progress = self.hub.progress_reporter("Bode sweep");
        let hub = self.hub.clone();
        let live = BodeLive {
            id: hub.next_bode_id(),
            request: request.clone(),
            points: Vec::new(),
            total: request.points,
            finished: false,
            error: None,
        };
        hub.publish_bode(live.clone());
        let mut state = live.clone();
        let r = self
            .device
            .call_mut(move |s| {
                s.bode_sweep_with(&request, &mut |done, total, p| {
                    progress(done, total);
                    state.points.push(p.clone());
                    hub.publish_bode(state.clone());
                })
            })
            .await;
        self.hub.progress_done("Bode sweep");
        let mut end = self
            .hub
            .subscribe_bode()
            .borrow()
            .as_deref()
            .cloned()
            .unwrap_or(live);
        end.finished = true;
        if let Err(e) = &r {
            end.error = Some(e.message.clone());
        }
        self.hub.publish_bode(end);
        r
    }

    async fn bode_native(&self, request: NativeBodeRequest) -> ApiResult<Vec<u8>> {
        request.validate().map_err(ApiError::from)?;
        let progress = self.hub.progress_reporter("Bode plot (scope)");
        let r = self
            .device
            .call_mut(move |s| s.bode_native(&request, &mut |a, b| progress(a, b)))
            .await;
        self.hub.progress_done("Bode plot (scope)");
        r
    }

    async fn modules(&self) -> ApiResult<Modules> {
        self.device
            .call(|s| {
                let c = s.capabilities()?;
                Ok(Modules {
                    logic_analyzer: c.logic_analyzer,
                    function_generator: c.generator.is_some(),
                })
            })
            .await
    }

    async fn capabilities(&self) -> ApiResult<Capabilities> {
        self.device.call(|s| s.capabilities()).await
    }

    async fn groups(&self) -> ApiResult<Vec<mho_driver::params::GroupInfo>> {
        use mho_driver::params::{GROUPS, Requires, describe};
        let (trusted, available) = self
            .device
            .call(|s| {
                let available = GROUPS
                    .iter()
                    .map(|g| s.requirement_met(g.requires))
                    .collect::<Result<Vec<bool>>>()?;
                Ok((s.trusts_unverified(), available))
            })
            .await
            .unwrap_or_else(|_| {
                let plain = GROUPS.iter().map(|g| g.requires == Requires::Nothing);
                (false, plain.collect())
            });
        Ok(GROUPS
            .iter()
            .zip(available)
            .map(|(g, available)| {
                let mut info = describe(g, available);
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
