use axum::Json;
use axum::body::Bytes;
use axum::extract::{Path, Query, State};
use axum::http::{StatusCode, header};
use axum::response::{Html, IntoResponse, Response};
use axum::routing::{get, post, put};
use mho_driver::params::{GroupInfo, ParamMap};
use mho_driver::*;
use mho_dsp::{Measurements, SpectrumPeak, Window};
use mho_service::live::{LiveConfig, encode_frame};
use mho_service::{
    ApiError, ErrorKind, Frame, MeasureRequest, ScopeApi, ScpiRequest, ScpiResponse, ServiceInfo,
    StatsRequest,
};
use serde::{Deserialize, Serialize};
use utoipa::{IntoParams, OpenApi, ToSchema};

use crate::AppState;

type Router = axum::Router<AppState>;

/// [`ApiError`] as HTTP response.
pub struct HttpError(ApiError);

impl From<ApiError> for HttpError {
    fn from(e: ApiError) -> Self {
        HttpError(e)
    }
}

impl IntoResponse for HttpError {
    fn into_response(self) -> Response {
        let status = match self.0.kind {
            ErrorKind::InvalidArgument => StatusCode::BAD_REQUEST,
            ErrorKind::Device => StatusCode::UNPROCESSABLE_ENTITY,
            ErrorKind::Timeout => StatusCode::GATEWAY_TIMEOUT,
            ErrorKind::Unavailable => StatusCode::SERVICE_UNAVAILABLE,
            ErrorKind::Forbidden => StatusCode::FORBIDDEN,
            ErrorKind::Conflict => StatusCode::CONFLICT,
            ErrorKind::Protocol => StatusCode::BAD_GATEWAY,
            ErrorKind::Internal => StatusCode::INTERNAL_SERVER_ERROR,
        };
        (status, Json(self.0)).into_response()
    }
}

type HttpResult<T> = Result<T, HttpError>;

/// Runs CPU-heavy work (FFT, format conversion of large records) off the async workers.
async fn blocking<T: Send + 'static>(
    f: impl FnOnce() -> Result<T, ApiError> + Send + 'static,
) -> HttpResult<T> {
    tokio::task::spawn_blocking(f)
        .await
        .map_err(|e| ApiError::new(ErrorKind::Internal, e.to_string()))?
        .map_err(Into::into)
}

pub fn routes() -> Router {
    Router::new()
        .route("/info", get(info))
        .route("/status", get(status))
        .route("/channels/{n}", get(channel).put(set_channel))
        .route("/timebase", get(timebase).put(set_timebase))
        .route("/trigger", get(trigger).put(set_trigger))
        .route("/acquire", get(acquire).put(set_acquire))
        .route("/actions/{action}", post(action))
        .route("/measure", get(measure_get).post(measure_post))
        .route("/measure/items", get(measure_items))
        .route("/measure/local", get(measure_local))
        .route("/fft", get(fft))
        .route("/waveform/{source}", get(waveform))
        .route("/screenshot.png", get(screenshot))
        .route("/options", get(options))
        .route("/options/install", post(install_option))
        .route("/setup", get(setup_save).put(setup_load))
        .route("/scpi", post(scpi))
        .route("/live", get(live_get).put(live_put))
        .route("/network", get(network).put(set_network))
        .route("/modules", get(modules))
        .route("/capabilities", get(capabilities))
        .route("/generator/{n}", put(set_generator))
        .route("/measure/stats", get(measure_stats))
        .route("/measure/actions/{action}", post(measure_action))
        .route("/counter", get(counter))
        .route("/dvm", get(dvm))
        .route("/bus/{n}/data", get(bus_data))
        .route("/math/{n}/peaks", get(math_peaks))
        .route("/readouts", get(readouts))
        .route("/readouts/{id}", get(readout))
        .route("/commands", get(commands))
        .route("/commands/{id}", post(run_command))
        .route("/logic/{bus}", get(logic))
        .route("/bode", post(bode))
        .route("/bode/native", post(bode_native))
        .route("/groups", get(groups))
        .route("/groups/{id}", get(group_get).put(group_set))
        .route("/openapi.json", get(openapi_json))
}

pub fn doc_routes() -> Router {
    Router::new()
        .route("/docs", get(docs))
        .route("/", get(|| async { axum::response::Redirect::to("/docs") }))
}

#[derive(OpenApi)]
#[openapi(
    info(
        title = "mho954 API",
        description = "Remote control for the Rigol MHO954 oscilloscope"
    ),
    paths(
        info,
        status,
        channel,
        set_channel,
        timebase,
        set_timebase,
        trigger,
        set_trigger,
        acquire,
        set_acquire,
        action,
        measure_get,
        measure_post,
        measure_items,
        measure_local,
        fft,
        waveform,
        screenshot,
        options,
        install_option,
        setup_save,
        setup_load,
        scpi,
        live_get,
        live_put,
        network,
        set_network,
        modules,
        capabilities,
        set_generator,
        groups,
        group_get,
        group_set,
        measure_stats,
        measure_action,
        counter,
        dvm,
        bus_data,
        math_peaks,
        readouts,
        readout,
        commands,
        run_command,
        logic,
        bode,
        bode_native
    ),
    components(schemas(ApiError, ErrorKind))
)]
pub struct ApiDoc;

async fn openapi_json() -> impl IntoResponse {
    Json(ApiDoc::openapi())
}

async fn docs() -> Html<&'static str> {
    // Self-contained: renders the OpenAPI document without loading third-party scripts into
    // the API's origin.
    Html(
        r#"<!doctype html><html><head><meta charset="utf-8"><title>mho954 API</title>
<style>body{font:14px system-ui,sans-serif;margin:2em;background:#111;color:#ddd}
h1{font-size:20px}h2{font-size:15px;margin:1.4em 0 .3em}code{color:#9cf}
.m{display:inline-block;width:4em;font-weight:bold}.get{color:#6c6}.put{color:#fc6}.post{color:#f96}
p{margin:.2em 0 .2em 4.5em;color:#aaa}a{color:#9cf}</style></head>
<body><h1>mho954 API</h1><p style="margin-left:0">OpenAPI: <a href="/api/v1/openapi.json">/api/v1/openapi.json</a>
· live stream: <code>ws://…/api/v1/stream</code> · MCP: <code>/mcp</code></p><div id="ops"></div>
<script>
fetch('/api/v1/openapi.json').then(r=>r.json()).then(doc=>{
  const out=document.getElementById('ops');
  for(const [path,ops] of Object.entries(doc.paths)){
    for(const [m,op] of Object.entries(ops)){
      const d=document.createElement('div');
      const h=document.createElement('h2');
      const s=document.createElement('span');s.className='m '+m;s.textContent=m.toUpperCase();
      const c=document.createElement('code');c.textContent=path;
      h.append(s,c);d.append(h);
      const txt=[op.summary,op.description].filter(Boolean).join(' — ');
      if(txt){const p=document.createElement('p');p.textContent=txt;d.append(p);}
      const params=(op.parameters||[]).map(x=>x.name+(x.required?'':'?')).join(', ');
      if(params){const p=document.createElement('p');p.textContent='params: '+params;d.append(p);}
      out.append(d);
    }
  }
});
</script></body></html>"#,
    )
}

#[utoipa::path(get, path = "/api/v1/info", responses((status = 200, body = ServiceInfo)))]
async fn info(State(s): State<AppState>) -> HttpResult<Json<ServiceInfo>> {
    Ok(Json(s.backend.info().await?))
}

#[utoipa::path(get, path = "/api/v1/status", responses((status = 200, body = ScopeStatus), (status = 503, body = ApiError)))]
async fn status(State(s): State<AppState>) -> HttpResult<Json<ScopeStatus>> {
    Ok(Json(s.backend.status().await?))
}

#[utoipa::path(get, path = "/api/v1/channels/{n}", params(("n" = u8, Path, description = "Channel 1..4")),
    responses((status = 200, body = ChannelState)))]
async fn channel(State(s): State<AppState>, Path(n): Path<u8>) -> HttpResult<Json<ChannelState>> {
    Ok(Json(s.backend.channel(n).await?))
}

#[utoipa::path(put, path = "/api/v1/channels/{n}", params(("n" = u8, Path, description = "Channel 1..4")),
    request_body = ChannelUpdate, responses((status = 200, body = ChannelState)))]
async fn set_channel(
    State(s): State<AppState>,
    Path(n): Path<u8>,
    Json(u): Json<ChannelUpdate>,
) -> HttpResult<Json<ChannelState>> {
    Ok(Json(s.backend.set_channel(n, u).await?))
}

#[utoipa::path(get, path = "/api/v1/timebase", responses((status = 200, body = TimebaseState)))]
async fn timebase(State(s): State<AppState>) -> HttpResult<Json<TimebaseState>> {
    Ok(Json(s.backend.timebase().await?))
}

#[utoipa::path(put, path = "/api/v1/timebase", request_body = TimebaseUpdate, responses((status = 200, body = TimebaseState)))]
async fn set_timebase(
    State(s): State<AppState>,
    Json(u): Json<TimebaseUpdate>,
) -> HttpResult<Json<TimebaseState>> {
    Ok(Json(s.backend.set_timebase(u).await?))
}

#[utoipa::path(get, path = "/api/v1/trigger", responses((status = 200, body = TriggerState)))]
async fn trigger(State(s): State<AppState>) -> HttpResult<Json<TriggerState>> {
    Ok(Json(s.backend.trigger().await?))
}

#[utoipa::path(put, path = "/api/v1/trigger", request_body = TriggerUpdate, responses((status = 200, body = TriggerState)))]
async fn set_trigger(
    State(s): State<AppState>,
    Json(u): Json<TriggerUpdate>,
) -> HttpResult<Json<TriggerState>> {
    Ok(Json(s.backend.set_trigger(u).await?))
}

#[utoipa::path(get, path = "/api/v1/acquire", responses((status = 200, body = AcquireState)))]
async fn acquire(State(s): State<AppState>) -> HttpResult<Json<AcquireState>> {
    Ok(Json(s.backend.acquire().await?))
}

#[utoipa::path(put, path = "/api/v1/acquire", request_body = AcquireUpdate, responses((status = 200, body = AcquireState)))]
async fn set_acquire(
    State(s): State<AppState>,
    Json(u): Json<AcquireUpdate>,
) -> HttpResult<Json<AcquireState>> {
    Ok(Json(s.backend.set_acquire(u).await?))
}

#[utoipa::path(post, path = "/api/v1/actions/{action}",
    params(("action" = String, Path, description = "run | stop | single | autoscale | clear | force")),
    responses((status = 204)))]
async fn action(State(s): State<AppState>, Path(a): Path<String>) -> HttpResult<StatusCode> {
    let action = Action::from_scpi(&a).map_err(ApiError::from)?;
    s.backend.action(action).await?;
    Ok(StatusCode::NO_CONTENT)
}

#[derive(Debug, Deserialize, IntoParams)]
struct MeasureQuery {
    /// Comma-separated items, e.g. `VPP,FREQ`.
    items: String,
    /// `CHAN1`..`CHAN4`.
    source: String,
    source2: Option<String>,
}

#[utoipa::path(get, path = "/api/v1/measure", params(MeasureQuery), responses((status = 200, body = Vec<Measurement>)))]
async fn measure_get(
    State(s): State<AppState>,
    Query(q): Query<MeasureQuery>,
) -> HttpResult<Json<Vec<Measurement>>> {
    let items = q
        .items
        .split(',')
        .map(|i| i.trim().to_string())
        .filter(|i| !i.is_empty())
        .collect();
    Ok(Json(
        s.backend
            .measure(MeasureRequest {
                items,
                source: q.source,
                source2: q.source2,
            })
            .await?,
    ))
}

#[utoipa::path(post, path = "/api/v1/measure", request_body = MeasureRequest, responses((status = 200, body = Vec<Measurement>)))]
async fn measure_post(
    State(s): State<AppState>,
    Json(r): Json<MeasureRequest>,
) -> HttpResult<Json<Vec<Measurement>>> {
    Ok(Json(s.backend.measure(r).await?))
}

#[utoipa::path(get, path = "/api/v1/measure/items", responses((status = 200, body = Vec<MeasureItem>)))]
async fn measure_items() -> Json<&'static [MeasureItem]> {
    Json(MEASURE_ITEMS)
}

#[derive(Debug, Deserialize, IntoParams)]
struct SourceQuery {
    /// `CHAN1`..`CHAN4`.
    source: String,
}

#[derive(Serialize, ToSchema)]
struct LocalMeasurements {
    source: String,
    /// `false`: the scope delivered no data for this channel (all samples 0xFFFF).
    has_data: bool,
    #[serde(flatten)]
    values: Measurements,
}

/// Measurements computed by this server from the screen waveform (not by the scope).
#[utoipa::path(get, path = "/api/v1/measure/local", params(SourceQuery), responses((status = 200, body = LocalMeasurements)))]
async fn measure_local(
    State(s): State<AppState>,
    Query(q): Query<SourceQuery>,
) -> HttpResult<Json<LocalMeasurements>> {
    let w = s.backend.waveform(q.source, WaveMode::Normal).await?;
    Ok(Json(LocalMeasurements {
        has_data: w.has_data(),
        values: w.analyze(),
        source: w.source,
    }))
}

#[derive(Debug, Deserialize, IntoParams)]
struct FftQuery {
    source: String,
    #[serde(default)]
    window: Window,
    /// Number of peaks to report (default 5).
    peaks: Option<usize>,
    /// Use the full acquisition memory (scope must be stopped).
    #[serde(default)]
    raw: bool,
    /// Include the full spectrum in the response.
    #[serde(default)]
    full: bool,
}

#[derive(Serialize, ToSchema)]
struct FftResponse {
    source: String,
    /// Bin spacing in Hz.
    df: f64,
    bins: usize,
    peaks: Vec<SpectrumPeak>,
    /// Full spectrum in dBV (only with `full=true`).
    dbv: Option<Vec<f64>>,
}

#[utoipa::path(get, path = "/api/v1/fft", params(FftQuery), responses((status = 200, body = FftResponse)))]
async fn fft(
    State(s): State<AppState>,
    Query(q): Query<FftQuery>,
) -> HttpResult<Json<FftResponse>> {
    let mode = if q.raw {
        WaveMode::Raw
    } else {
        WaveMode::Normal
    };
    let w = s.backend.waveform(q.source, mode).await?;
    let resp = blocking(move || {
        let spec = mho_dsp::spectrum(&w.volts(), w.preamble.x_increment, q.window);
        Ok(FftResponse {
            source: w.source,
            df: spec.df,
            bins: spec.dbv.len(),
            peaks: spec.peaks(q.peaks.unwrap_or(5).min(100)),
            dbv: q.full.then_some(spec.dbv),
        })
    })
    .await?;
    Ok(Json(resp))
}

#[derive(Debug, Deserialize, IntoParams)]
struct WaveformQuery {
    /// `norm` (screen, ~1000 pts) or `raw` (memory, scope must be stopped).
    mode: Option<String>,
    /// `json` (raw codes + preamble), `csv` (time, volts), `npy` (raw u16, preamble in
    /// `X-Preamble` header) or `bin` (live-stream frame encoding).
    format: Option<String>,
}

#[utoipa::path(get, path = "/api/v1/waveform/{source}", params(("source" = String, Path), WaveformQuery),
    responses((status = 200, body = Waveform)))]
async fn waveform(
    State(s): State<AppState>,
    Path(source): Path<String>,
    Query(q): Query<WaveformQuery>,
) -> HttpResult<Response> {
    let mode = WaveMode::from_scpi(q.mode.as_deref().unwrap_or("norm")).map_err(ApiError::from)?;
    let format = q.format.as_deref().unwrap_or("json").to_ascii_lowercase();
    if !matches!(format.as_str(), "json" | "csv" | "npy" | "bin") {
        return Err(ApiError::invalid(format!("unknown format {format:?}")).into());
    }
    let w = s.backend.waveform(source, mode).await?;
    blocking(move || {
        Ok(match format.as_str() {
            "json" => {
                let body = serde_json::to_vec(&w)
                    .map_err(|e| ApiError::new(ErrorKind::Internal, e.to_string()))?;
                ([(header::CONTENT_TYPE, "application/json")], body).into_response()
            }
            "csv" => {
                let volts = w.volts();
                let mut out = Vec::new();
                mho_dsp::export::write_csv(
                    &mut out,
                    w.preamble.time(0),
                    w.preamble.x_increment,
                    &[mho_dsp::export::CsvColumn {
                        name: &w.source,
                        volts: &volts,
                    }],
                )
                .map_err(|e| ApiError::new(ErrorKind::Internal, e.to_string()))?;
                ([(header::CONTENT_TYPE, "text/csv")], out).into_response()
            }
            "npy" => {
                let bytes = mho_dsp::export::npy_bytes(&w.raw)
                    .map_err(|e| ApiError::new(ErrorKind::Internal, e.to_string()))?;
                (
                    [
                        (header::CONTENT_TYPE, "application/octet-stream".to_string()),
                        (
                            header::HeaderName::from_static("x-preamble"),
                            w.preamble.to_scpi(),
                        ),
                    ],
                    bytes,
                )
                    .into_response()
            }
            "bin" => {
                let frame = Frame {
                    seq: 0,
                    timestamp_ms: mho_service::live::now_ms(),
                    trigger_status: String::new(),
                    fps: 0.0,
                    traces: vec![w],
                };
                (
                    [(header::CONTENT_TYPE, "application/octet-stream")],
                    encode_frame(&frame),
                )
                    .into_response()
            }
            _ => unreachable!("format checked above"),
        })
    })
    .await
}

#[utoipa::path(get, path = "/api/v1/screenshot.png", responses((status = 200, content_type = "image/png", body = Vec<u8>)))]
async fn screenshot(State(s): State<AppState>) -> HttpResult<Response> {
    let png = s.backend.screenshot().await?;
    Ok(([(header::CONTENT_TYPE, "image/png")], png).into_response())
}

#[utoipa::path(get, path = "/api/v1/options", responses((status = 200, body = Vec<OptionInfo>)))]
async fn options(State(s): State<AppState>) -> HttpResult<Json<Vec<OptionInfo>>> {
    Ok(Json(s.backend.options().await?))
}

#[derive(Deserialize, ToSchema)]
struct InstallRequest {
    /// Purchased license key.
    key: String,
}

#[utoipa::path(post, path = "/api/v1/options/install", request_body = InstallRequest, responses((status = 204)))]
async fn install_option(
    State(s): State<AppState>,
    Json(r): Json<InstallRequest>,
) -> HttpResult<StatusCode> {
    s.backend.install_option(r.key).await?;
    Ok(StatusCode::NO_CONTENT)
}

#[utoipa::path(get, path = "/api/v1/setup", responses((status = 200, content_type = "application/octet-stream", body = Vec<u8>)))]
async fn setup_save(State(s): State<AppState>) -> HttpResult<Response> {
    let blob = s.backend.setup_save().await?;
    Ok(([(header::CONTENT_TYPE, "application/octet-stream")], blob).into_response())
}

#[utoipa::path(put, path = "/api/v1/setup", request_body(content = Vec<u8>, content_type = "application/octet-stream"), responses((status = 204)))]
async fn setup_load(State(s): State<AppState>, body: Bytes) -> HttpResult<StatusCode> {
    s.backend.setup_load(body.to_vec()).await?;
    Ok(StatusCode::NO_CONTENT)
}

#[utoipa::path(post, path = "/api/v1/scpi", request_body = ScpiRequest,
    responses((status = 200, body = ScpiResponse), (status = 403, body = ApiError)))]
async fn scpi(
    State(s): State<AppState>,
    Json(r): Json<ScpiRequest>,
) -> HttpResult<Json<ScpiResponse>> {
    Ok(Json(s.backend.scpi(r).await?))
}

#[utoipa::path(get, path = "/api/v1/live", responses((status = 200, body = LiveConfig)))]
async fn live_get(State(s): State<AppState>) -> HttpResult<Json<LiveConfig>> {
    Ok(Json(s.backend.live_config().await?))
}

#[utoipa::path(put, path = "/api/v1/live", request_body = LiveConfig, responses((status = 200, body = LiveConfig)))]
async fn live_put(
    State(s): State<AppState>,
    Json(c): Json<LiveConfig>,
) -> HttpResult<Json<LiveConfig>> {
    Ok(Json(s.backend.set_live_config(c).await?))
}

/// LAN (RJ45) configuration. WLAN has no SCPI interface on the MHO900.
#[utoipa::path(get, path = "/api/v1/network", responses((status = 200, body = NetworkState)))]
async fn network(State(s): State<AppState>) -> HttpResult<Json<NetworkState>> {
    Ok(Json(s.backend.network().await?))
}

/// Changes LAN settings and applies them (`:LAN:APPLy`).
#[utoipa::path(put, path = "/api/v1/network", request_body = NetworkUpdate, responses((status = 200, body = NetworkState)))]
async fn set_network(
    State(s): State<AppState>,
    Json(u): Json<NetworkUpdate>,
) -> HttpResult<Json<NetworkState>> {
    Ok(Json(s.backend.set_network(u).await?))
}

/// Hardware modules: logic analyzer, function generator (`:SYST:MOD?`).
#[utoipa::path(get, path = "/api/v1/modules", responses((status = 200, body = Modules)))]
async fn modules(State(s): State<AppState>) -> HttpResult<Json<Modules>> {
    Ok(Json(s.backend.modules().await?))
}

/// What this scope can do: effective bandwidth (with upgrades), memory limits per channel
/// count, function generator, licensed protocol options.
#[utoipa::path(get, path = "/api/v1/capabilities", responses((status = 200, body = Capabilities)))]
async fn capabilities(State(s): State<AppState>) -> HttpResult<Json<Capabilities>> {
    Ok(Json(s.backend.capabilities().await?))
}

/// Changes generator channel 1 (GI) or 2 (GII); only given fields change, the output is
/// switched last. Returns the channel's settings once group `afg` is verified, else `null`.
#[utoipa::path(put, path = "/api/v1/generator/{n}", request_body = GeneratorUpdate,
    params(("n" = u8, Path, description = "1 = GI, 2 = GII")),
    responses((status = 200, body = Option<ParamMap>)))]
async fn set_generator(
    State(s): State<AppState>,
    Path(n): Path<u8>,
    Json(u): Json<GeneratorUpdate>,
) -> HttpResult<Json<Option<ParamMap>>> {
    Ok(Json(s.backend.set_generator(n, u).await?))
}

/// Catalogue of settings groups (trigger types, decoders, math, …) with parameter ranges,
/// hardware availability and whether the group was verified on the instrument.
#[utoipa::path(get, path = "/api/v1/groups", responses((status = 200, body = Vec<GroupInfo>)))]
async fn groups(State(s): State<AppState>) -> HttpResult<Json<Vec<GroupInfo>>> {
    Ok(Json(s.backend.groups().await?))
}

#[derive(Debug, Deserialize, IntoParams)]
struct GroupQuery {
    /// Instance for groups that exist several times (e.g. decoder 1..4).
    instance: Option<u8>,
    /// Also activate the group (e.g. switch the trigger type).
    #[serde(default)]
    activate: bool,
}

/// Reads a settings group (403-style refusal if not yet verified on the instrument).
#[utoipa::path(get, path = "/api/v1/groups/{id}", params(("id" = String, Path), GroupQuery),
    responses((status = 200, body = ParamMap)))]
async fn group_get(
    State(s): State<AppState>,
    Path(id): Path<String>,
    Query(q): Query<GroupQuery>,
) -> HttpResult<Json<ParamMap>> {
    Ok(Json(s.backend.group_get(id, q.instance).await?))
}

/// Writes parameters of a settings group; returns the new state if the group may be read.
#[utoipa::path(put, path = "/api/v1/groups/{id}", params(("id" = String, Path), GroupQuery),
    request_body = ParamMap, responses((status = 200, body = Option<ParamMap>)))]
async fn group_set(
    State(s): State<AppState>,
    Path(id): Path<String>,
    Query(q): Query<GroupQuery>,
    Json(values): Json<ParamMap>,
) -> HttpResult<Json<Option<ParamMap>>> {
    Ok(Json(
        s.backend
            .group_set(id, q.instance, values, q.activate)
            .await?,
    ))
}

/// Statistics (current, average, min, max, deviation, count) of one measurement.
#[utoipa::path(get, path = "/api/v1/measure/stats", params(StatsRequest),
    responses((status = 200, body = MeasureStats)))]
async fn measure_stats(
    State(s): State<AppState>,
    Query(q): Query<StatsRequest>,
) -> HttpResult<Json<MeasureStats>> {
    Ok(Json(s.backend.measure_stats(q).await?))
}

/// `reset_statistics`, `clear_all` (remove all measurements) or `clear_totalizer`.
#[utoipa::path(post, path = "/api/v1/measure/actions/{action}", params(("action" = String, Path)),
    responses((status = 204)))]
async fn measure_action(
    State(s): State<AppState>,
    Path(a): Path<String>,
) -> HttpResult<StatusCode> {
    let action = MeasureAction::from_scpi(&a).map_err(ApiError::from)?;
    s.backend.measure_action(action).await?;
    Ok(StatusCode::NO_CONTENT)
}

/// Hardware frequency counter reading (`null` = off or no signal).
#[utoipa::path(get, path = "/api/v1/counter", responses((status = 200, body = Option<f64>)))]
async fn counter(State(s): State<AppState>) -> HttpResult<Json<Option<f64>>> {
    Ok(Json(s.backend.counter_value().await?))
}

#[derive(Debug, Deserialize, IntoParams)]
struct BusQuery {
    /// `json` (default) or `csv` (like the scope's own export).
    format: Option<String>,
}

/// Event table of protocol decoder `n` (1..4). The decoder must be on (group `bus`);
/// its event table is switched on if needed.
#[utoipa::path(get, path = "/api/v1/bus/{n}/data", params(("n" = u8, Path), BusQuery),
    responses((status = 200, body = mho_dsp::DecodeTable)))]
async fn bus_data(
    State(s): State<AppState>,
    Path(n): Path<u8>,
    Query(q): Query<BusQuery>,
) -> HttpResult<Response> {
    let format = q.format.as_deref().unwrap_or("json").to_ascii_lowercase();
    if !matches!(format.as_str(), "json" | "csv") {
        return Err(ApiError::invalid(format!("unknown format {format:?}")).into());
    }
    let table = s.backend.decode_bus(n).await?;
    Ok(if format == "csv" {
        (
            [(header::CONTENT_TYPE, "text/csv")],
            mho_dsp::decode_csv(&table),
        )
            .into_response()
    } else {
        Json(table).into_response()
    })
}

/// Peak table of the scope's FFT on math channel `n` (group `math.fft` with
/// `peak_search=true`). For a spectrum computed here from waveform data see `/fft`.
#[utoipa::path(get, path = "/api/v1/math/{n}/peaks", params(("n" = u8, Path)),
    responses((status = 200, body = Vec<mho_dsp::ScopePeak>)))]
async fn math_peaks(
    State(s): State<AppState>,
    Path(n): Path<u8>,
) -> HttpResult<Json<Vec<mho_dsp::ScopePeak>>> {
    Ok(Json(s.backend.fft_peaks(n).await?))
}

/// Read-only value sets: cursor values, mask/search/histogram results, save status.
#[utoipa::path(get, path = "/api/v1/readouts",
    responses((status = 200, body = Vec<mho_driver::ops::ReadoutInfo>)))]
async fn readouts(
    State(s): State<AppState>,
) -> HttpResult<Json<Vec<mho_driver::ops::ReadoutInfo>>> {
    Ok(Json(s.backend.readouts().await?))
}

/// Reads a value set (409 if its mode is not active, e.g. cursor mode MANual).
#[utoipa::path(get, path = "/api/v1/readouts/{id}", params(("id" = String, Path)),
    responses((status = 200, body = ParamMap)))]
async fn readout(State(s): State<AppState>, Path(id): Path<String>) -> HttpResult<Json<ParamMap>> {
    Ok(Json(s.backend.readout(id).await?))
}

/// One-shot commands: save/load files on the scope, resets, recording.
#[utoipa::path(get, path = "/api/v1/commands",
    responses((status = 200, body = Vec<mho_driver::ops::CommandInfo>)))]
async fn commands(
    State(s): State<AppState>,
) -> HttpResult<Json<Vec<mho_driver::ops::CommandInfo>>> {
    Ok(Json(s.backend.commands().await?))
}

/// Runs a one-shot command; `reset` needs `"confirm": true`.
#[utoipa::path(post, path = "/api/v1/commands/{id}", params(("id" = String, Path)),
    request_body = mho_service::CommandRequest, responses((status = 204)))]
async fn run_command(
    State(s): State<AppState>,
    Path(id): Path<String>,
    body: Option<Json<mho_service::CommandRequest>>,
) -> HttpResult<StatusCode> {
    let request = body.map(|Json(b)| b).unwrap_or_default();
    s.backend.run_command(id, request).await?;
    Ok(StatusCode::NO_CONTENT)
}

#[derive(Debug, Deserialize, IntoParams)]
struct LogicQuery {
    /// `json` (default) or `vcd` (Value Change Dump for PulseView/sigrok, GTKWave).
    format: Option<String>,
}

/// Digital channels D0..D15, read through decoder `bus` as a 16-bit parallel bus (the
/// MHO954 does not deliver digital channels via `:WAV:DATA?`). Needs the logic probe.
#[utoipa::path(get, path = "/api/v1/logic/{bus}", params(("bus" = u8, Path), LogicQuery),
    responses((status = 200, body = mho_driver::LogicCapture)))]
async fn logic(
    State(s): State<AppState>,
    Path(bus): Path<u8>,
    Query(q): Query<LogicQuery>,
) -> HttpResult<Response> {
    let format = q.format.as_deref().unwrap_or("json").to_ascii_lowercase();
    if !matches!(format.as_str(), "json" | "vcd") {
        return Err(ApiError::invalid(format!("unknown format {format:?}")).into());
    }
    let cap = s.backend.logic_capture(bus).await?;
    Ok(if format == "vcd" {
        (
            [(header::CONTENT_TYPE, "text/plain")],
            mho_dsp::write_vcd(&cap.events, &cap.lines),
        )
            .into_response()
    } else {
        Json(cap).into_response()
    })
}

/// Software Bode sweep with the built-in function generator (AFG option). Blocks the scope
/// for the duration of the sweep.
#[utoipa::path(post, path = "/api/v1/bode", request_body = mho_driver::BodeRequest,
    responses((status = 200, body = Vec<mho_driver::BodePoint>)))]
async fn bode(
    State(s): State<AppState>,
    Json(r): Json<mho_driver::BodeRequest>,
) -> HttpResult<Json<Vec<mho_driver::BodePoint>>> {
    Ok(Json(s.backend.bode_sweep(r).await?))
}

/// Runs the scope's own Bode plot (`:BODeplot`, AFG option) and returns a screenshot of the
/// finished plot. The scope has no command to read the curve; use `/bode` for numbers.
#[utoipa::path(post, path = "/api/v1/bode/native", request_body = mho_driver::NativeBodeRequest,
    responses((status = 200, content_type = "image/png", description = "screenshot")))]
async fn bode_native(
    State(s): State<AppState>,
    Json(r): Json<mho_driver::NativeBodeRequest>,
) -> HttpResult<Response> {
    let png = s.backend.bode_native(r).await?;
    Ok(([(header::CONTENT_TYPE, "image/png")], png).into_response())
}

/// Digital voltmeter reading (enable it via group `dvm`).
#[utoipa::path(get, path = "/api/v1/dvm", responses((status = 200, body = Option<f64>)))]
async fn dvm(State(s): State<AppState>) -> HttpResult<Json<Option<f64>>> {
    Ok(Json(s.backend.dvm_value().await?))
}
