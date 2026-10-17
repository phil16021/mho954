//! Live acquisition: polls screen waveforms of all displayed channels and publishes them
//! through `watch` channels (latest value wins, slow consumers never stall the loop).

use std::sync::Arc;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use mho_driver::{CHANNELS, Measurement, Preamble, WaveMode, Waveform};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use tokio::sync::watch;
use tracing::{debug, warn};
use utoipa::ToSchema;

use crate::device::Device;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema, ToSchema)]
pub struct MeasureSpec {
    pub item: String,
    pub source: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema, ToSchema)]
#[serde(default)]
pub struct LiveConfig {
    /// Poll waveforms while at least one client is subscribed.
    pub enabled: bool,
    /// Upper frame rate limit while the scope is running.
    pub max_fps: f64,
    /// Frame rate while the scope is stopped (data only changes on user interaction).
    pub stopped_fps: f64,
    /// Scope-side measurements polled alongside the waveforms.
    pub measurements: Vec<MeasureSpec>,
    pub measure_interval_ms: u64,
    /// Re-read channel visibility and preambles at least this often, to follow changes
    /// made on the scope's front panel.
    pub preamble_refresh_ms: u64,
    /// Poll `:TRIG:STAT?` every N frames.
    pub status_every: u32,
}

impl Default for LiveConfig {
    fn default() -> Self {
        LiveConfig {
            enabled: true,
            max_fps: 30.0,
            stopped_fps: 2.0,
            measurements: vec![
                MeasureSpec { item: "VPP".into(), source: "CHAN1".into() },
                MeasureSpec { item: "FREQ".into(), source: "CHAN1".into() },
            ],
            measure_interval_ms: 1000,
            preamble_refresh_ms: 1000,
            status_every: 3,
        }
    }
}

/// One set of screen waveforms.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Frame {
    pub seq: u64,
    pub timestamp_ms: u64,
    pub trigger_status: String,
    pub fps: f64,
    pub traces: Vec<Waveform>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema, ToSchema)]
pub struct MeasureSnapshot {
    pub timestamp_ms: u64,
    pub values: Vec<Measurement>,
}

/// Messages delivered to live subscribers.
#[derive(Debug, Clone)]
pub enum LiveEvent {
    Frame(Arc<Frame>),
    Measurements(Arc<MeasureSnapshot>),
    Error(String),
}

/// Text messages on the WebSocket (frames travel as binary messages).
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum WsText {
    Measurements(MeasureSnapshot),
    Error { message: String },
}

pub fn now_ms() -> u64 {
    SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_millis() as u64).unwrap_or(0)
}

#[derive(Clone)]
pub struct LiveHub {
    pub(crate) frames: watch::Sender<Option<Arc<Frame>>>,
    pub(crate) measurements: watch::Sender<Option<Arc<MeasureSnapshot>>>,
    pub(crate) errors: watch::Sender<Option<String>>,
    pub(crate) config: watch::Sender<LiveConfig>,
}

impl LiveHub {
    pub fn new(config: LiveConfig) -> Self {
        LiveHub {
            frames: watch::channel(None).0,
            measurements: watch::channel(None).0,
            errors: watch::channel(None).0,
            config: watch::channel(config).0,
        }
    }

    pub fn subscribe_frames(&self) -> watch::Receiver<Option<Arc<Frame>>> {
        self.frames.subscribe()
    }

    pub fn subscribe_measurements(&self) -> watch::Receiver<Option<Arc<MeasureSnapshot>>> {
        self.measurements.subscribe()
    }

    pub fn subscribe_errors(&self) -> watch::Receiver<Option<String>> {
        self.errors.subscribe()
    }

    pub fn config(&self) -> LiveConfig {
        self.config.borrow().clone()
    }

    pub fn set_config(&self, config: LiveConfig) {
        self.config.send_replace(config);
    }

    fn has_subscribers(&self) -> bool {
        self.frames.receiver_count() > 0 || self.measurements.receiver_count() > 0
    }
}

#[derive(Default)]
struct Cache {
    epoch: u64,
    refreshed: Option<Instant>,
    /// Displayed channel sources with their preambles.
    traces: Vec<(String, Preamble)>,
    trigger_status: String,
}

/// Runs until `stop` is cancelled.
pub(crate) async fn run_loop(device: Device, hub: LiveHub, stop: tokio_util::sync::CancellationToken) {
    let mut cache = Cache::default();
    let mut seq: u64 = 0;
    let mut last_frame = Instant::now();
    let mut fps = 0.0;
    let mut last_measure: Option<Instant> = None;

    loop {
        if stop.is_cancelled() {
            return;
        }
        let cfg = hub.config();
        if !cfg.enabled || !hub.has_subscribers() {
            tokio::select! {
                _ = tokio::time::sleep(Duration::from_millis(200)) => {}
                _ = stop.cancelled() => return,
            }
            continue;
        }
        let started = Instant::now();

        let epoch = device.settings_epoch();
        let refresh = cache.epoch != epoch
            || cache.refreshed.is_none_or(|t| t.elapsed() >= Duration::from_millis(cfg.preamble_refresh_ms));
        let poll_status = refresh || seq % u64::from(cfg.status_every.max(1)) == 0;

        let job_cache = std::mem::take(&mut cache);
        let result = device
            .call(move |scope| {
                let mut c = job_cache;
                if refresh {
                    c.traces.clear();
                    for n in 1..=CHANNELS {
                        if scope.channel_displayed(n)? {
                            let src = format!("CHAN{n}");
                            let pre = scope.preamble(&src, WaveMode::Normal)?;
                            c.traces.push((src, pre));
                        }
                    }
                    c.refreshed = Some(Instant::now());
                    c.epoch = epoch;
                }
                if poll_status {
                    c.trigger_status = scope.trigger_status()?;
                }
                let mut traces = Vec::with_capacity(c.traces.len());
                for (src, pre) in &c.traces {
                    traces.push(scope.screen_waveform(src, Some(*pre))?);
                }
                Ok((c, traces))
            })
            .await;

        match result {
            Ok((c, traces)) => {
                cache = c;
                let dt = last_frame.elapsed().as_secs_f64();
                last_frame = Instant::now();
                if dt > 0.0 {
                    fps = if fps == 0.0 { 1.0 / dt } else { 0.8 * fps + 0.2 / dt };
                }
                seq += 1;
                let frame = Frame {
                    seq,
                    timestamp_ms: now_ms(),
                    trigger_status: cache.trigger_status.clone(),
                    fps,
                    traces,
                };
                hub.frames.send_replace(Some(Arc::new(frame)));
                if hub.errors.borrow().is_some() {
                    hub.errors.send_replace(None);
                }
            }
            Err(e) => {
                // The cache was moved into the failed job; start over with a full refresh.
                cache = Cache::default();
                warn!("live acquisition: {e}");
                hub.errors.send_replace(Some(e.message.clone()));
                tokio::select! {
                    _ = tokio::time::sleep(Duration::from_secs(1)) => {}
                    _ = stop.cancelled() => return,
                }
                continue;
            }
        }

        let measure_due = !cfg.measurements.is_empty()
            && last_measure.is_none_or(|t| t.elapsed() >= Duration::from_millis(cfg.measure_interval_ms));
        if measure_due {
            last_measure = Some(Instant::now());
            let specs = cfg.measurements.clone();
            let r = device
                .call(move |scope| {
                    let mut values = Vec::with_capacity(specs.len());
                    for s in &specs {
                        match scope.measure(&s.item, &s.source, None) {
                            Ok(m) => values.push(m),
                            // Skip invalid specs instead of failing the whole snapshot.
                            Err(mho_driver::DriverError::Invalid(msg)) => debug!("measure {s:?}: {msg}"),
                            Err(e) => return Err(e),
                        }
                    }
                    Ok(values)
                })
                .await;
            match r {
                Ok(values) => {
                    hub.measurements.send_replace(Some(Arc::new(MeasureSnapshot { timestamp_ms: now_ms(), values })));
                }
                Err(e) => warn!("live measurements: {e}"),
            }
        }

        let target_fps = if cache.trigger_status == "STOP" { cfg.stopped_fps } else { cfg.max_fps };
        let interval = Duration::from_secs_f64(1.0 / target_fps.clamp(0.1, 1000.0));
        if let Some(rest) = interval.checked_sub(started.elapsed()) {
            tokio::select! {
                _ = tokio::time::sleep(rest) => {}
                _ = stop.cancelled() => return,
            }
        } else {
            // Always yield so control requests get a turn on the device queue.
            tokio::task::yield_now().await;
        }
    }
}

// ---- binary frame encoding ------------------------------------------------------------------

#[derive(Serialize, Deserialize)]
struct FrameHeader {
    seq: u64,
    timestamp_ms: u64,
    trigger_status: String,
    fps: f64,
    traces: Vec<TraceHeader>,
}

#[derive(Serialize, Deserialize)]
struct TraceHeader {
    source: String,
    preamble: Preamble,
    len: usize,
}

/// `[u32 LE header length][JSON header][u16 LE samples of all traces]`.
pub fn encode_frame(f: &Frame) -> Vec<u8> {
    let header = FrameHeader {
        seq: f.seq,
        timestamp_ms: f.timestamp_ms,
        trigger_status: f.trigger_status.clone(),
        fps: f.fps,
        traces: f
            .traces
            .iter()
            .map(|t| TraceHeader { source: t.source.clone(), preamble: t.preamble, len: t.raw.len() })
            .collect(),
    };
    let json = serde_json::to_vec(&header).expect("frame header serialises");
    let samples: usize = f.traces.iter().map(|t| t.raw.len()).sum();
    let mut out = Vec::with_capacity(4 + json.len() + samples * 2);
    out.extend_from_slice(&(json.len() as u32).to_le_bytes());
    out.extend_from_slice(&json);
    for t in &f.traces {
        for v in &t.raw {
            out.extend_from_slice(&v.to_le_bytes());
        }
    }
    out
}

pub fn decode_frame(b: &[u8]) -> Result<Frame, String> {
    let len = b.get(..4).ok_or("frame too short")?;
    let hlen = u32::from_le_bytes(len.try_into().unwrap()) as usize;
    let json = b.get(4..4 + hlen).ok_or("truncated frame header")?;
    let header: FrameHeader = serde_json::from_slice(json).map_err(|e| e.to_string())?;
    let mut data = &b[4 + hlen..];
    let mut traces = Vec::with_capacity(header.traces.len());
    for t in header.traces {
        let bytes = data.get(..t.len * 2).ok_or("truncated frame data")?;
        let raw = bytes.chunks_exact(2).map(|c| u16::from_le_bytes([c[0], c[1]])).collect();
        data = &data[t.len * 2..];
        traces.push(Waveform { source: t.source, preamble: t.preamble, raw });
    }
    Ok(Frame { seq: header.seq, timestamp_ms: header.timestamp_ms, trigger_status: header.trigger_status, fps: header.fps, traces })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn frame_roundtrip() {
        let pre = Preamble::parse("1,0,3,1,2.0E-8,-1.0E-5,0,6.6667E-06,0,32768").unwrap();
        let f = Frame {
            seq: 7,
            timestamp_ms: 123,
            trigger_status: "TD".into(),
            fps: 12.5,
            traces: vec![
                Waveform { source: "CHAN1".into(), preamble: pre, raw: vec![1, 2, 3] },
                Waveform { source: "CHAN3".into(), preamble: pre, raw: vec![65535, 0] },
            ],
        };
        assert_eq!(decode_frame(&encode_frame(&f)).unwrap(), f);
    }
}
