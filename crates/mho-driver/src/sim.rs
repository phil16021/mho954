//! A software model of the MHO954 that speaks the same SCPI subset as [`crate::Scope`].
//!
//! Channels carry fixed test signals (CH1 1 kHz probe-compensation square wave, CH2 5 kHz
//! sine, CH3 50 kHz triangle, CH4 200 kHz AM sine) with fresh noise on every read, so live
//! views visibly update. Like the real instrument, unknown queries get no response.

use std::f64::consts::PI;
use std::io;
use std::time::{Duration, Instant};

use mho_scpi::{ReadChunk, Transport};

use crate::measure::find_item;
use crate::model::{AcquireType, Coupling, Impedance, Slope, TriggerSweep, WaveMode};
use crate::options::OPTIONS;

const SCREEN_POINTS: usize = 1000;
/// ADC codes per vertical division (matches the real scope's WORD scaling).
const CODES_PER_DIV: f64 = 7500.0;
const Y_REF: f64 = 32768.0;
/// Options installed on the upgraded test scope (order codes).
const SIM_LICENSED: &[&str] = &[
    "BWU05T08", "RLU05", "AFG50", "AFG100", "AUTOA", "FlexA", "AUDIOA", "AEROA",
];
/// Corner frequency of the simulated device under test behind the function generator.
pub const SIM_DUT_CORNER: f64 = 10e3;

/// `(channel, property)` of a generator node, e.g. `:SOUR2:VOLT:AMPL` → `('2', "VOLT:AMPL")`.
/// Table nodes (`:SOUR{n}:…`) give `'n'` as channel.
fn afg_prop(node: &str) -> Option<(char, &str)> {
    let rest = node.strip_prefix(":SOUR")?;
    if let Some(prop) = rest.strip_prefix("{n}:") {
        return Some(('n', prop));
    }
    let mut chars = rest.chars();
    let n = chars.next().filter(|c| matches!(c, '1' | '2'))?;
    Some((n, chars.as_str().strip_prefix(':')?))
}

/// Output of a simulated generator channel.
#[derive(Debug, Clone)]
struct AfgWave {
    /// Short form of `:SOUR<n>:FUNC` (SIN, SQU, RAMP, DC, NOIS, …).
    function: String,
    frequency: f64,
    /// Vpp.
    amplitude: f64,
    offset: f64,
    /// Start phase in radians.
    phase: f64,
    /// Square duty cycle 0..1.
    duty: f64,
    /// Ramp symmetry 0..1.
    symmetry: f64,
}

impl AfgWave {
    fn value(&self, t: f64) -> f64 {
        let a = self.amplitude / 2.0;
        let cycles = self.frequency * t + self.phase / (2.0 * PI);
        let x = cycles - cycles.floor();
        let shape = match self.function.as_str() {
            "SQU" => {
                if x < self.duty {
                    1.0
                } else {
                    -1.0
                }
            }
            "RAMP" => {
                let s = self.symmetry.clamp(1e-6, 1.0 - 1e-6);
                if x < s {
                    -1.0 + 2.0 * x / s
                } else {
                    1.0 - 2.0 * (x - s) / (1.0 - s)
                }
            }
            "DC" => 0.0,
            // Deterministic pseudo noise from the phase (no RNG state needed here).
            "NOIS" => ((t * 1e9).sin() * 43758.5453).fract() * 2.0 - 1.0,
            _ => (2.0 * PI * cycles).sin(),
        };
        self.offset + a * shape
    }
}

#[derive(Debug, Clone)]
struct SimChannel {
    display: bool,
    scale: f64,
    offset: f64,
    coupling: Coupling,
    bw_limit: String,
    probe: f64,
    impedance: Impedance,
    invert: bool,
    vernier: bool,
}

#[derive(Debug, Clone)]
struct SimLan {
    dhcp: bool,
    auto_ip: bool,
    manual: bool,
    ip: String,
    mask: String,
    gateway: String,
    dns: String,
    mdns: bool,
    hostname: String,
    description: String,
}

impl Default for SimLan {
    fn default() -> Self {
        SimLan {
            dhcp: true,
            auto_ip: true,
            manual: false,
            ip: "0.0.0.0".into(),
            mask: "0.0.0.0".into(),
            gateway: "0.0.0.0".into(),
            dns: "0.0.0.0".into(),
            mdns: false,
            hostname: "MHO954".into(),
            description: "MHO954".into(),
        }
    }
}

#[derive(Debug, Clone)]
struct SimState {
    ch: [SimChannel; 4],
    tim_scale: f64,
    tim_offset: f64,
    tim_mode: String,
    trig_mode: String,
    trig_sweep: TriggerSweep,
    trig_coupling: String,
    trig_holdoff: f64,
    trig_source: String,
    trig_slope: Slope,
    trig_level: f64,
    acq_type: AcquireType,
    acq_aver: u32,
    acq_mdep: Option<usize>,
    running: bool,
    wav_source: String,
    wav_mode: WaveMode,
    wav_word: bool,
    wav_start: usize,
    wav_stop: usize,
    lan: SimLan,
    /// Pending LAN settings, applied by `:LAN:APPLy`.
    lan_pending: SimLan,
}

impl Default for SimState {
    fn default() -> Self {
        let ch = |display: bool, scale: f64| SimChannel {
            display,
            scale,
            offset: 0.0,
            coupling: Coupling::DC,
            bw_limit: "OFF".into(),
            probe: 1.0,
            impedance: Impedance::OneMeg,
            invert: false,
            vernier: false,
        };
        SimState {
            ch: [ch(true, 1.0), ch(true, 0.5), ch(false, 0.2), ch(false, 0.5)],
            tim_scale: 200e-6,
            tim_offset: 0.0,
            tim_mode: "MAIN".into(),
            trig_mode: "EDGE".into(),
            trig_sweep: TriggerSweep::Auto,
            trig_coupling: "DC".into(),
            trig_holdoff: 8e-9,
            trig_source: "CHAN1".into(),
            trig_slope: Slope::Positive,
            trig_level: 1.5,
            acq_type: AcquireType::Normal,
            acq_aver: 2,
            acq_mdep: None,
            running: true,
            wav_source: "CHAN1".into(),
            wav_mode: WaveMode::Normal,
            wav_word: true,
            wav_start: 1,
            wav_stop: SCREEN_POINTS,
            lan: SimLan::default(),
            lan_pending: SimLan::default(),
        }
    }
}

/// Simulator behaviour switches; `MHO954_SIM` (e.g. `latency=25,nosignal=1+4,fullidn`)
/// configures the simulator used by `--connection sim`.
#[derive(Debug, Clone)]
pub struct SimOptions {
    /// Per-response latency (the real scope needs ~25 ms over USB/IP).
    pub latency: Duration,
    /// Channels that deliver no data (all 0xFFFF), like CH1/CH4 on the test scope.
    pub no_signal: [bool; 4],
    /// Report the shortened firmware version like the real `*IDN?` (`00.01.00`).
    pub truncate_idn: bool,
    /// Unknown queries since power-on after which the simulated firmware stops answering for
    /// good. The real MHO954 hung after the 5th, even with valid queries and device clears in
    /// between.
    pub hang_after: u32,
    /// Apply the hardware verification gate like on the real scope (option `strict`).
    /// Otherwise the simulator is trusted with commands not yet verified on the instrument,
    /// so every feature can be tried without hardware.
    pub strict: bool,
    /// Report logic analyzer and function generator hardware (`:SYST:MOD?`) and the AFG
    /// licenses; option `noafg` turns the generator off.
    pub afg: bool,
    /// Report the options of the upgraded test scope as installed (BWU05T08, RLU05, AFG,
    /// CAN-FD, FlexRay, I2S, MIL-STD-1553); option `noopt` reports none, like before the
    /// upgrade.
    pub licensed: bool,
    /// A logic probe is plugged in. Without one (option `noprobe`, like the test scope) the
    /// scope rejects D0..D15 as parallel decoder source with -200.
    pub la_probe: bool,
}

impl Default for SimOptions {
    fn default() -> Self {
        SimOptions {
            latency: Duration::ZERO,
            no_signal: [false; 4],
            truncate_idn: true,
            hang_after: 5,
            strict: false,
            afg: true,
            licensed: true,
            la_probe: true,
        }
    }
}

impl SimOptions {
    pub fn from_env() -> Self {
        let mut o = SimOptions::default();
        let Ok(spec) = std::env::var("MHO954_SIM") else {
            return o;
        };
        for part in spec.split(',').map(str::trim).filter(|p| !p.is_empty()) {
            let (k, v) = part.split_once('=').unwrap_or((part, ""));
            match k {
                "latency" => {
                    if let Ok(ms) = v.trim_end_matches("ms").parse::<u64>() {
                        o.latency = Duration::from_millis(ms);
                    }
                }
                "nosignal" => {
                    for n in v.split('+').filter_map(|c| c.parse::<usize>().ok()) {
                        if (1..=4).contains(&n) {
                            o.no_signal[n - 1] = true;
                        }
                    }
                }
                "fullidn" => o.truncate_idn = false,
                "strict" => o.strict = true,
                "noafg" => o.afg = false,
                "noopt" => o.licensed = false,
                "noprobe" => o.la_probe = false,
                "hang_after" => o.hang_after = v.parse().unwrap_or(o.hang_after),
                _ => tracing::warn!("MHO954_SIM: unknown option {k:?}"),
            }
        }
        o
    }
}

pub struct SimTransport {
    state: SimState,
    opts: SimOptions,
    /// An unknown query was received: nothing is answered until a device clear.
    blocked: bool,
    /// Unknown queries since power-on.
    unknown_count: u32,
    /// Firmware hung for good (power cycle needed).
    hung: bool,
    /// Length of `pending` when the current message arrived (compound answers).
    message_start: usize,
    /// Values of table-driven settings (`crate::params`), keyed by node (+ source).
    params: std::collections::HashMap<String, String>,
    /// `:MEAS:ITEM?` item/source pairs seen; the first query of each returns 9.9E37.
    measured: std::collections::HashSet<String>,
    errors: Vec<(i32, String)>,
    pending: Vec<u8>,
    rng: u64,
    started: Instant,
    /// Frozen capture time while stopped, so RAW reads are consistent.
    frozen_at: f64,
    timeout: Duration,
    /// Artificial per-query latency (the real scope needs ~25 ms over USB).
    pub latency: Duration,
}

impl Default for SimTransport {
    fn default() -> Self {
        Self::new()
    }
}

impl SimTransport {
    pub fn new() -> Self {
        Self::with_options(SimOptions::default())
    }

    /// Simulator configured from `MHO954_SIM`.
    pub fn from_env() -> Self {
        Self::with_options(SimOptions::from_env())
    }

    pub fn with_options(opts: SimOptions) -> Self {
        SimTransport {
            state: SimState::default(),
            latency: opts.latency,
            opts,
            blocked: false,
            unknown_count: 0,
            hung: false,
            measured: Default::default(),
            message_start: 0,
            params: Default::default(),
            errors: Vec::new(),
            pending: Vec::new(),
            rng: 0x2545_F491_4F6C_DD1D,
            started: Instant::now(),
            frozen_at: 0.0,
            timeout: Duration::from_secs(2),
        }
    }

    fn noise(&mut self) -> f64 {
        // xorshift64*, mapped to roughly N(0, 1) via sum of uniforms.
        let mut acc = 0.0;
        for _ in 0..4 {
            self.rng ^= self.rng >> 12;
            self.rng ^= self.rng << 25;
            self.rng ^= self.rng >> 27;
            let u =
                (self.rng.wrapping_mul(0x2545_F491_4F6C_DD1D) >> 11) as f64 / (1u64 << 53) as f64;
            acc += u - 0.5;
        }
        acc * 1.7
    }

    fn now(&self) -> f64 {
        if self.state.running {
            self.started.elapsed().as_secs_f64()
        } else {
            self.frozen_at
        }
    }

    /// Ideal signal of channel `n` (1-based) at time `t` relative to the trigger point.
    fn signal(n: u8, t: f64, wall: f64) -> f64 {
        match n {
            1 => {
                // Probe-compensation output: 1 kHz, 0..3 V, ~100 ns edges.
                let ph = (t * 1e3).rem_euclid(1.0);
                let edge = 1e-4;
                if ph < edge {
                    3.0 * ph / edge
                } else if ph < 0.5 {
                    3.0
                } else if ph < 0.5 + edge {
                    3.0 * (1.0 - (ph - 0.5) / edge)
                } else {
                    0.0
                }
            }
            2 => (2.0 * PI * 5e3 * t).sin(),
            3 => {
                let ph = (t * 50e3 + 0.25).rem_euclid(1.0);
                0.5 * (4.0 * (ph - 0.5).abs() - 1.0)
            }
            _ => {
                let m = 0.5 * (1.0 + 0.6 * (2.0 * PI * 0.5 * wall).sin());
                m * (2.0 * PI * 200e3 * t).sin()
            }
        }
    }

    fn channel_index(src: &str) -> Option<usize> {
        src.strip_prefix("CHAN")
            .and_then(|d| d.parse::<usize>().ok())
            .filter(|n| (1..=4).contains(n))
    }

    fn mdepth_points(&self) -> usize {
        self.state.acq_mdep.unwrap_or(10_000)
    }

    fn srate(&self) -> f64 {
        self.mdepth_points() as f64 / (10.0 * self.state.tim_scale)
    }

    /// Preamble values for the current waveform settings.
    fn preamble(&self) -> (usize, f64, f64, f64, f64) {
        self.preamble_of(&self.state.wav_source)
    }

    /// Preamble values `(points, x_inc, x_org, y_inc, y_org)` for `src`.
    fn preamble_of(&self, src: &str) -> (usize, f64, f64, f64, f64) {
        let s = &self.state;
        let points = match s.wav_mode {
            WaveMode::Raw => self.mdepth_points(),
            _ => SCREEN_POINTS,
        };
        let x_inc = 10.0 * s.tim_scale / points as f64;
        let x_org = -5.0 * s.tim_scale + s.tim_offset;
        let (y_inc, y_org) = match Self::channel_index(src) {
            Some(n) => {
                let c = &s.ch[n - 1];
                let y_inc = c.scale / CODES_PER_DIV;
                (y_inc, -(c.offset / y_inc).round())
            }
            None => match Self::math_index(src) {
                Some(m) => {
                    let (scale, offset) = self.math_scale(m);
                    let y_inc = scale / CODES_PER_DIV;
                    (y_inc, -(offset / y_inc).round())
                }
                None => (1.0, 0.0),
            },
        };
        (points, x_inc, x_org, y_inc, y_org)
    }

    /// Screen-resolution sample interval and start time.
    fn screen_timing(&self) -> (f64, f64) {
        let s = &self.state;
        (
            10.0 * s.tim_scale / SCREEN_POINTS as f64,
            -5.0 * s.tim_scale + s.tim_offset,
        )
    }

    fn samples(
        &mut self,
        src: &str,
        first: usize,
        count: usize,
        x_inc: f64,
        x_org: f64,
    ) -> Vec<f64> {
        if let Some(m) = Self::math_index(src) {
            return self.math_samples(m, first, count, x_inc, x_org);
        }
        let Some(n) = Self::channel_index(src) else {
            return vec![0.0; count];
        };
        let wall = self.now();
        let ch = self.state.ch[n - 1].clone();
        let noise_amp = ch.scale * 0.02;
        let gi = self.afg_wave(1);
        let gii = self.afg_wave(2);
        (first..first + count)
            .map(|i| {
                let t = i as f64 * x_inc + x_org;
                let mut v = match (n, &gi, &gii) {
                    // GI is cabled to CH1 and GII to CH2 (like the test bench). With GII off,
                    // GI also drives a first-order low-pass (fc = 10 kHz) seen on CH2.
                    (1, Some(w), _) => w.value(t),
                    (2, _, Some(w)) => w.value(t),
                    (2, Some(w), None) if w.function == "SIN" => {
                        let r = w.frequency / SIM_DUT_CORNER;
                        let gain = 1.0 / (1.0 + r * r).sqrt();
                        let a = w.amplitude / 2.0;
                        w.offset
                            + gain * a * (2.0 * PI * w.frequency * t + w.phase - r.atan()).sin()
                    }
                    _ => Self::signal(n as u8, t, wall),
                };
                if ch.coupling == Coupling::AC {
                    v -= [1.5, 0.0, 0.0, 0.0][n - 1];
                }
                if ch.coupling == Coupling::GND {
                    v = 0.0;
                }
                if ch.invert {
                    v = -v;
                }
                v + noise_amp * self.noise()
            })
            .collect()
    }

    /// Keeps the generator's dependent values consistent after `hdr` was written:
    /// frequency ↔ period, amplitude/offset ↔ high/low level.
    fn afg_coupled(&mut self, hdr: &str) {
        let Some((n, prop)) = afg_prop(hdr) else {
            return;
        };
        let get = |p: &str, default: f64| {
            self.params
                .get(&format!(":SOUR{n}:{p}"))
                .and_then(|v| v.parse::<f64>().ok())
                .unwrap_or(default)
        };
        let derived: Vec<(&str, f64)> = match prop {
            "FREQ" => vec![("PER", 1.0 / get("FREQ", 1e3))],
            "PER" => vec![("FREQ", 1.0 / get("PER", 1e-3))],
            "VOLT:AMPL" | "VOLT:OFFS" => {
                let (a, o) = (get("VOLT:AMPL", 5.0), get("VOLT:OFFS", 0.0));
                vec![("VOLT:HIGH", o + a / 2.0), ("VOLT:LOW", o - a / 2.0)]
            }
            "VOLT:HIGH" | "VOLT:LOW" => {
                let (h, l) = (get("VOLT:HIGH", 2.5), get("VOLT:LOW", -2.5));
                vec![("VOLT:AMPL", h - l), ("VOLT:OFFS", (h + l) / 2.0)]
            }
            _ => vec![],
        };
        for (p, v) in derived {
            self.params
                .insert(format!(":SOUR{n}:{p}"), format!("{v:E}"));
        }
    }

    /// Waveform of generator `n` (1 = GI, 2 = GII) while its output is on.
    fn afg_wave(&self, n: u8) -> Option<AfgWave> {
        if !self.opts.afg {
            return None;
        }
        let p = |prop: &str, default: &str| self.param_or(&format!(":SOUR{n}:{prop}"), default);
        let num = |prop: &str, default: f64| p(prop, "").parse::<f64>().unwrap_or(default);
        let on = p("OUTP:STAT", "0");
        if on != "ON" && on != "1" {
            return None;
        }
        let function = p("FUNC", "SIN").to_ascii_uppercase();
        Some(AfgWave {
            function: crate::params::short_form(&function),
            frequency: num("FREQ", 1000.0),
            amplitude: num("VOLT:AMPL", 5.0),
            offset: num("VOLT:OFFS", 0.0),
            phase: num("PHAS", 0.0).to_radians(),
            duty: num("FUNC:SQU:DUTY", 50.0) / 100.0,
            symmetry: num("FUNC:RAMP:SYMM", 50.0) / 100.0,
        })
    }

    fn math_index(src: &str) -> Option<u8> {
        src.strip_prefix("MATH")
            .and_then(|d| d.parse::<u8>().ok())
            .filter(|n| (1..=4).contains(n))
    }

    fn param_or(&self, key: &str, default: &str) -> String {
        self.params
            .get(key)
            .cloned()
            .unwrap_or_else(|| default.into())
            .to_ascii_uppercase()
    }

    /// `(scale, offset)` of math channel `m`.
    fn math_scale(&self, m: u8) -> (f64, f64) {
        let scale = self
            .param_or(&format!(":MATH{m}:SCAL"), "1")
            .parse::<f64>()
            .ok()
            .filter(|s| *s > 0.0)
            .unwrap_or(1.0);
        let offset = self
            .param_or(&format!(":MATH{m}:OFFS"), "0")
            .parse::<f64>()
            .unwrap_or(0.0);
        (scale, offset)
    }

    /// Result of math channel `m` (channel operands only; others count as 0 V).
    fn math_samples(
        &mut self,
        m: u8,
        first: usize,
        count: usize,
        x_inc: f64,
        x_org: f64,
    ) -> Vec<f64> {
        let op = self.param_or(&format!(":MATH{m}:OPER"), "ADD");
        let s1 = self.param_or(&format!(":MATH{m}:SOUR1"), "CHAN1");
        let s2 = self.param_or(&format!(":MATH{m}:SOUR2"), "CHAN1");
        let mut operand = |s: &str| {
            if Self::channel_index(s).is_some() {
                self.samples(s, first, count, x_inc, x_org)
            } else {
                vec![0.0; count]
            }
        };
        let a = operand(&s1);
        let b = operand(&s2);
        match op.as_str() {
            "ADD" => a.iter().zip(&b).map(|(x, y)| x + y).collect(),
            "SUBT" => a.iter().zip(&b).map(|(x, y)| x - y).collect(),
            "MULT" => a.iter().zip(&b).map(|(x, y)| x * y).collect(),
            "DIV" => a
                .iter()
                .zip(&b)
                .map(|(x, y)| if *y == 0.0 { 0.0 } else { x / y })
                .collect(),
            "ABS" => a.iter().map(|x| x.abs()).collect(),
            "SQRT" => a.iter().map(|x| x.max(0.0).sqrt()).collect(),
            "INTG" => a
                .iter()
                .scan(0.0, |acc, x| {
                    *acc += x * x_inc;
                    Some(*acc)
                })
                .collect(),
            "DIFF" => {
                let mut d: Vec<f64> = a.windows(2).map(|w| (w[1] - w[0]) / x_inc).collect();
                d.push(d.last().copied().unwrap_or(0.0));
                d
            }
            _ => a,
        }
    }

    /// `:MATH<n>:FFT:SEARch:RES?` lines for the FFT source of math channel `m`, formatted as
    /// the MHO954 does (2026-10-18, FW 00.01.00): `1,1.02520kHz,-6.802dBV` or
    /// `1,1.02516kHz,98.41mVrms`. Peaks below the threshold (in the unit's scale) are left
    /// out; in VRMS the flat-top window lacks its gain correction, as on the scope.
    fn fft_peak_lines(&mut self, m: u8) -> Vec<String> {
        let src = self.param_or(&format!(":MATH{m}:FFT:SOUR"), "CHAN1");
        let num = self
            .param_or(&format!(":MATH{m}:FFT:SEAR:NUM"), "5")
            .parse::<usize>()
            .unwrap_or(5);
        let vrms = self.param_or(&format!(":MATH{m}:FFT:UNIT"), "DB") == "VRMS";
        let threshold = self
            .param_or(&format!(":MATH{m}:FFT:SEAR:THR"), "5.5")
            .parse::<f64>()
            .unwrap_or(5.5);
        let wind = self.param_or(&format!(":MATH{m}:FFT:WIND"), "HANN");
        let window = match wind.as_str() {
            w if w.starts_with("RECT") => mho_dsp::Window::Rectangular,
            w if w.starts_with("BLAC") => mho_dsp::Window::Blackman,
            w if w.starts_with("FLAT") => mho_dsp::Window::FlatTop,
            _ => mho_dsp::Window::Hann,
        };
        let by_freq = self
            .param_or(&format!(":MATH{m}:FFT:SEAR:ORD"), "AMP")
            .starts_with("FREQ");
        let (x_inc, x_org) = self.screen_timing();
        let v = self.samples(&src, 0, SCREEN_POINTS, x_inc, x_org);
        let flat_vrms = if vrms && window == mho_dsp::Window::FlatTop {
            0.2156
        } else {
            1.0
        };
        let mut peaks: Vec<(f64, f64)> = mho_dsp::spectrum(&v, x_inc, window)
            .peaks(num)
            .iter()
            .map(|p| {
                let a = if vrms { p.vrms * flat_vrms } else { p.dbv };
                (p.frequency, a)
            })
            .filter(|&(_, a)| a >= threshold)
            .collect();
        if by_freq {
            peaks.sort_by(|a, b| a.0.total_cmp(&b.0));
        }
        peaks
            .iter()
            .enumerate()
            .map(|(i, &(f, a))| {
                let amp = if vrms {
                    let (x, prefix) = si_scaled(a);
                    format!("{}{prefix}Vrms", significant(x, 4, true))
                } else {
                    format!("{}dBV", significant(a, 4, true))
                };
                let (x, prefix) = si_scaled(f);
                format!("{},{}{prefix}Hz,{amp}", i + 1, significant(x, 6, false))
            })
            .collect()
    }

    fn no_signal(&self, src: &str) -> bool {
        Self::channel_index(src).is_some_and(|n| self.opts.no_signal[n - 1])
    }

    fn wave_codes(&mut self) -> Vec<u16> {
        if self.no_signal(&self.state.wav_source.clone()) {
            let (points, ..) = self.preamble();
            let n = match self.state.wav_mode {
                WaveMode::Raw => {
                    let start = self.state.wav_start.clamp(1, points);
                    self.state.wav_stop.clamp(start, points) - start + 1
                }
                _ => points,
            };
            return vec![u16::MAX; n];
        }
        let (points, x_inc, x_org, y_inc, y_org) = self.preamble();
        let src = self.state.wav_source.clone();
        let (first, count) = match self.state.wav_mode {
            WaveMode::Raw => {
                let start = self.state.wav_start.clamp(1, points);
                let stop = self.state.wav_stop.clamp(start, points);
                (start - 1, stop - start + 1)
            }
            _ => (0, points),
        };
        self.samples(&src, first, count, x_inc, x_org)
            .into_iter()
            .map(|v| (v / y_inc + y_org + Y_REF).round().clamp(0.0, 65535.0) as u16)
            .collect()
    }

    fn measure(&mut self, item: &str, src: &str) -> Option<f64> {
        let it = find_item(item).ok()?;
        if self.no_signal(src) {
            return Some(9.9e37);
        }
        Self::channel_index(src)?;
        let (x_inc, x_org) = self.screen_timing();
        let v = self.samples(src, 0, SCREEN_POINTS, x_inc, x_org);
        let m = mho_dsp::analyze(&v, x_inc);
        let val = match it.token {
            "VMAX" => Some(m.vmax),
            "VMIN" => Some(m.vmin),
            "VPP" => Some(m.vpp),
            "VTOP" => Some(m.vtop),
            "VBAS" => Some(m.vbase),
            "VAMP" => Some(m.vamp),
            "VAVG" => Some(m.vavg),
            "VRMS" => Some(m.vrms),
            "PER" => m.period,
            "FREQ" => m.frequency,
            "RTIM" => m.rise_time,
            "FTIM" => m.fall_time,
            "PDUT" => m.duty,
            "NDUT" => m.duty.map(|d| 100.0 - d),
            "OVER" => m.overshoot,
            "PRES" => m.preshoot,
            _ => None,
        };
        Some(val.unwrap_or(9.9e37))
    }

    fn respond(&mut self, s: impl AsRef<[u8]>) {
        // Compound queries are answered in one line with `;` separators, like the real scope.
        if self.pending.len() > self.message_start && self.pending.last() == Some(&b'\n') {
            self.pending.pop();
            self.pending.push(b';');
        }
        self.pending.extend_from_slice(s.as_ref());
        self.pending.push(b'\n');
    }

    fn respond_block(&mut self, data: &[u8]) {
        let len = data.len().to_string();
        self.pending
            .extend_from_slice(format!("#{}{}", len.len(), len).as_bytes());
        self.pending.extend_from_slice(data);
        self.pending.push(b'\n');
    }

    fn error(&mut self, code: i32, msg: &str) {
        if self.errors.len() < 16 {
            self.errors.push((code, msg.to_string()));
        }
    }

    fn execute(&mut self, line: &str) {
        let line = line.trim();
        if line.is_empty() {
            return;
        }
        let (header, args) = match line.split_once(char::is_whitespace) {
            Some((h, a)) => (h, a.trim()),
            None => (line, ""),
        };
        if self.hung || self.blocked {
            // Like the real firmware: nothing is processed until a device clear (or, once
            // hung, a power cycle).
            return;
        }
        let query = header.ends_with('?');
        let hdr = normalize_header(header.trim_end_matches('?'));
        if query {
            if !self.query(&hdr, args) {
                tracing::debug!(%line, "sim: unknown query (no response, like the real scope)");
                self.blocked = true;
                self.unknown_count += 1;
                if self.unknown_count >= self.opts.hang_after {
                    tracing::warn!(
                        "sim: firmware hung after {} unknown queries",
                        self.unknown_count
                    );
                    self.hung = true;
                }
            }
        } else if let Err(msg) = self.command(&hdr, args) {
            let code = if msg.starts_with("undefined header") {
                -113
            } else if msg.starts_with("execute failed") {
                -200
            } else {
                -224
            };
            self.error(code, &msg);
        }
    }

    /// Returns `false` for unknown queries.
    /// Finds a table-driven parameter by its node.
    fn param_spec(hdr: &str) -> Option<crate::params::ParamSpec> {
        crate::params::GROUPS.iter().find_map(|g| {
            let (lo, hi) = g.instances.unwrap_or((1, 1));
            g.params
                .iter()
                .find(|p| {
                    (lo..=hi).any(|n| {
                        let node = crate::params::node(p, n);
                        node == hdr || normalize_header(&node) == hdr
                    })
                })
                .copied()
        })
    }

    fn param_default(spec: &crate::params::ParamSpec) -> String {
        use crate::params::ParamKind::*;
        // Generator defaults from guide 3.25: 1 kHz sine, 5 Vpp, 50 % duty/symmetry.
        if let Some((_, prop)) = afg_prop(spec.node) {
            let v = match prop {
                "FREQ" => Some("1.000000E3"),
                "PER" => Some("1.000000E-3"),
                "VOLT:AMPL" => Some("5.000000E0"),
                "VOLT:HIGH" => Some("2.500000E0"),
                "VOLT:LOW" => Some("-2.500000E0"),
                "FUNC:SQU:DUTY" | "FUNC:RAMP:SYMM" => Some("5.000000E1"),
                _ => None,
            };
            if let Some(v) = v {
                return v.into();
            }
        }
        // FFT factory values of the MHO954 (the threshold hides a 1 Vpp signal's peaks).
        if spec.node.starts_with(":MATH") {
            let v = match spec.node.split_once(":FFT:").map(|(_, r)| r) {
                Some("UNIT") => Some("DB"),
                Some("WIND") => Some("HANN"),
                Some("SEAR:THR") => Some("5.500000E0"),
                Some("SEAR:EXC") => Some("1.800000E0"),
                _ => None,
            };
            if let Some(v) = v {
                return v.into();
            }
        }
        match spec.kind {
            Real { unit: "s", .. } => "1.000000E-6".into(),
            Real { .. } if spec.node.ends_with(":SCAL") => "1.000000E0".into(),
            Real { unit: "Hz", .. } => "1.000000E6".into(),
            Real { .. } | SourceLevel { .. } | Keyed { .. } => "0.000000".into(),
            Int { min, .. } => min.to_string(),
            Enum(v) => crate::params::short_form(v[0]).to_string(),
            Bool => "0".into(),
            Source { .. } => "CHAN1".into(),
            Text { .. } => "XXXXXXXX".into(),
            Pattern { count, .. } => vec!["X"; count].join(","),
            Prefixed { inner, .. } => Self::param_default(&crate::params::ParamSpec {
                kind: *inner,
                ..*spec
            }),
        }
    }

    /// Returns `false` for unknown queries.
    fn query(&mut self, hdr: &str, args: &str) -> bool {
        if let Some(spec) = Self::param_spec(hdr) {
            let key = if args.is_empty() {
                hdr.to_string()
            } else {
                format!("{hdr} {}", args.to_ascii_uppercase())
            };
            let v = self
                .params
                .get(&key)
                .cloned()
                .unwrap_or_else(|| Self::param_default(&spec));
            self.respond(v);
            return true;
        }
        let fmt_f = |v: f64| format!("{v:.6E}");
        let on = |b: bool| if b { "1" } else { "0" }.to_string();
        if let Some((n, prop)) = parse_chan(hdr) {
            let c = self.state.ch[n - 1].clone();
            let r = match prop {
                "DISP" => on(c.display),
                "SCAL" => fmt_f(c.scale),
                "OFFS" => fmt_f(c.offset),
                "COUP" => c.coupling.scpi().into(),
                "BWL" => c.bw_limit,
                "PROB" => fmt_f(c.probe),
                "IMP" => c.impedance.scpi().into(),
                "INV" => on(c.invert),
                "VERN" => on(c.vernier),
                "UNIT" => "VOLT".into(),
                _ => return false,
            };
            self.respond(r);
            return true;
        }
        if let Some(item) = crate::ops::READOUTS
            .iter()
            .flat_map(|r| r.items.iter().map(move |i| (r, i)))
            .find(|(_, i)| i.query.trim_end_matches('?') == hdr)
        {
            let (r, item) = item;
            if let Some((q, expect)) = r.guard {
                let key = q.trim_end_matches('?');
                let v = self.params.get(key).cloned().unwrap_or_else(|| {
                    Self::param_spec(key)
                        .map(|s| Self::param_default(&s))
                        .unwrap_or_default()
                });
                if !crate::ops::guard_ok(&v, expect) {
                    // Like the scope outside the matching mode (assumed): no answer.
                    return false;
                }
            }
            let answer = match item.kind {
                crate::ops::ReadKind::Real { unit: "Hz" } => "1.000000E3".to_string(),
                crate::ops::ReadKind::Real { .. } => "1.000000E-3".to_string(),
                crate::ops::ReadKind::Int => "1".to_string(),
                crate::ops::ReadKind::Text => "SIM".to_string(),
                crate::ops::ReadKind::KeyValues => {
                    "[Sum:5.6khits,Peaks:14hits,Max:3.9us,Min:-4us,Bin width:20ns]".to_string()
                }
            };
            self.respond(answer);
            return true;
        }
        if let Some(m) = hdr
            .strip_prefix(":MATH")
            .and_then(|r| r.strip_suffix(":FFT:SEAR:RES"))
            .and_then(|d| d.parse::<u8>().ok())
        {
            let on = self.param_or(&format!(":MATH{m}:FFT:SEAR:ENAB"), "0");
            if on != "ON" && on != "1" {
                // Conservative: treat like an unknown query while the search is off.
                return false;
            }
            let lines = self.fft_peak_lines(m);
            for l in lines {
                self.pending.extend_from_slice(l.as_bytes());
                self.pending.push(b'\n');
            }
            if self.pending.is_empty() {
                self.pending.push(b'\n');
            }
            return true;
        }
        if let Some(n) = hdr
            .strip_prefix(":BUS")
            .and_then(|r| r.strip_suffix(":DATA"))
        {
            let get = |p: &str| self.params.get(&format!(":BUS{n}:{p}")).cloned();
            if !get("DISP").is_some_and(|v| v == "ON" || v == "1") {
                // Conservative: treat like an unknown query while the decoder is off.
                return false;
            }
            let table = sim_decode_table(
                &get("MODE").unwrap_or_else(|| "PAR".into()),
                &get("FORM").unwrap_or_else(|| "HEX".into()),
            );
            self.respond_block(table.as_bytes());
            return true;
        }
        let r = match hdr {
            "*IDN" => format!(
                "RIGOL TECHNOLOGIES,MHO954,SIM0000001,{}",
                if self.opts.truncate_idn {
                    "00.01.00"
                } else {
                    "00.01.00.00.26"
                }
            ),
            "*OPC" => "1".into(),
            ":SYST:ERR" => match self.errors.first().cloned() {
                Some((c, m)) => {
                    self.errors.remove(0);
                    format!("{c},\"{m}\"")
                }
                None => "0,\"No error\"".into(),
            },
            ":SYST:VERS" => "3.0".into(),
            ":SYST:MOD" => format!("1,{},0,0,0", u8::from(self.opts.afg)),
            ":SOUR1:LOAD:ARB" | ":SOUR2:LOAD:ARB" if self.opts.afg => {
                self.params.get(hdr).cloned().unwrap_or_default()
            }
            ":LAN:STAT" => if self.state.lan.manual && self.state.lan.ip != "0.0.0.0" {
                "CONFIGURED"
            } else {
                "UNLINK"
            }
            .into(),
            ":LAN:DHCP" => on(self.state.lan.dhcp),
            ":LAN:AUT" => on(self.state.lan.auto_ip),
            ":LAN:MAN" => on(self.state.lan.manual),
            ":LAN:IPAD" => self.state.lan.ip.clone(),
            ":LAN:SMAS" => self.state.lan.mask.clone(),
            ":LAN:GAT" => self.state.lan.gateway.clone(),
            ":LAN:DNS" => self.state.lan.dns.clone(),
            ":LAN:MAC" => "00:19:AF:00:00:01".into(),
            ":LAN:DSER" => "0.0.0.0".into(),
            ":LAN:MDNS" => on(self.state.lan.mdns),
            ":LAN:HOST:NAME" => self.state.lan.hostname.clone(),
            ":LAN:DESC" => self.state.lan.description.clone(),
            ":LAN:VISA" => format!("TCPIP0::{}::INSTR", self.state.lan.ip),
            ":SYST:OPT:VAL" | ":SYST:OPT:STAT" => {
                let Some(o) = OPTIONS.iter().find(|o| o.token.eq_ignore_ascii_case(args)) else {
                    return false;
                };
                let installed = self.opts.licensed
                    && SIM_LICENSED.contains(&o.code)
                    && (self.opts.afg || !o.code.starts_with("AFG"));
                u8::from(installed).to_string()
            }
            ":TRIG:STAT" => {
                if !self.state.running {
                    "STOP".into()
                } else if self.state.trig_sweep == TriggerSweep::Auto {
                    "AUTO".into()
                } else {
                    "TD".into()
                }
            }
            ":TRIG:MODE" => self.state.trig_mode.clone(),
            ":TRIG:SWE" => self.state.trig_sweep.scpi().into(),
            ":TRIG:COUP" => self.state.trig_coupling.clone(),
            ":TRIG:HOLD" => fmt_f(self.state.trig_holdoff),
            ":TRIG:EDGE:SOUR" => self.state.trig_source.clone(),
            ":TRIG:EDGE:SLOP" => self.state.trig_slope.scpi().into(),
            ":TRIG:EDGE:LEV" => fmt_f(self.state.trig_level),
            ":TIM:SCAL" => fmt_f(self.state.tim_scale),
            ":TIM:OFFS" => fmt_f(self.state.tim_offset),
            ":TIM:MODE" => self.state.tim_mode.clone(),
            ":ACQ:SRAT" => fmt_f(self.srate()),
            ":ACQ:MDEP" => format!("{:.4E}", self.mdepth_points() as f64),
            ":ACQ:TYPE" => self.state.acq_type.scpi().into(),
            ":ACQ:AVER" => self.state.acq_aver.to_string(),
            ":WAV:SOUR" => self.state.wav_source.clone(),
            ":WAV:MODE" => self.state.wav_mode.scpi().into(),
            ":WAV:FORM" => if self.state.wav_word { "WORD" } else { "BYTE" }.into(),
            ":WAV:STAR" => self.state.wav_start.to_string(),
            ":WAV:STOP" => self.state.wav_stop.to_string(),
            ":WAV:POIN" => self.preamble().0.to_string(),
            ":WAV:PRE" => {
                let (points, x_inc, x_org, y_inc, y_org) = self.preamble();
                let kind = if self.state.wav_mode == WaveMode::Raw {
                    2
                } else {
                    0
                };
                format!(
                    "{},{kind},{points},1,{x_inc:.6E},{x_org:.6E},0.000000,{y_inc:.4E},{y_org},{}",
                    u8::from(self.state.wav_word),
                    Y_REF
                )
            }
            ":WAV:DATA" => {
                let codes = self.wave_codes();
                let bytes: Vec<u8> = if self.state.wav_word {
                    codes.iter().flat_map(|c| c.to_le_bytes()).collect()
                } else {
                    codes.iter().map(|c| (c >> 8) as u8).collect()
                };
                self.respond_block(&bytes);
                return true;
            }
            ":DISP:DATA" => {
                let png = self.render_png();
                self.respond_block(&png);
                return true;
            }
            ":SYST:SET" => {
                let blob = self.setup_blob();
                self.respond_block(&blob);
                return true;
            }
            ":MEAS:STAT:ITEM" => {
                let parts: Vec<&str> = args.split(',').map(str::trim).collect();
                if parts.len() < 3 {
                    return false;
                }
                match self.measure(parts[1], &parts[2].to_ascii_uppercase()) {
                    Some(v) if v.abs() < 9e37 => match parts[0].to_ascii_uppercase().as_str() {
                        "CNT" => "1.000000E+03".into(),
                        "DEV" => format!("{:.4E}", v.abs() * 0.01),
                        _ => format!("{v:.4E}"),
                    },
                    Some(_) => "9.9000E+37".into(),
                    None => return false,
                }
            }
            ":COUN:CURR" => {
                let src = self
                    .params
                    .get(":COUN:SOUR")
                    .cloned()
                    .unwrap_or_else(|| "CHAN1".into());
                let on = self
                    .params
                    .get(":COUN:ENAB")
                    .is_some_and(|v| v == "ON" || v == "1");
                if on {
                    let f = self.measure("FREQ", &src).unwrap_or(9.9e37);
                    format!("{f:.6E}")
                } else {
                    "9.90E+37".into()
                }
            }
            ":DVM:CURR" => {
                let src = self
                    .params
                    .get(":DVM:SOUR")
                    .cloned()
                    .unwrap_or_else(|| "CHAN1".into());
                let on = self
                    .params
                    .get(":DVM:ENAB")
                    .is_some_and(|v| v == "ON" || v == "1");
                if !on {
                    // Like the real scope: no answer while the DVM is off.
                    return false;
                }
                format!("{:.6E}", self.measure("VRMS", &src).unwrap_or(9.9e37))
            }
            ":MEAS:ITEM" => {
                let mut parts = args.split(',').map(str::trim);
                let (Some(item), Some(src)) = (parts.next(), parts.next()) else {
                    return false;
                };
                let key = format!("{item},{src}").to_ascii_uppercase();
                match self.measure(item, &src.to_ascii_uppercase()) {
                    // The real scope creates the measurement on first use and reports
                    // "invalid" until it has been evaluated once.
                    Some(_) if self.measured.insert(key) => "9.9000E+37".into(),
                    Some(v) => format!("{v:.4E}"),
                    None => return false,
                }
            }
            _ => return false,
        };
        self.respond(r);
        true
    }

    /// Value of a table-driven setting (stored, or its default), as the scope would answer.
    fn setting(&self, query: &str) -> String {
        let hdr = normalize_header(query.trim_end_matches('?'));
        self.params.get(&hdr).cloned().unwrap_or_else(|| {
            Self::param_spec(&hdr)
                .map(|spec| Self::param_default(&spec))
                .unwrap_or_default()
        })
    }

    fn command(&mut self, hdr: &str, args: &str) -> Result<(), String> {
        // One-shot commands that need a mode (histogram, mask test, recording) fail like on
        // the instrument when it is off.
        if let Some(c) = crate::ops::COMMANDS
            .iter()
            .find(|c| c.command.split_whitespace().next() == Some(hdr))
            && let Some(g) = c.needs
            && !crate::ops::guard_ok(&self.setting(g.query), g.expect)
        {
            return Err("execute failed: mode off".into());
        }
        if !self.opts.la_probe
            && hdr.starts_with(":BUS")
            && hdr.ends_with(":PAR:BUS")
            && args.trim().starts_with(['D', 'd'])
        {
            return Err("execute failed: no logic probe".into());
        }
        if let Some(spec) = Self::param_spec(hdr) {
            if let crate::params::ParamKind::SourceLevel { .. } = spec.kind {
                let (src, v) = args.split_once(',').ok_or("missing source")?;
                self.params.insert(
                    format!("{hdr} {}", src.trim().to_ascii_uppercase()),
                    v.trim().to_string(),
                );
            } else if let crate::params::ParamKind::Prefixed { .. } = spec.kind {
                let (key, v) = args.split_once(',').ok_or("missing index")?;
                self.params.insert(
                    format!("{hdr} {}", key.trim().to_ascii_uppercase()),
                    v.trim().to_string(),
                );
            } else if let crate::params::ParamKind::Keyed { .. } = spec.kind {
                let (v, key) = args.split_once(',').ok_or("missing key")?;
                self.params.insert(
                    format!("{hdr} {}", key.trim().to_ascii_uppercase()),
                    v.trim().to_string(),
                );
            } else {
                self.params.insert(hdr.to_string(), args.trim().to_string());
                self.afg_coupled(hdr);
                if hdr == ":BOD:RUNS" {
                    // The simulated Bode sweep finishes at once.
                    self.params.insert(hdr.to_string(), "0".into());
                }
            }
            return Ok(());
        }
        let num = || {
            args.parse::<f64>()
                .map_err(|_| format!("bad number {args:?}"))
        };
        let boolean = || match args.to_ascii_uppercase().as_str() {
            "1" | "ON" => Ok(true),
            "0" | "OFF" => Ok(false),
            _ => Err(format!("bad boolean {args:?}")),
        };
        if let Some((n, prop)) = parse_chan(hdr) {
            let c = &mut self.state.ch[n - 1];
            match prop {
                "DISP" => c.display = boolean()?,
                "SCAL" => c.scale = num()?.clamp(500e-6, 10.0),
                "OFFS" => c.offset = num()?,
                "COUP" => c.coupling = Coupling::from_scpi(args).map_err(|e| e.to_string())?,
                "BWL" => c.bw_limit = args.to_ascii_uppercase(),
                "PROB" => c.probe = num()?,
                "IMP" => c.impedance = Impedance::from_scpi(args).map_err(|e| e.to_string())?,
                "INV" => c.invert = boolean()?,
                "VERN" => c.vernier = boolean()?,
                _ => return Err(format!("unknown channel property {prop}")),
            }
            return Ok(());
        }
        let s = &mut self.state;
        match hdr {
            ":RUN" => s.running = true,
            ":STOP" | ":SING" => {
                s.running = false;
                self.frozen_at = self.started.elapsed().as_secs_f64();
            }
            ":AUT" | ":CLE" | ":TFOR" | "*CLS" => {}
            "*RST" => {
                *s = SimState::default();
                self.params.clear();
            }
            ":SOUR1:LOAD:ARB" | ":SOUR2:LOAD:ARB" => {
                self.params.insert(hdr.to_string(), args.trim().to_string());
            }
            // One-shot commands of `ops::COMMANDS` (save/load on the scope, resets, recording).
            h if crate::ops::COMMANDS
                .iter()
                .any(|c| c.command.split_whitespace().next() == Some(h)) => {}
            ":TIM:SCAL" => s.tim_scale = num()?.clamp(1e-9, 1000.0),
            ":TIM:OFFS" => s.tim_offset = num()?,
            ":TIM:MODE" => s.tim_mode = args.to_ascii_uppercase(),
            ":TRIG:MODE" => s.trig_mode = args.to_ascii_uppercase(),
            ":TRIG:SWE" => {
                s.trig_sweep = TriggerSweep::from_scpi(args).map_err(|e| e.to_string())?
            }
            ":TRIG:COUP" => s.trig_coupling = args.to_ascii_uppercase(),
            ":TRIG:HOLD" => s.trig_holdoff = num()?,
            ":TRIG:EDGE:SOUR" => s.trig_source = args.to_ascii_uppercase(),
            ":TRIG:EDGE:SLOP" => {
                s.trig_slope = Slope::from_scpi(args).map_err(|e| e.to_string())?
            }
            ":TRIG:EDGE:LEV" => s.trig_level = num()?,
            ":ACQ:TYPE" => s.acq_type = AcquireType::from_scpi(args).map_err(|e| e.to_string())?,
            ":ACQ:AVER" => s.acq_aver = num()? as u32,
            ":ACQ:MDEP" => s.acq_mdep = parse_depth(args)?,
            ":WAV:SOUR" => s.wav_source = args.to_ascii_uppercase(),
            ":WAV:MODE" => s.wav_mode = WaveMode::from_scpi(args).map_err(|e| e.to_string())?,
            ":WAV:FORM" => s.wav_word = args.eq_ignore_ascii_case("WORD"),
            ":WAV:STAR" => s.wav_start = num()? as usize,
            ":WAV:STOP" => s.wav_stop = num()? as usize,
            ":LAN:DHCP" => s.lan_pending.dhcp = boolean()?,
            ":LAN:AUT" => s.lan_pending.auto_ip = boolean()?,
            ":LAN:MAN" => s.lan_pending.manual = boolean()?,
            ":LAN:IPAD" => s.lan_pending.ip = args.to_string(),
            ":LAN:SMAS" => s.lan_pending.mask = args.to_string(),
            ":LAN:GAT" => s.lan_pending.gateway = args.to_string(),
            ":LAN:DNS" => s.lan_pending.dns = args.to_string(),
            ":LAN:MDNS" => s.lan_pending.mdns = boolean()?,
            ":LAN:HOST:NAME" => s.lan_pending.hostname = args.to_string(),
            ":LAN:DESC" => s.lan_pending.description = args.to_string(),
            ":LAN:APPL" => {
                let p = &s.lan_pending;
                if !(p.dhcp || p.auto_ip || p.manual) {
                    return Err("all IP configuration modes off".into());
                }
                s.lan = p.clone();
            }
            ":MEAS:STAT:ITEM" | ":MEAS:ITEM" | ":MEAS:STAT:RES" | ":MEAS:DEL" | ":COUN:TOT:CLE" => {
            }
            ":SYST:SET" => {}
            ":SYST:OPT:INST" => return Err("invalid license key".into()),
            _ => return Err(format!("undefined header {hdr}")),
        }
        Ok(())
    }

    /// Renders displayed channels as a 400×240 PNG (dark background, scope colours).
    fn render_png(&mut self) -> Vec<u8> {
        const W: usize = 400;
        const H: usize = 240;
        let mut img = vec![[16u8, 16, 20]; W * H];
        for gx in 0..=10 {
            let x = (gx * (W - 1)) / 10;
            for y in (0..H).step_by(4) {
                img[y * W + x] = [60, 60, 60];
            }
        }
        for gy in 0..=8 {
            let y = (gy * (H - 1)) / 8;
            for x in (0..W).step_by(4) {
                img[y * W + x] = [60, 60, 60];
            }
        }
        let colors = [[255, 230, 0], [0, 230, 255], [255, 0, 200], [60, 120, 255]];
        for n in 1..=4usize {
            if !self.state.ch[n - 1].display {
                continue;
            }
            let c = self.state.ch[n - 1].clone();
            let (x_inc, x_org) = self.screen_timing();
            let v = self.samples(&format!("CHAN{n}"), 0, SCREEN_POINTS, x_inc, x_org);
            for (i, val) in v.iter().enumerate() {
                let x = i * (W - 1) / (SCREEN_POINTS - 1);
                let div = (val + c.offset) / c.scale;
                let y = (H as f64 / 2.0 - div * H as f64 / 8.0).round();
                if (0.0..H as f64).contains(&y) {
                    img[y as usize * W + x] = colors[n - 1];
                }
            }
        }
        let mut out = Vec::new();
        {
            let mut enc = png::Encoder::new(&mut out, W as u32, H as u32);
            enc.set_color(png::ColorType::Rgb);
            enc.set_depth(png::BitDepth::Eight);
            let mut w = enc.write_header().expect("png header");
            let flat: Vec<u8> = img.iter().flatten().copied().collect();
            w.write_image_data(&flat).expect("png data");
        }
        out
    }
}

impl SimTransport {
    /// Setup blob: `SIMSETUP` followed by `key=value` lines (the real scope's format is binary).
    fn setup_blob(&self) -> Vec<u8> {
        let s = &self.state;
        let mut out = String::from("SIMSETUP\n");
        for (i, c) in s.ch.iter().enumerate() {
            let n = i + 1;
            out += &format!(
                "ch{n}.display={}\nch{n}.scale={}\nch{n}.offset={}\n",
                c.display, c.scale, c.offset
            );
        }
        out += &format!("tim.scale={}\ntim.offset={}\n", s.tim_scale, s.tim_offset);
        out += &format!(
            "trig.level={}\ntrig.source={}\n",
            s.trig_level, s.trig_source
        );
        out.into_bytes()
    }

    fn load_setup(&mut self, blob: &[u8]) -> Result<(), String> {
        let text = std::str::from_utf8(blob).map_err(|e| e.to_string())?;
        let mut lines = text.lines();
        if lines.next() != Some("SIMSETUP") {
            return Err("not a simulator setup".into());
        }
        for line in lines {
            let (k, v) = line.split_once('=').ok_or("bad line")?;
            let f = || v.parse::<f64>().map_err(|e| e.to_string());
            match k {
                "tim.scale" => self.state.tim_scale = f()?,
                "tim.offset" => self.state.tim_offset = f()?,
                "trig.level" => self.state.trig_level = f()?,
                "trig.source" => self.state.trig_source = v.to_string(),
                _ => {
                    let (ch, prop) = k.split_once('.').ok_or("bad key")?;
                    let n: usize = ch
                        .strip_prefix("ch")
                        .and_then(|n| n.parse().ok())
                        .ok_or("bad channel")?;
                    let c = self
                        .state
                        .ch
                        .get_mut(n.wrapping_sub(1))
                        .ok_or("bad channel")?;
                    match prop {
                        "display" => c.display = v == "true",
                        "scale" => c.scale = f()?,
                        "offset" => c.offset = f()?,
                        _ => return Err(format!("unknown key {k}")),
                    }
                }
            }
        }
        Ok(())
    }
}

fn parse_depth(s: &str) -> Result<Option<usize>, String> {
    let u = s.trim().to_ascii_uppercase();
    if u == "AUTO" {
        return Ok(None);
    }
    let (num, mult) = if let Some(n) = u.strip_suffix('K') {
        (n, 1e3)
    } else if let Some(n) = u.strip_suffix('M') {
        (n, 1e6)
    } else {
        (u.as_str(), 1.0)
    };
    let v = num
        .parse::<f64>()
        .map_err(|_| format!("bad memory depth {s:?}"))?
        * mult;
    Ok(Some((v as usize).clamp(1000, 1_000_000)))
}

fn parse_chan(hdr: &str) -> Option<(usize, &str)> {
    let rest = hdr.strip_prefix(":CHAN")?;
    let (n, prop) = rest.split_once(':')?;
    let n: usize = n.parse().ok()?;
    (1..=4).contains(&n).then_some((n, prop))
}

/// Maps long-form SCPI nodes to their short form, e.g. `:CHANnel1:SCALe` → `:CHAN1:SCAL`.
fn normalize_header(h: &str) -> String {
    const LONG: &[(&str, &str)] = &[
        ("CHANNEL", "CHAN"),
        ("SCALE", "SCAL"),
        ("OFFSET", "OFFS"),
        ("DISPLAY", "DISP"),
        ("COUPLING", "COUP"),
        ("BWLIMIT", "BWL"),
        ("PROBE", "PROB"),
        ("IMPEDANCE", "IMP"),
        ("INVERT", "INV"),
        ("VERNIER", "VERN"),
        ("TIMEBASE", "TIM"),
        ("TRIGGER", "TRIG"),
        ("STATUS", "STAT"),
        ("SWEEP", "SWE"),
        ("SOURCE", "SOUR"),
        ("SLOPE", "SLOP"),
        ("LEVEL", "LEV"),
        ("HOLDOFF", "HOLD"),
        ("ACQUIRE", "ACQ"),
        ("SRATE", "SRAT"),
        ("MDEPTH", "MDEP"),
        ("AVERAGES", "AVER"),
        ("WAVEFORM", "WAV"),
        ("FORMAT", "FORM"),
        ("PREAMBLE", "PRE"),
        ("START", "STAR"),
        ("POINTS", "POIN"),
        ("MEASURE", "MEAS"),
        ("SYSTEM", "SYST"),
        ("ERROR", "ERR"),
        ("OPTION", "OPT"),
        ("VALID", "VAL"),
        ("INSTALL", "INST"),
        ("SETUP", "SET"),
        ("SINGLE", "SING"),
        ("AUTOSCALE", "AUT"),
        ("CLEAR", "CLE"),
        ("TFORCE", "TFOR"),
        ("VERSION", "VERS"),
        ("AUTOIP", "AUT"),
        ("MANUAL", "MAN"),
        ("IPADDRESS", "IPAD"),
        ("SMASK", "SMAS"),
        ("GATEWAY", "GAT"),
        ("DSERVER", "DSER"),
        ("DESCRIPTION", "DESC"),
        ("APPLY", "APPL"),
    ];
    let u = h.to_ascii_uppercase();
    let lead = if u.starts_with(':') || u.starts_with('*') {
        ""
    } else {
        ":"
    };
    let mut out = String::from(lead);
    for (i, node) in u.split(':').enumerate() {
        if i > 0 {
            out.push(':');
        }
        let split = node
            .find(|c: char| c.is_ascii_digit())
            .unwrap_or(node.len());
        let (word, digits) = node.split_at(split);
        let short = LONG
            .iter()
            .find(|(l, _)| *l == word)
            .map(|(_, s)| *s)
            .unwrap_or(word);
        out.push_str(short);
        out.push_str(digits);
    }
    out
}

/// Event table like `:BUS<n>:DATA?` returns it: protocol line, column titles, rows, each line
/// ending with a comma.
fn sim_decode_table(mode: &str, format: &str) -> String {
    let text = b"Hello";
    let cell = |b: u8| match format.to_ascii_uppercase().as_str() {
        f if f.starts_with("ASC") => (b as char).to_string(),
        "DEC" => b.to_string(),
        "BIN" => format!("{b:08b}"),
        _ => format!("0x{b:02X}"),
    };
    let mode = mode.to_ascii_uppercase();
    let (proto, cols) = match mode.as_str() {
        "RS232" => ("RS232", "Time,TX,RX,"),
        "IIC" => ("I2C", "Time,Address,Data,ACK,"),
        "SPI" => ("SPI", "Time,MISO,MOSI,"),
        "CAN" => ("CAN", "Time,ID,DLC,Data,CRC,ACK,"),
        "LIN" => ("LIN", "Time,ID,Data,Checksum,"),
        _ => ("PARALLEL", "Time,Data,"),
    };
    let mut out = format!("{proto}\n{cols}\n");
    for (i, &b) in text.iter().enumerate() {
        let t = format!("{:.2}us", -200.0 + 100.0 * i as f64);
        let row = match proto {
            "RS232" => format!("{t},{},,", cell(b)),
            "I2C" => format!("{t},0x50,{},ACK,", cell(b)),
            "SPI" => format!("{t},{},{},", cell(b), cell(b.wrapping_add(1))),
            "CAN" => format!("{t},0x123,1,{},0x1A2B,ACK,", cell(b)),
            "LIN" => format!("{t},0x10,{},0x55,", cell(b)),
            _ => format!("{t},{},", cell(b)),
        };
        out.push_str(&row);
        out.push('\n');
    }
    out
}

impl Transport for SimTransport {
    fn write_all(&mut self, data: &[u8]) -> io::Result<()> {
        if data.starts_with(b":SYST:SET #") || data.starts_with(b":SYSTEM:SETUP #") {
            if !(self.hung || self.blocked) {
                let start = data.iter().position(|&b| b == b'#').unwrap_or(0);
                if let Ok(Some(h)) = mho_scpi::parse_block_header(&data[start..]) {
                    let body = &data[start + h.header_len..];
                    let body = &body[..h.data_len.unwrap_or(body.len()).min(body.len())];
                    if self.load_setup(body).is_err() {
                        self.error(-224, "invalid setup data");
                    }
                }
            }
            return Ok(());
        }
        let text = String::from_utf8_lossy(data).into_owned();
        self.message_start = self.pending.len();
        for line in text.split(['\n', ';']) {
            self.execute(line);
        }
        Ok(())
    }

    fn read_some(&mut self, buf: &mut [u8]) -> io::Result<ReadChunk> {
        if self.pending.is_empty() {
            return Err(io::Error::new(
                io::ErrorKind::TimedOut,
                "sim: no response pending",
            ));
        }
        if !self.latency.is_zero() {
            std::thread::sleep(self.latency);
        }
        let n = buf.len().min(self.pending.len());
        buf[..n].copy_from_slice(&self.pending[..n]);
        self.pending.drain(..n);
        Ok(ReadChunk {
            n,
            end: self.pending.is_empty(),
        })
    }

    fn clear(&mut self) -> io::Result<()> {
        self.pending.clear();
        if !self.hung {
            self.blocked = false;
        }
        Ok(())
    }

    fn set_timeout(&mut self, timeout: Duration) -> io::Result<()> {
        self.timeout = timeout;
        Ok(())
    }

    fn timeout(&self) -> Duration {
        self.timeout
    }

    fn reports_eom(&self) -> bool {
        true
    }

    fn describe(&self) -> String {
        "sim".into()
    }
}

/// `(value, SI prefix)` with the value in 1..1000.
fn si_scaled(v: f64) -> (f64, &'static str) {
    const P: [(f64, &str); 7] = [
        (1e9, "G"),
        (1e6, "M"),
        (1e3, "k"),
        (1.0, ""),
        (1e-3, "m"),
        (1e-6, "u"),
        (1e-9, "n"),
    ];
    let a = v.abs();
    P.iter()
        .find(|(s, _)| a >= *s)
        .map(|&(s, p)| (v / s, p))
        .unwrap_or((v / 1e-9, "n"))
}

/// `x` with `digits` significant digits, trailing zeros dropped if `trim` (`14`, `-20.7`).
fn significant(x: f64, digits: i32, trim: bool) -> String {
    let mag = if x == 0.0 {
        0
    } else {
        x.abs().log10().floor() as i32
    };
    let decimals = (digits - 1 - mag).max(0) as usize;
    let s = format!("{x:.decimals$}");
    if trim && s.contains('.') {
        s.trim_end_matches('0').trim_end_matches('.').to_string()
    } else {
        s
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{ChannelUpdate, Scope};

    fn scope() -> Scope {
        Scope::from_transport(Box::new(SimTransport::new())).unwrap()
    }

    #[test]
    fn header_normalisation() {
        assert_eq!(normalize_header(":CHANnel1:SCALe"), ":CHAN1:SCAL");
        assert_eq!(normalize_header("trigger:edge:level"), ":TRIG:EDGE:LEV");
        assert_eq!(normalize_header("*IDN"), "*IDN");
    }

    #[test]
    fn status_roundtrip() {
        let mut s = scope();
        let st = s.status().unwrap();
        assert_eq!(st.idn.model, "MHO954");
        assert_eq!(st.channels.len(), 4);
        let ch = s
            .set_channel(
                2,
                &ChannelUpdate {
                    scale: Some(0.2),
                    ..Default::default()
                },
            )
            .unwrap();
        assert_eq!(ch.scale, 0.2);
    }

    #[test]
    fn screen_waveform_scaling() {
        let mut s = scope();
        let w = s.screen_waveform("CHAN2", None).unwrap();
        assert_eq!(w.raw.len(), SCREEN_POINTS);
        let m = w.analyze();
        // 5 kHz sine, 2 Vpp, at 200 µs/div → 10 periods on screen.
        assert!((m.vpp - 2.0).abs() < 0.15, "vpp {}", m.vpp);
        assert!(
            (m.frequency.unwrap() - 5e3).abs() < 50.0,
            "freq {:?}",
            m.frequency
        );
    }

    #[test]
    fn measure_and_unknown_query() {
        let mut s = scope();
        let f = s.measure("FREQ", "CHAN1", None).unwrap();
        assert!((f.value.unwrap() - 1e3).abs() < 10.0, "{f:?}");
        // Unknown query: no answer → timeout error, session stays usable.
        assert!(s.raw("*OPT?", Duration::from_millis(100)).is_err());
        assert_eq!(s.trigger_status().unwrap(), "AUTO");
    }

    #[test]
    fn raw_download_needs_stop() {
        let mut s = scope();
        assert!(s.raw_waveform("CHAN1", &mut |_, _| {}).is_err());
        s.action(crate::Action::Stop).unwrap();
        s.raw_chunk_points = 3000;
        let mut calls = 0;
        let w = s.raw_waveform("CHAN1", &mut |_, _| calls += 1).unwrap();
        assert_eq!(w.raw.len(), 10_000);
        assert!(calls >= 4);
    }

    #[test]
    fn screenshot_is_png() {
        let mut s = scope();
        let png = s.screenshot().unwrap();
        assert!(png.starts_with(b"\x89PNG"));
    }

    #[test]
    fn network_static_ip() {
        use crate::{IpMode, NetworkUpdate};
        let mut s = scope();
        let n = s.network().unwrap();
        assert!(n.dhcp && !n.manual);
        let upd = NetworkUpdate {
            mode: Some(IpMode::Static),
            ip: Some("192.168.50.20".into()),
            mask: Some("255.255.255.0".into()),
            gateway: Some("192.168.50.1".into()),
            hostname: Some("mho954-lab".into()),
            ..Default::default()
        };
        let n = s.set_network(&upd).unwrap();
        assert!(n.manual && !n.dhcp && !n.auto_ip);
        assert_eq!(n.ip, "192.168.50.20");
        assert_eq!(n.hostname, "mho954-lab");
        let bad = NetworkUpdate {
            ip: Some("300.1.1.1".into()),
            ..Default::default()
        };
        assert!(s.set_network(&bad).is_err());
        let dhcp = s
            .set_network(&NetworkUpdate {
                mode: Some(IpMode::Dhcp),
                ..Default::default()
            })
            .unwrap();
        assert!(dhcp.dhcp && !dhcp.manual);
    }

    fn scope_with(opts: SimOptions) -> Scope {
        Scope::from_transport(Box::new(SimTransport::with_options(opts))).unwrap()
    }

    #[test]
    fn unknown_query_blocks_until_clear_then_recovers() {
        let mut s = scope();
        // raw(): timeout → device clear → health check → usable again.
        let err = s.raw(":WLAN:SSID?", Duration::from_millis(50)).unwrap_err();
        assert!(err.to_string().contains("no answer"), "{err}");
        assert!(!err.to_string().contains("power-cycle"));
        assert_eq!(s.trigger_status().unwrap(), "AUTO");
    }

    #[test]
    fn repeated_unknown_queries_hang_the_firmware() {
        let mut s = scope_with(SimOptions {
            hang_after: 2,
            ..Default::default()
        });
        let _ = s.raw(":FOO?", Duration::from_millis(50));
        let err = s.raw(":BAR?", Duration::from_millis(50)).unwrap_err();
        assert!(err.to_string().contains("power-cycle"), "{err}");
        // Further raw access is refused instead of poking the hung scope.
        let err = s.raw(":TIM:SCAL?", Duration::from_millis(50)).unwrap_err();
        assert!(err.to_string().contains("power-cycle"), "{err}");
    }

    #[test]
    fn channel_without_signal_has_no_data() {
        let mut opts = SimOptions::default();
        opts.no_signal[0] = true;
        let mut s = scope_with(opts);
        let w = s.screen_waveform("CHAN1", None).unwrap();
        assert!(!w.has_data());
        assert_eq!(w.analyze().samples, 0);
        assert_eq!(s.measure("VPP", "CHAN1", None).unwrap().value, None);
        assert!(s.screen_waveform("CHAN2", None).unwrap().has_data());
    }

    #[test]
    fn first_measurement_is_retried() {
        let mut s = scope();
        // The simulator answers 9.9E37 on first use like the real scope; the driver retries.
        let m = s.measure("FREQ", "CHAN2", None).unwrap();
        assert!(m.value.is_some(), "{m:?}");
    }

    #[test]
    fn idn_is_truncated_like_the_real_scope() {
        assert_eq!(scope().idn().firmware, "00.01.00");
    }

    #[test]
    fn setup_roundtrip() {
        let mut s = scope();
        let blob = s.setup_save().unwrap();
        s.set_timebase(&crate::TimebaseUpdate {
            scale: Some(1e-3),
            ..Default::default()
        })
        .unwrap();
        s.setup_load(&blob).unwrap();
        assert!((s.timebase().unwrap().scale - 200e-6).abs() < 1e-12);
    }

    #[test]
    fn bad_command_reports_error() {
        let mut s = scope();
        let err = s.raw(":FOO:BAR 1", Duration::from_millis(100)).unwrap_err();
        assert!(err.to_string().contains("-113"), "{err}");
    }
}
