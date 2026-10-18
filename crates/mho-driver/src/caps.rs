//! What this particular scope can do: model bandwidth plus upgrades, memory depth, generator
//! and the licensed protocol options. Derived from `*IDN?`, `:SYST:OPT:STAT?` and
//! `:SYST:MOD?`, all verified queries.
//!
//! `*IDN?` keeps reporting the original model after a bandwidth upgrade (an MHO954 with
//! BWU05T08 still says `MHO954`), so the effective bandwidth comes from the options.

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use utoipa::ToSchema;

use crate::model::{Idn, Modules, OptionInfo};
use crate::options::OPTIONS;

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema, ToSchema)]
pub struct GeneratorCaps {
    /// GI and GII.
    pub channels: u8,
    /// Highest sine frequency (100 MHz with AFG100, 50 MHz with AFG50).
    pub max_sine_hz: f64,
    /// Licensed option (`AFG100`, `AFG50`), `None` if only the hardware flag is set.
    pub option: Option<String>,
    /// DG field of `:SYST:MOD?`.
    pub hardware_flag: bool,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema, ToSchema)]
pub struct Capabilities {
    pub model: String,
    /// Analog bandwidth of the model as shipped (Hz), `None` for unknown models.
    pub base_bandwidth_hz: Option<f64>,
    /// Effective bandwidth with one or two channels on (Hz), after bandwidth upgrades.
    pub bandwidth_hz: Option<f64>,
    /// Effective bandwidth with three or four channels on (Hz).
    pub bandwidth_4ch_hz: Option<f64>,
    /// Installed bandwidth upgrade (`BWU05T08`, ...).
    pub bandwidth_upgrade: Option<String>,
    /// Maximum memory depth in points with 1, 2, 3 and 4 channels on.
    pub max_memory_depth: [u64; 4],
    pub logic_analyzer: bool,
    /// Built-in function generator, if present and licensed.
    pub generator: Option<GeneratorCaps>,
    /// Optional protocol decoders/triggers that are licensed (CAN-FD, FlexRay, I2S, MIL-STD-1553).
    pub protocols: Vec<String>,
    /// Active options (order codes); standard features without license are not included.
    pub options: Vec<String>,
}

/// Order codes covered by the BND bundle.
const BUNDLE: &[&str] = &["AFG100", "AUDIOA", "AUTOA", "FlexA", "AEROA"];

/// Bandwidth of the model as shipped: (1–2 channels, 3–4 channels), guide table 1.
fn model_bandwidth(model: &str) -> Option<(f64, f64)> {
    match model {
        "MHO934" => Some((350e6, 350e6)),
        "MHO954" => Some((500e6, 400e6)),
        "MHO984" => Some((800e6, 400e6)),
        _ => None,
    }
}

impl Capabilities {
    /// `generator_confirmed`: the generator answered on this instrument (its canary query
    /// is verified), so a licensed AFG counts even if `:SYST:MOD?` reports no DG.
    pub fn derive(
        idn: &Idn,
        options: &[OptionInfo],
        modules: Modules,
        generator_confirmed: bool,
    ) -> Self {
        let active = |code: &str| {
            options.iter().any(|o| {
                o.active
                    && !o.standard
                    && (o.name == code || (o.name == "BND" && BUNDLE.contains(&code)))
            })
        };
        let base = model_bandwidth(&idn.model);
        let upgrade = ["BWU05T08", "BWU03T08", "BWU03T05"]
            .into_iter()
            .find(|c| active(c));
        let upgraded = match upgrade {
            Some("BWU03T05") => Some(500e6_f64),
            Some(_) => Some(800e6),
            None => None,
        };
        let bandwidth = match (base, upgraded) {
            (Some((b, _)), Some(u)) => Some(u.max(b)),
            (Some((b, _)), None) => Some(b),
            (None, _) => None,
        };
        let bandwidth_4ch = match (base, bandwidth) {
            (Some((_, b4)), Some(bw)) => Some(if bw > 350e6 {
                bw.min(400e6_f64).max(b4)
            } else {
                b4
            }),
            _ => None,
        };
        let max_memory_depth = if active("RLU05") {
            [500_000_000, 250_000_000, 125_000_000, 125_000_000]
        } else {
            [100_000_000, 50_000_000, 25_000_000, 25_000_000]
        };
        let afg_option = ["AFG100", "AFG50"].into_iter().find(|c| active(c));
        let generator = (modules.function_generator
            || (afg_option.is_some() && generator_confirmed))
            .then(|| GeneratorCaps {
                channels: 2,
                max_sine_hz: if afg_option == Some("AFG50") {
                    50e6
                } else {
                    100e6
                },
                option: afg_option.map(String::from),
                hardware_flag: modules.function_generator,
            });
        let protocols = [
            ("AUTOA", "CAN-FD"),
            ("FlexA", "FlexRay"),
            ("AUDIOA", "I2S"),
            ("AEROA", "MIL-STD-1553"),
        ]
        .into_iter()
        .filter(|(c, _)| active(c))
        .map(|(_, p)| p.to_string())
        .collect();
        Capabilities {
            model: idn.model.clone(),
            base_bandwidth_hz: base.map(|b| b.0),
            bandwidth_hz: bandwidth,
            bandwidth_4ch_hz: bandwidth_4ch,
            bandwidth_upgrade: upgrade.map(String::from),
            max_memory_depth,
            logic_analyzer: modules.logic_analyzer,
            generator,
            protocols,
            options: options
                .iter()
                .filter(|o| o.active && !o.standard)
                .map(|o| o.name.clone())
                .collect(),
        }
    }

    /// `true` if the option with this order code is active (directly or through BND).
    pub fn has_option(&self, code: &str) -> bool {
        self.options.iter().any(|o| o == code)
            || (BUNDLE.contains(&code) && self.options.iter().any(|o| o == "BND"))
    }

    /// Maximum memory depth with `channels_on` analog channels on.
    pub fn memory_limit(&self, channels_on: usize) -> u64 {
        self.max_memory_depth[channels_on.clamp(1, 4) - 1]
    }

    /// One line for `idn`/`status`, e.g. `MHO954, 800 MHz (BWU05T08), 500 Mpts, AFG 100 MHz`.
    pub fn summary(&self) -> String {
        let mut parts = vec![self.model.clone()];
        if let Some(bw) = self.bandwidth_hz {
            let mut s = format!("{:.0} MHz", bw / 1e6);
            if let Some(u) = &self.bandwidth_upgrade {
                s.push_str(&format!(" ({u})"));
            }
            parts.push(s);
        }
        parts.push(format!("{} Mpts", self.max_memory_depth[0] / 1_000_000));
        if let Some(g) = &self.generator {
            parts.push(format!("AFG {:.0} MHz", g.max_sine_hz / 1e6));
        }
        if !self.protocols.is_empty() {
            parts.push(self.protocols.join("/"));
        }
        parts.join(", ")
    }
}

/// `true` if `code` is a known option order code (see [`OPTIONS`]).
pub fn known_option(code: &str) -> bool {
    OPTIONS.iter().any(|o| o.code == code)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn idn(model: &str) -> Idn {
        Idn::parse(&format!("RIGOL TECHNOLOGIES,{model},X,00.01.00")).unwrap()
    }

    fn opts(active: &[&str]) -> Vec<OptionInfo> {
        OPTIONS
            .iter()
            .map(|o| OptionInfo {
                name: o.code.into(),
                description: o.description.into(),
                active: active.contains(&o.code),
                standard: false,
            })
            .collect()
    }

    #[test]
    fn plain_mho954() {
        let c = Capabilities::derive(&idn("MHO954"), &opts(&[]), Modules::default(), false);
        assert_eq!(c.bandwidth_hz, Some(500e6));
        assert_eq!(c.bandwidth_4ch_hz, Some(400e6));
        assert_eq!(c.memory_limit(1), 100_000_000);
        assert_eq!(c.memory_limit(4), 25_000_000);
        assert!(c.generator.is_none() && c.protocols.is_empty());
        assert_eq!(c.summary(), "MHO954, 500 MHz, 100 Mpts");
    }

    #[test]
    fn upgraded_mho954() {
        // The state of the test scope after the upgrade: IDN still MHO954, DG field 0.
        let all = [
            "BWU05T08", "RLU05", "AFG50", "AFG100", "AUTOA", "FlexA", "AUDIOA", "AEROA",
        ];
        let la = Modules {
            logic_analyzer: true,
            function_generator: false,
        };
        let c = Capabilities::derive(&idn("MHO954"), &opts(&all), la, false);
        assert_eq!(c.bandwidth_hz, Some(800e6));
        assert_eq!(c.bandwidth_4ch_hz, Some(400e6));
        assert_eq!(c.bandwidth_upgrade.as_deref(), Some("BWU05T08"));
        assert_eq!(c.memory_limit(1), 500_000_000);
        assert_eq!(c.memory_limit(2), 250_000_000);
        assert_eq!(c.memory_limit(3), 125_000_000);
        assert_eq!(c.protocols, ["CAN-FD", "FlexRay", "I2S", "MIL-STD-1553"]);
        // Licensed, but not confirmed and no DG flag: no generator yet.
        assert!(c.generator.is_none());
        let c = Capabilities::derive(&idn("MHO954"), &opts(&all), la, true);
        let g = c.generator.as_ref().unwrap();
        assert_eq!(
            (g.max_sine_hz, g.option.as_deref()),
            (100e6, Some("AFG100"))
        );
        assert_eq!(
            c.summary(),
            "MHO954, 800 MHz (BWU05T08), 500 Mpts, AFG 100 MHz, CAN-FD/FlexRay/I2S/MIL-STD-1553"
        );
    }

    #[test]
    fn bundle_and_small_generator() {
        let c = Capabilities::derive(&idn("MHO934"), &opts(&["BND"]), Modules::default(), true);
        assert!(c.has_option("FlexA") && c.has_option("AFG100") && !c.has_option("RLU05"));
        assert_eq!(c.protocols.len(), 4);
        assert_eq!(c.bandwidth_hz, Some(350e6));
        let c = Capabilities::derive(
            &idn("MHO934"),
            &opts(&["AFG50", "BWU03T05"]),
            Modules::default(),
            true,
        );
        assert_eq!(c.generator.unwrap().max_sine_hz, 50e6);
        assert_eq!(
            (c.bandwidth_hz, c.bandwidth_4ch_hz),
            (Some(500e6), Some(400e6))
        );
        // Unknown models: no bandwidth claims.
        let c = Capabilities::derive(&idn("MHO98"), &opts(&[]), Modules::default(), false);
        assert_eq!(c.bandwidth_hz, None);
    }
}
