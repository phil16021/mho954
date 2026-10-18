//! Built-in function generator GI/GII: waveform, level, modulation and output switch.

use eframe::egui::{self, Color32, RichText};
use mho_driver::GeneratorUpdate;
use mho_driver::params::{ParamMap, ParamValue};
use mho_service::{CommandRequest, ScopeApi};

use crate::app::parse_si;
use crate::state::{Client, Shared};

const FUNCTIONS: &[(&str, &str)] = &[
    ("SIN", "sine"),
    ("SQU", "square"),
    ("RAMP", "ramp"),
    ("NOIS", "noise"),
    ("DC", "DC"),
    ("ARB", "arbitrary"),
    ("EXPR", "exp. rise"),
    ("EXPF", "exp. fall"),
    ("ECG1", "ECG"),
    ("GAUS", "Gaussian"),
    ("LOR", "Lorentz"),
    ("HAV", "haversine"),
    ("SINC", "sinc"),
];
const MOD_WAVES: &[&str] = &["SIN", "SQU", "TRI", "UPR", "DNR", "NOIS"];

/// Editable settings of one generator channel.
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
#[serde(default)]
pub struct GenForm {
    function: String,
    frequency: String,
    amplitude: String,
    offset: String,
    phase: String,
    duty: String,
    symmetry: String,
    fifty_ohm: bool,
    /// `OFF`, `AM`, `FM` or `PM`.
    modulation: String,
    mod_depth: String,
    mod_deviation: String,
    mod_frequency: String,
    mod_wave: String,
    arb_path: String,
}

impl Default for GenForm {
    fn default() -> Self {
        GenForm {
            function: "SIN".into(),
            frequency: "1k".into(),
            amplitude: "1".into(),
            offset: "0".into(),
            phase: "0".into(),
            duty: "50".into(),
            symmetry: "50".into(),
            fifty_ohm: false,
            modulation: "OFF".into(),
            mod_depth: "50".into(),
            mod_deviation: "100".into(),
            mod_frequency: "100".into(),
            mod_wave: "SIN".into(),
            arb_path: "D:/wave.csv".into(),
        }
    }
}

impl GenForm {
    fn update(&self) -> Result<GeneratorUpdate, String> {
        let num = |s: &str, what: &str| {
            parse_si(s)
                .filter(|v| v.is_finite())
                .ok_or_else(|| format!("{what}: number expected, got {s:?}"))
        };
        let f = self.function.as_str();
        let periodic = !matches!(f, "DC" | "NOIS");
        let modulated = self.modulation != "OFF";
        Ok(GeneratorUpdate {
            function: Some(f.into()),
            frequency: periodic
                .then(|| num(&self.frequency, "frequency"))
                .transpose()?,
            amplitude: (f != "DC")
                .then(|| num(&self.amplitude, "amplitude"))
                .transpose()?,
            offset: Some(num(&self.offset, "offset")?),
            phase: periodic.then(|| num(&self.phase, "phase")).transpose()?,
            duty: (f == "SQU").then(|| num(&self.duty, "duty")).transpose()?,
            symmetry: (f == "RAMP")
                .then(|| num(&self.symmetry, "symmetry"))
                .transpose()?,
            load: Some(if self.fifty_ohm { "50" } else { "highz" }.into()),
            modulation: Some(self.modulation.clone()),
            mod_depth: (self.modulation == "AM")
                .then(|| num(&self.mod_depth, "depth"))
                .transpose()?,
            mod_deviation: matches!(self.modulation.as_str(), "FM" | "PM")
                .then(|| num(&self.mod_deviation, "deviation"))
                .transpose()?,
            mod_frequency: modulated
                .then(|| num(&self.mod_frequency, "modulation frequency"))
                .transpose()?,
            mod_wave: modulated.then(|| self.mod_wave.clone()),
            output: None,
        })
    }

    /// Takes over the values read from the scope (group `afg`).
    fn load(&mut self, st: &ParamMap) {
        let real = |k: &str| match st.get(k) {
            Some(ParamValue::Real(x)) => Some(mho_dsp::format_si(*x, "").replace(' ', "")),
            Some(ParamValue::Int(i)) => Some(i.to_string()),
            _ => None,
        };
        if let Some(ParamValue::Text(f)) = st.get("function") {
            self.function = f.clone();
        }
        for (k, slot) in [
            ("frequency", &mut self.frequency),
            ("amplitude", &mut self.amplitude),
            ("offset", &mut self.offset),
            ("phase", &mut self.phase),
            ("duty", &mut self.duty),
            ("symmetry", &mut self.symmetry),
        ] {
            if let Some(v) = real(k) {
                *slot = v;
            }
        }
        if let Some(ParamValue::Text(l)) = st.get("load") {
            self.fifty_ohm = l.starts_with("FIFT");
        }
    }
}

#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
#[serde(default)]
pub struct GeneratorPanel {
    channel: u8,
    forms: [GenForm; 2],
}

impl Default for GeneratorPanel {
    fn default() -> Self {
        GeneratorPanel {
            channel: 1,
            forms: Default::default(),
        }
    }
}

impl GeneratorPanel {
    /// `max_sine`: generator present (its highest sine frequency), `None` if not usable.
    /// `outputs`: output state per channel as last read or set (`None` = unknown).
    /// `state`: last read-back of the selected channel to take over into the form.
    pub fn ui(
        &mut self,
        ui: &mut egui::Ui,
        client: &Client,
        max_sine: Option<f64>,
        outputs: [Option<bool>; 2],
        readable: bool,
    ) {
        let Some(max_sine) = max_sine else {
            ui.label(
                RichText::new(
                    "no usable function generator (AFG option not installed, or not yet \
                     confirmed: run `mho954 verify afg`)",
                )
                .small()
                .color(Color32::YELLOW),
            );
            return;
        };
        ui.horizontal(|ui| {
            for (n, name) in [(1u8, "GI"), (2, "GII")] {
                let on = outputs[(n - 1) as usize] == Some(true);
                let text = RichText::new(name).strong();
                let text = if on {
                    text.color(Color32::LIGHT_RED)
                } else {
                    text
                };
                ui.selectable_value(&mut self.channel, n, text);
            }
            ui.label(
                RichText::new(format!("sine <= {}", mho_dsp::format_si(max_sine, "Hz")))
                    .small()
                    .color(Color32::GRAY),
            );
        });
        let n = self.channel;
        let form = &mut self.forms[(n - 1) as usize];
        let f = form.function.clone();
        egui::Grid::new(("afg_form", n))
            .num_columns(2)
            .show(ui, |ui| {
                ui.label("waveform");
                let label = FUNCTIONS
                    .iter()
                    .find(|(t, _)| *t == f)
                    .map_or(f.as_str(), |(_, l)| l);
                egui::ComboBox::from_id_salt(("afg_func", n))
                    .selected_text(label)
                    .width(100.0)
                    .show_ui(ui, |ui| {
                        for (t, l) in FUNCTIONS {
                            ui.selectable_value(&mut form.function, t.to_string(), *l);
                        }
                    });
                ui.end_row();
                let field = |ui: &mut egui::Ui, name: &str, v: &mut String, unit: &str| {
                    ui.label(name);
                    ui.horizontal(|ui| {
                        ui.add(egui::TextEdit::singleline(v).desired_width(70.0));
                        ui.label(unit);
                    });
                    ui.end_row();
                };
                if !matches!(f.as_str(), "DC" | "NOIS") {
                    field(ui, "frequency", &mut form.frequency, "Hz");
                }
                if f != "DC" {
                    field(ui, "amplitude", &mut form.amplitude, "Vpp");
                }
                field(ui, "offset", &mut form.offset, "V");
                if !matches!(f.as_str(), "DC" | "NOIS") {
                    field(ui, "phase", &mut form.phase, "°");
                }
                if f == "SQU" {
                    field(ui, "duty", &mut form.duty, "%");
                }
                if f == "RAMP" {
                    field(ui, "symmetry", &mut form.symmetry, "%");
                }
                ui.label("load");
                ui.horizontal(|ui| {
                    ui.selectable_value(&mut form.fifty_ohm, false, "high-Z");
                    ui.selectable_value(&mut form.fifty_ohm, true, "50 Ω");
                });
                ui.end_row();
                ui.label("modulation");
                ui.horizontal(|ui| {
                    for m in ["OFF", "AM", "FM", "PM"] {
                        ui.selectable_value(&mut form.modulation, m.to_string(), m);
                    }
                });
                ui.end_row();
                match form.modulation.as_str() {
                    "AM" => field(ui, "depth", &mut form.mod_depth, "%"),
                    "FM" => field(ui, "deviation", &mut form.mod_deviation, "Hz"),
                    "PM" => field(ui, "deviation", &mut form.mod_deviation, "°"),
                    _ => {}
                }
                if form.modulation != "OFF" {
                    field(ui, "rate", &mut form.mod_frequency, "Hz");
                    ui.label("shape");
                    egui::ComboBox::from_id_salt(("afg_modwave", n))
                        .selected_text(&form.mod_wave)
                        .width(70.0)
                        .show_ui(ui, |ui| {
                            for w in MOD_WAVES {
                                ui.selectable_value(&mut form.mod_wave, w.to_string(), *w);
                            }
                        });
                    ui.end_row();
                }
            });
        if f == "ARB" {
            ui.horizontal(|ui| {
                ui.add(egui::TextEdit::singleline(&mut form.arb_path).desired_width(120.0))
                    .on_hover_text("file on the scope (C:/) or its USB stick (D:/)");
                if ui.button("Load file").clicked() {
                    let req = CommandRequest {
                        path: Some(form.arb_path.clone()),
                        confirm: false,
                    };
                    let id = format!("afg{n}.load_arb");
                    client.op(
                        "load arbitrary waveform",
                        move |api| async move { api.run_command(id, req).await },
                        |sh: &mut Shared, ()| sh.log("arbitrary waveform loaded", false),
                    );
                }
            });
        }
        ui.horizontal(|ui| {
            if ui
                .button("Apply")
                .on_hover_text("output state unchanged")
                .clicked()
            {
                match form.update() {
                    Ok(u) => apply(client, n, u, "generator"),
                    Err(e) => client
                        .lock()
                        .log(format!("GI{}: {e}", if n == 2 { "I" } else { "" }), true),
                }
            }
            let on = outputs[(n - 1) as usize] == Some(true);
            let (text, color) = if on {
                ("Output ON", Color32::from_rgb(200, 40, 40))
            } else {
                ("Output OFF", Color32::from_gray(70))
            };
            let b =
                egui::Button::new(RichText::new(text).strong().color(Color32::WHITE)).fill(color);
            if ui
                .add(b)
                .on_hover_text(
                    "switches the generator output; the signal reaches whatever is connected",
                )
                .clicked()
            {
                let u = GeneratorUpdate {
                    output: Some(!on),
                    ..Default::default()
                };
                apply(client, n, u, "output");
            }
            if ui
                .button("Align phase")
                .on_hover_text("restart GI and GII with their phases")
                .clicked()
            {
                client.op(
                    "align phase",
                    |api| async move {
                        api.run_command("afg.align_phase".into(), CommandRequest::default())
                            .await
                    },
                    |sh: &mut Shared, ()| sh.log("generator phases aligned", false),
                );
            }
            if readable && ui.button("Read").clicked() {
                client.op(
                    "generator",
                    move |api| async move { api.group_get("afg".into(), Some(n)).await },
                    move |sh: &mut Shared, st: ParamMap| sh.set_generator_state(n, st),
                );
            }
        });
        if let Some(st) = client.lock().take_generator_readback(n) {
            form.load(&st);
        }
    }
}

/// Sends `u` to channel `n` and records the output state (from the read-back if the
/// group is verified, else from what was sent).
fn apply(client: &Client, n: u8, u: GeneratorUpdate, label: &'static str) {
    let sent_output = u.output;
    client.op(
        label,
        move |api| async move { api.set_generator(n, u).await },
        move |sh: &mut Shared, st: Option<ParamMap>| match st {
            Some(st) => sh.set_generator_state(n, st),
            None => {
                if let Some(on) = sent_output {
                    sh.generator_output[(n - 1) as usize] = Some(on);
                }
                sh.log("generator settings applied", false);
            }
        },
    );
}
