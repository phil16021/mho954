use async_trait::async_trait;
use mho_driver::{
    AcquireState, AcquireUpdate, Action, ChannelState, ChannelUpdate, DriverError, Idn,
    Measurement, NetworkState, NetworkUpdate, OptionInfo, ScopeStatus, TimebaseState,
    TimebaseUpdate, TriggerState, TriggerUpdate, WaveMode, Waveform,
};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use utoipa::ToSchema;

use crate::live::LiveConfig;
use mho_driver::params::{GroupInfo, ParamMap};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum ErrorKind {
    InvalidArgument,
    /// The scope reported an error (SCPI error queue).
    Device,
    Timeout,
    /// Scope not connected / not reachable.
    Unavailable,
    /// Operation disabled by configuration (e.g. raw SCPI).
    Forbidden,
    /// Not possible in the scope's current state (e.g. RAW readout while running).
    Conflict,
    /// The scope answered in an unexpected format.
    Protocol,
    Internal,
}

#[derive(
    Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema, ToSchema, thiserror::Error,
)]
#[error("{message}")]
pub struct ApiError {
    pub kind: ErrorKind,
    pub message: String,
}

impl ApiError {
    pub fn new(kind: ErrorKind, message: impl Into<String>) -> Self {
        ApiError {
            kind,
            message: message.into(),
        }
    }

    pub fn invalid(message: impl Into<String>) -> Self {
        Self::new(ErrorKind::InvalidArgument, message)
    }

    pub fn unavailable(message: impl Into<String>) -> Self {
        Self::new(ErrorKind::Unavailable, message)
    }
}

impl From<DriverError> for ApiError {
    fn from(e: DriverError) -> Self {
        let kind = match &e {
            DriverError::Invalid(_) => ErrorKind::InvalidArgument,
            DriverError::Device(_) => ErrorKind::Device,
            DriverError::Unsupported(_) => ErrorKind::InvalidArgument,
            DriverError::Protocol(_) => ErrorKind::Protocol,
            DriverError::State(_) => ErrorKind::Conflict,
            DriverError::Scpi(s) if s.is_timeout() => ErrorKind::Timeout,
            DriverError::Scpi(mho_scpi::ScpiError::NotFound(_)) => ErrorKind::Unavailable,
            DriverError::Scpi(mho_scpi::ScpiError::Io(io))
                if io.kind() == std::io::ErrorKind::NotFound =>
            {
                ErrorKind::Unavailable
            }
            DriverError::Scpi(mho_scpi::ScpiError::Parse { .. }) => ErrorKind::Protocol,
            DriverError::Scpi(_) => ErrorKind::Unavailable,
        };
        ApiError::new(kind, e.to_string())
    }
}

pub type ApiResult<T> = Result<T, ApiError>;

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema, ToSchema)]
pub struct ServiceInfo {
    pub idn: Option<Idn>,
    /// `usbtmc:/dev/...`, `tcp:...`, `sim`.
    pub connection: String,
    pub connected: bool,
    pub raw_scpi_allowed: bool,
    pub version: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema, ToSchema)]
pub struct MeasureRequest {
    /// Measurement items, e.g. `["VPP", "FREQ"]` (see `/api/v1/measure/items`).
    pub items: Vec<String>,
    /// `CHAN1`..`CHAN4`, `MATH1`.., also `1`..`4`.
    pub source: String,
    /// Second source for delay/phase items.
    #[serde(default)]
    pub source2: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema, ToSchema, utoipa::IntoParams)]
pub struct StatsRequest {
    /// Measurement item, e.g. `VPP` (see `/api/v1/measure/items`).
    pub item: String,
    /// `CHAN1`..`CHAN4`, `MATH1`.., also `1`..`4`.
    pub source: String,
    /// Second source for delay/phase items.
    #[serde(default)]
    pub source2: Option<String>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, JsonSchema, ToSchema)]
pub struct CommandRequest {
    /// File path on the scope for save/load commands (e.g. `D:/bench1.stp`), or the index for
    /// indexed commands (`ref.save`: 1..10).
    #[serde(default)]
    pub path: Option<String>,
    /// Required for destructive commands (`reset`).
    #[serde(default)]
    pub confirm: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema, ToSchema)]
pub struct ScpiRequest {
    pub command: String,
    /// Timeout for queries in milliseconds (default 2000).
    #[serde(default)]
    pub timeout_ms: Option<u64>,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema, ToSchema)]
pub struct ScpiResponse {
    /// Response line for queries, `null` for commands.
    pub response: Option<String>,
}

/// Everything a frontend can do with the scope. Implemented locally ([`crate::LocalBackend`])
/// and over HTTP ([`crate::RemoteBackend`]).
#[async_trait]
pub trait ScopeApi: Send + Sync + 'static {
    async fn info(&self) -> ApiResult<ServiceInfo>;
    async fn status(&self) -> ApiResult<ScopeStatus>;
    async fn channel(&self, n: u8) -> ApiResult<ChannelState>;
    async fn set_channel(&self, n: u8, update: ChannelUpdate) -> ApiResult<ChannelState>;
    async fn timebase(&self) -> ApiResult<TimebaseState>;
    async fn set_timebase(&self, update: TimebaseUpdate) -> ApiResult<TimebaseState>;
    async fn trigger(&self) -> ApiResult<TriggerState>;
    async fn set_trigger(&self, update: TriggerUpdate) -> ApiResult<TriggerState>;
    async fn acquire(&self) -> ApiResult<AcquireState>;
    async fn set_acquire(&self, update: AcquireUpdate) -> ApiResult<AcquireState>;
    async fn action(&self, action: Action) -> ApiResult<()>;
    async fn measure(&self, request: MeasureRequest) -> ApiResult<Vec<Measurement>>;
    async fn waveform(&self, source: String, mode: WaveMode) -> ApiResult<Waveform>;
    async fn screenshot(&self) -> ApiResult<Vec<u8>>;
    async fn options(&self) -> ApiResult<Vec<OptionInfo>>;
    async fn install_option(&self, key: String) -> ApiResult<()>;
    async fn setup_save(&self) -> ApiResult<Vec<u8>>;
    async fn setup_load(&self, blob: Vec<u8>) -> ApiResult<()>;
    async fn scpi(&self, request: ScpiRequest) -> ApiResult<ScpiResponse>;
    /// Statistics (current/avg/min/max/deviation/count) of one measurement.
    async fn measure_stats(&self, request: StatsRequest) -> ApiResult<mho_driver::MeasureStats>;
    /// Reset statistics, clear all measurements or the counter's totalizer.
    async fn measure_action(&self, action: mho_driver::MeasureAction) -> ApiResult<()>;
    /// Hardware frequency counter (`None` = no valid reading).
    async fn counter_value(&self) -> ApiResult<Option<f64>>;
    /// Digital voltmeter (must be enabled via group `dvm`).
    async fn dvm_value(&self) -> ApiResult<Option<f64>>;
    /// Event table of protocol decoder `bus` (1..4); configure it with groups `bus`, `bus.*`.
    async fn decode_bus(&self, bus: u8) -> ApiResult<mho_dsp::DecodeTable>;
    /// Peak table of the scope's FFT on math channel `math` (1..4).
    async fn fft_peaks(&self, math: u8) -> ApiResult<Vec<mho_dsp::ScopePeak>>;
    /// Catalogue of read-only value sets (cursor values, mask/search/histogram results).
    async fn readouts(&self) -> ApiResult<Vec<mho_driver::ops::ReadoutInfo>>;
    /// Reads one value set (its mode must be active, e.g. cursor mode MANual).
    async fn readout(&self, id: String) -> ApiResult<ParamMap>;
    /// Catalogue of one-shot commands (save/load on the scope, resets, recording).
    async fn commands(&self) -> ApiResult<Vec<mho_driver::ops::CommandInfo>>;
    async fn run_command(&self, id: String, request: CommandRequest) -> ApiResult<()>;
    /// Digital channels D0..D15 through decoder `bus` (needs the logic probe).
    async fn logic_capture(&self, bus: u8) -> ApiResult<mho_driver::LogicCapture>;
    /// Software Bode sweep with the built-in function generator (AFG option).
    async fn bode_sweep(
        &self,
        request: mho_driver::BodeRequest,
    ) -> ApiResult<Vec<mho_driver::BodePoint>>;
    /// Hardware modules (logic analyzer, function generator).
    async fn modules(&self) -> ApiResult<mho_driver::Modules>;
    /// Catalogue of table-driven settings groups (trigger types, decoders, …).
    async fn groups(&self) -> ApiResult<Vec<GroupInfo>>;
    /// Reads a settings group (only groups verified on the instrument).
    async fn group_get(&self, id: String, instance: Option<u8>) -> ApiResult<ParamMap>;
    /// Writes parameters of a group; `activate` also selects it (e.g. the trigger type).
    /// Returns the new state if the group may be read.
    async fn group_set(
        &self,
        id: String,
        instance: Option<u8>,
        values: ParamMap,
        activate: bool,
    ) -> ApiResult<Option<ParamMap>>;
    async fn network(&self) -> ApiResult<NetworkState>;
    async fn set_network(&self, update: NetworkUpdate) -> ApiResult<NetworkState>;
    async fn live_config(&self) -> ApiResult<LiveConfig>;
    async fn set_live_config(&self, config: LiveConfig) -> ApiResult<LiveConfig>;
}
