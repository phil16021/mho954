//! Higher-level procedures built from several commands: logic-analyzer capture through the
//! parallel decoder and a software Bode sweep with the built-in function generator.

use std::time::{Duration, Instant};

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use utoipa::ToSchema;

use crate::error::{DriverError, Result};
use crate::model::{Action, ChannelUpdate, TimebaseUpdate, TriggerSweep, TriggerUpdate};
use crate::scope::Scope;

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema, ToSchema)]
pub struct LogicCapture {
    /// Decoder bus used for the capture.
    pub bus: u8,
    /// Line names, bit i = `lines[i]`.
    pub lines: Vec<String>,
    /// Bus value after each change.
    pub events: Vec<mho_dsp::LogicEvent>,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema, ToSchema)]
#[serde(default)]
pub struct BodeRequest {
    /// Generator channel 1 (GI) or 2 (GII).
    pub generator: u8,
    /// Channel probing the input of the device under test.
    pub input: u8,
    /// Channel probing its output.
    pub output: u8,
    /// Hz.
    pub start: f64,
    /// Hz.
    pub stop: f64,
    /// Logarithmically spaced points (2..=200).
    pub points: usize,
    /// Generator amplitude, V peak-to-peak.
    pub amplitude: f64,
    /// Wait after each frequency step.
    pub settle_ms: u64,
}

impl Default for BodeRequest {
    fn default() -> Self {
        BodeRequest {
            generator: 1,
            input: 1,
            output: 2,
            start: 100.0,
            stop: 1e6,
            points: 31,
            amplitude: 1.0,
            settle_ms: 100,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema, ToSchema)]
pub struct BodePoint {
    pub frequency: f64,
    pub gain_db: f64,
    /// Output relative to input, -180..180°.
    pub phase_deg: f64,
    /// Peak amplitudes in volts.
    pub input_amplitude: f64,
    pub output_amplitude: f64,
}

impl BodeRequest {
    pub fn validate(&self) -> Result<()> {
        let bad = |m: String| Err(DriverError::Invalid(m));
        if !(1..=2).contains(&self.generator) {
            return bad(format!("generator {} outside 1..=2", self.generator));
        }
        for c in [self.input, self.output] {
            if !(1..=4).contains(&c) {
                return bad(format!("channel {c} outside 1..=4"));
            }
        }
        if self.input == self.output {
            return bad("input and output must be different channels".into());
        }
        if !(1e-3..=100e6).contains(&self.start)
            || !(1e-3..=100e6).contains(&self.stop)
            || self.start >= self.stop
        {
            return bad("need 1 mHz <= start < stop <= 100 MHz".into());
        }
        if !(2..=200).contains(&self.points) {
            return bad("points must be 2..=200".into());
        }
        if !(0.01..=10.0).contains(&self.amplitude) {
            return bad("amplitude must be 0.01..=10 Vpp".into());
        }
        if self.settle_ms > 10_000 {
            return bad("settle_ms at most 10000".into());
        }
        Ok(())
    }

    /// Log-spaced sweep frequencies, rounded to 4 significant digits (exact end points).
    pub fn frequencies(&self) -> Vec<f64> {
        let (a, b) = (self.start.ln(), self.stop.ln());
        let round = |x: f64| {
            let p = 10f64.powi(x.log10().floor() as i32 - 3);
            (x / p).round() * p
        };
        (0..self.points)
            .map(|i| match i {
                0 => self.start,
                i if i == self.points - 1 => self.stop,
                i => round((a + (b - a) * i as f64 / (self.points - 1) as f64).exp()),
            })
            .collect()
    }
}

/// Next value of the 1-2-5 sequence above (`up`) or below `v`.
fn step_125(v: f64, up: bool) -> f64 {
    let e = v.log10().floor();
    let base = 10f64.powf(e);
    let seq = [1.0, 2.0, 5.0, 10.0, 20.0];
    let m = v / base;
    if up {
        seq.iter().find(|&&s| s > m * 1.001).unwrap_or(&20.0) * base
    } else {
        let below = [0.5, 1.0, 2.0, 5.0, 10.0];
        below.iter().rev().find(|&&s| s < m * 0.999).unwrap_or(&0.5) * base
    }
}

/// `(peak amplitude, phase in rad)` of the swept tone on one channel.
type Tone = (f64, f64);

fn wrap_deg(rad: f64) -> f64 {
    let d = rad.to_degrees().rem_euclid(360.0);
    if d > 180.0 { d - 360.0 } else { d }
}

impl Scope {
    /// Captures D0..D15 through decoder `bus` set up as a 16-bit parallel bus without clock
    /// (one event per change). The MHO954 rejects `:WAV:SOUR D0`, so this is the only way to
    /// read the digital channels. Bit order assumes `D15D0` = D15 is the most significant bit.
    pub fn logic_capture(&mut self, bus: u8) -> Result<LogicCapture> {
        if !(1..=4).contains(&bus) {
            return Err(DriverError::Invalid(format!("bus {bus} outside 1..=4")));
        }
        if !self.modules()?.logic_analyzer {
            return Err(DriverError::Unsupported(
                "this scope reports no logic analyzer hardware".into(),
            ));
        }
        self.drain_errors()?;
        for c in [
            format!(":BUS{bus}:MODE PAR"),
            format!(":BUS{bus}:DISP ON"),
            format!(":BUS{bus}:PAR:BUS D15D0"),
            format!(":BUS{bus}:PAR:CLK OFF"),
            format!(":BUS{bus}:FORM HEX"),
            format!(":BUS{bus}:EVEN ON"),
        ] {
            self.scpi().write(&c)?;
        }
        self.check_errors()?;
        std::thread::sleep(Duration::from_millis(300));
        let table = self.bus_data(bus)?;
        let col = table
            .columns
            .iter()
            .position(|c| c.eq_ignore_ascii_case("data"))
            .unwrap_or(1);
        let events = table
            .rows
            .iter()
            .filter_map(|r| {
                Some(mho_dsp::LogicEvent {
                    time: r.time?,
                    value: mho_dsp::parse_hex_value(r.cells.get(col)?)?,
                })
            })
            .collect();
        Ok(LogicCapture {
            bus,
            lines: (0..16).map(|i| format!("D{i}")).collect(),
            events,
        })
    }

    /// Software Bode sweep: steps the generator through the frequencies, takes a single
    /// acquisition per step and compares the tone on the input and output channels (single-bin
    /// DFT on the screen waveforms, so both come from the same acquisition). Vertical scales
    /// are adjusted per step; timebase, scales, trigger sweep and run state are restored, the
    /// generator output is switched off at the end.
    pub fn bode_sweep(
        &mut self,
        req: &BodeRequest,
        progress: &mut dyn FnMut(usize, usize),
    ) -> Result<Vec<BodePoint>> {
        req.validate()?;
        if !self.modules()?.function_generator {
            return Err(DriverError::Unsupported(
                "this scope has no function generator (AFG option)".into(),
            ));
        }
        let tb = self.timebase()?;
        let trig = self.trigger()?;
        let running = self.trigger_status()? != "STOP";
        let scales = [
            self.channel(req.input)?.scale,
            self.channel(req.output)?.scale,
        ];
        let g = req.generator;
        self.drain_errors()?;
        for c in [
            format!(":SOUR{g}:FUNC SIN"),
            format!(":SOUR{g}:VOLT:AMPL {:e}", req.amplitude),
            format!(":SOUR{g}:VOLT:OFFS 0"),
            format!(":SOUR{g}:OUTP:STAT ON"),
        ] {
            self.scpi().write(&c)?;
        }
        self.check_errors()?;
        self.set_trigger(&TriggerUpdate {
            sweep: Some(TriggerSweep::Auto),
            ..Default::default()
        })?;
        let result = self.bode_points(req, progress);

        // Restore, even after an error in the sweep.
        let restore = (|| -> Result<()> {
            self.scpi().write(&format!(":SOUR{g}:OUTP:STAT OFF"))?;
            self.set_timebase(&TimebaseUpdate {
                scale: Some(tb.scale),
                offset: Some(tb.offset),
                ..Default::default()
            })?;
            for (ch, s) in [req.input, req.output].into_iter().zip(scales) {
                self.set_channel(
                    ch,
                    &ChannelUpdate {
                        scale: Some(s),
                        ..Default::default()
                    },
                )?;
            }
            self.set_trigger(&TriggerUpdate {
                sweep: Some(trig.sweep),
                ..Default::default()
            })?;
            if running {
                self.action(Action::Run)?;
            }
            Ok(())
        })();
        let points = result?;
        restore?;
        Ok(points)
    }

    fn bode_points(
        &mut self,
        req: &BodeRequest,
        progress: &mut dyn FnMut(usize, usize),
    ) -> Result<Vec<BodePoint>> {
        let freqs = req.frequencies();
        let mut out = Vec::with_capacity(freqs.len());
        for (i, &f) in freqs.iter().enumerate() {
            self.scpi()
                .write(&format!(":SOUR{}:FREQ {f:e}", req.generator))?;
            // About five periods on screen.
            self.set_timebase(&TimebaseUpdate {
                scale: Some(0.5 / f),
                offset: Some(0.0),
                ..Default::default()
            })?;
            std::thread::sleep(Duration::from_millis(req.settle_ms));
            let mut tones = None;
            for _ in 0..4 {
                let (inp, outp, rescaled) = self.bode_acquire(req, f)?;
                if !rescaled {
                    tones = Some((inp, outp));
                    break;
                }
            }
            let ((ai, pi), (ao, po)) = tones.ok_or_else(|| {
                DriverError::State(format!("no stable vertical range at {f:.4e} Hz"))
            })?;
            if ai <= 0.0 {
                return Err(DriverError::State(format!(
                    "no signal on CH{} at {f:.4e} Hz",
                    req.input
                )));
            }
            out.push(BodePoint {
                frequency: f,
                gain_db: 20.0 * (ao / ai).log10(),
                phase_deg: wrap_deg(po - pi),
                input_amplitude: ai,
                output_amplitude: ao,
            });
            progress(i + 1, freqs.len());
        }
        Ok(out)
    }

    /// One single acquisition: tones `(amplitude, phase)` on input and output, and whether a
    /// channel scale had to be changed (then the caller repeats).
    fn bode_acquire(&mut self, req: &BodeRequest, f: f64) -> Result<(Tone, Tone, bool)> {
        self.action(Action::Single)?;
        let deadline = Instant::now() + Duration::from_secs(5);
        while self.trigger_status()? != "STOP" {
            if Instant::now() > deadline {
                return Err(DriverError::State(
                    "single acquisition did not finish".into(),
                ));
            }
            std::thread::sleep(Duration::from_millis(20));
        }
        let mut tones = Vec::with_capacity(2);
        let mut rescaled = false;
        for ch in [req.input, req.output] {
            let w = self.screen_waveform(&format!("CHAN{ch}"), None)?;
            let v = w.volts();
            let (amp, phase) = mho_dsp::tone(&v, w.preamble.x_increment, w.preamble.time(0), f)
                .ok_or_else(|| DriverError::State(format!("too few periods at {f:.4e} Hz")))?;
            let scale = self.channel(ch)?.scale;
            // Peak-to-peak in divisions: keep the tone between ~1.5 and ~7 div.
            let divs = 2.0 * amp / scale;
            let new = if divs > 7.0 {
                Some(step_125(scale, true))
            } else if divs < 1.5 && scale > 1e-3 {
                Some(step_125(scale, false))
            } else {
                None
            };
            if let Some(s) = new {
                self.set_channel(
                    ch,
                    &ChannelUpdate {
                        scale: Some(s),
                        ..Default::default()
                    },
                )?;
                rescaled = true;
            }
            tones.push((amp, phase));
        }
        Ok((tones[0], tones[1], rescaled))
    }
}

#[cfg(test)]
mod tests {
    use std::f64::consts::PI;

    use super::*;

    #[test]
    fn requests() {
        let r = BodeRequest::default();
        r.validate().unwrap();
        let f = r.frequencies();
        assert_eq!(f.len(), 31);
        assert_eq!((f[0], f[30]), (100.0, 1e6));
        assert_eq!(f[15], 10_000.0);
        for bad in [
            BodeRequest {
                input: 2,
                ..r.clone()
            },
            BodeRequest {
                start: 2e6,
                ..r.clone()
            },
            BodeRequest {
                points: 1,
                ..r.clone()
            },
            BodeRequest {
                generator: 3,
                ..r.clone()
            },
            BodeRequest {
                amplitude: 0.0,
                ..r.clone()
            },
        ] {
            assert!(bad.validate().is_err(), "{bad:?}");
        }
    }

    #[test]
    fn helpers() {
        assert_eq!(step_125(0.5, true), 1.0);
        assert_eq!(step_125(0.5, false), 0.2);
        assert_eq!(step_125(2.0, false), 1.0);
        assert!((wrap_deg(3.5 * PI) + 90.0).abs() < 1e-9);
        assert!((wrap_deg(-0.25 * PI) + 45.0).abs() < 1e-9);
    }
}
