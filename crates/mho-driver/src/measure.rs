//! Catalogue of `:MEASure:ITEM?` items.
//!
//! Validating items locally matters: the MHO900 does not answer unknown queries at all, so a
//! typo would cost a full I/O timeout plus a device clear.

use schemars::JsonSchema;
use serde::Serialize;
use utoipa::ToSchema;

use crate::error::{DriverError, Result};

#[derive(Debug, Clone, Copy, Serialize, JsonSchema, ToSchema)]
pub struct MeasureItem {
    /// Short SCPI token sent to the scope.
    pub token: &'static str,
    /// Long SCPI form (also accepted as input).
    pub long: &'static str,
    pub unit: &'static str,
    pub description: &'static str,
    /// Item needs two sources (delay/phase).
    pub dual_source: bool,
}

const fn item(token: &'static str, long: &'static str, unit: &'static str, description: &'static str) -> MeasureItem {
    MeasureItem { token, long, unit, description, dual_source: false }
}

const fn dual(token: &'static str, long: &'static str, unit: &'static str, description: &'static str) -> MeasureItem {
    MeasureItem { token, long, unit, description, dual_source: true }
}

pub const MEASURE_ITEMS: &[MeasureItem] = &[
    item("VMAX", "VMAX", "V", "Maximum voltage"),
    item("VMIN", "VMIN", "V", "Minimum voltage"),
    item("VPP", "VPP", "V", "Peak-to-peak voltage"),
    item("VTOP", "VTOP", "V", "Top level (flat top)"),
    item("VBAS", "VBASE", "V", "Base level (flat bottom)"),
    item("VAMP", "VAMP", "V", "Amplitude (top - base)"),
    item("VAVG", "VAVG", "V", "Average voltage"),
    item("VRMS", "VRMS", "V", "RMS voltage"),
    item("PVRM", "PVRMS", "V", "RMS over one period"),
    item("VUPP", "VUPPER", "V", "Upper threshold level"),
    item("VMID", "VMID", "V", "Middle threshold level"),
    item("VLOW", "VLOWER", "V", "Lower threshold level"),
    item("ACRM", "ACRMS", "V", "AC RMS (DC removed)"),
    item("OVER", "OVERSHOOT", "%", "Overshoot"),
    item("PRES", "PRESHOOT", "%", "Preshoot"),
    item("MAR", "MAREA", "Vs", "Area"),
    item("MPAR", "MPAREA", "Vs", "Area over one period"),
    item("PER", "PERIOD", "s", "Period"),
    item("FREQ", "FREQUENCY", "Hz", "Frequency"),
    item("RTIM", "RTIME", "s", "Rise time (10-90 %)"),
    item("FTIM", "FTIME", "s", "Fall time (90-10 %)"),
    item("PWID", "PWIDTH", "s", "Positive pulse width"),
    item("NWID", "NWIDTH", "s", "Negative pulse width"),
    item("PDUT", "PDUTY", "%", "Positive duty cycle"),
    item("NDUT", "NDUTY", "%", "Negative duty cycle"),
    item("TVMAX", "TVMAX", "s", "Time of maximum"),
    item("TVMIN", "TVMIN", "s", "Time of minimum"),
    item("PSL", "PSLEWRATE", "V/s", "Rising slew rate"),
    item("NSL", "NSLEWRATE", "V/s", "Falling slew rate"),
    item("PPUL", "PPULSES", "", "Positive pulse count"),
    item("NPUL", "NPULSES", "", "Negative pulse count"),
    item("PEDG", "PEDGES", "", "Rising edge count"),
    item("NEDG", "NEDGES", "", "Falling edge count"),
    dual("RRD", "RRDELAY", "s", "Delay rising→rising"),
    dual("RFD", "RFDELAY", "s", "Delay rising→falling"),
    dual("FRD", "FRDELAY", "s", "Delay falling→rising"),
    dual("FFD", "FFDELAY", "s", "Delay falling→falling"),
    dual("RRPH", "RRPHASE", "°", "Phase rising→rising"),
    dual("RFPH", "RFPHASE", "°", "Phase rising→falling"),
    dual("FRPH", "FRPHASE", "°", "Phase falling→rising"),
    dual("FFPH", "FFPHASE", "°", "Phase falling→falling"),
];

/// Looks up an item by short token, long form or a few common aliases (case-insensitive).
pub fn find_item(name: &str) -> Result<&'static MeasureItem> {
    let u = name.trim().to_ascii_uppercase();
    let u = match u.as_str() {
        "FREQUENCY" | "F" => "FREQ".to_string(),
        "RISE" | "RISETIME" => "RTIM".to_string(),
        "FALL" | "FALLTIME" => "FTIM".to_string(),
        "DUTY" => "PDUT".to_string(),
        "AMPLITUDE" => "VAMP".to_string(),
        "MEAN" | "AVG" => "VAVG".to_string(),
        "RMS" => "VRMS".to_string(),
        "PP" => "VPP".to_string(),
        _ => u,
    };
    MEASURE_ITEMS
        .iter()
        .find(|i| i.token == u || i.long == u)
        .ok_or_else(|| DriverError::Invalid(format!("unknown measurement item {name:?}")))
}

/// The scope reports unavailable measurements as 9.9E37.
pub fn valid_value(v: f64) -> Option<f64> {
    (v.is_finite() && v.abs() < 9e37).then_some(v)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn lookup() {
        assert_eq!(find_item("vpp").unwrap().token, "VPP");
        assert_eq!(find_item("Frequency").unwrap().token, "FREQ");
        assert_eq!(find_item("rise").unwrap().token, "RTIM");
        assert!(find_item("bogus").is_err());
        assert_eq!(valid_value(9.9e37), None);
        assert_eq!(valid_value(1.5), Some(1.5));
    }
}
