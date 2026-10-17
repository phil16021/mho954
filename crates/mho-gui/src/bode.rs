//! Software Bode sweep with the built-in function generator: form and gain/phase plot.

use eframe::egui::{self, Color32, RichText};
use egui_plot::{Legend, Line, Plot, PlotPoints};
use mho_driver::{BodePoint, BodeRequest};
use mho_dsp::format_si;
use mho_service::ScopeApi;

use crate::app::parse_si;
use crate::state::{Client, Shared};

#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
#[serde(default)]
pub struct BodePanel {
    start: String,
    stop: String,
    points: usize,
    amplitude: String,
    input: u8,
    output: u8,
    generator: u8,
}

impl Default for BodePanel {
    fn default() -> Self {
        BodePanel {
            start: "100".into(),
            stop: "1M".into(),
            points: 31,
            amplitude: "1".into(),
            input: 1,
            output: 2,
            generator: 1,
        }
    }
}

impl BodePanel {
    pub fn ui(&mut self, ui: &mut egui::Ui, client: &Client, afg: Option<bool>, running: bool) {
        if afg == Some(false) {
            ui.label(
                RichText::new("this scope has no function generator (AFG option)")
                    .small()
                    .color(Color32::YELLOW),
            );
        }
        egui::Grid::new("bode_form").num_columns(2).show(ui, |ui| {
            ui.label("start / stop");
            ui.horizontal(|ui| {
                ui.add(egui::TextEdit::singleline(&mut self.start).desired_width(60.0));
                ui.add(egui::TextEdit::singleline(&mut self.stop).desired_width(60.0));
                ui.label("Hz");
            });
            ui.end_row();
            ui.label("points");
            ui.add(egui::Slider::new(&mut self.points, 2..=200));
            ui.end_row();
            ui.label("amplitude");
            ui.horizontal(|ui| {
                ui.add(egui::TextEdit::singleline(&mut self.amplitude).desired_width(60.0));
                ui.label("Vpp");
            });
            ui.end_row();
            ui.label("generator");
            ui.horizontal(|ui| {
                ui.selectable_value(&mut self.generator, 1, "GI");
                ui.selectable_value(&mut self.generator, 2, "GII");
            });
            ui.end_row();
            ui.label("DUT in / out");
            ui.horizontal(|ui| {
                for (v, salt) in [(&mut self.input, "bode_in"), (&mut self.output, "bode_out")] {
                    egui::ComboBox::from_id_salt(salt)
                        .selected_text(format!("CH{v}"))
                        .width(55.0)
                        .show_ui(ui, |ui| {
                            for n in 1..=4u8 {
                                ui.selectable_value(v, n, format!("CH{n}"));
                            }
                        });
                }
            });
            ui.end_row();
        });
        let run = ui
            .add_enabled(!running && afg != Some(false), egui::Button::new("Sweep"))
            .on_hover_text("switches the generator output on during the sweep");
        if run.clicked() {
            let parse = |s: &str| parse_si(s).filter(|v| v.is_finite());
            let (Some(start), Some(stop), Some(amplitude)) = (
                parse(&self.start),
                parse(&self.stop),
                parse(&self.amplitude),
            ) else {
                client
                    .lock()
                    .log("Bode: start, stop and amplitude must be numbers", true);
                return;
            };
            let req = BodeRequest {
                generator: self.generator,
                input: self.input,
                output: self.output,
                start,
                stop,
                points: self.points,
                amplitude,
                ..Default::default()
            };
            if let Err(e) = req.validate() {
                client.lock().log(format!("Bode: {e}"), true);
                return;
            }
            client.lock().bode_running = true;
            client.op(
                "Bode sweep",
                move |api| async move { api.bode_sweep(req).await },
                |sh: &mut Shared, pts: Vec<BodePoint>| {
                    sh.bode_running = false;
                    sh.log(format!("Bode: {} points", pts.len()), false);
                    sh.bode = Some(pts);
                },
            );
        }
        if running {
            ui.label("sweeping… (the scope is busy)");
        }
    }
}

/// Gain and phase over log frequency.
pub fn bode_plot(ui: &mut egui::Ui, pts: &[BodePoint]) {
    let lg = |p: &BodePoint| p.frequency.log10();
    let fmt = |m: egui_plot::GridMark, _: &std::ops::RangeInclusive<f64>| {
        format_si(10f64.powf(m.value), "Hz")
    };
    let h = (ui.available_height() / 2.0 - 8.0).max(80.0);
    Plot::new("bode_gain")
        .height(h)
        .legend(Legend::default())
        .x_axis_formatter(fmt)
        .y_axis_formatter(|m, _| format!("{:.0} dB", m.value))
        .show(ui, |pui| {
            let line: PlotPoints = pts.iter().map(|p| [lg(p), p.gain_db]).collect();
            pui.line(Line::new("gain", line).color(Color32::from_rgb(0, 220, 255)));
        });
    Plot::new("bode_phase")
        .height(h)
        .legend(Legend::default())
        .x_axis_formatter(fmt)
        .y_axis_formatter(|m, _| format!("{:.0}°", m.value))
        .show(ui, |pui| {
            let line: PlotPoints = pts.iter().map(|p| [lg(p), p.phase_deg]).collect();
            pui.line(Line::new("phase", line).color(Color32::from_rgb(255, 150, 40)));
        });
}
