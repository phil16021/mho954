//! Bode measurement with the built-in function generator: software sweep (data, plot,
//! CSV) or the scope's own Bode plot (screenshot only), and the gain/phase plot.

use eframe::egui::{self, Color32, RichText};
use egui_plot::{Legend, Line, Plot, PlotPoints};
use mho_driver::{BodePoint, BodeRequest, NativeBodeRequest};
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
    fifty_ohm: bool,
    averages: u32,
    /// Drive the other generator in step (path comparison without a T piece).
    follow: bool,
    /// Run the scope's own Bode plot (result as screenshot) instead of the software sweep.
    native: bool,
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
            fifty_ohm: false,
            averages: 1,
            follow: false,
            native: false,
        }
    }
}

impl BodePanel {
    /// `max_sine`: highest generator frequency, `None` without a usable generator (or not
    /// known yet). Screenshots of the scope's Bode plot go to `capture_dir`.
    pub fn ui(
        &mut self,
        ui: &mut egui::Ui,
        client: &Client,
        max_sine: Option<Option<f64>>,
        running: bool,
        capture_dir: &std::path::Path,
    ) {
        let afg = max_sine.map(|m| m.is_some());
        if afg == Some(false) {
            ui.label(
                RichText::new("no usable function generator (AFG option; see `mho954 caps`)")
                    .small()
                    .color(Color32::YELLOW),
            );
        }
        ui.horizontal(|ui| {
            ui.selectable_value(&mut self.native, false, "software sweep")
                .on_hover_text("steps the generator, measures both channels: data, CSV, plot");
            ui.selectable_value(&mut self.native, true, "on the scope")
                .on_hover_text("the scope's own Bode plot; result as screenshot only");
        });
        egui::Grid::new("bode_form").num_columns(2).show(ui, |ui| {
            ui.label("start / stop");
            ui.horizontal(|ui| {
                ui.add(egui::TextEdit::singleline(&mut self.start).desired_width(60.0));
                ui.add(egui::TextEdit::singleline(&mut self.stop).desired_width(60.0));
                ui.label("Hz");
            });
            ui.end_row();
            if self.native {
                ui.label("points/decade");
                ui.add(egui::Slider::new(&mut self.points, 10..=100));
            } else {
                ui.label("points");
                ui.add(egui::Slider::new(&mut self.points, 2..=200));
            }
            ui.end_row();
            ui.label("amplitude");
            ui.horizontal(|ui| {
                ui.add(egui::TextEdit::singleline(&mut self.amplitude).desired_width(60.0));
                ui.label("Vpp");
            });
            ui.end_row();
            if !self.native {
                ui.label("generator");
                ui.horizontal(|ui| {
                    ui.selectable_value(&mut self.generator, 1, "GI");
                    ui.selectable_value(&mut self.generator, 2, "GII");
                });
                ui.end_row();
                ui.label("load");
                ui.horizontal(|ui| {
                    ui.selectable_value(&mut self.fifty_ohm, false, "high-Z");
                    ui.selectable_value(&mut self.fifty_ohm, true, "50 Ω");
                });
                ui.end_row();
                ui.label("averages");
                ui.add(egui::Slider::new(&mut self.averages, 1..=16));
                ui.end_row();
                ui.label("");
                ui.checkbox(&mut self.follow, "other generator in step")
                    .on_hover_text(
                        "no T piece: GI→CH1 and GII→CH2 run the same sine, CH2/CH1 compares the paths",
                    );
                ui.end_row();
            }
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
        if run.clicked() && self.native {
            self.run_native(client, capture_dir);
        } else if run.clicked() {
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
                averages: self.averages,
                follow: self.follow.then_some(3 - self.generator),
                load: Some(if self.fifty_ohm { "50" } else { "highz" }.into()),
                ..Default::default()
            };
            if let Some(Some(max)) = max_sine
                && stop > max
            {
                client.lock().log(
                    format!(
                        "Bode: stop above the generator limit of {}",
                        format_si(max, "Hz")
                    ),
                    true,
                );
                return;
            }
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
                    sh.set_bode(pts);
                },
            );
        }
        if running {
            ui.label("sweeping… (the scope is busy)");
        }
    }

    fn run_native(&self, client: &Client, capture_dir: &std::path::Path) {
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
        let req = NativeBodeRequest {
            input: self.input,
            output: self.output,
            start,
            stop,
            points_per_decade: self.points.clamp(10, 100) as u32,
            amplitude,
            ..Default::default()
        };
        if let Err(e) = req.validate() {
            client.lock().log(format!("Bode: {e}"), true);
            return;
        }
        let stamp = file_stamp();
        let path = capture_dir.join(format!("bode-{stamp}.png"));
        client.lock().bode_running = true;
        client.op(
            "Bode plot (scope)",
            move |api| async move { api.bode_native(req).await },
            move |sh: &mut Shared, png: Vec<u8>| {
                sh.bode_running = false;
                let r = path
                    .parent()
                    .map_or(Ok(()), std::fs::create_dir_all)
                    .and_then(|()| std::fs::write(&path, png));
                match r {
                    Ok(()) => sh.log(format!("Bode plot saved: {}", path.display()), false),
                    Err(e) => sh.log(format!("Bode plot: {e}"), true),
                }
            },
        );
    }
}

/// Local time stamp for file names (seconds since the epoch if the clock is odd).
fn file_stamp() -> String {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs().to_string())
        .unwrap_or_default()
}

/// Summary line under the plot (−3 dB, 0 dB crossover, margins).
fn summary_text(pts: &[BodePoint]) -> String {
    let (f, g, p) = columns(pts);
    let s = mho_dsp::bode_summary(&f, &g, &p);
    let hz = |v: Option<f64>| v.map_or("-".into(), |v| format_si(v, "Hz"));
    let mut t = format!(
        "-3 dB: {}   0 dB: {}",
        hz(s.corner_3db_hz),
        hz(s.unity_gain_hz)
    );
    if let Some(m) = s.phase_margin_deg {
        t.push_str(&format!("   phase margin {m:.1}°"));
    }
    if let (Some(fx), Some(m)) = (s.phase_crossover_hz, s.gain_margin_db) {
        t.push_str(&format!(
            "   -180° at {} (gain margin {m:.1} dB)",
            format_si(fx, "Hz")
        ));
    }
    t
}

fn columns(pts: &[BodePoint]) -> (Vec<f64>, Vec<f64>, Vec<f64>) {
    (
        pts.iter().map(|p| p.frequency).collect(),
        pts.iter().map(|p| p.gain_db).collect(),
        pts.iter().map(|p| p.phase_deg).collect(),
    )
}

/// Bode CSV (with summary lines) of the sweep, written to `capture_dir`.
pub fn export_csv(
    pts: &[BodePoint],
    capture_dir: &std::path::Path,
) -> std::io::Result<std::path::PathBuf> {
    let (f, g, p) = columns(pts);
    std::fs::create_dir_all(capture_dir)?;
    let path = capture_dir.join(format!("bode-{}.csv", file_stamp()));
    std::fs::write(&path, mho_dsp::bode_csv(&f, &g, &p))?;
    Ok(path)
}

/// Gain and (unwrapped) phase over log frequency, the previous sweep dimmed behind.
pub fn bode_plot(
    ui: &mut egui::Ui,
    pts: &[BodePoint],
    previous: Option<&[BodePoint]>,
    client: &Client,
    capture_dir: &std::path::Path,
) {
    ui.horizontal(|ui| {
        ui.label(RichText::new(summary_text(pts)).small());
        if ui
            .button("CSV")
            .on_hover_text("export to the capture folder")
            .clicked()
        {
            match export_csv(pts, capture_dir) {
                Ok(p) => client
                    .lock()
                    .log(format!("Bode CSV: {}", p.display()), false),
                Err(e) => client.lock().log(format!("Bode CSV: {e}"), true),
            }
        }
    });
    let lg = |p: &BodePoint| p.frequency.log10();
    let fmt = |m: egui_plot::GridMark, _: &std::ops::RangeInclusive<f64>| {
        format_si(10f64.powf(m.value), "Hz")
    };
    let phase = |pts: &[BodePoint]| -> PlotPoints {
        let unwrapped = mho_dsp::unwrap_phase(&columns(pts).2);
        pts.iter()
            .zip(unwrapped)
            .map(|(p, ph)| [lg(p), ph])
            .collect()
    };
    let dim = Color32::from_gray(110);
    let h = (ui.available_height() / 2.0 - 8.0).max(80.0);
    Plot::new("bode_gain")
        .height(h)
        .legend(Legend::default())
        .x_axis_formatter(fmt)
        .y_axis_formatter(|m, _| format!("{:.0} dB", m.value))
        .show(ui, |pui| {
            if let Some(prev) = previous {
                let line: PlotPoints = prev.iter().map(|p| [lg(p), p.gain_db]).collect();
                pui.line(Line::new("previous", line).color(dim));
            }
            let line: PlotPoints = pts.iter().map(|p| [lg(p), p.gain_db]).collect();
            pui.line(Line::new("gain", line).color(Color32::from_rgb(0, 220, 255)));
        });
    Plot::new("bode_phase")
        .height(h)
        .legend(Legend::default())
        .x_axis_formatter(fmt)
        .y_axis_formatter(|m, _| format!("{:.0}°", m.value))
        .show(ui, |pui| {
            if let Some(prev) = previous {
                pui.line(Line::new("previous", phase(prev)).color(dim));
            }
            pui.line(Line::new("phase", phase(pts)).color(Color32::from_rgb(255, 150, 40)));
        });
}
