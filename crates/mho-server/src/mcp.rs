//! MCP tools for LLM agents. Works against any [`ScopeApi`] (in-process or remote daemon).

use std::path::PathBuf;
use std::sync::Arc;

use base64::Engine;
use mho_driver::*;
use mho_dsp::Window;
use mho_service::{ApiError, MeasureRequest, ScopeApi, ScpiRequest};
use rmcp::handler::server::wrapper::Parameters;
use rmcp::model::{CallToolResult, ContentBlock};
use rmcp::{ErrorData, ServerHandler, tool, tool_handler, tool_router};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

type ToolResult = Result<CallToolResult, ErrorData>;

#[derive(Clone)]
pub struct McpScope {
    api: Arc<dyn ScopeApi>,
    capture_dir: PathBuf,
}

impl McpScope {
    pub fn new(api: Arc<dyn ScopeApi>, capture_dir: PathBuf) -> Self {
        McpScope { api, capture_dir }
    }
}

/// Runs CPU-heavy work off the async workers.
async fn blocking<T: Send + 'static>(
    f: impl FnOnce() -> Result<T, ApiError> + Send + 'static,
) -> Result<T, ApiError> {
    tokio::task::spawn_blocking(f)
        .await
        .map_err(|e| ApiError::new(mho_service::ErrorKind::Internal, e.to_string()))?
}

fn ok_json<T: Serialize>(v: &T) -> ToolResult {
    let text = serde_json::to_string_pretty(v)
        .map_err(|e| ErrorData::internal_error(e.to_string(), None))?;
    Ok(CallToolResult::success(vec![ContentBlock::text(text)]))
}

/// Scope/API errors are reported as tool errors so the model can read and react to them.
fn tool_error(e: ApiError) -> ToolResult {
    Ok(CallToolResult::error(vec![ContentBlock::text(format!(
        "{:?}: {}",
        e.kind, e.message
    ))]))
}

macro_rules! try_api {
    ($e:expr) => {
        match $e {
            Ok(v) => v,
            Err(err) => return tool_error(err),
        }
    };
}

#[derive(Debug, Deserialize, JsonSchema)]
pub struct ConfigureChannel {
    /// Channel number 1..4.
    pub channel: u8,
    #[serde(flatten)]
    pub settings: ChannelUpdate,
}

#[derive(Debug, Deserialize, JsonSchema)]
pub struct GeneratorParams {
    /// 1 = GI, 2 = GII.
    pub channel: u8,
    #[serde(flatten)]
    pub update: mho_driver::GeneratorUpdate,
}

#[derive(Debug, Deserialize, JsonSchema)]
pub struct RunControl {
    /// One of: run, stop, single, autoscale, clear, force (force a trigger).
    pub action: String,
}

#[derive(Debug, Deserialize, JsonSchema)]
pub struct SourceArg {
    /// CHAN1..CHAN4 (or 1..4).
    pub source: String,
}

#[derive(Debug, Deserialize, JsonSchema)]
pub struct GetWaveform {
    /// CHAN1..CHAN4 (or 1..4).
    pub source: String,
    /// Read the full acquisition memory instead of the ~1000-point screen record.
    /// The scope must be stopped (use run_control stop/single first).
    #[serde(default)]
    pub raw: bool,
    /// Maximum number of (time, volts) points returned inline, min/max decimated. Default 200, max 2000.
    pub max_points: Option<usize>,
    /// Save the complete record as a file in the capture directory: a plain file name
    /// ending in `.csv` (time + volts) or `.npy` (raw ADC codes; preamble saved as `.json`).
    pub save_as: Option<String>,
}

#[derive(Debug, Deserialize, JsonSchema)]
pub struct FftArgs {
    /// CHAN1..CHAN4 (or 1..4).
    pub source: String,
    /// Number of spectral peaks to report (default 5).
    pub count: Option<usize>,
    /// rectangular | hann (default) | blackman | flattop (best amplitude accuracy).
    pub window: Option<Window>,
    /// Use the full acquisition memory (scope must be stopped) for finer resolution.
    #[serde(default)]
    pub raw: bool,
}

#[derive(Debug, Deserialize, JsonSchema)]
pub struct DecodeArgs {
    /// Decoder bus 1..4.
    pub bus: u8,
    /// Return at most this many events (default 200).
    #[serde(default)]
    pub max_rows: Option<usize>,
}

#[derive(Debug, Deserialize, JsonSchema)]
pub struct ReadoutArg {
    /// cursor.manual | cursor.track | mask | search | histogram | save | afg.arb
    pub id: String,
    /// Switch the mode on first if it is off (histogram, mask test, search, cursor mode).
    #[serde(default)]
    pub enable: bool,
}

#[derive(Debug, Deserialize, JsonSchema)]
pub struct CommandArgs {
    /// Command id, see the tool description.
    pub id: String,
    /// File on the scope for save/load: C:/ (internal) or D:/ (USB stick), e.g. D:/bench1.stp.
    #[serde(default)]
    pub path: Option<String>,
    /// Required for `reset`; only set it after the user explicitly agreed.
    #[serde(default)]
    pub confirm: bool,
}

#[derive(Debug, Deserialize, JsonSchema)]
pub struct LogicArgs {
    /// Decoder bus 1..4 used for the capture (it is reconfigured as parallel bus).
    pub bus: u8,
    /// Return at most this many events (default 500).
    #[serde(default)]
    pub max_events: Option<usize>,
    /// Also save a VCD file under this name in the capture directory.
    #[serde(default)]
    pub save_as: Option<String>,
}

#[derive(Debug, Deserialize, JsonSchema)]
pub struct MathArg {
    /// Math channel 1..4.
    pub math: u8,
}

#[derive(Debug, Deserialize, JsonSchema)]
pub struct ActionArg {
    /// reset_statistics | clear_all | clear_totalizer
    pub action: String,
}

#[derive(Debug, Deserialize, JsonSchema)]
pub struct GroupsArgs {
    /// Only groups whose id starts with this, e.g. `trigger` or `bus`.
    pub prefix: Option<String>,
}

#[derive(Debug, Deserialize, JsonSchema)]
pub struct GroupArgs {
    /// Group id from list_settings_groups, e.g. `trigger.pulse`.
    pub group: String,
    /// Instance for repeated groups (decoder 1..4, math 1..4).
    pub instance: Option<u8>,
}

#[derive(Debug, Deserialize, JsonSchema)]
pub struct ConfigureGroup {
    /// Group id from list_settings_groups, e.g. `trigger.pulse`.
    pub group: String,
    pub instance: Option<u8>,
    /// Parameter values by name, e.g. {"polarity": "NEGative", "lower": 2e-6}.
    pub values: mho_driver::params::ParamMap,
    /// Also activate the group (e.g. make this the active trigger type).
    #[serde(default)]
    pub activate: bool,
}

#[derive(Serialize)]
struct WaveformSummary {
    source: String,
    points: usize,
    sample_interval_s: f64,
    t_start_s: f64,
    t_end_s: f64,
    /// `false`: the scope delivered no data for this channel (all samples 0xFFFF).
    has_data: bool,
    measurements: mho_dsp::Measurements,
    /// `[time_s, volts]` pairs (min/max decimated).
    samples: Vec<[f64; 2]>,
    saved_to: Option<String>,
}

#[tool_router]
impl McpScope {
    #[tool(
        description = "Identify the connected oscilloscope (model, serial, firmware) and the connection state."
    )]
    async fn identify(&self) -> ToolResult {
        ok_json(&try_api!(self.api.info().await))
    }

    #[tool(
        description = "Read the complete scope state: all 4 channels (on/off, V/div, offset, coupling, \
        bandwidth limit, probe ratio, 50Ω/1MΩ), timebase, trigger and acquisition settings, trigger status."
    )]
    async fn get_status(&self) -> ToolResult {
        ok_json(&try_api!(self.api.status().await))
    }

    #[tool(
        description = "Change settings of one analog channel. Only the given fields are changed. \
        scale is volts/div, offset in volts, coupling DC|AC|GND, impedance OneMeg|Fifty, bw_limit OFF|20M|250M."
    )]
    async fn configure_channel(&self, Parameters(p): Parameters<ConfigureChannel>) -> ToolResult {
        ok_json(&try_api!(self.api.set_channel(p.channel, p.settings).await))
    }

    #[tool(
        description = "Change the timebase: scale in seconds/div, offset (trigger position) in seconds, mode MAIN|XY|ROLL."
    )]
    async fn configure_timebase(&self, Parameters(p): Parameters<TimebaseUpdate>) -> ToolResult {
        ok_json(&try_api!(self.api.set_timebase(p).await))
    }

    #[tool(
        description = "Change the edge trigger: edge_source (CHAN1..4), edge_slope (Positive|Negative|Either), \
        edge_level in volts, sweep (Auto|Normal|Single), coupling, holdoff in seconds."
    )]
    async fn configure_trigger(&self, Parameters(p): Parameters<TriggerUpdate>) -> ToolResult {
        ok_json(&try_api!(self.api.set_trigger(p).await))
    }

    #[tool(
        description = "Change acquisition: memory_depth (AUTO, 1k, 10k, 100k, 1M, 10M, 25M, 50M, 100M, 125M, \
        200M, 250M, 500M; the maximum depends on the active channels and RLU05: 500M/250M/125M with \
        1/2/3-4 channels, see get_capabilities), acq_type (Normal|Peak|Average|HighRes), averages \
        (power of two). Deep records take minutes to download (~1.3 MB/s over USB)."
    )]
    async fn configure_acquisition(&self, Parameters(p): Parameters<AcquireUpdate>) -> ToolResult {
        ok_json(&try_api!(self.api.set_acquire(p).await))
    }

    #[tool(
        description = "Front-panel actions: run, stop, single (arm one acquisition), autoscale, clear, force (force trigger)."
    )]
    async fn run_control(&self, Parameters(p): Parameters<RunControl>) -> ToolResult {
        let action = try_api!(Action::from_scpi(&p.action).map_err(ApiError::from));
        try_api!(self.api.action(action).await);
        ok_json(&serde_json::json!({ "done": p.action }))
    }

    #[tool(
        description = "Scope-side automatic measurements, e.g. items [\"VPP\",\"FREQ\",\"VRMS\",\"RTIM\"] on source CHAN1. \
        Delay/phase items (RRDELAY, RRPHASE, ...) need source2. A null value means the scope cannot measure it \
        on the current signal. See list_measurement_items for all items."
    )]
    async fn measure(&self, Parameters(p): Parameters<MeasureRequest>) -> ToolResult {
        ok_json(&try_api!(self.api.measure(p).await))
    }

    #[tool(description = "List all measurement items supported by the measure tool, with units.")]
    async fn list_measurement_items(&self) -> ToolResult {
        ok_json(&MEASURE_ITEMS)
    }

    #[tool(
        description = "Compute measurements locally from the screen waveform (Vpp, Vrms, mean, top/base, \
        frequency, duty, rise/fall time, overshoot). Fast and independent of the scope's measurement engine."
    )]
    async fn analyze_waveform(&self, Parameters(p): Parameters<SourceArg>) -> ToolResult {
        let w = try_api!(self.api.waveform(p.source, WaveMode::Normal).await);
        ok_json(&serde_json::json!({
            "source": w.source,
            "points": w.raw.len(),
            "has_data": w.has_data(),
            "note": if w.has_data() { "" } else { "the scope reports no data for this channel (all samples 0xFFFF)" },
            "measurements": w.analyze(),
        }))
    }

    #[tool(
        description = "Fetch a waveform as (time, volts) samples plus local measurements. Inline data is \
        decimated to max_points; use save_as to store the complete record as CSV or NPY."
    )]
    async fn get_waveform(&self, Parameters(p): Parameters<GetWaveform>) -> ToolResult {
        let mode = if p.raw {
            WaveMode::Raw
        } else {
            WaveMode::Normal
        };
        let w = try_api!(self.api.waveform(p.source, mode).await);
        // Conversion, decimation and file output can be heavy for RAW records.
        let this = self.clone();
        let summary = blocking(move || {
            let volts = w.volts();
            let saved_to = match &p.save_as {
                Some(name) => Some(this.save(name, &w, &volts)?),
                None => None,
            };
            let max = p.max_points.unwrap_or(200).clamp(10, 2000);
            let buckets = mho_dsp::minmax_decimate(&volts, max / 2);
            let samples = mho_dsp::decimate::minmax_polyline(&buckets)
                .into_iter()
                .map(|(i, v)| [w.preamble.time(i), v])
                .collect();
            Ok(WaveformSummary {
                source: w.source.clone(),
                points: w.raw.len(),
                sample_interval_s: w.preamble.x_increment,
                t_start_s: w.preamble.time(0),
                t_end_s: w.preamble.time(w.raw.len().saturating_sub(1)),
                has_data: w.has_data(),
                measurements: w.analyze(),
                samples,
                saved_to,
            })
        })
        .await;
        ok_json(&try_api!(summary))
    }

    #[tool(
        description = "Spectrum analysis (FFT) of a channel: returns the strongest peaks with frequency, \
        level in dBV and RMS volts, plus the bin spacing."
    )]
    async fn fft_peaks(&self, Parameters(p): Parameters<FftArgs>) -> ToolResult {
        let mode = if p.raw {
            WaveMode::Raw
        } else {
            WaveMode::Normal
        };
        let w = try_api!(self.api.waveform(p.source, mode).await);
        let result = blocking(move || {
            let spec = mho_dsp::spectrum(
                &w.volts(),
                w.preamble.x_increment,
                p.window.unwrap_or_default(),
            );
            Ok(serde_json::json!({
                "source": w.source,
                "bin_spacing_hz": spec.df,
                "max_frequency_hz": spec.df * spec.dbv.len() as f64,
                "peaks": spec.peaks(p.count.unwrap_or(5).clamp(1, 50)),
            }))
        })
        .await;
        ok_json(&try_api!(result))
    }

    #[tool(description = "Capture the scope's screen as a PNG image.")]
    async fn screenshot(&self) -> ToolResult {
        let png = try_api!(self.api.screenshot().await);
        let data = base64::engine::general_purpose::STANDARD.encode(png);
        Ok(CallToolResult::success(vec![ContentBlock::image(
            data,
            "image/png",
        )]))
    }

    #[tool(
        description = "Read the scope's LAN (RJ45) settings and link status: DHCP/Auto-IP/static mode, \
        IP, mask, gateway, DNS, MAC, hostname, mDNS, VISA address."
    )]
    async fn get_network(&self) -> ToolResult {
        ok_json(&try_api!(self.api.network().await))
    }

    #[tool(
        description = "Change the scope's LAN (RJ45) settings and apply them. mode: Dhcp | AutoIp | Static; \
        ip/mask/gateway/dns need Static. Ask the user before changing the network of their instrument; \
        if the scope is controlled over LAN the connection may drop."
    )]
    async fn configure_network(&self, Parameters(p): Parameters<NetworkUpdate>) -> ToolResult {
        ok_json(&try_api!(self.api.set_network(p).await))
    }

    #[tool(
        description = "List settings groups beyond the basic channel/timebase/edge trigger: all trigger \
        types (pulse, slope, runt, window, timeout, pattern, delay, setup/hold, nth edge, RS232, I2C, SPI, CAN, \
        LIN), measurement setup, counter, voltmeter, decoders, math, cursors, display… with each parameter's \
        range and accepted values. `verified` groups can be read; `available` tells whether the hardware exists."
    )]
    async fn list_settings_groups(&self, Parameters(p): Parameters<GroupsArgs>) -> ToolResult {
        let mut groups = try_api!(self.api.groups().await);
        if let Some(prefix) = p.prefix {
            groups.retain(|g| g.id.starts_with(&prefix));
        }
        ok_json(&groups)
    }

    #[tool(description = "Read the current values of a settings group (see list_settings_groups).")]
    async fn get_settings(&self, Parameters(p): Parameters<GroupArgs>) -> ToolResult {
        ok_json(&try_api!(self.api.group_get(p.group, p.instance).await))
    }

    #[tool(
        description = "Change parameters of a settings group, e.g. group `trigger.pulse` with values \
        {\"source\": \"CHAN1\", \"when\": \"GREater\", \"lower\": 1e-6} and activate=true to make it the \
        active trigger. Values are validated against the ranges from list_settings_groups."
    )]
    async fn configure_settings(&self, Parameters(p): Parameters<ConfigureGroup>) -> ToolResult {
        ok_json(&try_api!(
            self.api
                .group_set(p.group, p.instance, p.values, p.activate)
                .await
        ))
    }

    #[tool(
        description = "Statistics of one scope measurement over many acquisitions: current, average, \
        minimum, maximum, standard deviation and count, e.g. item VPP on CHAN1. Values are null while invalid."
    )]
    async fn measurement_statistics(
        &self,
        Parameters(p): Parameters<mho_service::StatsRequest>,
    ) -> ToolResult {
        ok_json(&try_api!(self.api.measure_stats(p).await))
    }

    #[tool(
        description = "Measurement housekeeping: action reset_statistics, clear_all (remove all \
        measurements from the screen) or clear_totalizer (frequency counter)."
    )]
    async fn measurement_action(&self, Parameters(p): Parameters<ActionArg>) -> ToolResult {
        let action =
            try_api!(mho_driver::MeasureAction::from_scpi(&p.action).map_err(ApiError::from));
        try_api!(self.api.measure_action(action).await);
        ok_json(&serde_json::json!({ "done": p.action }))
    }

    #[tool(
        description = "Read the hardware frequency counter (configure it with group `counter`; null = no \
        valid reading)."
    )]
    async fn read_counter(&self) -> ToolResult {
        ok_json(&serde_json::json!({ "value": try_api!(self.api.counter_value().await) }))
    }

    #[tool(
        description = "Read the decoded events of protocol decoder bus 1..4 (time in seconds plus \
        the columns the scope reports, e.g. TX/RX, address/data, ID/data). Set the decoder up first \
        with configure_settings: group `bus.rs232`, `bus.i2c`, `bus.spi`, `bus.can`, `bus.lin` or \
        `bus.parallel` with activate=true (switches the decoder on), and group `bus` for the \
        number format."
    )]
    async fn decode_bus(&self, Parameters(p): Parameters<DecodeArgs>) -> ToolResult {
        let mut table = try_api!(self.api.decode_bus(p.bus).await);
        let total = table.rows.len();
        table.rows.truncate(p.max_rows.unwrap_or(200));
        ok_json(&serde_json::json!({
            "bus": p.bus,
            "total_events": total,
            "table": table,
        }))
    }

    #[tool(
        description = "Read the peak table of the scope's own FFT on math channel 1..4 (frequency in \
        Hz, amplitude in dBV or V). Set it up first with configure_settings group `math.fft`, \
        activate=true, values {source, window, span, center, peak_search: true, peaks}. \
        fft_peaks computes a spectrum here from waveform data instead."
    )]
    async fn scope_fft_peaks(&self, Parameters(p): Parameters<MathArg>) -> ToolResult {
        ok_json(&try_api!(self.api.fft_peaks(p.math).await))
    }

    #[tool(
        description = "Read a set of values from the scope: cursor.manual / cursor.track (cursor \
        positions and deltas; set group `cursor.manual`/`cursor.track` with activate=true first), mask \
        (pass/fail counts; group `mask`), search (number of found events; group `search`), histogram \
        (statistics; group `histogram`), save (1 = last save finished), afg.arb (loaded arbitrary \
        files). enable=true switches an inactive mode on first (changes the scope's display)."
    )]
    async fn read_values(&self, Parameters(p): Parameters<ReadoutArg>) -> ToolResult {
        ok_json(&try_api!(self.api.readout(p.id, p.enable).await))
    }

    #[tool(
        description = "Run a one-shot command on the scope. Files on the scope (path C:/… internal or \
        D:/… USB stick, name ≤ 26 characters): save.setup (.stp), save.waveform (.csv/.bin/.wfm), \
        save.memory (.bin/.csv), save.image (.png/.bmp/.jpg), save.mask (.pf), load.setup (.stp), \
        load.mask (.pf), histogram.save (.csv). Without path: display.clear, mask.create, mask.reset, \
        histogram.reset, record.start, record.stop, record.play, record.pause. reset (*RST, factory \
        defaults) changes every setting: ask the user first and pass confirm=true only after they agreed."
    )]
    async fn run_scope_command(&self, Parameters(p): Parameters<CommandArgs>) -> ToolResult {
        try_api!(
            self.api
                .run_command(
                    p.id.clone(),
                    mho_service::CommandRequest {
                        path: p.path,
                        confirm: p.confirm,
                    },
                )
                .await
        );
        ok_json(&serde_json::json!({ "done": p.id }))
    }

    #[tool(
        description = "Capture the digital channels D0..D15 (logic probe needed) through decoder \
        `bus`, returned as value changes (bit i = Di). save_as writes a VCD file for PulseView/GTKWave \
        into the capture directory. Configure thresholds/channels with groups `la`, `la.channel`."
    )]
    async fn logic_capture(&self, Parameters(p): Parameters<LogicArgs>) -> ToolResult {
        let cap = try_api!(self.api.logic_capture(p.bus).await);
        let mut saved = None;
        if let Some(name) = &p.save_as {
            let vcd = mho_dsp::write_vcd(&cap.events, &cap.lines);
            saved = Some(try_api!(self.save_file(name, vcd.as_bytes(), &["vcd"])));
        }
        let total = cap.events.len();
        let events: Vec<_> = cap
            .events
            .iter()
            .take(p.max_events.unwrap_or(500))
            .collect();
        ok_json(&serde_json::json!({
            "bus": cap.bus,
            "lines": cap.lines,
            "total_events": total,
            "events": events,
            "saved_to": saved,
        }))
    }

    #[tool(
        description = "Measure a frequency response (Bode plot) with the built-in function generator \
        (AFG option): sine sweep from start to stop Hz (points, log spaced), generator 1|2, input/output \
        = channel numbers probing DUT input and output, amplitude in Vpp. Returns gain in dB and phase \
        in degrees per frequency. Switches the generator output on during the sweep: make sure the \
        user expects a signal on the generator output."
    )]
    async fn bode_sweep(&self, Parameters(p): Parameters<mho_driver::BodeRequest>) -> ToolResult {
        ok_json(&try_api!(self.api.bode_sweep(p).await))
    }

    #[tool(
        description = "Set the built-in function generator (AFG option) channel 1 (GI) or 2 (GII): \
        waveform (SIN, SQU, RAMP, NOIS, DC, ARB, EXPR, EXPF, ECG1, GAUS, LOR, HAV, SINC), frequency \
        Hz, amplitude Vpp, offset V, phase °, duty/symmetry %, load (highz or 50), modulation (am, \
        fm, pm, off with mod_depth/mod_deviation/mod_frequency/mod_wave) and output on/off. Only \
        given fields change; limits depend on waveform, load and AFG50/AFG100 (see \
        get_capabilities). output=true drives a real signal into whatever is connected to the \
        generator: only switch it on when the user asked for it."
    )]
    async fn set_generator(&self, Parameters(p): Parameters<GeneratorParams>) -> ToolResult {
        let state = try_api!(self.api.set_generator(p.channel, p.update).await);
        ok_json(&serde_json::json!({ "channel": p.channel, "state": state }))
    }

    #[tool(
        description = "Run the scope's own Bode plot (AFG option) from start to stop Hz (10 Hz..30 MHz, \
        start <= stop/10) with input/output channel numbers, points per decade and sweep amplitude, \
        and return a screenshot of the finished plot. The scope cannot export the curve; use \
        bode_sweep for numbers. Drives only generator GI during the sweep. keep_on_screen=true \
        leaves the plot on the scope display (Bode mode on, channels stay AC/1 mV/div); otherwise \
        Bode mode is switched off and the channels restored."
    )]
    async fn bode_native(
        &self,
        Parameters(p): Parameters<mho_driver::NativeBodeRequest>,
    ) -> ToolResult {
        let png = try_api!(self.api.bode_native(p).await);
        let data = base64::engine::general_purpose::STANDARD.encode(png);
        Ok(CallToolResult::success(vec![ContentBlock::image(
            data,
            "image/png",
        )]))
    }

    #[tool(description = "Read the digital voltmeter (enable/configure it with group `dvm`).")]
    async fn read_dvm(&self) -> ToolResult {
        ok_json(&serde_json::json!({ "value": try_api!(self.api.dvm_value().await) }))
    }

    #[tool(
        description = "List the scope's software options (bandwidth upgrades, memory, AFG, decoders) and whether each is active."
    )]
    async fn list_options(&self) -> ToolResult {
        ok_json(&try_api!(self.api.options().await))
    }

    #[tool(
        description = "What this scope can do: model, effective analog bandwidth after upgrades (1-2 \
        and 3-4 channels), maximum memory depth per number of active channels, whether the built-in \
        function generator is usable (and its max sine frequency), and which optional protocol \
        decoders/triggers (CAN-FD, FlexRay, I2S, MIL-STD-1553) are licensed. Check this before using \
        generator, Bode or optional decoder groups."
    )]
    async fn get_capabilities(&self) -> ToolResult {
        ok_json(&try_api!(self.api.capabilities().await))
    }

    #[tool(
        description = "Send a raw SCPI command or query (MHO900 programming guide syntax). Queries contain '?'. \
        Only available if the server was started with --allow-raw-scpi. Only send commands documented \
        in the MHO900 programming guide: the scope does not answer unknown queries, and several of them \
        in a row hang its firmware until it is power-cycled."
    )]
    async fn raw_scpi(&self, Parameters(p): Parameters<ScpiRequest>) -> ToolResult {
        ok_json(&try_api!(self.api.scpi(p).await))
    }
}

/// Plain file name inside the capture directory (no paths, no hidden files).
fn valid_name(name: &str) -> bool {
    !name.is_empty()
        && !name.starts_with('.')
        && name
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.'))
}

fn io_error(e: std::io::Error) -> ApiError {
    ApiError::new(mho_service::ErrorKind::Internal, e.to_string())
}

/// Never overwrites (and never follows a planted symlink): `create_new` fails if anything
/// exists at the path.
fn create_new(p: &std::path::Path) -> Result<std::fs::File, ApiError> {
    std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(p)
        .map_err(|e| {
            if e.kind() == std::io::ErrorKind::AlreadyExists {
                ApiError::invalid(format!(
                    "{} already exists; choose another name",
                    p.display()
                ))
            } else {
                io_error(e)
            }
        })
}

impl McpScope {
    /// Writes `data` to a new file `name` (one of `extensions`) in the capture directory.
    fn save_file(&self, name: &str, data: &[u8], extensions: &[&str]) -> Result<String, ApiError> {
        let ext_ok = extensions
            .iter()
            .any(|e| name.to_ascii_lowercase().ends_with(&format!(".{e}")));
        if !valid_name(name) || !ext_ok {
            return Err(ApiError::invalid(format!(
                "save_as must be a plain file name ending in .{}",
                extensions.join(" or .")
            )));
        }
        std::fs::create_dir_all(&self.capture_dir).map_err(io_error)?;
        let path = self.capture_dir.join(name);
        use std::io::Write;
        create_new(&path)?.write_all(data).map_err(io_error)?;
        Ok(path.display().to_string())
    }

    fn save(&self, name: &str, w: &Waveform, volts: &[f64]) -> Result<String, ApiError> {
        if !valid_name(name) {
            return Err(ApiError::invalid(
                "save_as must be a plain file name like capture1.csv",
            ));
        }
        const MAX_POINTS: usize = 200_000_000;
        if w.raw.len() > MAX_POINTS {
            return Err(ApiError::invalid(format!(
                "record too large to save via MCP ({} points)",
                w.raw.len()
            )));
        }
        let io = io_error;
        std::fs::create_dir_all(&self.capture_dir).map_err(io)?;
        let path = self.capture_dir.join(name);
        let create = create_new;
        if name.ends_with(".csv") {
            let file = create(&path)?;
            mho_dsp::export::write_csv(
                std::io::BufWriter::new(file),
                w.preamble.time(0),
                w.preamble.x_increment,
                &[mho_dsp::export::CsvColumn {
                    name: &w.source,
                    volts,
                }],
            )
            .map_err(io)?;
        } else if name.ends_with(".npy") {
            let meta_path = path.with_extension("json");
            let mut npy = create(&path)?;
            let mut meta_file = create(&meta_path)?;
            use std::io::Write;
            npy.write_all(&mho_dsp::export::npy_bytes(&w.raw).map_err(io)?)
                .map_err(io)?;
            let meta = serde_json::json!({ "source": w.source, "preamble": w.preamble });
            meta_file
                .write_all(&serde_json::to_vec_pretty(&meta).unwrap())
                .map_err(io)?;
        } else {
            return Err(ApiError::invalid("save_as must end in .csv or .npy"));
        }
        Ok(path.display().to_string())
    }
}

#[tool_handler(
    name = "mho954",
    instructions = "Controls a Rigol MHO954 oscilloscope (4 channels, 12-bit). Start with get_status to see the \
        current setup. Prefer measure (scope-side) or analyze_waveform (local) over pulling raw samples. \
        get_waveform returns decimated samples; raw=true needs a stopped scope. screenshot shows the display."
)]
impl ServerHandler for McpScope {}
