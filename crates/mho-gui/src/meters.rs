//! Hardware frequency counter and digital voltmeter: switch on, pick the source, show the
//! reading (refreshed once per second while the section is open).

use std::time::{Duration, Instant};

use eframe::egui::{self, RichText};
use mho_driver::params::{ParamMap, ParamValue};
use mho_dsp::format_si;
use mho_service::ScopeApi;

use crate::state::{Client, Shared};

const POLL: Duration = Duration::from_secs(1);

/// Counter or DVM as last read from the scope.
#[derive(Debug, Clone, Default)]
pub struct Meter {
    pub on: Option<bool>,
    pub source: Option<String>,
    /// Last reading (`Some(None)`: no valid value).
    pub value: Option<Option<f64>>,
}

#[derive(Debug, Clone, Default)]
pub struct Meters {
    pub counter: Meter,
    pub dvm: Meter,
}

#[derive(Default)]
pub struct MetersPanel {
    loaded: bool,
    polled: Option<Instant>,
}

#[derive(Clone, Copy)]
enum Kind {
    Counter,
    Dvm,
}

impl Kind {
    fn group(self) -> &'static str {
        match self {
            Kind::Counter => "counter",
            Kind::Dvm => "dvm",
        }
    }

    fn slot(self, m: &mut Meters) -> &mut Meter {
        match self {
            Kind::Counter => &mut m.counter,
            Kind::Dvm => &mut m.dvm,
        }
    }
}

fn take_state(sh: &mut Shared, kind: Kind, st: &ParamMap) {
    let slot = kind.slot(&mut sh.meters);
    if let Some(ParamValue::Bool(on)) = st.get("enable") {
        slot.on = Some(*on);
    }
    if let Some(ParamValue::Text(src)) = st.get("source") {
        slot.source = Some(src.clone());
    }
}

fn set(client: &Client, kind: Kind, key: &str, v: ParamValue) {
    let mut m = ParamMap::new();
    m.insert(key.into(), v);
    client.op(
        kind.group(),
        move |api| async move { api.group_set(kind.group().into(), None, m, false).await },
        move |sh: &mut Shared, st: Option<ParamMap>| {
            if let Some(st) = st {
                take_state(sh, kind, &st);
            }
        },
    );
}

impl MetersPanel {
    /// Returns a settings group to open, if asked.
    pub fn ui(&mut self, ui: &mut egui::Ui, client: &Client, m: &Meters) -> Option<&'static str> {
        if !self.loaded {
            self.loaded = true;
            for kind in [Kind::Counter, Kind::Dvm] {
                client.op(
                    kind.group(),
                    move |api| async move { api.group_get(kind.group().into(), None).await },
                    move |sh: &mut Shared, st: ParamMap| take_state(sh, kind, &st),
                );
            }
        }
        if self.polled.is_none_or(|t| t.elapsed() >= POLL) {
            self.polled = Some(Instant::now());
            if m.counter.on == Some(true) {
                client.op(
                    "counter",
                    |api| async move { api.counter_value().await },
                    |sh: &mut Shared, v| sh.meters.counter.value = Some(v),
                );
            }
            if m.dvm.on == Some(true) {
                client.op(
                    "dvm",
                    |api| async move { api.dvm_value().await },
                    |sh: &mut Shared, v| sh.meters.dvm.value = Some(v),
                );
            }
        }
        ui.ctx().request_repaint_after(POLL);
        let mut open = None;
        egui::Grid::new("meters").num_columns(4).show(ui, |ui| {
            for (kind, name, unit, meter) in [
                (Kind::Counter, "Counter", "Hz", &m.counter),
                (Kind::Dvm, "DVM", "V", &m.dvm),
            ] {
                let mut on = meter.on.unwrap_or(false);
                if ui.checkbox(&mut on, name).changed() {
                    set(client, kind, "enable", ParamValue::Bool(on));
                }
                let src = meter.source.clone().unwrap_or_else(|| "CHAN1".into());
                egui::ComboBox::from_id_salt(("meter_src", name))
                    .selected_text(&src)
                    .width(70.0)
                    .show_ui(ui, |ui| {
                        for n in 1..=4 {
                            let s = format!("CHAN{n}");
                            if ui.selectable_label(src == s, &s).clicked() {
                                set(client, kind, "source", ParamValue::Text(s));
                            }
                        }
                    });
                let text = match (meter.on, meter.value) {
                    (Some(true), Some(Some(v))) => format_si(v, unit),
                    (Some(true), Some(None)) => "---".into(),
                    (Some(true), None) => "...".into(),
                    _ => "off".into(),
                };
                ui.label(RichText::new(text).monospace().strong());
                if ui
                    .small_button("Settings...")
                    .on_hover_text(format!("all settings of group {}", kind.group()))
                    .clicked()
                {
                    open = Some(kind.group());
                }
                ui.end_row();
            }
        });
        if m.dvm.on == Some(true) {
            ui.label(
                RichText::new("the DVM needs ~2 s to settle after switching on")
                    .small()
                    .weak(),
            );
        }
        open
    }
}
