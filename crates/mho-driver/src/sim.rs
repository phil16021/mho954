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

pub struct SimTransport {
    state: SimState,
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
        SimTransport {
            state: SimState::default(),
            errors: Vec::new(),
            pending: Vec::new(),
            rng: 0x2545_F491_4F6C_DD1D,
            started: Instant::now(),
            frozen_at: 0.0,
            timeout: Duration::from_secs(2),
            latency: Duration::ZERO,
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
            None => (1.0, 0.0),
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
        let Some(n) = Self::channel_index(src) else {
            return vec![0.0; count];
        };
        let wall = self.now();
        let ch = self.state.ch[n - 1].clone();
        let noise_amp = ch.scale * 0.02;
        (first..first + count)
            .map(|i| {
                let t = i as f64 * x_inc + x_org;
                let mut v = Self::signal(n as u8, t, wall);
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

    fn wave_codes(&mut self) -> Vec<u16> {
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
        let query = header.ends_with('?');
        let hdr = normalize_header(header.trim_end_matches('?'));
        if query {
            if !self.query(&hdr, args) {
                tracing::debug!(%line, "sim: unknown query (no response, like the real scope)");
            }
        } else if let Err(msg) = self.command(&hdr, args) {
            self.error(-113, &msg);
        }
    }

    /// Returns `false` for unknown queries.
    fn query(&mut self, hdr: &str, args: &str) -> bool {
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
        let r = match hdr {
            "*IDN" => "RIGOL TECHNOLOGIES,MHO954,SIM0000001,00.01.00.00.26".into(),
            "*OPC" => "1".into(),
            ":SYST:ERR" => match self.errors.first().cloned() {
                Some((c, m)) => {
                    self.errors.remove(0);
                    format!("{c},\"{m}\"")
                }
                None => "0,\"No error\"".into(),
            },
            ":SYST:VERS" => "3.0".into(),
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
                if OPTIONS.iter().any(|o| o.token.eq_ignore_ascii_case(args)) {
                    "0".into()
                } else {
                    return false;
                }
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
                let blob = format!("SIMSETUP{:?}", self.state).into_bytes();
                self.respond_block(&blob);
                return true;
            }
            ":MEAS:ITEM" => {
                let mut parts = args.split(',').map(str::trim);
                let (Some(item), Some(src)) = (parts.next(), parts.next()) else {
                    return false;
                };
                match self.measure(item, &src.to_ascii_uppercase()) {
                    Some(v) => format!("{v:.4E}"),
                    None => return false,
                }
            }
            _ => return false,
        };
        self.respond(r);
        true
    }

    fn command(&mut self, hdr: &str, args: &str) -> Result<(), String> {
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
            "*RST" => *s = SimState::default(),
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

impl Transport for SimTransport {
    fn write_all(&mut self, data: &[u8]) -> io::Result<()> {
        // Binary setup block: accept without parsing.
        if data.starts_with(b":SYST:SET #") || data.starts_with(b":SYSTEM:SETUP #") {
            return Ok(());
        }
        let text = String::from_utf8_lossy(data).into_owned();
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
        Ok(())
    }

    fn set_timeout(&mut self, timeout: Duration) -> io::Result<()> {
        self.timeout = timeout;
        Ok(())
    }

    fn timeout(&self) -> Duration {
        self.timeout
    }

    fn describe(&self) -> String {
        "sim".into()
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

    #[test]
    fn bad_command_reports_error() {
        let mut s = scope();
        let err = s.raw(":FOO:BAR 1", Duration::from_millis(100)).unwrap_err();
        assert!(err.to_string().contains("-113"), "{err}");
    }
}
