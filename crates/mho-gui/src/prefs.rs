//! View preferences kept between sessions in `~/.config/mho954/gui.json`. Scope settings
//! are not stored here: they live in the instrument.

use std::path::PathBuf;

use mho_dsp::Window;
use serde::{Deserialize, Serialize};

use crate::bode::BodePanel;
use crate::generator::GeneratorPanel;

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Prefs {
    pub volts_axis: bool,
    pub show_fft: bool,
    pub fft_source: String,
    pub fft_window: Window,
    pub show_decoder: bool,
    pub decoder_bus: u8,
    pub peaks_math: u8,
    pub meas_item: String,
    pub meas_source: String,
    pub raw_source: String,
    pub bode: BodePanel,
    pub generator: GeneratorPanel,
}

impl Default for Prefs {
    fn default() -> Self {
        Prefs {
            volts_axis: false,
            show_fft: false,
            fft_source: "CHAN1".into(),
            fft_window: Window::Hann,
            show_decoder: false,
            decoder_bus: 1,
            peaks_math: 1,
            meas_item: "VRMS".into(),
            meas_source: "CHAN1".into(),
            raw_source: "CHAN1".into(),
            bode: BodePanel::default(),
            generator: GeneratorPanel::default(),
        }
    }
}

fn path() -> Option<PathBuf> {
    let base = std::env::var_os("XDG_CONFIG_HOME")
        .map(PathBuf::from)
        .filter(|p| p.is_absolute())
        .or_else(|| std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".config")))?;
    Some(base.join("mho954").join("gui.json"))
}

/// Stored preferences, defaults if there are none or they cannot be read.
pub fn load() -> Prefs {
    path()
        .and_then(|p| std::fs::read_to_string(p).ok())
        .and_then(|t| serde_json::from_str(&t).ok())
        .unwrap_or_default()
}

pub fn save(p: &Prefs) {
    let Some(path) = path() else { return };
    let r = path
        .parent()
        .map_or(Ok(()), std::fs::create_dir_all)
        .and_then(|_| {
            let json = serde_json::to_string_pretty(p).map_err(std::io::Error::other)?;
            std::fs::write(&path, json)
        });
    if let Err(e) = r {
        tracing::debug!("saving {}: {e}", path.display());
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn roundtrip_and_partial_files() {
        let p = Prefs {
            show_fft: true,
            decoder_bus: 3,
            ..Default::default()
        };
        let json = serde_json::to_string(&p).unwrap();
        assert_eq!(serde_json::from_str::<Prefs>(&json).unwrap(), p);
        // Missing or unknown fields fall back to defaults (older/newer versions).
        let partial: Prefs = serde_json::from_str(r#"{"show_decoder": true, "x": 1}"#).unwrap();
        assert!(partial.show_decoder && partial.fft_source == "CHAN1");
    }
}
