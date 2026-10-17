//! Typed access to the MHO900 SCPI command set.

use std::collections::HashSet;
use std::path::PathBuf;
use std::time::Duration;

use mho_scpi::{Scpi, TcpTransport, Transport, UsbTmcTransport, UsbTmcUserTransport};
use serde::{Deserialize, Serialize};
use tracing::{debug, info};

use crate::error::{DriverError, Result};
use crate::measure::{find_item, valid_value};
use crate::model::*;
use crate::options::OPTIONS;
use crate::sim::SimTransport;

/// Where to find the scope.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "lowercase")]
pub enum Connection {
    /// USBTMC in user space (nusb); robust over USB/IP. `serial = None` picks the first scope.
    Usb { serial: Option<String> },
    /// Linux `usbtmc` kernel driver; `path = None` auto-detects the device node.
    Usbtmc { path: Option<PathBuf> },
    /// Raw SCPI socket, `host` or `host:port` (default port 5555).
    Tcp { addr: String },
    /// Built-in simulator, no hardware needed.
    Sim,
}

impl Default for Connection {
    fn default() -> Self {
        Connection::Usb { serial: None }
    }
}

impl std::str::FromStr for Connection {
    type Err = DriverError;

    /// `usb`, `usb:SERIAL`, `usbtmc`, `usbtmc:/dev/usbtmc0`, `/dev/usbtmc0`,
    /// `tcp:192.168.1.20[:5555]`, `sim`.
    fn from_str(s: &str) -> Result<Self> {
        let s = s.trim();
        if s.eq_ignore_ascii_case("sim") {
            return Ok(Connection::Sim);
        }
        if s.eq_ignore_ascii_case("usb") {
            return Ok(Connection::Usb { serial: None });
        }
        if let Some(serial) = s.strip_prefix("usb:") {
            return Ok(Connection::Usb {
                serial: Some(serial.into()),
            });
        }
        if s.eq_ignore_ascii_case("usbtmc") {
            return Ok(Connection::Usbtmc { path: None });
        }
        if let Some(p) = s.strip_prefix("usbtmc:") {
            return Ok(Connection::Usbtmc {
                path: Some(p.into()),
            });
        }
        if s.starts_with("/dev/") {
            return Ok(Connection::Usbtmc {
                path: Some(s.into()),
            });
        }
        if let Some(a) = s.strip_prefix("tcp:") {
            return Ok(Connection::Tcp { addr: a.into() });
        }
        Err(DriverError::Invalid(format!(
            "unknown connection {s:?} (usb[:serial] | usbtmc[:path] | tcp:host[:port] | sim)"
        )))
    }
}

impl std::fmt::Display for Connection {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Connection::Usb { serial: None } => write!(f, "usb"),
            Connection::Usb { serial: Some(s) } => write!(f, "usb:{s}"),
            Connection::Usbtmc { path: None } => write!(f, "usbtmc"),
            Connection::Usbtmc { path: Some(p) } => write!(f, "usbtmc:{}", p.display()),
            Connection::Tcp { addr } => write!(f, "tcp:{addr}"),
            Connection::Sim => write!(f, "sim"),
        }
    }
}

/// Cached `:WAVeform` settings, to skip redundant writes in the live loop.
#[derive(Debug, Default, Clone, PartialEq)]
struct WavSettings {
    source: Option<String>,
    mode: Option<WaveMode>,
    word: bool,
}

pub struct Scope {
    scpi: Scpi,
    idn: Idn,
    wav: WavSettings,
    /// Points per `:WAV:DATA?` request when downloading RAW memory.
    pub raw_chunk_points: usize,
    /// Set when an unknown raw query left the scope unresponsive.
    raw_blocked: bool,
    /// `:MEAS:ITEM?` commands already issued; the first query of an item often returns 9.9E37.
    measured: HashSet<String>,
    /// Last full state per channel (for hidden channels in [`Self::status`]).
    chan_cache: [Option<ChannelState>; CHANNELS as usize],
}

/// Maximum RAW record accepted (500 Mpts with the RLU-05 option).
const MAX_RAW_POINTS: usize = 500_000_000;

impl Scope {
    pub fn open(conn: &Connection) -> Result<Scope> {
        let transport: Box<dyn Transport> = match conn {
            Connection::Usb { serial } => Box::new(UsbTmcUserTransport::open(serial.as_deref())?),
            Connection::Usbtmc { path: Some(p) } => Box::new(UsbTmcTransport::open(p)?),
            Connection::Usbtmc { path: None } => Box::new(UsbTmcTransport::open_first()?),
            Connection::Tcp { addr } => Box::new(TcpTransport::connect(addr)?),
            Connection::Sim => Box::new(SimTransport::new()),
        };
        Self::from_transport(transport)
    }

    pub fn from_transport(transport: Box<dyn Transport>) -> Result<Scope> {
        let mut scpi = Scpi::new(transport);
        // No unconditional device clear. A failed or garbled first answer gets one recovery
        // and a second attempt.
        let first = scpi
            .query("*IDN?")
            .map_err(DriverError::from)
            .and_then(|r| Idn::parse(&r));
        let idn = match first {
            Ok(idn) => idn,
            Err(e) => {
                tracing::warn!("first *IDN? failed ({e}), retrying");
                scpi.recover();
                Idn::parse(&scpi.query("*IDN?")?)?
            }
        };
        info!(model = %idn.model, serial = %idn.serial, firmware = %idn.firmware, via = %scpi.describe(), "connected");
        let known = idn.model.starts_with("MHO9") || idn.model == "MHO98";
        if !known && std::env::var_os("MHO954_ALLOW_ANY_MODEL").is_none() {
            return Err(DriverError::Unsupported(format!(
                "{} {} is not an MHO900-series scope; set MHO954_ALLOW_ANY_MODEL=1 to try anyway",
                idn.manufacturer, idn.model
            )));
        }
        Ok(Scope {
            scpi,
            idn,
            wav: WavSettings::default(),
            raw_chunk_points: 250_000,
            raw_blocked: false,
            measured: HashSet::new(),
            chan_cache: Default::default(),
        })
    }

    /// Discards stale entries in the error queue so they are not attributed to the next change.
    fn drain_errors(&mut self) -> Result<()> {
        let old = self.scpi.errors()?;
        if !old.is_empty() {
            debug!(?old, "discarding stale error queue entries");
        }
        Ok(())
    }

    /// Runs `f`, forgetting the cached `:WAVeform` settings if it fails (the scope may have
    /// rejected a source change).
    fn wav_guard<R>(&mut self, f: impl FnOnce(&mut Self) -> Result<R>) -> Result<R> {
        let r = f(self);
        if r.is_err() {
            self.wav = WavSettings::default();
        }
        r
    }

    pub fn idn(&self) -> &Idn {
        &self.idn
    }

    pub fn describe(&self) -> String {
        self.scpi.describe()
    }

    /// Direct SCPI access. Invalidates cached waveform settings.
    pub fn scpi(&mut self) -> &mut Scpi {
        self.wav = WavSettings::default();
        &mut self.scpi
    }

    /// Drains the error queue; returns an error listing all entries if it was not empty.
    pub fn check_errors(&mut self) -> Result<()> {
        let errs = self.scpi.errors()?;
        if errs.is_empty() {
            Ok(())
        } else {
            let msg = errs
                .iter()
                .map(|(c, m)| format!("{c}: {m}"))
                .collect::<Vec<_>>()
                .join("; ");
            Err(DriverError::Device(msg))
        }
    }

    // ---- status -------------------------------------------------------------------------

    /// Complete state. Hidden channels are reported from the last known state (queried once),
    /// which roughly halves the ~60 queries a full read needs (≈25 ms each over USB/IP).
    pub fn status(&mut self) -> Result<ScopeStatus> {
        let mut channels = Vec::with_capacity(CHANNELS as usize);
        for n in 1..=CHANNELS {
            let displayed = self.channel_displayed(n)?;
            let cached = self.chan_cache[(n - 1) as usize].clone();
            let state = match (displayed, cached) {
                (false, Some(mut c)) => {
                    c.display = false;
                    c
                }
                _ => self.channel(n)?,
            };
            channels.push(state);
        }
        let trigger_status = self.trigger_status()?;
        Ok(ScopeStatus {
            idn: self.idn.clone(),
            trigger: self.trigger_with_status(trigger_status.clone())?,
            trigger_status,
            channels,
            timebase: self.timebase()?,
            acquire: self.acquire()?,
        })
    }

    pub fn trigger_status(&mut self) -> Result<String> {
        Ok(self.scpi.query(":TRIG:STAT?")?.trim().to_string())
    }

    pub fn action(&mut self, action: Action) -> Result<()> {
        let cmd = match action {
            Action::Run => ":RUN",
            Action::Stop => ":STOP",
            Action::Single => ":SING",
            Action::Autoscale => ":AUT",
            Action::Clear => ":CLE",
            Action::Force => ":TFOR",
        };
        self.scpi.write(cmd)?;
        if action == Action::Autoscale {
            // Autoscale takes a few seconds; the next query would otherwise time out.
            self.scpi
                .with_timeout(Duration::from_secs(10), |s| s.query("*OPC?"))?;
        }
        Ok(())
    }

    // ---- channels -----------------------------------------------------------------------

    fn check_channel(n: u8) -> Result<()> {
        if (1..=CHANNELS).contains(&n) {
            Ok(())
        } else {
            Err(DriverError::Invalid(format!(
                "channel {n} out of range 1..={CHANNELS}"
            )))
        }
    }

    pub fn channel(&mut self, n: u8) -> Result<ChannelState> {
        Self::check_channel(n)?;
        let p = format!(":CHAN{n}");
        let state = ChannelState {
            channel: n,
            display: self.scpi.query_bool(&format!("{p}:DISP?"))?,
            scale: self.scpi.query_f64(&format!("{p}:SCAL?"))?,
            offset: self.scpi.query_f64(&format!("{p}:OFFS?"))?,
            coupling: Coupling::from_scpi(&self.scpi.query(&format!("{p}:COUP?"))?)?,
            bw_limit: self.scpi.query(&format!("{p}:BWL?"))?,
            probe: self.scpi.query_f64(&format!("{p}:PROB?"))?,
            impedance: Impedance::from_scpi(&self.scpi.query(&format!("{p}:IMP?"))?)?,
            invert: self.scpi.query_bool(&format!("{p}:INV?"))?,
            vernier: self.scpi.query_bool(&format!("{p}:VERN?"))?,
            unit: self.scpi.query(&format!("{p}:UNIT?"))?,
        };
        self.chan_cache[(n - 1) as usize] = Some(state.clone());
        Ok(state)
    }

    pub fn channel_displayed(&mut self, n: u8) -> Result<bool> {
        Self::check_channel(n)?;
        Ok(self.scpi.query_bool(&format!(":CHAN{n}:DISP?"))?)
    }

    pub fn set_channel(&mut self, n: u8, u: &ChannelUpdate) -> Result<ChannelState> {
        Self::check_channel(n)?;
        self.drain_errors()?;
        // Display changes can invalidate the cached waveform source.
        self.wav = WavSettings::default();
        let p = format!(":CHAN{n}");
        // Probe ratio first: it rescales V/div and offset.
        if let Some(v) = u.probe {
            positive("probe", v)?;
            self.scpi.write(&format!("{p}:PROB {v}"))?;
        }
        if let Some(v) = u.impedance {
            self.scpi.write(&format!("{p}:IMP {}", v.scpi()))?;
        }
        if let Some(v) = u.display {
            self.scpi.write(&format!("{p}:DISP {}", on_off(v)))?;
        }
        if let Some(v) = u.vernier {
            self.scpi.write(&format!("{p}:VERN {}", on_off(v)))?;
        }
        if let Some(v) = u.scale {
            positive("scale", v)?;
            self.scpi.write(&format!("{p}:SCAL {v:e}"))?;
        }
        if let Some(v) = u.offset {
            finite("offset", v)?;
            self.scpi.write(&format!("{p}:OFFS {v:e}"))?;
        }
        if let Some(v) = u.coupling {
            self.scpi.write(&format!("{p}:COUP {}", v.scpi()))?;
        }
        if let Some(v) = &u.bw_limit {
            self.scpi
                .write(&format!("{p}:BWL {}", token("bw_limit", v)?))?;
        }
        if let Some(v) = u.invert {
            self.scpi.write(&format!("{p}:INV {}", on_off(v)))?;
        }
        self.check_errors()?;
        self.channel(n)
    }

    // ---- timebase -----------------------------------------------------------------------

    pub fn timebase(&mut self) -> Result<TimebaseState> {
        Ok(TimebaseState {
            scale: self.scpi.query_f64(":TIM:SCAL?")?,
            offset: self.scpi.query_f64(":TIM:OFFS?")?,
            mode: self.scpi.query(":TIM:MODE?")?,
        })
    }

    pub fn set_timebase(&mut self, u: &TimebaseUpdate) -> Result<TimebaseState> {
        self.drain_errors()?;
        if let Some(m) = &u.mode {
            self.scpi
                .write(&format!(":TIM:MODE {}", token("mode", m)?))?;
        }
        if let Some(v) = u.scale {
            positive("scale", v)?;
            self.scpi.write(&format!(":TIM:SCAL {v:e}"))?;
        }
        if let Some(v) = u.offset {
            finite("offset", v)?;
            self.scpi.write(&format!(":TIM:OFFS {v:e}"))?;
        }
        self.check_errors()?;
        self.timebase()
    }

    // ---- trigger ------------------------------------------------------------------------

    pub fn trigger(&mut self) -> Result<TriggerState> {
        let status = self.trigger_status()?;
        self.trigger_with_status(status)
    }

    fn trigger_with_status(&mut self, status: String) -> Result<TriggerState> {
        Ok(TriggerState {
            mode: self.scpi.query(":TRIG:MODE?")?,
            sweep: TriggerSweep::from_scpi(&self.scpi.query(":TRIG:SWE?")?)?,
            status,
            coupling: self.scpi.query(":TRIG:COUP?")?,
            holdoff: self.scpi.query_f64(":TRIG:HOLD?")?,
            edge_source: self.scpi.query(":TRIG:EDGE:SOUR?")?,
            edge_slope: Slope::from_scpi(&self.scpi.query(":TRIG:EDGE:SLOP?")?)?,
            edge_level: self.scpi.query_f64(":TRIG:EDGE:LEV?")?,
        })
    }

    pub fn set_trigger(&mut self, u: &TriggerUpdate) -> Result<TriggerState> {
        self.drain_errors()?;
        if let Some(m) = &u.mode {
            self.scpi
                .write(&format!(":TRIG:MODE {}", token("mode", m)?))?;
        }
        if let Some(v) = &u.edge_source {
            let src = match normalize_source(v) {
                Ok(s) => s,
                Err(_) => token("edge_source", v)?.to_ascii_uppercase(),
            };
            self.scpi.write(&format!(":TRIG:EDGE:SOUR {src}"))?;
        }
        if let Some(v) = u.edge_slope {
            self.scpi.write(&format!(":TRIG:EDGE:SLOP {}", v.scpi()))?;
        }
        if let Some(v) = u.edge_level {
            finite("edge_level", v)?;
            self.scpi.write(&format!(":TRIG:EDGE:LEV {v:e}"))?;
        }
        if let Some(v) = &u.coupling {
            self.scpi
                .write(&format!(":TRIG:COUP {}", token("coupling", v)?))?;
        }
        if let Some(v) = u.holdoff {
            positive("holdoff", v)?;
            self.scpi.write(&format!(":TRIG:HOLD {v:e}"))?;
        }
        if let Some(v) = u.sweep {
            self.scpi.write(&format!(":TRIG:SWE {}", v.scpi()))?;
        }
        self.check_errors()?;
        self.trigger()
    }

    // ---- acquisition --------------------------------------------------------------------

    pub fn acquire(&mut self) -> Result<AcquireState> {
        Ok(AcquireState {
            sample_rate: self.scpi.query_f64(":ACQ:SRAT?")?,
            memory_depth: match self.scpi.query_f64(":ACQ:MDEP?") {
                Ok(v) => Some(v),
                Err(mho_scpi::ScpiError::Parse { .. }) => None,
                Err(e) => return Err(e.into()),
            },
            acq_type: AcquireType::from_scpi(&self.scpi.query(":ACQ:TYPE?")?)?,
            averages: self.scpi.query_f64(":ACQ:AVER?")? as u32,
        })
    }

    pub fn set_acquire(&mut self, u: &AcquireUpdate) -> Result<AcquireState> {
        self.drain_errors()?;
        if let Some(t) = u.acq_type {
            self.scpi.write(&format!(":ACQ:TYPE {}", t.scpi()))?;
        }
        if let Some(n) = u.averages {
            if !n.is_power_of_two() || n < 2 {
                return Err(DriverError::Invalid(format!(
                    "averages must be a power of two ≥ 2, got {n}"
                )));
            }
            self.scpi.write(&format!(":ACQ:AVER {n}"))?;
        }
        if let Some(d) = &u.memory_depth {
            self.scpi
                .write(&format!(":ACQ:MDEP {}", token("memory_depth", d)?))?;
        }
        self.check_errors()?;
        self.acquire()
    }

    // ---- measurements -------------------------------------------------------------------

    /// Queries one `:MEASure:ITEM?`. `source2` is required for delay/phase items.
    pub fn measure(
        &mut self,
        item: &str,
        source: &str,
        source2: Option<&str>,
    ) -> Result<Measurement> {
        let it = find_item(item)?;
        let src = measure_source(source)?;
        let cmd = if it.dual_source {
            let s2 = source2.ok_or_else(|| {
                DriverError::Invalid(format!("{} needs a second source", it.long))
            })?;
            format!(":MEAS:ITEM? {},{},{}", it.token, src, measure_source(s2)?)
        } else {
            format!(":MEAS:ITEM? {},{}", it.token, src)
        };
        let mut raw = self.scpi.query_f64(&cmd)?;
        if valid_value(raw).is_none() && self.measured.insert(cmd.clone()) {
            // The scope creates the measurement on first use and reports 9.9E37 until it has
            // been evaluated once.
            std::thread::sleep(Duration::from_millis(150));
            raw = self.scpi.query_f64(&cmd)?;
        }
        self.measured.insert(cmd);
        Ok(Measurement {
            item: it.long.to_string(),
            source: src,
            value: valid_value(raw),
            unit: it.unit.to_string(),
        })
    }

    pub fn measure_many(&mut self, items: &[String], source: &str) -> Result<Vec<Measurement>> {
        items
            .iter()
            .map(|i| self.measure(i, source, None))
            .collect()
    }

    // ---- waveforms ----------------------------------------------------------------------

    fn configure_wav(&mut self, source: &str, mode: WaveMode) -> Result<()> {
        self.wav_guard(|s| s.configure_wav_inner(source, mode))
    }

    fn configure_wav_inner(&mut self, source: &str, mode: WaveMode) -> Result<()> {
        if self.wav.source.as_deref() != Some(source) {
            self.scpi.write(&format!(":WAV:SOUR {source}"))?;
            self.wav.source = Some(source.to_string());
        }
        if self.wav.mode != Some(mode) {
            self.scpi.write(&format!(":WAV:MODE {}", mode.scpi()))?;
            self.wav.mode = Some(mode);
        }
        if !self.wav.word {
            self.scpi.write(":WAV:FORM WORD")?;
            self.wav.word = true;
        }
        Ok(())
    }

    pub fn preamble(&mut self, source: &str, mode: WaveMode) -> Result<Preamble> {
        let src = normalize_source(source)?;
        self.configure_wav(&src, mode)?;
        self.wav_guard(|s| Preamble::parse(&s.scpi.query(":WAV:PRE?")?))
    }

    /// Reads waveform data for an already configured source (see [`Self::preamble`]).
    fn read_words(
        &mut self,
        cmd: &str,
        progress: &mut dyn FnMut(usize, usize),
    ) -> Result<Vec<u16>> {
        let bytes = self.scpi.query_block_progress(cmd, progress)?;
        if bytes.len() % 2 != 0 {
            return Err(DriverError::Protocol(format!(
                "odd WORD data length {}",
                bytes.len()
            )));
        }
        Ok(bytes
            .as_chunks::<2>()
            .0
            .iter()
            .map(|b| u16::from_le_bytes([b[0], b[1]]))
            .collect())
    }

    /// Screen waveform (`NORM`, ~1000 points). Pass a cached preamble to save a round trip.
    pub fn screen_waveform(
        &mut self,
        source: &str,
        preamble: Option<Preamble>,
    ) -> Result<Waveform> {
        let src = normalize_source(source)?;
        self.configure_wav(&src, WaveMode::Normal)?;
        self.wav_guard(|s| {
            let preamble = match preamble {
                Some(p) => p,
                None => Preamble::parse(&s.scpi.query(":WAV:PRE?")?)?,
            };
            let raw = s.read_words(":WAV:DATA?", &mut |_, _| {})?;
            Ok(Waveform {
                source: src,
                preamble,
                raw,
            })
        })
    }

    /// Full acquisition memory (`RAW`). The scope must be stopped; downloads in chunks of
    /// [`Self::raw_chunk_points`] and reports `(points_done, points_total)`.
    pub fn raw_waveform(
        &mut self,
        source: &str,
        progress: &mut dyn FnMut(usize, usize),
    ) -> Result<Waveform> {
        let status = self.trigger_status()?;
        if status != "STOP" {
            return Err(DriverError::State(format!(
                "RAW readout needs a stopped acquisition (trigger status is {status}); send STOP or SINGLE first"
            )));
        }
        let src = normalize_source(source)?;
        self.configure_wav(&src, WaveMode::Raw)?;
        self.wav_guard(|s| s.raw_download(src, progress))
    }

    fn raw_download(
        &mut self,
        src: String,
        progress: &mut dyn FnMut(usize, usize),
    ) -> Result<Waveform> {
        let preamble = Preamble::parse(&self.scpi.query(":WAV:PRE?")?)?;
        let total = preamble.points;
        if total > MAX_RAW_POINTS {
            return Err(DriverError::Protocol(format!(
                "preamble reports {total} points"
            )));
        }
        let chunk = self.raw_chunk_points.max(1000);
        let mut raw = Vec::with_capacity(total.min(16_000_000));
        let timeout = self.scpi.timeout().max(Duration::from_secs(10));
        self.scpi.with_timeout(timeout, |scpi| {
            let mut start = 1usize;
            while start <= total {
                let stop = (start + chunk - 1).min(total);
                // STOP before STAR so that start never exceeds the previous stop.
                scpi.write(&format!(":WAV:STOP {stop}"))?;
                scpi.write(&format!(":WAV:STAR {start}"))?;
                let done = raw.len();
                let bytes = scpi.query_block_progress(":WAV:DATA?", &mut |got, _| {
                    progress(done + got / 2, total)
                })?;
                let expected = 2 * (stop - start + 1);
                if bytes.len() != expected {
                    return Err(mho_scpi::ScpiError::parse(
                        ":WAV:DATA?",
                        format!(
                            "RAW chunk {start}..{stop}: {} bytes, expected {expected}",
                            bytes.len()
                        ),
                    ));
                }
                raw.extend(
                    bytes
                        .as_chunks::<2>()
                        .0
                        .iter()
                        .map(|b| u16::from_le_bytes([b[0], b[1]])),
                );
                debug!(start, stop, got = raw.len(), "raw chunk");
                start = stop + 1;
            }
            // Leave the read window covering the whole record again.
            scpi.write(":WAV:STAR 1")?;
            scpi.write(&format!(":WAV:STOP {total}"))?;
            Ok(())
        })?;
        progress(raw.len(), total);
        Ok(Waveform {
            source: src,
            preamble,
            raw,
        })
    }

    // ---- display / setup ----------------------------------------------------------------

    pub fn screenshot(&mut self) -> Result<Vec<u8>> {
        let png = self.scpi.with_timeout(Duration::from_secs(15), |s| {
            s.query_block(":DISP:DATA? PNG")
        })?;
        if !png.starts_with(b"\x89PNG") {
            return Err(DriverError::Protocol("screenshot is not a PNG".into()));
        }
        Ok(png)
    }

    /// Binary setup blob (`:SYSTem:SETup?`) for later restore.
    pub fn setup_save(&mut self) -> Result<Vec<u8>> {
        Ok(self
            .scpi
            .with_timeout(Duration::from_secs(10), |s| s.query_block(":SYST:SET?"))?)
    }

    pub fn setup_load(&mut self, blob: &[u8]) -> Result<()> {
        self.scpi.write_block(":SYST:SET", blob)?;
        self.wav = WavSettings::default();
        self.scpi
            .with_timeout(Duration::from_secs(10), |s| s.query("*OPC?"))?;
        self.check_errors()
    }

    // ---- options ------------------------------------------------------------------------

    pub fn options(&mut self) -> Result<Vec<OptionInfo>> {
        OPTIONS
            .iter()
            .map(|o| {
                Ok(OptionInfo {
                    name: o.code.to_string(),
                    description: o.description.to_string(),
                    active: self
                        .scpi
                        .query_bool(&format!(":SYST:OPT:STAT? {}", o.token))?,
                })
            })
            .collect()
    }

    /// Installs a purchased option license (`<series>-<option>@<code>`, generated on
    /// rigol.com → License Activation from the purchased key and the serial number).
    pub fn install_option(&mut self, license: &str) -> Result<()> {
        let key: String = license.chars().filter(|c| !c.is_whitespace()).collect();
        let valid = key.split_once('@').is_some_and(|(head, code)| {
            head.contains('-')
                && head.chars().all(|c| c.is_ascii_alphanumeric() || c == '-')
                && !code.is_empty()
                && code.chars().all(|c| c.is_ascii_alphanumeric())
        });
        if !valid {
            return Err(DriverError::Invalid(
                "license must look like MHO900-<OPTION>@<code> (from rigol.com License Activation)"
                    .into(),
            ));
        }
        self.scpi.write(&format!(":SYST:OPT:INST {key}"))?;
        self.scpi
            .with_timeout(Duration::from_secs(10), |s| s.query("*OPC?"))?;
        self.check_errors()
    }

    // ---- network (RJ45) -----------------------------------------------------------------

    pub fn network(&mut self) -> Result<NetworkState> {
        let s = &mut self.scpi;
        Ok(NetworkState {
            status: s.query(":LAN:STAT?")?,
            dhcp: s.query_bool(":LAN:DHCP?")?,
            auto_ip: s.query_bool(":LAN:AUT?")?,
            manual: s.query_bool(":LAN:MAN?")?,
            ip: s.query(":LAN:IPAD?")?,
            mask: s.query(":LAN:SMAS?")?,
            gateway: s.query(":LAN:GAT?")?,
            dns: s.query(":LAN:DNS?")?,
            mac: s.query(":LAN:MAC?")?,
            dhcp_server: s.query(":LAN:DSER?")?,
            mdns: s.query_bool(":LAN:MDNS?")?,
            hostname: s.query(":LAN:HOST:NAME?")?,
            description: s.query(":LAN:DESC?")?,
            visa: s.query(":LAN:VISA?")?,
        })
    }

    /// Changes the LAN configuration and sends `:LAN:APPLy`. When connected over LAN, the
    /// connection may drop if the address changes.
    pub fn set_network(&mut self, u: &NetworkUpdate) -> Result<NetworkState> {
        let label = |name: &str, v: &str| -> Result<String> {
            let v = v.trim();
            if v.is_empty()
                || v.len() > 64
                || !v
                    .chars()
                    .all(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.'))
            {
                return Err(DriverError::Invalid(format!(
                    "{name}: use 1-64 letters, digits, '-', '_', '.'"
                )));
            }
            Ok(v.to_string())
        };
        // Validate everything before sending anything.
        let ip =
            u.ip.as_deref()
                .map(|v| validate_ipv4(v, false))
                .transpose()?;
        let mask = u
            .mask
            .as_deref()
            .map(|v| validate_ipv4(v, true))
            .transpose()?;
        let gateway = u
            .gateway
            .as_deref()
            .map(|v| validate_ipv4(v, false))
            .transpose()?;
        let dns = u
            .dns
            .as_deref()
            .map(|v| validate_ipv4(v, false))
            .transpose()?;
        let hostname = u
            .hostname
            .as_deref()
            .map(|v| label("hostname", v))
            .transpose()?;
        let description = u
            .description
            .as_deref()
            .map(|v| label("description", v))
            .transpose()?;
        if u.mode.is_none()
            && (ip.is_some() || mask.is_some() || gateway.is_some() || dns.is_some())
            && !self.scpi.query_bool(":LAN:MAN?")?
        {
            return Err(DriverError::Invalid(
                "ip/mask/gateway/dns need mode Static".into(),
            ));
        }

        let s = &mut self.scpi;
        // Order matters: the three modes may never be off at the same time.
        match u.mode {
            Some(IpMode::Dhcp) => {
                s.write(":LAN:DHCP ON")?;
                s.write(":LAN:MAN OFF")?;
            }
            Some(IpMode::AutoIp) => {
                s.write(":LAN:AUT ON")?;
                s.write(":LAN:DHCP OFF")?;
                s.write(":LAN:MAN OFF")?;
            }
            Some(IpMode::Static) => {
                s.write(":LAN:MAN ON")?;
                s.write(":LAN:DHCP OFF")?;
                s.write(":LAN:AUT OFF")?;
            }
            None => {}
        }
        if let Some(v) = &ip {
            s.write(&format!(":LAN:IPAD {v}"))?;
        }
        if let Some(v) = &mask {
            s.write(&format!(":LAN:SMAS {v}"))?;
        }
        if let Some(v) = &gateway {
            s.write(&format!(":LAN:GAT {v}"))?;
        }
        if let Some(v) = &dns {
            s.write(&format!(":LAN:DNS {v}"))?;
        }
        if let Some(v) = u.mdns {
            s.write(&format!(":LAN:MDNS {}", on_off(v)))?;
        }
        if let Some(v) = &hostname {
            s.write(&format!(":LAN:HOST:NAME {v}"))?;
        }
        if let Some(v) = &description {
            s.write(&format!(":LAN:DESC {v}"))?;
        }
        self.check_errors()?;
        self.scpi.write(":LAN:APPL")?;
        self.scpi
            .with_timeout(Duration::from_secs(10), |s| s.query("*OPC?"))?;
        self.check_errors()?;
        self.network()
    }

    // ---- raw SCPI -----------------------------------------------------------------------

    /// Sends arbitrary SCPI. Queries (containing `?`) return the response line; commands
    /// return `None` after checking the error queue.
    ///
    /// The MHO954 does not answer unknown queries and its firmware stops answering anything
    /// after several of them in a row, even after a device clear. So after an unanswered query
    /// the scope's health is checked with `*IDN?`, and further raw queries are refused until
    /// that check succeeds.
    pub fn raw(&mut self, cmd: &str, timeout: Duration) -> Result<Option<String>> {
        let cmd = cmd.trim();
        if cmd.is_empty() {
            return Err(DriverError::Invalid("empty command".into()));
        }
        if cmd.chars().any(|c| c.is_control()) {
            return Err(DriverError::Invalid(
                "one command per request; control characters are not allowed".into(),
            ));
        }
        if self.raw_blocked {
            match self.scpi.query("*IDN?") {
                Ok(_) => self.raw_blocked = false,
                Err(_) => {
                    return Err(DriverError::Device(
                        "scope stopped answering after an unknown query; power-cycle it".into(),
                    ));
                }
            }
        }
        let scpi = self.scpi();
        if cmd.contains('?') {
            match scpi.with_timeout(timeout, |s| s.query_auto(cmd)) {
                Ok(mho_scpi::Response::Line(r)) => Ok(Some(r)),
                Ok(mho_scpi::Response::Block(b)) => {
                    let head: String = b.iter().take(32).map(|x| format!("{x:02x}")).collect();
                    Ok(Some(format!("<binary block, {} bytes: {head}…>", b.len())))
                }
                Err(e) if e.is_timeout() => {
                    // The transport has sent a device clear; verify the scope recovered.
                    let healthy = self.scpi.query("*IDN?").is_ok();
                    self.raw_blocked = !healthy;
                    Err(DriverError::Invalid(format!(
                        "no answer to `{cmd}` (unknown query?){}",
                        if healthy {
                            ""
                        } else {
                            "; the scope stopped responding — power-cycle it"
                        }
                    )))
                }
                Err(e) => Err(e.into()),
            }
        } else {
            scpi.write(cmd)?;
            self.check_errors()?;
            Ok(None)
        }
    }
}

/// Measurement sources: analog channels and math. Digital channels need the logic probe and
/// are not offered (an unanswered query would block the scope).
fn measure_source(s: &str) -> Result<String> {
    let src = normalize_source(s)?;
    if src.starts_with('D') {
        return Err(DriverError::Unsupported(format!(
            "{src}: digital measurement sources are not supported"
        )));
    }
    Ok(src)
}

fn on_off(v: bool) -> &'static str {
    if v { "ON" } else { "OFF" }
}

fn positive(name: &str, v: f64) -> Result<()> {
    if v.is_finite() && v > 0.0 {
        Ok(())
    } else {
        Err(DriverError::Invalid(format!("{name} must be > 0, got {v}")))
    }
}

fn finite(name: &str, v: f64) -> Result<()> {
    if v.is_finite() {
        Ok(())
    } else {
        Err(DriverError::Invalid(format!("{name} must be finite")))
    }
}

/// Accepts a bare SCPI token (letters, digits, `.`, `+`, `-`) to prevent command injection
/// through `;` or newlines in string parameters.
fn token<'a>(name: &str, v: &'a str) -> Result<&'a str> {
    let v = v.trim();
    if !v.is_empty()
        && v.chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '+' | '-'))
    {
        Ok(v)
    } else {
        Err(DriverError::Invalid(format!("{name}: invalid value {v:?}")))
    }
}
