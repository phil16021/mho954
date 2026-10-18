//! Typed access to the MHO900 SCPI command set.

use std::collections::HashSet;
use std::path::PathBuf;
use std::time::Duration;

use mho_scpi::{Scpi, TcpTransport, Transport, UsbTmcTransport, UsbTmcUserTransport};
use serde::{Deserialize, Serialize};
use tracing::{debug, info};

use crate::caps::Capabilities;
use crate::error::{DriverError, Result};
use crate::measure::{find_item, valid_value};
use crate::model::*;
use crate::options::{OPTIONS, STANDARD};
use crate::params::{self, ParamMap, ParamValue};
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
    /// Send queries that were not verified on the instrument (simulator only).
    trust_unverified: bool,
    /// Capabilities of this connection (options and modules do not change while connected).
    caps: Option<Capabilities>,
}

/// Query whose answer shows that the built-in generator is enabled on the instrument
/// (`:SYST:MOD?` reports no DG on the upgraded test scope, so the license alone is not proof).
pub const GENERATOR_CANARY: &str = ":SOUR1:OUTP:STAT?";

/// Maximum RAW record accepted (500 Mpts with the RLU-05 option).
const MAX_RAW_POINTS: usize = 500_000_000;

impl Scope {
    pub fn open(conn: &Connection) -> Result<Scope> {
        let transport: Box<dyn Transport> = match conn {
            Connection::Usb { serial } => Box::new(UsbTmcUserTransport::open(serial.as_deref())?),
            Connection::Usbtmc { path: Some(p) } => Box::new(UsbTmcTransport::open(p)?),
            Connection::Usbtmc { path: None } => Box::new(UsbTmcTransport::open_first()?),
            Connection::Tcp { addr } => Box::new(TcpTransport::connect(addr)?),
            Connection::Sim => {
                let opts = crate::sim::SimOptions::from_env();
                let strict = opts.strict;
                let mut scope = Self::from_transport(Box::new(SimTransport::with_options(opts)))?;
                scope.trust_unverified = !strict;
                return Ok(scope);
            }
        };
        Self::from_transport(transport)
    }

    /// `true` if queries not yet verified on the instrument are sent anyway (simulator).
    pub fn trusts_unverified(&self) -> bool {
        self.trust_unverified
    }

    /// `query` may be sent: confirmed on the instrument, or the simulator is trusted.
    pub(crate) fn may_query(&self, query: &str) -> bool {
        self.trust_unverified || params::query_verified(query)
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
            trust_unverified: false,
            caps: None,
        })
    }

    /// Discards stale entries in the error queue so they are not attributed to the next change.
    pub(crate) fn drain_errors(&mut self) -> Result<()> {
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
        let props = [
            "DISP", "SCAL", "OFFS", "COUP", "BWL", "PROB", "IMP", "INV", "VERN", "UNIT",
        ];
        let cmds: Vec<String> = props.iter().map(|p| format!(":CHAN{n}:{p}?")).collect();
        let a = self.scpi.query_many(&cmds)?;
        let state = ChannelState {
            channel: n,
            display: parse_bool(&cmds[0], &a[0])?,
            scale: parse_f64(&cmds[1], &a[1])?,
            offset: parse_f64(&cmds[2], &a[2])?,
            coupling: Coupling::from_scpi(&a[3])?,
            bw_limit: a[4].clone(),
            probe: parse_f64(&cmds[5], &a[5])?,
            impedance: Impedance::from_scpi(&a[6])?,
            invert: parse_bool(&cmds[7], &a[7])?,
            vernier: parse_bool(&cmds[8], &a[8])?,
            unit: a[9].clone(),
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
        let cmds = queries(&[":TIM:SCAL?", ":TIM:OFFS?", ":TIM:MODE?"]);
        let a = self.scpi.query_many(&cmds)?;
        Ok(TimebaseState {
            scale: parse_f64(&cmds[0], &a[0])?,
            offset: parse_f64(&cmds[1], &a[1])?,
            mode: a[2].clone(),
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
        let cmds = queries(&[
            ":TRIG:MODE?",
            ":TRIG:SWE?",
            ":TRIG:COUP?",
            ":TRIG:HOLD?",
            ":TRIG:EDGE:SOUR?",
            ":TRIG:EDGE:SLOP?",
            ":TRIG:EDGE:LEV?",
        ]);
        let a = self.scpi.query_many(&cmds)?;
        Ok(TriggerState {
            mode: a[0].clone(),
            sweep: TriggerSweep::from_scpi(&a[1])?,
            status,
            coupling: a[2].clone(),
            holdoff: parse_f64(&cmds[3], &a[3])?,
            edge_source: a[4].clone(),
            edge_slope: Slope::from_scpi(&a[5])?,
            edge_level: parse_f64(&cmds[6], &a[6])?,
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
        let cmds = queries(&[":ACQ:SRAT?", ":ACQ:MDEP?", ":ACQ:TYPE?", ":ACQ:AVER?"]);
        let a = self.scpi.query_many(&cmds)?;
        Ok(AcquireState {
            sample_rate: parse_f64(&cmds[0], &a[0])?,
            // Unparsable depth (e.g. while AUTO settles) is reported as unknown.
            memory_depth: a[1].parse::<f64>().ok(),
            acq_type: AcquireType::from_scpi(&a[2])?,
            averages: parse_f64(&cmds[3], &a[3])? as u32,
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
            let d = token("memory_depth", d)?;
            if let Some(points) = depth_points(d) {
                let mut on = 0;
                for n in 1..=CHANNELS {
                    on += usize::from(self.channel_displayed(n)?);
                }
                let max = self.capabilities()?.memory_limit(on);
                if points > max as f64 {
                    return Err(DriverError::Invalid(format!(
                        "memory depth {d} above the maximum of {}M with {} channel(s) on",
                        max / 1_000_000,
                        on.max(1)
                    )));
                }
            }
            self.scpi.write(&format!(":ACQ:MDEP {d}"))?;
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

    /// Statistics of a measurement. Enables statistics for the item first (set form, no
    /// query) and retries once if the scope has not evaluated it yet.
    pub fn measure_stats(
        &mut self,
        item: &str,
        source: &str,
        source2: Option<&str>,
    ) -> Result<MeasureStats> {
        let it = find_item(item)?;
        let src = measure_source(source)?;
        let tail = if it.dual_source {
            let s2 = source2.ok_or_else(|| {
                DriverError::Invalid(format!("{} needs a second source", it.long))
            })?;
            format!("{},{},{}", it.token, src, measure_source(s2)?)
        } else {
            format!("{},{}", it.token, src)
        };
        let key = format!(":MEAS:STAT:ITEM {tail}");
        if !self.measured.contains(&key) {
            self.scpi.write(&key)?;
        }
        let kinds = ["CURR", "AVER", "MIN", "MAX", "DEV", "CNT"];
        let cmds: Vec<String> = kinds
            .iter()
            .map(|k| format!(":MEAS:STAT:ITEM? {k},{tail}"))
            .collect();
        // Not as one compound query: the MHO954 then answers every position with the value of
        // the last statistic (verified on hardware).
        let read = |scpi: &mut mho_scpi::Scpi| -> Result<Vec<String>> {
            cmds.iter()
                .map(|c| scpi.query(c).map_err(DriverError::from))
                .collect()
        };
        let mut a = read(&mut self.scpi)?;
        let first_invalid = a
            .first()
            .and_then(|v| v.parse::<f64>().ok())
            .and_then(valid_value)
            .is_none();
        if first_invalid && self.measured.insert(key.clone()) {
            std::thread::sleep(Duration::from_millis(150));
            a = read(&mut self.scpi)?;
        }
        self.measured.insert(key);
        let v = |i: usize| a[i].trim().parse::<f64>().ok().and_then(valid_value);
        Ok(MeasureStats {
            item: it.long.into(),
            source: src,
            unit: it.unit.into(),
            current: v(0),
            average: v(1),
            minimum: v(2),
            maximum: v(3),
            deviation: v(4),
            count: v(5),
        })
    }

    pub fn measure_action(&mut self, action: MeasureAction) -> Result<()> {
        self.scpi.write(match action {
            MeasureAction::ResetStatistics => ":MEAS:STAT:RES",
            MeasureAction::ClearAll => ":MEAS:DEL",
            MeasureAction::ClearTotalizer => ":COUN:TOT:CLE",
        })?;
        if action == MeasureAction::ClearAll {
            self.measured.clear();
        }
        self.check_errors()
    }

    /// Hardware frequency counter reading (`None` when off or no signal).
    pub fn counter_value(&mut self) -> Result<Option<f64>> {
        Ok(valid_value(self.scpi.query_f64(":COUN:CURR?")?))
    }

    /// Voltmeter reading. `:DVM:CURR?` went unanswered with the DVM switched off, so it is only
    /// queried while the DVM is enabled.
    pub fn dvm_value(&mut self) -> Result<Option<f64>> {
        if !self.may_query(":DVM:CURR?") {
            return Err(DriverError::Unsupported(
                "the DVM reading has not been verified on this instrument yet \
                 (run `mho954 verify dvm.value` with the DVM enabled)"
                    .into(),
            ));
        }
        if !self.scpi.query_bool(":DVM:ENAB?")? {
            return Err(DriverError::State(
                "the DVM is off; enable it first (group `dvm`, enable=true)".into(),
            ));
        }
        Ok(valid_value(self.scpi.query_f64(":DVM:CURR?")?))
    }

    /// Reads the event table of decoder bus `n` (1..4). The decoder must be on; its event
    /// table is switched on if needed (the data come from it).
    pub fn bus_data(&mut self, n: u8) -> Result<mho_dsp::DecodeTable> {
        if !(1..=4).contains(&n) {
            return Err(DriverError::Invalid(format!("bus {n} outside 1..=4")));
        }
        if !self.may_query(":BUS1:DATA?") {
            return Err(DriverError::Unsupported(
                ":BUS<n>:DATA? has not been verified on this instrument yet \
                 (run `mho954 verify bus.data`)"
                    .into(),
            ));
        }
        // An unanswered query costs a lot here, so the preconditions are checked first.
        if self.may_query(":BUS1:DISP?") && !self.scpi.query_bool(&format!(":BUS{n}:DISP?"))? {
            return Err(DriverError::State(format!(
                "decoder {n} is off; enable it first (group `bus`, display=true)"
            )));
        }
        if self.may_query(":BUS1:EVEN?") && !self.scpi.query_bool(&format!(":BUS{n}:EVEN?"))? {
            self.scpi.write(&format!(":BUS{n}:EVEN ON"))?;
            std::thread::sleep(Duration::from_millis(300));
        }
        let data = self.scpi.query_block_message(&format!(":BUS{n}:DATA?"))?;
        Ok(mho_dsp::parse_decode_table(&String::from_utf8_lossy(&data)))
    }

    /// Reads a set of values (cursor readings, mask/search/histogram results). The guard
    /// query is checked first: outside the matching mode the scope would not answer.
    pub fn readout(&mut self, id: &str) -> Result<ParamMap> {
        let r = crate::ops::find_readout(id)?;
        if !self.trust_unverified && !crate::ops::readout_verified(r) {
            return Err(DriverError::Unsupported(format!(
                "readout {} has not been verified on this instrument yet (run `mho954 verify {}`)",
                r.id, r.id
            )));
        }
        if let Some((q, expect)) = r.guard {
            let a = self.scpi.query(q)?;
            if !crate::ops::guard_ok(&a, expect) {
                return Err(DriverError::State(format!(
                    "{}: needs {} {expect} (is {a}); {}",
                    r.id,
                    q.trim_end_matches('?'),
                    r.title
                )));
            }
        }
        let mut out = ParamMap::new();
        for item in r.items {
            let a = self.scpi.query(item.query)?;
            out.extend(crate::ops::decode_item(item, &a)?);
        }
        Ok(out)
    }

    /// Runs a one-shot command (save/load on the scope, resets, recording). `arg` is the
    /// file path for save/load; destructive commands need `confirm`.
    pub fn run_command(&mut self, id: &str, arg: Option<&str>, confirm: bool) -> Result<()> {
        let c = crate::ops::find_command(id)?;
        if !self.requirement_met(c.requires)? {
            return Err(DriverError::Unsupported(format!(
                "{} needs {}",
                c.id,
                c.requires.describe()
            )));
        }
        if c.confirm && !confirm {
            return Err(DriverError::Invalid(format!(
                "{} must be confirmed explicitly ({})",
                c.id, c.title
            )));
        }
        let line = match (c.arg, arg) {
            (crate::ops::CommandArg::None, None) => c.command.to_string(),
            (crate::ops::CommandArg::None, Some(_)) => {
                return Err(DriverError::Invalid(format!("{} takes no argument", c.id)));
            }
            (crate::ops::CommandArg::Path { extensions }, Some(p)) => {
                format!(
                    "{} {}",
                    c.command,
                    crate::ops::validate_path(p, extensions)?
                )
            }
            (crate::ops::CommandArg::Index { min, max }, Some(a)) => match a.trim().parse::<u8>() {
                Ok(i) if (min..=max).contains(&i) => format!("{} {i}", c.command),
                _ => {
                    return Err(DriverError::Invalid(format!(
                        "{}: index {a:?} outside {min}..={max}",
                        c.id
                    )));
                }
            },
            (crate::ops::CommandArg::Index { min, max }, None) => {
                return Err(DriverError::Invalid(format!(
                    "{} needs an index {min}..={max}",
                    c.id
                )));
            }
            (crate::ops::CommandArg::Path { extensions }, None) => {
                return Err(DriverError::Invalid(format!(
                    "{} needs a path like D:/name.{}",
                    c.id, extensions[0]
                )));
            }
        };
        self.drain_errors()?;
        self.scpi.write(&line)?;
        if c.id == "reset" {
            // Settings changed wholesale: forget everything cached.
            self.wav = WavSettings::default();
            self.chan_cache = Default::default();
            self.measured.clear();
            std::thread::sleep(Duration::from_millis(500));
        }
        self.check_errors()
    }

    /// Peak table of the FFT on math channel `n` (FFT operator and peak search must be on).
    pub fn fft_peaks(&mut self, n: u8) -> Result<Vec<mho_dsp::ScopePeak>> {
        if !(1..=4).contains(&n) {
            return Err(DriverError::Invalid(format!("math {n} outside 1..=4")));
        }
        if !self.may_query(":MATH1:FFT:SEAR:RES?") {
            return Err(DriverError::Unsupported(
                ":MATH<n>:FFT:SEARch:RES? has not been verified on this instrument yet \
                 (run `mho954 verify math.peaks`)"
                    .into(),
            ));
        }
        // An unanswered query costs a lot here, so the preconditions are checked first.
        if self.may_query(":MATH1:FFT:SEAR:ENAB?") {
            let state = self.scpi.query_many(&[
                format!(":MATH{n}:DISP?"),
                format!(":MATH{n}:OPER?"),
                format!(":MATH{n}:FFT:SEAR:ENAB?"),
            ])?;
            let on = |s: &str| s.trim() == "1" || s.trim().eq_ignore_ascii_case("ON");
            if !on(&state[0]) || !state[1].trim().eq_ignore_ascii_case("FFT") || !on(&state[2]) {
                return Err(DriverError::State(format!(
                    "math {n} must be an FFT with peak search on \
                     (group `math.fft`, activate=true, peak_search=true)"
                )));
            }
        }
        let lines = self.scpi.query_lines(&format!(":MATH{n}:FFT:SEAR:RES?"))?;
        Ok(mho_dsp::parse_fft_peaks(&lines))
    }

    /// Math channels shown in the time domain (FFT results are left out: their x axis is
    /// frequency). Empty until the math queries are verified on the instrument.
    pub fn displayed_math(&mut self) -> Result<Vec<u8>> {
        if !self.may_query(":MATH1:DISP?") || !self.may_query(":MATH1:OPER?") {
            return Ok(Vec::new());
        }
        let disp = self.scpi.query_many(
            &(1..=4)
                .map(|n| format!(":MATH{n}:DISP?"))
                .collect::<Vec<_>>(),
        )?;
        let mut out = Vec::new();
        for (n, d) in (1..=4u8).zip(&disp) {
            if d.trim() == "1" || d.trim().eq_ignore_ascii_case("ON") {
                let op = self.scpi.query(&format!(":MATH{n}:OPER?"))?;
                if !op.trim().eq_ignore_ascii_case("FFT") {
                    out.push(n);
                }
            }
        }
        Ok(out)
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
        if normalize_source(source)?.starts_with("MATH") {
            return Err(DriverError::Invalid(
                "math channels can only be read from the screen (mode norm)".into(),
            ));
        }
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
        // The preamble reports the current read window (STAR..STOP), not the record: after
        // a 10M read at 500M depth it still said 10M. Open the window to the memory depth
        // first (seen on the device: 500 000 000 points, beyond the guide's 50M).
        let depth = self
            .scpi
            .query(":ACQ:MDEP?")
            .ok()
            .and_then(|a| a.trim().parse::<f64>().ok())
            .filter(|d| *d >= 1.0)
            .map_or(MAX_RAW_POINTS, |d| (d as usize).min(MAX_RAW_POINTS));
        self.scpi.write(":WAV:STAR 1")?;
        self.scpi.write(&format!(":WAV:STOP {depth}"))?;
        let rejected = self.scpi.errors()?;
        if !rejected.is_empty() {
            debug!(
                ?rejected,
                depth, "RAW window beyond the record; using the scope's window"
            );
        }
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

    /// Licensable options with their state, followed by the standard features that other
    /// series sell as options (`standard`, not queried).
    pub fn options(&mut self) -> Result<Vec<OptionInfo>> {
        let mut out = OPTIONS
            .iter()
            .map(|o| {
                Ok(OptionInfo {
                    name: o.code.to_string(),
                    description: o.description.to_string(),
                    active: self
                        .scpi
                        .query_bool(&format!(":SYST:OPT:STAT? {}", o.token))?,
                    standard: false,
                })
            })
            .collect::<Result<Vec<_>>>()?;
        out.extend(STANDARD.iter().map(|o| OptionInfo {
            name: o.code.to_string(),
            description: o.description.to_string(),
            active: true,
            standard: true,
        }));
        Ok(out)
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
        self.caps = None;
        self.scpi
            .with_timeout(Duration::from_secs(10), |s| s.query("*OPC?"))?;
        self.check_errors()
    }

    // ---- network (RJ45) -----------------------------------------------------------------

    pub fn network(&mut self) -> Result<NetworkState> {
        let cmds = queries(&[
            ":LAN:STAT?",
            ":LAN:DHCP?",
            ":LAN:AUT?",
            ":LAN:MAN?",
            ":LAN:IPAD?",
            ":LAN:SMAS?",
            ":LAN:GAT?",
            ":LAN:DNS?",
            ":LAN:MAC?",
            ":LAN:DSER?",
            ":LAN:MDNS?",
            ":LAN:HOST:NAME?",
            ":LAN:DESC?",
            ":LAN:VISA?",
        ]);
        let a = self.scpi.query_many(&cmds)?;
        Ok(NetworkState {
            status: a[0].clone(),
            dhcp: parse_bool(&cmds[1], &a[1])?,
            auto_ip: parse_bool(&cmds[2], &a[2])?,
            manual: parse_bool(&cmds[3], &a[3])?,
            ip: a[4].clone(),
            mask: a[5].clone(),
            gateway: a[6].clone(),
            dns: a[7].clone(),
            mac: a[8].clone(),
            dhcp_server: a[9].clone(),
            mdns: parse_bool(&cmds[10], &a[10])?,
            hostname: a[11].clone(),
            description: a[12].clone(),
            visa: a[13].clone(),
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

    // ---- capabilities and setting groups --------------------------------------------------

    /// Hardware modules (`:SYST:MOD?`, verified): logic analyzer and function generator.
    pub fn modules(&mut self) -> Result<Modules> {
        let a = self.scpi.query(":SYST:MOD?")?;
        let f: Vec<&str> = a.split(',').map(str::trim).collect();
        Ok(Modules {
            logic_analyzer: f.first() == Some(&"1"),
            function_generator: f.get(1) == Some(&"1"),
        })
    }

    /// Model bandwidth with upgrades, memory limits, generator and licensed protocols. Queried
    /// once per connection (about 13 verified queries), then cached.
    pub fn capabilities(&mut self) -> Result<Capabilities> {
        if let Some(c) = &self.caps {
            return Ok(c.clone());
        }
        let options = self.options()?;
        let modules = self.modules()?;
        let confirmed = self.may_query(GENERATOR_CANARY);
        let c = Capabilities::derive(&self.idn, &options, modules, confirmed);
        self.caps = Some(c.clone());
        Ok(c)
    }

    /// `true` if the hardware or option a group needs is present.
    pub fn requirement_met(&mut self, r: params::Requires) -> Result<bool> {
        Ok(match r {
            params::Requires::Nothing => true,
            params::Requires::LogicAnalyzer => self.capabilities()?.logic_analyzer,
            params::Requires::FunctionGenerator => self.capabilities()?.generator.is_some(),
            params::Requires::Option(code) => self.capabilities()?.has_option(code),
        })
    }

    fn ensure_available(&mut self, g: &params::Group) -> Result<()> {
        if self.requirement_met(g.requires)? {
            Ok(())
        } else {
            Err(DriverError::Unsupported(format!(
                "{} needs {} (see `mho954 caps`)",
                g.id,
                g.requires.describe()
            )))
        }
    }

    /// Reads all parameters of a settings group. Refuses groups whose queries have not been
    /// confirmed on the instrument unless `allow_unverified` (an unknown query blocks the scope).
    pub fn group_get(
        &mut self,
        id: &str,
        instance: Option<u8>,
        allow_unverified: bool,
    ) -> Result<ParamMap> {
        let g = params::find_group(id)?;
        let n = check_instance(g, instance)?;
        self.ensure_available(g)?;
        if !allow_unverified && !self.trust_unverified && !params::group_verified(g) {
            return Err(DriverError::Unsupported(format!(
                "{} has not been verified on this instrument yet (run `mho954 verify {}`)",
                g.id, g.id
            )));
        }
        let plain: Vec<&params::ParamSpec> = g
            .params
            .iter()
            .filter(|p| !matches!(p.kind, params::ParamKind::SourceLevel { .. }))
            .collect();
        let cmds: Vec<String> = plain
            .iter()
            .map(|p| params::query_for(p, n, None))
            .collect();
        let answers = self.scpi.query_many(&cmds)?;
        let mut out = ParamMap::new();
        for (p, a) in plain.iter().zip(&answers) {
            out.insert(p.name.to_string(), params::decode(p, a)?);
        }
        for p in g.params {
            if let params::ParamKind::SourceLevel { source_param } = p.kind {
                let source = match out.get(source_param) {
                    Some(ParamValue::Text(s)) => s.clone(),
                    _ => "CHAN1".into(),
                };
                let a = self.scpi.query(&params::query_for(p, n, Some(&source)))?;
                out.insert(p.name.to_string(), params::decode(p, &a)?);
            }
        }
        Ok(out)
    }

    /// Writes the given parameters (all validated first). `select` also activates the group
    /// (e.g. switches the trigger type). Returns the group's state if it may be read.
    pub fn group_set(
        &mut self,
        id: &str,
        instance: Option<u8>,
        values: &ParamMap,
        select: bool,
    ) -> Result<Option<ParamMap>> {
        let g = params::find_group(id)?;
        let n = check_instance(g, instance)?;
        self.ensure_available(g)?;
        if g.id == "afg"
            && let Some(gen_caps) = self.capabilities()?.generator
        {
            let current = if self.trust_unverified || params::group_verified(g) {
                Some(self.group_get(id, instance, false)?)
            } else {
                None
            };
            crate::generator::check_update(&gen_caps, values, current.as_ref())?;
        }
        let mut cmds = Vec::new();
        for (name, v) in values {
            let spec = g.params.iter().find(|p| p.name == name).ok_or_else(|| {
                DriverError::Invalid(format!("{}: unknown parameter {name:?}", g.id))
            })?;
            let encoded = params::encode(spec, v)?;
            let source = match spec.kind {
                params::ParamKind::SourceLevel { source_param } => match values.get(source_param) {
                    Some(src) => {
                        let sp = g
                            .params
                            .iter()
                            .find(|p| p.name == source_param)
                            .expect("checked in tables");
                        Some(params::encode(sp, src)?)
                    }
                    None => {
                        return Err(DriverError::Invalid(format!(
                            "{name}: give `{source_param}` too (the level belongs to a source)"
                        )));
                    }
                },
                _ => None,
            };
            cmds.push(params::write_command(spec, n, &encoded, source.as_deref()));
        }
        self.drain_errors()?;
        if select && let Some(sel) = g.select {
            self.scpi.write(&sel.replace("{n}", &n.to_string()))?;
        }
        for c in &cmds {
            self.scpi.write(c)?;
        }
        if !self.scpi.errors()?.is_empty() {
            // Interdependent limits (lower ≤ upper) can reject the first pass; the second pass
            // sees the already updated counterpart.
            for c in &cmds {
                self.scpi.write(c)?;
            }
            self.check_errors()?;
        }
        if self.trust_unverified || params::group_verified(g) {
            Ok(Some(self.group_get(id, instance, false)?))
        } else {
            Ok(None)
        }
    }

    // ---- raw SCPI -----------------------------------------------------------------------

    /// Sends arbitrary SCPI. Queries (containing `?`) return the response line; commands
    /// return `None` after checking the error queue.
    ///
    /// The MHO954 does not answer unknown queries; after about five of them since power-on its
    /// firmware stops answering anything, even with device clears and valid queries in between. So after an unanswered query
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

fn check_instance(g: &params::Group, instance: Option<u8>) -> Result<u8> {
    match (g.instances, instance) {
        (None, None) => Ok(1),
        (None, Some(_)) => Err(DriverError::Invalid(format!("{} has no instances", g.id))),
        (Some((lo, _)), None) => Ok(lo),
        (Some((lo, hi)), Some(n)) if (lo..=hi).contains(&n) => Ok(n),
        (Some((lo, hi)), Some(n)) => Err(DriverError::Invalid(format!(
            "{}: instance {n} outside {lo}..={hi}",
            g.id
        ))),
    }
}

fn queries(q: &[&str]) -> Vec<String> {
    q.iter().map(|s| s.to_string()).collect()
}

fn parse_f64(cmd: &str, v: &str) -> Result<f64> {
    v.trim()
        .parse::<f64>()
        .map_err(|e| mho_scpi::ScpiError::parse(cmd, format!("{v:?}: {e}")).into())
}

fn parse_bool(cmd: &str, v: &str) -> Result<bool> {
    match v.trim().to_ascii_uppercase().as_str() {
        "1" | "ON" => Ok(true),
        "0" | "OFF" => Ok(false),
        other => {
            Err(mho_scpi::ScpiError::parse(cmd, format!("expected boolean, got {other:?}")).into())
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

/// Points of a memory depth token (`10k`, `125M`, `1e6`, `25000000`); `None` for AUTO or
/// anything unparsable (left to the scope).
fn depth_points(d: &str) -> Option<f64> {
    let u = d.trim().to_ascii_uppercase();
    let (num, mult) = match u.as_bytes().last()? {
        b'K' => (&u[..u.len() - 1], 1e3),
        b'M' => (&u[..u.len() - 1], 1e6),
        _ => (u.as_str(), 1.0),
    };
    num.parse::<f64>().ok().map(|v| v * mult)
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
