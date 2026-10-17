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

fn ok_json<T: Serialize>(v: &T) -> ToolResult {
    let text = serde_json::to_string_pretty(v).map_err(|e| ErrorData::internal_error(e.to_string(), None))?;
    Ok(CallToolResult::success(vec![ContentBlock::text(text)]))
}

/// Scope/API errors are reported as tool errors so the model can read and react to them.
fn tool_error(e: ApiError) -> ToolResult {
    Ok(CallToolResult::error(vec![ContentBlock::text(format!("{:?}: {}", e.kind, e.message))]))
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

#[derive(Serialize)]
struct WaveformSummary {
    source: String,
    points: usize,
    sample_interval_s: f64,
    t_start_s: f64,
    t_end_s: f64,
    measurements: mho_dsp::Measurements,
    /// `[time_s, volts]` pairs (min/max decimated).
    samples: Vec<[f64; 2]>,
    saved_to: Option<String>,
}

#[tool_router]
impl McpScope {
    #[tool(description = "Identify the connected oscilloscope (model, serial, firmware) and the connection state.")]
    async fn identify(&self) -> ToolResult {
        ok_json(&try_api!(self.api.info().await))
    }

    #[tool(description = "Read the complete scope state: all 4 channels (on/off, V/div, offset, coupling, \
        bandwidth limit, probe ratio, 50Ω/1MΩ), timebase, trigger and acquisition settings, trigger status.")]
    async fn get_status(&self) -> ToolResult {
        ok_json(&try_api!(self.api.status().await))
    }

    #[tool(description = "Change settings of one analog channel. Only the given fields are changed. \
        scale is volts/div, offset in volts, coupling DC|AC|GND, impedance OneMeg|Fifty, bw_limit OFF|20M|250M.")]
    async fn configure_channel(&self, Parameters(p): Parameters<ConfigureChannel>) -> ToolResult {
        ok_json(&try_api!(self.api.set_channel(p.channel, p.settings).await))
    }

    #[tool(description = "Change the timebase: scale in seconds/div, offset (trigger position) in seconds, mode MAIN|XY|ROLL.")]
    async fn configure_timebase(&self, Parameters(p): Parameters<TimebaseUpdate>) -> ToolResult {
        ok_json(&try_api!(self.api.set_timebase(p).await))
    }

    #[tool(description = "Change the edge trigger: edge_source (CHAN1..4), edge_slope (Positive|Negative|Either), \
        edge_level in volts, sweep (Auto|Normal|Single), coupling, holdoff in seconds.")]
    async fn configure_trigger(&self, Parameters(p): Parameters<TriggerUpdate>) -> ToolResult {
        ok_json(&try_api!(self.api.set_trigger(p).await))
    }

    #[tool(description = "Change acquisition: memory_depth (AUTO, 1k, 10k, 100k, 1M, 10M, 25M, 50M, 100M; up to 500M with RLU-05), \
        acq_type (Normal|Peak|Average|HighRes), averages (power of two).")]
    async fn configure_acquisition(&self, Parameters(p): Parameters<AcquireUpdate>) -> ToolResult {
        ok_json(&try_api!(self.api.set_acquire(p).await))
    }

    #[tool(description = "Front-panel actions: run, stop, single (arm one acquisition), autoscale, clear, force (force trigger).")]
    async fn run_control(&self, Parameters(p): Parameters<RunControl>) -> ToolResult {
        let action = try_api!(Action::from_scpi(&p.action).map_err(ApiError::from));
        try_api!(self.api.action(action).await);
        ok_json(&serde_json::json!({ "done": p.action }))
    }

    #[tool(description = "Scope-side automatic measurements, e.g. items [\"VPP\",\"FREQ\",\"VRMS\",\"RTIM\"] on source CHAN1. \
        Delay/phase items (RRDELAY, RRPHASE, ...) need source2. A null value means the scope cannot measure it \
        on the current signal. See list_measurement_items for all items.")]
    async fn measure(&self, Parameters(p): Parameters<MeasureRequest>) -> ToolResult {
        ok_json(&try_api!(self.api.measure(p).await))
    }

    #[tool(description = "List all measurement items supported by the measure tool, with units.")]
    async fn list_measurement_items(&self) -> ToolResult {
        ok_json(&MEASURE_ITEMS)
    }

    #[tool(description = "Compute measurements locally from the screen waveform (Vpp, Vrms, mean, top/base, \
        frequency, duty, rise/fall time, overshoot). Fast and independent of the scope's measurement engine.")]
    async fn analyze_waveform(&self, Parameters(p): Parameters<SourceArg>) -> ToolResult {
        let w = try_api!(self.api.waveform(p.source, WaveMode::Normal).await);
        ok_json(&serde_json::json!({ "source": w.source, "points": w.raw.len(), "measurements": w.analyze() }))
    }

    #[tool(description = "Fetch a waveform as (time, volts) samples plus local measurements. Inline data is \
        decimated to max_points; use save_as to store the complete record as CSV or NPY.")]
    async fn get_waveform(&self, Parameters(p): Parameters<GetWaveform>) -> ToolResult {
        let mode = if p.raw { WaveMode::Raw } else { WaveMode::Normal };
        let w = try_api!(self.api.waveform(p.source, mode).await);
        let volts = w.volts();
        let saved_to = match &p.save_as {
            Some(name) => Some(try_api!(self.save(name, &w, &volts))),
            None => None,
        };
        let max = p.max_points.unwrap_or(200).clamp(10, 2000);
        let buckets = mho_dsp::minmax_decimate(&volts, max / 2);
        let samples = mho_dsp::decimate::minmax_polyline(&buckets)
            .into_iter()
            .map(|(i, v)| [w.preamble.time(i), v])
            .collect();
        ok_json(&WaveformSummary {
            source: w.source.clone(),
            points: w.raw.len(),
            sample_interval_s: w.preamble.x_increment,
            t_start_s: w.preamble.time(0),
            t_end_s: w.preamble.time(w.raw.len().saturating_sub(1)),
            measurements: mho_dsp::analyze(&volts, w.preamble.x_increment),
            samples,
            saved_to,
        })
    }

    #[tool(description = "Spectrum analysis (FFT) of a channel: returns the strongest peaks with frequency, \
        level in dBV and RMS volts, plus the bin spacing.")]
    async fn fft_peaks(&self, Parameters(p): Parameters<FftArgs>) -> ToolResult {
        let mode = if p.raw { WaveMode::Raw } else { WaveMode::Normal };
        let w = try_api!(self.api.waveform(p.source, mode).await);
        let spec = mho_dsp::spectrum(&w.volts(), w.preamble.x_increment, p.window.unwrap_or_default());
        ok_json(&serde_json::json!({
            "source": w.source,
            "bin_spacing_hz": spec.df,
            "max_frequency_hz": spec.df * spec.dbv.len() as f64,
            "peaks": spec.peaks(p.count.unwrap_or(5).clamp(1, 50)),
        }))
    }

    #[tool(description = "Capture the scope's screen as a PNG image.")]
    async fn screenshot(&self) -> ToolResult {
        let png = try_api!(self.api.screenshot().await);
        let data = base64::engine::general_purpose::STANDARD.encode(png);
        Ok(CallToolResult::success(vec![ContentBlock::image(data, "image/png")]))
    }

    #[tool(description = "List the scope's software options (bandwidth upgrades, memory, AFG, decoders) and whether each is active.")]
    async fn list_options(&self) -> ToolResult {
        ok_json(&try_api!(self.api.options().await))
    }

    #[tool(description = "Send a raw SCPI command or query (MHO900 programming guide syntax). Queries contain '?'. \
        Only available if the server was started with --allow-raw-scpi. Unknown queries time out.")]
    async fn raw_scpi(&self, Parameters(p): Parameters<ScpiRequest>) -> ToolResult {
        ok_json(&try_api!(self.api.scpi(p).await))
    }
}

impl McpScope {
    fn save(&self, name: &str, w: &Waveform, volts: &[f64]) -> Result<String, ApiError> {
        let valid = !name.is_empty()
            && !name.starts_with('.')
            && name.chars().all(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.'));
        if !valid {
            return Err(ApiError::invalid("save_as must be a plain file name like capture1.csv"));
        }
        let io = |e: std::io::Error| ApiError::new(mho_service::ErrorKind::Internal, e.to_string());
        std::fs::create_dir_all(&self.capture_dir).map_err(io)?;
        let path = self.capture_dir.join(name);
        if name.ends_with(".csv") {
            let file = std::fs::File::create(&path).map_err(io)?;
            mho_dsp::export::write_csv(
                std::io::BufWriter::new(file),
                w.preamble.time(0),
                w.preamble.x_increment,
                &[mho_dsp::export::CsvColumn { name: &w.source, volts }],
            )
            .map_err(io)?;
        } else if name.ends_with(".npy") {
            mho_dsp::export::write_npy(&path, &w.raw).map_err(io)?;
            let meta = serde_json::json!({ "source": w.source, "preamble": w.preamble });
            std::fs::write(path.with_extension("json"), serde_json::to_vec_pretty(&meta).unwrap()).map_err(io)?;
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
