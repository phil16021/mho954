//! Limits of the built-in function generator (guide 3.25.4, 3.25.10, 3.25.11). The scope
//! rejects out-of-range values with an error in the queue that does not say which limit;
//! checking first gives a useful message.

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use utoipa::ToSchema;

use crate::caps::GeneratorCaps;
use crate::error::{DriverError, Result};
use crate::params::{ParamMap, ParamValue};

/// Typed change of one generator channel; fields left out stay as they are. Applied as
/// groups `afg` and `afg.modulation`, output switched last (or first when turning off).
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize, JsonSchema, ToSchema)]
#[serde(default)]
pub struct GeneratorUpdate {
    /// Output on/off.
    pub output: Option<bool>,
    /// SIN, SQU, RAMP, NOIS, DC, ARB, EXPR, EXPF, ECG1, GAUS, LOR, HAV, SINC.
    pub function: Option<String>,
    /// Hz.
    pub frequency: Option<f64>,
    /// Vpp.
    pub amplitude: Option<f64>,
    /// V.
    pub offset: Option<f64>,
    /// Degrees 0..360.
    pub phase: Option<f64>,
    /// Square duty cycle in %.
    pub duty: Option<f64>,
    /// Ramp symmetry in %.
    pub symmetry: Option<f64>,
    /// `highz` or `50` (the load the generator expects, sets the displayed amplitude).
    pub load: Option<String>,
    /// `AM`, `FM`, `PM` or `OFF`.
    pub modulation: Option<String>,
    /// AM depth in %.
    pub mod_depth: Option<f64>,
    /// FM deviation in Hz, PM deviation in degrees.
    pub mod_deviation: Option<f64>,
    /// Modulating frequency in Hz.
    pub mod_frequency: Option<f64>,
    /// Modulating waveform: SIN, SQU, TRI, UPR, DNR, NOIS.
    pub mod_wave: Option<String>,
}

impl GeneratorUpdate {
    /// Parameters of group `afg` without `output`.
    pub fn settings(&self) -> Result<ParamMap> {
        let mut m = ParamMap::new();
        let mut real = |k: &str, v: Option<f64>| {
            if let Some(x) = v {
                m.insert(k.into(), ParamValue::Real(x));
            }
        };
        real("frequency", self.frequency);
        real("amplitude", self.amplitude);
        real("offset", self.offset);
        real("phase", self.phase);
        real("duty", self.duty);
        real("symmetry", self.symmetry);
        if let Some(f) = &self.function {
            m.insert("function".into(), ParamValue::Text(f.trim().into()));
        }
        if let Some(l) = &self.load {
            let token = match l.trim().to_ascii_lowercase().as_str() {
                "highz" | "high-z" | "hiz" | "high_z" | "omeg" | "1m" => "OMEG",
                "50" | "fifty" | "fift" | "50ohm" => "FIFT",
                other => {
                    return Err(DriverError::Invalid(format!(
                        "load {other:?}: use highz or 50"
                    )));
                }
            };
            m.insert("load".into(), ParamValue::Text(token.into()));
        }
        Ok(m)
    }

    /// Parameters of group `afg.modulation` (empty if nothing about modulation is given).
    pub fn modulation_settings(&self) -> Result<ParamMap> {
        let mut m = ParamMap::new();
        let kind = match &self.modulation {
            Some(t) if t.eq_ignore_ascii_case("off") => {
                m.insert("enable".into(), ParamValue::Bool(false));
                return Ok(m);
            }
            Some(t) => {
                let t = t.trim().to_ascii_lowercase();
                if !matches!(t.as_str(), "am" | "fm" | "pm") {
                    return Err(DriverError::Invalid(format!(
                        "modulation {t:?}: use am, fm, pm or off"
                    )));
                }
                m.insert("enable".into(), ParamValue::Bool(true));
                m.insert("type".into(), ParamValue::Text(t.to_ascii_uppercase()));
                t
            }
            None => {
                let rest = self.mod_depth.is_some()
                    || self.mod_deviation.is_some()
                    || self.mod_frequency.is_some()
                    || self.mod_wave.is_some();
                if rest {
                    return Err(DriverError::Invalid(
                        "give the modulation type (am, fm, pm) with its parameters".into(),
                    ));
                }
                return Ok(m);
            }
        };
        if let Some(d) = self.mod_depth {
            if kind != "am" {
                return Err(DriverError::Invalid("depth is an AM parameter".into()));
            }
            m.insert("am_depth".into(), ParamValue::Real(d));
        }
        if let Some(d) = self.mod_deviation {
            if kind == "am" {
                return Err(DriverError::Invalid(
                    "deviation is an FM/PM parameter".into(),
                ));
            }
            m.insert(format!("{kind}_deviation"), ParamValue::Real(d));
        }
        if let Some(f) = self.mod_frequency {
            m.insert(format!("{kind}_frequency"), ParamValue::Real(f));
        }
        if let Some(w) = &self.mod_wave {
            m.insert(format!("{kind}_wave"), ParamValue::Text(w.trim().into()));
        }
        Ok(m)
    }
}

/// Highest frequency of a waveform (short form of `:SOUR<n>:FUNC`), `None` for waveforms
/// without frequency (DC, noise).
pub fn max_frequency(function: &str, g: &GeneratorCaps) -> Option<f64> {
    let full = g.max_sine_hz >= 100e6;
    let pick = |afg100: f64, afg50: f64| Some(if full { afg100 } else { afg50 });
    match function.to_ascii_uppercase().as_str() {
        "SIN" | "SINUSOID" => Some(g.max_sine_hz),
        "SQU" | "SQUARE" => pick(20e6, 10e6),
        "RAMP" => pick(2e6, 1e6),
        "NOIS" | "NOISE" | "DC" => None,
        // ARB and the built-in shapes (EXPR, EXPF, ECG1, GAUS, LOR, HAV, SINC).
        _ => pick(20e6, 10e6),
    }
}

/// Lowest frequency: 2 mHz with AFG100, 20 mHz with AFG50.
pub fn min_frequency(g: &GeneratorCaps) -> f64 {
    if g.max_sine_hz >= 100e6 { 2e-3 } else { 20e-3 }
}

/// Maximum amplitude (Vpp) at `frequency` into the load: 50 Ω (`fifty`) or high-Z.
pub fn max_amplitude(frequency: f64, fifty: bool) -> f64 {
    let base = if fifty { 10.0 } else { 20.0 };
    if frequency > 50e6 { base / 2.0 } else { base }
}

pub fn min_amplitude(fifty: bool) -> f64 {
    if fifty { 1e-3 } else { 2e-3 }
}

fn number(v: Option<&ParamValue>) -> Option<f64> {
    match v? {
        ParamValue::Real(x) => Some(*x),
        ParamValue::Int(i) => Some(*i as f64),
        ParamValue::Text(t) => t.trim().parse().ok(),
        ParamValue::Bool(_) => None,
    }
}

fn text(v: Option<&ParamValue>) -> Option<String> {
    match v? {
        ParamValue::Text(t) => Some(t.trim().to_ascii_uppercase()),
        _ => None,
    }
}

/// Checks an update of group `afg` against the limits. `current` holds the channel's
/// present settings if they could be read (otherwise only given values are checked, and
/// unknown settings are assumed to be the most permissive).
pub fn check_update(
    g: &GeneratorCaps,
    update: &ParamMap,
    current: Option<&ParamMap>,
) -> Result<()> {
    let get = |k: &str| update.get(k).or_else(|| current.and_then(|c| c.get(k)));
    let bad = |m: String| Err(DriverError::Invalid(m));
    let function = text(get("function"));
    let frequency = number(get("frequency")).or_else(|| number(get("period")).map(|p| 1.0 / p));
    // OMEG (high-Z) or FIFT (50 Ω); unknown load → high-Z, which allows more.
    let fifty = text(get("load")).is_some_and(|l| l.starts_with("FIFT") || l == "50");

    if let Some(f) = frequency {
        let min = min_frequency(g);
        if f < min {
            return bad(format!(
                "frequency {f} Hz below the generator minimum of {min} Hz"
            ));
        }
        let max = match &function {
            Some(func) => max_frequency(func, g),
            None => Some(g.max_sine_hz),
        };
        if let Some(max) = max
            && f > max
        {
            let w = function.as_deref().unwrap_or("SIN");
            return bad(format!(
                "frequency {f:e} Hz above the {w} limit of {max:e} Hz"
            ));
        }
    }
    let amplitude = number(get("amplitude"));
    let f_for_amp = frequency.unwrap_or(0.0);
    if let Some(a) = amplitude {
        let (min, max) = (min_amplitude(fifty), max_amplitude(f_for_amp, fifty));
        if a < min || a > max {
            let load = if fifty { "50 Ω" } else { "high-Z" };
            return bad(format!(
                "amplitude {a} Vpp outside {min}..={max} Vpp ({load}, {f_for_amp:e} Hz)"
            ));
        }
    }
    if let Some(o) = number(update.get("offset")) {
        let vpp = amplitude.unwrap_or(min_amplitude(fifty));
        let room = (max_amplitude(f_for_amp, fifty) - vpp) / 2.0;
        if o.abs() > room + 1e-9 {
            return bad(format!(
                "offset {o} V too large for {vpp} Vpp: at most ±{room} V (±(Vmax − Vpp)/2)"
            ));
        }
    }
    Ok(())
}

/// Splits modulation settings into `enable`/`type` and the parameters, written in that order.
pub fn split_modulation(m: ParamMap) -> (ParamMap, ParamMap) {
    m.into_iter()
        .partition(|(k, _)| k == "enable" || k == "type")
}

impl crate::Scope {
    /// Changes generator channel `n` in one go: output off first (if asked), then waveform
    /// settings and modulation, output on last. Returns the channel's state if group `afg`
    /// may be read.
    pub fn set_generator(&mut self, n: u8, update: &GeneratorUpdate) -> Result<Option<ParamMap>> {
        let settings = update.settings()?;
        let modulation = update.modulation_settings()?;
        let output = |on: bool| {
            let mut m = ParamMap::new();
            m.insert("output".into(), ParamValue::Bool(on));
            m
        };
        let mut state = None;
        if update.output == Some(false) {
            state = self.group_set("afg", Some(n), &output(false), false)?;
        }
        if !settings.is_empty() {
            state = self.group_set("afg", Some(n), &settings, false)?;
        }
        // Switching modulation on or changing its type resets its parameters (seen on the
        // device: AM depth 40 % became 100 %), so state and type go first.
        let (head, rest) = split_modulation(modulation);
        for m in [head, rest] {
            if !m.is_empty() {
                self.group_set("afg.modulation", Some(n), &m, false)?;
            }
        }
        if update.output == Some(true) {
            state = self.group_set("afg", Some(n), &output(true), false)?;
        }
        if state.is_none() && *update != GeneratorUpdate::default() {
            state = self.group_get("afg", Some(n), false).ok();
        }
        Ok(state)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn afg(max: f64) -> GeneratorCaps {
        GeneratorCaps {
            channels: 2,
            max_sine_hz: max,
            option: None,
            hardware_flag: true,
        }
    }

    fn map(kv: &[(&str, ParamValue)]) -> ParamMap {
        kv.iter().map(|(k, v)| (k.to_string(), v.clone())).collect()
    }

    #[test]
    fn modulation_state_goes_first() {
        let u = GeneratorUpdate {
            modulation: Some("am".into()),
            mod_depth: Some(40.0),
            mod_frequency: Some(10.0),
            ..Default::default()
        };
        let (head, rest) = split_modulation(u.modulation_settings().unwrap());
        assert_eq!(head.keys().collect::<Vec<_>>(), ["enable", "type"]);
        assert_eq!(
            rest.keys().collect::<Vec<_>>(),
            ["am_depth", "am_frequency"]
        );
    }

    #[test]
    fn typed_update() {
        let u = GeneratorUpdate {
            output: Some(true),
            function: Some("squ".into()),
            frequency: Some(1e3),
            load: Some("50".into()),
            modulation: Some("fm".into()),
            mod_deviation: Some(100.0),
            mod_frequency: Some(10.0),
            ..Default::default()
        };
        let s = u.settings().unwrap();
        assert_eq!(s["load"], ParamValue::Text("FIFT".into()));
        assert!(!s.contains_key("output"));
        let m = u.modulation_settings().unwrap();
        assert_eq!(m["type"], ParamValue::Text("FM".into()));
        assert_eq!(m["fm_deviation"], ParamValue::Real(100.0));
        assert_eq!(m["fm_frequency"], ParamValue::Real(10.0));
        let bad = GeneratorUpdate {
            modulation: Some("am".into()),
            mod_deviation: Some(1.0),
            ..Default::default()
        };
        assert!(bad.modulation_settings().is_err());
        let off = GeneratorUpdate {
            modulation: Some("OFF".into()),
            ..Default::default()
        };
        assert_eq!(off.modulation_settings().unwrap().len(), 1);
        assert!(
            GeneratorUpdate::default()
                .modulation_settings()
                .unwrap()
                .is_empty()
        );
    }

    #[test]
    fn waveform_limits() {
        let g = afg(100e6);
        assert_eq!(max_frequency("SIN", &g), Some(100e6));
        assert_eq!(max_frequency("squ", &g), Some(20e6));
        assert_eq!(max_frequency("RAMP", &afg(50e6)), Some(1e6));
        assert_eq!(max_frequency("SINC", &g), Some(20e6));
        assert_eq!(max_frequency("DC", &g), None);
        assert_eq!(max_amplitude(60e6, true), 5.0);
        assert_eq!(max_amplitude(1e3, false), 20.0);
    }

    #[test]
    fn updates() {
        use ParamValue::*;
        let g = afg(100e6);
        let ok = |kv: &[(&str, ParamValue)]| check_update(&g, &map(kv), None).is_ok();
        assert!(ok(&[("frequency", Real(100e6))]));
        assert!(!ok(&[("frequency", Real(101e6))]));
        assert!(!ok(&[
            ("function", Text("SQU".into())),
            ("frequency", Real(25e6))
        ]));
        assert!(!ok(&[("frequency", Real(1e-3))]));
        assert!(ok(&[
            ("function", Text("DC".into())),
            ("offset", Real(1.0))
        ]));
        assert!(!ok(&[
            ("load", Text("FIFT".into())),
            ("amplitude", Real(12.0))
        ]));
        assert!(ok(&[("amplitude", Real(12.0))]), "high-Z allows 20 Vpp");
        // guide example: 5 MHz, 6 V into 50 Ω → offset ±2 V
        let base = [
            ("load", Text("FIFT".into())),
            ("frequency", Real(5e6)),
            ("amplitude", Real(6.0)),
        ];
        let mut with = base.to_vec();
        with.push(("offset", Real(2.0)));
        assert!(ok(&with));
        with.pop();
        with.push(("offset", Real(2.5)));
        assert!(!ok(&with));
        // current state fills in what the update leaves out
        let cur = map(&[("function", Text("RAMP".into()))]);
        assert!(check_update(&g, &map(&[("frequency", Real(3e6))]), Some(&cur)).is_err());
        assert!(check_update(&afg(50e6), &map(&[("frequency", Real(60e6))]), None).is_err());
    }
}
