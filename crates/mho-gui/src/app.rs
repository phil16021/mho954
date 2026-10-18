use std::path::PathBuf;
use std::sync::Arc;

use eframe::egui::{self, Color32, RichText};
use egui_plot::{GridMark, HLine, Legend, Line, LineStyle, Plot, PlotBounds, PlotPoints, VLine};
use mho_driver::params::{GroupInfo, ParamMap};
use mho_driver::*;
use mho_dsp::DecodeTable;
use mho_dsp::{Measurements, Window, format_si};
use mho_service::{Frame, LiveConfig, MeasureSpec, ScopeApi, StatsRequest};

use crate::bode::{BodePanel, bode_plot};
use crate::generator::GeneratorPanel;
use crate::plot_tools::{self, Handle, PlotTools, ViewAction, gated_volts, zoom_range};
use crate::prefs::{self, Prefs};
use crate::settings::SettingsPanel;
use crate::state::{Client, Shared};
use crate::tools::ToolsPanel;

/// Rigol channel colours.
pub const CH_COLORS: [Color32; 4] = [
    Color32::from_rgb(255, 222, 0),
    Color32::from_rgb(0, 220, 255),
    Color32::from_rgb(255, 64, 200),
    Color32::from_rgb(70, 130, 255),
];

const DEPTHS: &[&str] = &[
    "AUTO", "1k", "10k", "100k", "1M", "10M", "25M", "50M", "100M", "125M", "200M", "250M", "500M",
];
const H_DIVS: f64 = 10.0;
const V_DIVS: f64 = 8.0;

/// Decoder rows shown in the GUI table / labels drawn on the plot.
const MAX_DECODE_ROWS: usize = 2000;
const MAX_DECODE_MARKS: usize = 300;
const MATH_SCALE_REFRESH: std::time::Duration = std::time::Duration::from_secs(10);

fn channel_color(source: &str) -> Color32 {
    // "CHAN1 [A-B]" (gated measurement rows) has the colour of CHAN1.
    let source = source.split(' ').next().unwrap_or(source);
    if let Some(n) = source
        .strip_prefix("MATH")
        .and_then(|d| d.parse::<usize>().ok())
    {
        return MATH_COLORS[(n.clamp(1, 4)) - 1];
    }
    channel_of(source)
        .map(|n| CH_COLORS[(n - 1) as usize])
        .unwrap_or(Color32::LIGHT_GRAY)
}

const MATH_COLORS: [Color32; 4] = [
    Color32::from_rgb(255, 150, 40),
    Color32::from_rgb(140, 230, 110),
    Color32::from_rgb(230, 230, 230),
    Color32::from_rgb(190, 130, 255),
];

/// Next value in a 1-2-5 sequence.
fn step_125(v: f64, up: bool) -> f64 {
    if v.is_nan() || v <= 0.0 {
        return 1.0;
    }
    let e = v.log10().floor();
    let base = 10f64.powf(e);
    let m = v / base;
    let seq = [1.0, 2.0, 5.0, 10.0];
    let idx = seq.iter().position(|&s| m <= s * 1.001).unwrap_or(3);
    let snapped = seq[idx];
    let new = if up {
        if (m - snapped).abs() < 1e-3 * snapped {
            seq.get(idx + 1).copied().unwrap_or(20.0)
        } else {
            snapped
        }
    } else if (m - snapped).abs() < 1e-3 * snapped {
        if idx == 0 { 0.5 } else { seq[idx - 1] }
    } else if idx == 0 {
        0.5
    } else {
        seq[idx - 1]
    };
    new * base
}

#[derive(PartialEq, Clone, Copy)]
enum YMode {
    Divisions,
    Volts,
}

pub struct ScopeApp {
    client: Client,
    capture_dir: PathBuf,
    y_mode: YMode,
    show_fft: bool,
    show_decoder: bool,
    /// Last request of each math channel's scale (re-read periodically: it may change on
    /// the scope).
    math_requested: std::collections::HashMap<u8, std::time::Instant>,
    peaks_math: u8,
    decoder_bus: u8,
    fft_source: String,
    fft_window: Window,
    /// Cursors, gated regions and pending view changes of the scope plot.
    view: PlotTools,
    /// Screen/plot mapping of the last frame (to find cursors under the pointer).
    plot_transform: Option<egui_plot::PlotTransform>,
    fft_log_x: bool,
    fft_auto_y: bool,
    /// Offsets being edited (applied on release), per channel.
    offset_edit: [Option<f64>; 4],
    tb_offset_edit: Option<f64>,
    trig_level_edit: Option<f64>,
    new_meas_item: String,
    new_meas_source: String,
    raw_source: String,
    last_x_range: Option<(f64, f64)>,
    net_form: NetForm,
    settings: SettingsPanel,
    tools: ToolsPanel,
    bode: BodePanel,
    show_bode: bool,
    generator: GeneratorPanel,
    /// Preferences as last written, and when.
    prefs_saved: Prefs,
    prefs_saved_at: std::time::Instant,
    caps_requested: bool,
    options_requested: bool,
    setup_path: String,
    groups_requested: bool,
    /// Forces the settings section open on the next frame.
    settings_open: Option<bool>,
}

/// Editable copy of the LAN settings.
#[derive(Default)]
struct NetForm {
    loaded: bool,
    mode: Option<IpMode>,
    ip: String,
    mask: String,
    gateway: String,
    dns: String,
    hostname: String,
    mdns: bool,
}

impl ScopeApp {
    pub fn new(
        cc: &eframe::CreationContext<'_>,
        url: String,
        token: Option<String>,
        rt: tokio::runtime::Handle,
        capture_dir: PathBuf,
    ) -> Self {
        cc.egui_ctx.set_visuals(egui::Visuals::dark());
        let p = prefs::load();
        ScopeApp {
            prefs_saved: p.clone(),
            prefs_saved_at: std::time::Instant::now(),
            client: Client::new(&url, token, rt, cc.egui_ctx.clone()),
            capture_dir,
            y_mode: if p.volts_axis {
                YMode::Volts
            } else {
                YMode::Divisions
            },
            show_fft: p.show_fft,
            show_decoder: p.show_decoder,
            math_requested: Default::default(),
            peaks_math: p.peaks_math.clamp(1, 4),
            decoder_bus: p.decoder_bus.clamp(1, 4),
            fft_source: p.fft_source,
            fft_window: p.fft_window,
            view: PlotTools::default(),
            plot_transform: None,
            fft_log_x: false,
            fft_auto_y: false,
            offset_edit: [None; 4],
            tb_offset_edit: None,
            trig_level_edit: None,
            new_meas_item: p.meas_item,
            new_meas_source: p.meas_source,
            raw_source: p.raw_source,
            last_x_range: None,
            net_form: NetForm::default(),
            settings: SettingsPanel::default(),
            tools: ToolsPanel::default(),
            bode: p.bode,
            show_bode: false,
            generator: p.generator,
            caps_requested: false,
            options_requested: false,
            setup_path: String::new(),
            groups_requested: false,
            settings_open: None,
        }
    }

    fn snapshot(&self) -> Snapshot {
        let sh = self.client.lock();
        Snapshot {
            frame: sh.frame.clone(),
            status: sh.status.clone(),
            info: sh.info.clone(),
            live: sh.live.clone(),
            network: sh.network.clone(),
            groups: sh.groups.clone(),
            group_values: sh.group_values.clone(),
            stats: sh.stats.clone(),
            math_scale: sh.math_scale.clone(),
            scope_peaks: sh.scope_peaks.clone(),
            readouts: sh.readouts.clone(),
            bode: sh.bode.clone(),
            bode_previous: sh.bode_previous.clone(),
            bode_running: sh.bode_running,
            bode_live: sh.bode_live.clone(),
            capabilities: sh.capabilities.clone(),
            generator_output: sh.generator_output,
            options: sh.options.clone(),
            commands: sh.commands.clone(),
            readout_values: sh.readout_values.clone(),
            decode: sh.decode.clone(),
            measurements: sh.measurements.clone(),
            live_error: sh.live_error.clone(),
            ws_connected: sh.ws_connected,
            progress: sh.progress.clone(),
            busy: sh.busy,
            log: sh.log.iter().cloned().collect(),
        }
    }

    // ---- actions --------------------------------------------------------------------------------

    fn set_channel(&self, n: u8, update: ChannelUpdate) {
        self.client.op(
            format!("CH{n}"),
            move |api| async move { api.set_channel(n, update).await },
            move |sh: &mut Shared, c: ChannelState| {
                if let Some(st) = sh.status.as_mut()
                    && let Some(slot) = st.channels.get_mut((n - 1) as usize)
                {
                    *slot = c;
                }
            },
        );
    }

    fn set_timebase(&self, update: TimebaseUpdate) {
        self.client.op(
            "timebase",
            move |api| async move { api.set_timebase(update).await },
            |sh: &mut Shared, t: TimebaseState| {
                if let Some(st) = sh.status.as_mut() {
                    st.timebase = t;
                }
            },
        );
    }

    fn set_trigger(&self, update: TriggerUpdate) {
        self.client.op(
            "trigger",
            move |api| async move { api.set_trigger(update).await },
            |sh: &mut Shared, t: TriggerState| {
                if let Some(st) = sh.status.as_mut() {
                    st.trigger = t;
                }
            },
        );
    }

    fn set_acquire(&self, update: AcquireUpdate) {
        self.client.op(
            "acquire",
            move |api| async move { api.set_acquire(update).await },
            |sh: &mut Shared, a: AcquireState| {
                if let Some(st) = sh.status.as_mut() {
                    st.acquire = a;
                }
            },
        );
    }

    fn action(&self, a: Action) {
        self.client.op(
            format!("{a:?}"),
            move |api| async move { api.action(a).await },
            move |sh: &mut Shared, ()| {
                if a == Action::Autoscale {
                    sh.status_dirty = true;
                }
            },
        );
    }

    fn set_live(&self, cfg: LiveConfig) {
        self.client.op(
            "live config",
            move |api| async move { api.set_live_config(cfg).await },
            |sh: &mut Shared, c: LiveConfig| sh.live = Some(c),
        );
    }

    fn screenshot(&self) {
        let dir = self.capture_dir.clone();
        self.client.op(
            "screenshot",
            move |api| async move {
                let png = api.screenshot().await?;
                let path = dir.join(format!(
                    "mho954-{}.png",
                    chrono::Local::now().format("%Y%m%d-%H%M%S")
                ));
                std::fs::create_dir_all(&dir)
                    .and_then(|_| std::fs::write(&path, png))
                    .map_err(|e| {
                        mho_service::ApiError::new(mho_service::ErrorKind::Internal, e.to_string())
                    })?;
                Ok(path)
            },
            |sh: &mut Shared, p: PathBuf| sh.log(format!("saved {}", p.display()), false),
        );
    }

    fn export_csv(&self, frame: &Frame) {
        let dir = self.capture_dir.clone();
        let path = dir.join(format!(
            "mho954-{}.csv",
            chrono::Local::now().format("%Y%m%d-%H%M%S")
        ));
        let volts: Vec<Vec<f64>> = frame.traces.iter().map(|t| t.volts()).collect();
        let cols: Vec<mho_dsp::export::CsvColumn<'_>> = frame
            .traces
            .iter()
            .zip(&volts)
            .map(|(t, v)| mho_dsp::export::CsvColumn {
                name: &t.source,
                volts: v,
            })
            .collect();
        let Some(first) = frame.traces.first() else {
            return;
        };
        let r = std::fs::create_dir_all(&dir).and_then(|_| {
            let f = std::io::BufWriter::new(std::fs::File::create(&path)?);
            mho_dsp::export::write_csv(f, first.preamble.time(0), first.preamble.x_increment, &cols)
        });
        let mut sh = self.client.lock();
        match r {
            Ok(()) => sh.log(format!("saved {}", path.display()), false),
            Err(e) => sh.log(format!("CSV export: {e}"), true),
        }
    }

    fn capture_raw(&self) {
        let dir = self.capture_dir.clone();
        let source = self.raw_source.clone();
        self.client.op(
            "RAW capture",
            move |api| async move {
                let w = api.waveform(source, WaveMode::Raw).await?;
                let stem = format!(
                    "mho954-{}-{}",
                    w.source,
                    chrono::Local::now().format("%Y%m%d-%H%M%S")
                );
                let path = dir.join(format!("{stem}.npy"));
                let io = |e: std::io::Error| {
                    mho_service::ApiError::new(mho_service::ErrorKind::Internal, e.to_string())
                };
                std::fs::create_dir_all(&dir).map_err(io)?;
                mho_dsp::export::write_npy(&path, &w.raw).map_err(io)?;
                let meta = serde_json::json!({ "source": w.source, "preamble": w.preamble });
                std::fs::write(
                    path.with_extension("json"),
                    serde_json::to_vec_pretty(&meta).unwrap(),
                )
                .map_err(io)?;
                Ok((path, w.raw.len()))
            },
            |sh: &mut Shared, (p, n): (PathBuf, usize)| {
                sh.log(format!("RAW {n} pts -> {}", p.display()), false)
            },
        );
    }

    // ---- panels ----------------------------------------------------------------------------------

    fn header(&mut self, ui: &mut egui::Ui, s: &Snapshot) {
        ui.horizontal_wrapped(|ui| {
            let model = s
                .info
                .as_ref()
                .and_then(|i| i.idn.as_ref())
                .map(|i| i.model.clone())
                .unwrap_or("MHO954".into());
            ui.heading(model);
            if let Some(c) = &s.capabilities {
                let text = c.summary();
                let rest = text.split_once(", ").map_or("", |(_, r)| r).to_string();
                ui.label(RichText::new(rest).color(Color32::GRAY))
                    .on_hover_text("effective bandwidth (1-2 channels), max. memory depth, generator, licensed protocols");
            }
            for (i, on) in s.generator_output.iter().enumerate() {
                if *on == Some(true) {
                    ui.label(
                        RichText::new(if i == 0 { "GI ON" } else { "GII ON" })
                            .strong()
                            .color(Color32::LIGHT_RED),
                    );
                }
            }
            let (txt, col) = match (&s.info, s.ws_connected) {
                (Some(i), true) if i.connected => {
                    (format!("connected: {}", i.connection), Color32::LIGHT_GREEN)
                }
                (Some(i), _) => (
                    format!("disconnected: {}", i.connection),
                    Color32::LIGHT_RED,
                ),
                (None, _) => ("server unreachable".to_string(), Color32::LIGHT_RED),
            };
            ui.label(RichText::new(txt).color(col));
            if let Some(f) = &s.frame {
                let st_col = match f.trigger_status.as_str() {
                    "STOP" => Color32::LIGHT_RED,
                    "TD" => Color32::LIGHT_GREEN,
                    "WAIT" => Color32::YELLOW,
                    _ => Color32::LIGHT_BLUE,
                };
                ui.label(RichText::new(&f.trigger_status).strong().color(st_col));
                ui.label(format!("{:.1} fps", f.fps));
            }
            if s.busy > 0 {
                ui.spinner();
            }
            if let Some(p) = &s.progress {
                ui.add(
                    egui::ProgressBar::new(p.done as f32 / p.total.max(1) as f32)
                        .desired_width(180.0)
                        .text(format!("{} {}/{}", p.label, p.done, p.total)),
                );
            }
            if let Some(e) = &s.live_error {
                ui.label(RichText::new(e).color(Color32::LIGHT_RED));
            }
        });
    }

    fn controls(&mut self, ui: &mut egui::Ui, s: &Snapshot) {
        ui.horizontal(|ui| {
            if ui.button("▶ Run").clicked() {
                self.action(Action::Run);
            }
            if ui.button("■ Stop").clicked() {
                self.action(Action::Stop);
            }
            if ui.button("Single").clicked() {
                self.action(Action::Single);
            }
            if ui.button("Auto").clicked() {
                self.action(Action::Autoscale);
            }
            if ui.button("Force").clicked() {
                self.action(Action::Force);
            }
        });
        ui.separator();

        let Some(st) = &s.status else {
            ui.label("waiting for scope status…");
            return;
        };

        for c in &st.channels {
            self.channel_controls(ui, c);
        }

        ui.separator();
        ui.label(RichText::new("Horizontal").strong());
        let tb = &st.timebase;
        ui.horizontal(|ui| {
            if ui.small_button("−").clicked() {
                self.set_timebase(TimebaseUpdate {
                    scale: Some(step_125(tb.scale, false)),
                    ..Default::default()
                });
            }
            ui.label(RichText::new(format!("{}/div", format_si(tb.scale, "s"))).monospace());
            if ui.small_button("+").clicked() {
                self.set_timebase(TimebaseUpdate {
                    scale: Some(step_125(tb.scale, true)),
                    ..Default::default()
                });
            }
        });
        ui.horizontal(|ui| {
            ui.label("offset");
            let mut v = self.tb_offset_edit.unwrap_or(tb.offset);
            let r = ui.add(
                egui::DragValue::new(&mut v)
                    .speed(tb.scale * 0.05)
                    .custom_formatter(|v, _| format_si(v, "s"))
                    .custom_parser(parse_si),
            );
            if r.changed() {
                self.tb_offset_edit = Some(v);
            }
            if (r.drag_stopped() || r.lost_focus()) && self.tb_offset_edit.is_some() {
                self.set_timebase(TimebaseUpdate {
                    offset: Some(v),
                    ..Default::default()
                });
                self.tb_offset_edit = None;
            }
            if ui.small_button("0").clicked() {
                self.set_timebase(TimebaseUpdate {
                    offset: Some(0.0),
                    ..Default::default()
                });
            }
        });

        ui.separator();
        ui.label(RichText::new("Trigger").strong());
        self.trigger_type(ui, &st.trigger.mode);
        let tr = &st.trigger;
        ui.horizontal(|ui| {
            for sweep in TriggerSweep::ALL {
                if ui
                    .selectable_label(tr.sweep == *sweep, format!("{sweep:?}"))
                    .clicked()
                {
                    self.set_trigger(TriggerUpdate {
                        sweep: Some(*sweep),
                        ..Default::default()
                    });
                }
            }
        });
        // Source, slope and level of the other types live in their settings group.
        if tr.mode.eq_ignore_ascii_case("EDGE") {
            ui.horizontal(|ui| {
                egui::ComboBox::from_id_salt("trig_src")
                    .selected_text(&tr.edge_source)
                    .width(70.0)
                    .show_ui(ui, |ui| {
                        for n in 1..=CHANNELS {
                            let src = format!("CHAN{n}");
                            if ui.selectable_label(tr.edge_source == src, &src).clicked() {
                                self.set_trigger(TriggerUpdate {
                                    edge_source: Some(src),
                                    ..Default::default()
                                });
                            }
                        }
                    });
                for (slope, label) in [
                    (Slope::Positive, "Rise"),
                    (Slope::Negative, "Fall"),
                    (Slope::Either, "Both"),
                ] {
                    if ui.selectable_label(tr.edge_slope == slope, label).clicked() {
                        self.set_trigger(TriggerUpdate {
                            edge_slope: Some(slope),
                            ..Default::default()
                        });
                    }
                }
            });
            ui.horizontal(|ui| {
                ui.label("level");
                let scale = channel_of(&tr.edge_source)
                    .and_then(|n| st.channels.get((n - 1) as usize))
                    .map(|c| c.scale)
                    .unwrap_or(0.1);
                let mut v = self.trig_level_edit.unwrap_or(tr.edge_level);
                let r = ui.add(
                    egui::DragValue::new(&mut v)
                        .speed(scale * 0.02)
                        .custom_formatter(|v, _| format_si(v, "V"))
                        .custom_parser(parse_si),
                );
                if r.changed() {
                    self.trig_level_edit = Some(v);
                }
                if (r.drag_stopped() || r.lost_focus()) && self.trig_level_edit.is_some() {
                    self.set_trigger(TriggerUpdate {
                        edge_level: Some(v),
                        ..Default::default()
                    });
                    self.trig_level_edit = None;
                }
            });
        }

        ui.separator();
        ui.label(RichText::new("Acquisition").strong());
        let acq = &st.acquire;
        ui.label(format!(
            "{}  ·  {} pts",
            format_si(acq.sample_rate, "Sa/s"),
            acq.memory_depth
                .map(|d| format_si(d, ""))
                .unwrap_or_else(|| "?".into())
        ));
        let channels_on = st.channels.iter().filter(|c| c.display).count();
        let depth_limit = s
            .capabilities
            .as_ref()
            .map(|c| c.memory_limit(channels_on) as f64);
        ui.horizontal(|ui| {
            egui::ComboBox::from_id_salt("mdepth")
                .selected_text("depth")
                .width(70.0)
                .show_ui(ui, |ui| {
                    let allowed = DEPTHS
                        .iter()
                        .filter(|d| match (depth_parse(d), depth_limit) {
                            (Some(v), Some(max)) => v <= max,
                            _ => true,
                        });
                    for d in allowed {
                        if ui.selectable_label(false, *d).clicked() {
                            self.set_acquire(AcquireUpdate {
                                memory_depth: Some((*d).to_string()),
                                ..Default::default()
                            });
                        }
                    }
                });
            egui::ComboBox::from_id_salt("acqtype")
                .selected_text(format!("{:?}", acq.acq_type))
                .width(80.0)
                .show_ui(ui, |ui| {
                    for t in AcquireType::ALL {
                        if ui
                            .selectable_label(acq.acq_type == *t, format!("{t:?}"))
                            .clicked()
                        {
                            self.set_acquire(AcquireUpdate {
                                acq_type: Some(*t),
                                ..Default::default()
                            });
                        }
                    }
                });
        });

        ui.separator();
        ui.label(RichText::new("Capture").strong());
        ui.horizontal(|ui| {
            if ui.button("Screenshot").clicked() {
                self.screenshot();
            }
            if let Some(f) = &s.frame
                && ui
                    .button("CSV")
                    .on_hover_text("Export the displayed waveforms")
                    .clicked()
            {
                self.export_csv(f);
            }
        });
        ui.horizontal(|ui| {
            egui::ComboBox::from_id_salt("raw_src")
                .selected_text(&self.raw_source)
                .width(70.0)
                .show_ui(ui, |ui| {
                    for n in 1..=CHANNELS {
                        let src = format!("CHAN{n}");
                        ui.selectable_value(&mut self.raw_source, src.clone(), &src);
                    }
                });
            if ui
                .button("RAW to .npy")
                .on_hover_text("Full memory of the channel; stop the scope first")
                .clicked()
            {
                self.capture_raw();
            }
        });

        if let Some(live) = &s.live {
            ui.separator();
            ui.label(RichText::new("Live").strong());
            let mut fps = live.max_fps;
            let r = ui.add(egui::Slider::new(&mut fps, 1.0..=60.0).text("max fps"));
            if r.drag_stopped() || (r.changed() && !r.dragged()) {
                self.set_live(LiveConfig {
                    max_fps: fps,
                    ..live.clone()
                });
            }
        }

        ui.separator();
        let settings = egui::CollapsingHeader::new(RichText::new("Advanced settings").strong())
            .id_salt("advanced_settings")
            .open(self.settings_open.take())
            .show(ui, |ui| {
                self.settings.ui(
                    ui,
                    &self.client,
                    s.groups.as_deref(),
                    s.group_values.as_ref(),
                );
            });
        if settings.body_returned.is_some() {
            self.load_groups();
        }

        ui.separator();
        let tools = egui::CollapsingHeader::new(RichText::new("Readouts & commands").strong())
            .id_salt("scope_tools")
            .show(ui, |ui| {
                self.tools.ui(
                    ui,
                    &self.client,
                    s.readouts.as_deref(),
                    s.commands.as_deref(),
                    s.readout_values.as_ref(),
                );
            });
        if tools.body_returned.is_some() {
            self.tools.load(&self.client);
        }

        ui.separator();
        let max_sine = s
            .capabilities
            .as_ref()
            .map(|c| c.generator.as_ref().map(|g| g.max_sine_hz));
        let readable = s
            .groups
            .as_ref()
            .and_then(|g| g.iter().find(|g| g.id == "afg"))
            .is_some_and(|g| g.verified);
        let gen_open =
            egui::CollapsingHeader::new(RichText::new("Function generator (AFG)").strong())
                .id_salt("function_generator")
                .show(ui, |ui| match max_sine {
                    None => {
                        ui.spinner();
                    }
                    Some(m) => self
                        .generator
                        .ui(ui, &self.client, m, s.generator_output, readable),
                });
        if gen_open.body_returned.is_some() {
            self.load_groups();
        }

        ui.separator();
        egui::CollapsingHeader::new(RichText::new("Bode plot (AFG)").strong())
            .id_salt("bode_sweep")
            .show(ui, |ui| {
                self.bode.ui(
                    ui,
                    &self.client,
                    max_sine,
                    s.bode_running,
                    &self.capture_dir,
                );
                if s.bode.is_some() {
                    ui.checkbox(&mut self.show_bode, "show plot");
                }
            });

        ui.separator();
        egui::CollapsingHeader::new(RichText::new("Network (RJ45)").strong())
            .default_open(false)
            .show(ui, |ui| {
                self.network_controls(ui, s);
            });

        ui.separator();
        let system = egui::CollapsingHeader::new(RichText::new("System").strong())
            .id_salt("system")
            .show(ui, |ui| self.system_controls(ui, s));
        if system.body_returned.is_some() && !self.options_requested {
            self.options_requested = true;
            self.client.op(
                "options",
                |api| async move { api.options().await },
                |sh: &mut Shared, o: Vec<OptionInfo>| sh.options = Some(o),
            );
        }

        ui.separator();
        for (msg, err) in &s.log {
            ui.label(RichText::new(msg).small().color(if *err {
                Color32::LIGHT_RED
            } else {
                Color32::GRAY
            }));
        }
    }

    /// Loads the capabilities once the scope is connected (again after a reconnect: a
    /// different scope may be attached).
    fn load_capabilities(&mut self, s: &Snapshot) {
        let connected = s.info.as_ref().is_some_and(|i| i.connected);
        if !connected {
            self.caps_requested = false;
            return;
        }
        if self.caps_requested {
            return;
        }
        self.caps_requested = true;
        self.client.op(
            "capabilities",
            |api| async move {
                let caps = api.capabilities().await?;
                // Output state of both generator channels, for the header and the panel.
                let mut outputs = Vec::new();
                if caps.generator.is_some() {
                    for n in 1..=2u8 {
                        if let Ok(st) = api.group_get("afg".into(), Some(n)).await {
                            outputs.push((n, st));
                        }
                    }
                }
                Ok((caps, outputs))
            },
            |sh: &mut Shared, (c, outputs): (Capabilities, Vec<(u8, ParamMap)>)| {
                sh.capabilities = Some(c);
                for (n, st) in outputs {
                    sh.set_generator_state(n, st);
                }
            },
        );
    }

    /// Loads the settings-group catalogue once.
    fn load_groups(&mut self) {
        if self.groups_requested {
            return;
        }
        self.groups_requested = true;
        self.client.op(
            "settings catalogue",
            |api| async move { api.groups().await },
            |sh: &mut Shared, g: Vec<GroupInfo>| sh.groups = Some(g),
        );
    }

    /// Trigger types other than EDGE are edited in the settings panel.
    fn trigger_type(&mut self, ui: &mut egui::Ui, mode: &str) {
        let types: Vec<(&str, &str)> = mho_driver::params::GROUPS
            .iter()
            .filter_map(|g| Some((g.select?.strip_prefix(":TRIG:MODE ")?, g.id)))
            .collect();
        let current = mode.to_ascii_uppercase();
        let matches = |token: &str| current.starts_with(token) || token.starts_with(&current);
        ui.horizontal(|ui| {
            ui.label("type");
            egui::ComboBox::from_id_salt("trig_type")
                .selected_text(&current)
                .width(90.0)
                .show_ui(ui, |ui| {
                    if ui.selectable_label(matches("EDGE"), "EDGE").clicked() {
                        self.set_trigger(TriggerUpdate {
                            mode: Some("EDGE".into()),
                            ..Default::default()
                        });
                    }
                    for (token, id) in &types {
                        if ui.selectable_label(matches(token), *token).clicked() {
                            self.open_settings(id, None, true);
                        }
                    }
                });
            if let Some((_, id)) = types.iter().find(|(t, _)| matches(t))
                && ui.small_button("edit…").clicked()
            {
                self.open_settings(id, None, false);
            }
        });
    }

    /// Opens the settings panel at group `id` (the catalogue is loaded on demand).
    fn open_settings(&mut self, id: &str, instance: Option<u8>, activate: bool) {
        self.load_groups();
        self.settings_open = Some(true);
        self.settings.request(id, instance, activate);
    }

    /// Fetches statistics for every live measurement (one query per statistic).
    fn load_stats(&self, live: &LiveConfig) {
        let specs = live.measurements.clone();
        self.client.op(
            "statistics",
            move |api| async move {
                let mut out = Vec::new();
                for m in specs {
                    out.push(
                        api.measure_stats(StatsRequest {
                            item: m.item,
                            source: m.source,
                            source2: None,
                        })
                        .await?,
                    );
                }
                Ok(out)
            },
            |sh: &mut Shared, v: Vec<MeasureStats>| sh.stats = v,
        );
    }

    fn reset_stats(&self) {
        self.client.op(
            "reset statistics",
            |api| async move { api.measure_action(MeasureAction::ResetStatistics).await },
            |sh: &mut Shared, ()| {
                sh.stats.clear();
                sh.log("statistics reset", false);
            },
        );
    }

    fn stats_table(&self, ui: &mut egui::Ui, s: &Snapshot) {
        if s.stats.is_empty() {
            return;
        }
        let opt =
            |v: Option<f64>, u: &str| v.map(|v| format_si(v, u)).unwrap_or_else(|| "---".into());
        egui::Grid::new("meas_stats")
            .striped(true)
            .spacing([18.0, 2.0])
            .show(ui, |ui| {
                for h in [
                    "statistics",
                    "current",
                    "average",
                    "min",
                    "max",
                    "σ",
                    "count",
                ] {
                    ui.label(RichText::new(h).strong());
                }
                ui.end_row();
                for st in &s.stats {
                    ui.label(
                        RichText::new(format!("{} {}", st.source, st.item))
                            .color(channel_color(&st.source)),
                    );
                    for v in [st.current, st.average, st.minimum, st.maximum, st.deviation] {
                        ui.label(RichText::new(opt(v, &st.unit)).monospace());
                    }
                    ui.label(
                        RichText::new(st.count.map(|c| format!("{c:.0}")).unwrap_or("---".into()))
                            .monospace(),
                    );
                    ui.end_row();
                }
            });
    }

    fn read_decoder(&self) {
        let bus = self.decoder_bus;
        self.client.op(
            format!("decoder {bus}"),
            move |api| async move { api.decode_bus(bus).await },
            move |sh: &mut Shared, t: DecodeTable| {
                sh.log(format!("decoder {bus}: {} events", t.rows.len()), false);
                sh.decode = Some((bus, Arc::new(t)));
            },
        );
    }

    fn capture_logic(&self) {
        let bus = self.decoder_bus;
        let path = self.capture_dir.join(format!(
            "mho954-logic-{}.vcd",
            chrono::Local::now().format("%Y%m%d-%H%M%S")
        ));
        let dir = self.capture_dir.clone();
        self.client.op(
            format!("logic via bus {bus}"),
            move |api| async move { api.logic_capture(bus).await },
            move |sh: &mut Shared, cap: mho_driver::LogicCapture| {
                let vcd = mho_dsp::write_vcd(&cap.events, &cap.lines);
                match std::fs::create_dir_all(&dir).and_then(|_| std::fs::write(&path, vcd)) {
                    Ok(()) => sh.log(
                        format!("{} changes saved to {}", cap.events.len(), path.display()),
                        false,
                    ),
                    Err(e) => sh.log(format!("VCD export: {e}"), true),
                }
            },
        );
    }

    fn export_decoder(&self, bus: u8, table: &DecodeTable) {
        let path = self.capture_dir.join(format!(
            "mho954-bus{bus}-{}.csv",
            chrono::Local::now().format("%Y%m%d-%H%M%S")
        ));
        let r = std::fs::create_dir_all(&self.capture_dir)
            .and_then(|_| std::fs::write(&path, mho_dsp::decode_csv(table)));
        let mut sh = self.client.lock();
        match r {
            Ok(()) => sh.log(format!("saved {}", path.display()), false),
            Err(e) => sh.log(format!("decoder export: {e}"), true),
        }
    }

    fn decoder_panel(&mut self, ui: &mut egui::Ui, s: &Snapshot) {
        ui.horizontal(|ui| {
            ui.label(RichText::new("Decoder").strong());
            for n in 1..=4u8 {
                ui.selectable_value(&mut self.decoder_bus, n, format!("Bus {n}"));
            }
            if ui
                .button("Read")
                .on_hover_text("read the scope's decoder event table")
                .clicked()
            {
                self.read_decoder();
            }
            if ui
                .button("Setup…")
                .on_hover_text("decoder type, lines, thresholds")
                .clicked()
            {
                self.open_settings("bus", Some(self.decoder_bus), false);
            }
            if ui
                .button("Logic (VCD)")
                .on_hover_text(
                    "capture D0..D15 through this bus (parallel decoding) and save a VCD file",
                )
                .clicked()
            {
                self.capture_logic();
            }
            if let Some((bus, t)) = &s.decode {
                if ui.button("CSV").clicked() {
                    self.export_decoder(*bus, t);
                }
                ui.label(
                    RichText::new(format!(
                        "bus {bus} · {} · {} events",
                        if t.protocol.is_empty() {
                            "?"
                        } else {
                            &t.protocol
                        },
                        t.rows.len()
                    ))
                    .weak(),
                );
            }
        });
        let Some((_, table)) = &s.decode else {
            return;
        };
        egui::ScrollArea::both()
            .max_height(180.0)
            .id_salt("decoder_table")
            .show(ui, |ui| {
                egui::Grid::new("decoder_grid")
                    .striped(true)
                    .spacing([16.0, 2.0])
                    .show(ui, |ui| {
                        for c in &table.columns {
                            ui.label(RichText::new(c).strong());
                        }
                        ui.end_row();
                        for r in table.rows.iter().take(MAX_DECODE_ROWS) {
                            for c in &r.cells {
                                ui.label(RichText::new(c).monospace());
                            }
                            ui.end_row();
                        }
                    });
                if table.rows.len() > MAX_DECODE_ROWS {
                    ui.label(format!(
                        "… {} more events (use CSV export)",
                        table.rows.len() - MAX_DECODE_ROWS
                    ));
                }
            });
    }

    fn system_controls(&mut self, ui: &mut egui::Ui, s: &Snapshot) {
        ui.label(RichText::new("Setup").strong());
        if ui
            .button("Save to file")
            .on_hover_text("scope setup blob, restorable with Load or `mho954 restore`")
            .clicked()
        {
            let path = self.capture_dir.join(format!(
                "mho954-setup-{}.bin",
                chrono::Local::now().format("%Y%m%d-%H%M%S")
            ));
            let dir = self.capture_dir.clone();
            self.setup_path = path.display().to_string();
            self.client.op(
                "save setup",
                |api| async move { api.setup_save().await },
                move |sh: &mut Shared, blob: Vec<u8>| match std::fs::create_dir_all(&dir)
                    .and_then(|_| std::fs::write(&path, blob))
                {
                    Ok(()) => sh.log(format!("setup saved to {}", path.display()), false),
                    Err(e) => sh.log(format!("saving setup: {e}"), true),
                },
            );
        }
        ui.horizontal(|ui| {
            ui.add(
                egui::TextEdit::singleline(&mut self.setup_path)
                    .desired_width(200.0)
                    .hint_text("setup file"),
            );
            if ui.button("Load").clicked() {
                match std::fs::read(&self.setup_path) {
                    Ok(blob) => self.client.op(
                        "load setup",
                        move |api| async move { api.setup_load(blob).await },
                        |sh: &mut Shared, ()| {
                            sh.log("setup loaded", false);
                            sh.status_dirty = true;
                        },
                    ),
                    Err(e) => {
                        let msg = format!("{}: {e}", self.setup_path);
                        self.client.lock().log(msg, true);
                    }
                }
            }
        });
        ui.separator();
        ui.label(RichText::new("Options").strong());
        match &s.options {
            None => {
                ui.label("loading…");
            }
            Some(opts) => {
                egui::Grid::new("options").striped(true).show(ui, |ui| {
                    for o in opts {
                        ui.label(&o.name).on_hover_text(&o.description);
                        ui.label(if o.active {
                            RichText::new("active").color(Color32::LIGHT_GREEN)
                        } else {
                            RichText::new("-").weak()
                        });
                        ui.end_row();
                    }
                });
                ui.label(
                    RichText::new("install a purchased license: mho954 options install '<key>'")
                        .small()
                        .weak(),
                );
            }
        }
    }

    fn load_network(&self) {
        self.client.op(
            "network",
            |api| async move { api.network().await },
            |sh: &mut Shared, n: NetworkState| sh.network = Some(n),
        );
    }

    fn network_controls(&mut self, ui: &mut egui::Ui, s: &Snapshot) {
        let Some(n) = &s.network else {
            if ui.button("Load LAN settings").clicked() {
                self.load_network();
            }
            return;
        };
        let f = &mut self.net_form;
        if !f.loaded {
            *f = NetForm {
                loaded: true,
                mode: Some(if n.manual && !n.dhcp {
                    IpMode::Static
                } else if n.dhcp {
                    IpMode::Dhcp
                } else {
                    IpMode::AutoIp
                }),
                ip: n.ip.clone(),
                mask: n.mask.clone(),
                gateway: n.gateway.clone(),
                dns: n.dns.clone(),
                hostname: n.hostname.clone(),
                mdns: n.mdns,
            };
        }
        let col = if n.status == "CONNECTED" || n.status == "CONFIGURED" {
            Color32::LIGHT_GREEN
        } else {
            Color32::GRAY
        };
        ui.label(RichText::new(format!("{}  ·  MAC {}", n.status, n.mac)).color(col));
        ui.horizontal(|ui| {
            for (m, label) in [
                (IpMode::Dhcp, "DHCP"),
                (IpMode::AutoIp, "Auto-IP"),
                (IpMode::Static, "Static"),
            ] {
                ui.selectable_value(&mut f.mode, Some(m), label);
            }
        });
        let static_mode = f.mode == Some(IpMode::Static);
        egui::Grid::new("netgrid").num_columns(2).show(ui, |ui| {
            for (label, value) in [
                ("IP", &mut f.ip),
                ("Mask", &mut f.mask),
                ("Gateway", &mut f.gateway),
                ("DNS", &mut f.dns),
            ] {
                ui.label(label);
                ui.add_enabled(
                    static_mode,
                    egui::TextEdit::singleline(value).desired_width(130.0),
                );
                ui.end_row();
            }
            ui.label("Hostname");
            ui.add(egui::TextEdit::singleline(&mut f.hostname).desired_width(130.0));
            ui.end_row();
        });
        ui.checkbox(&mut f.mdns, "mDNS");
        let (mut apply, mut reload) = (false, false);
        ui.horizontal(|ui| {
            apply = ui
                .button("Apply")
                .on_hover_text("Sends the settings and :LAN:APPLy")
                .clicked();
            reload = ui.button("Reload").clicked();
        });
        if apply {
            let u = NetworkUpdate {
                mode: f.mode,
                ip: static_mode.then(|| f.ip.clone()),
                mask: static_mode.then(|| f.mask.clone()),
                gateway: static_mode.then(|| f.gateway.clone()),
                dns: static_mode.then(|| f.dns.clone()),
                mdns: Some(f.mdns),
                hostname: Some(f.hostname.clone()),
                description: None,
            };
            self.client.op(
                "LAN apply",
                move |api| async move { api.set_network(u).await },
                |sh: &mut Shared, n: NetworkState| {
                    sh.log(format!("LAN applied: {} {}", n.status, n.ip), false);
                    sh.network = Some(n);
                },
            );
            self.net_form.loaded = false;
        }
        if reload {
            self.net_form.loaded = false;
            self.load_network();
        }
        ui.label(
            RichText::new("WLAN (USB dongle) has no SCPI interface; configure it on the scope.")
                .small()
                .weak(),
        );
    }

    fn channel_controls(&mut self, ui: &mut egui::Ui, c: &ChannelState) {
        let n = c.channel;
        let color = CH_COLORS[(n - 1) as usize];
        let id = ui.make_persistent_id(format!("ch{n}"));
        egui::collapsing_header::CollapsingState::load_with_default_open(ui.ctx(), id, n <= 2)
            .show_header(ui, |ui| {
                let mut on = c.display;
                if ui
                    .checkbox(
                        &mut on,
                        RichText::new(format!("CH{n}")).color(color).strong(),
                    )
                    .changed()
                {
                    self.set_channel(
                        n,
                        ChannelUpdate {
                            display: Some(on),
                            ..Default::default()
                        },
                    );
                }
                ui.label(
                    RichText::new(format!("{}/div", format_si(c.scale, "V")))
                        .monospace()
                        .color(color),
                );
            })
            .body(|ui| {
                ui.horizontal(|ui| {
                    if ui.small_button("−").clicked() {
                        self.set_channel(
                            n,
                            ChannelUpdate {
                                scale: Some(step_125(c.scale, false)),
                                ..Default::default()
                            },
                        );
                    }
                    ui.label("V/div");
                    if ui.small_button("+").clicked() {
                        self.set_channel(
                            n,
                            ChannelUpdate {
                                scale: Some(step_125(c.scale, true)),
                                ..Default::default()
                            },
                        );
                    }
                    ui.label("offset");
                    let idx = (n - 1) as usize;
                    let mut v = self.offset_edit[idx].unwrap_or(c.offset);
                    let r = ui.add(
                        egui::DragValue::new(&mut v)
                            .speed(c.scale * 0.02)
                            .custom_formatter(|v, _| format_si(v, "V"))
                            .custom_parser(parse_si),
                    );
                    if r.changed() {
                        self.offset_edit[idx] = Some(v);
                    }
                    if (r.drag_stopped() || r.lost_focus()) && self.offset_edit[idx].is_some() {
                        self.set_channel(
                            n,
                            ChannelUpdate {
                                offset: Some(v),
                                ..Default::default()
                            },
                        );
                        self.offset_edit[idx] = None;
                    }
                    if ui.small_button("0").clicked() {
                        self.set_channel(
                            n,
                            ChannelUpdate {
                                offset: Some(0.0),
                                ..Default::default()
                            },
                        );
                    }
                });
                ui.horizontal(|ui| {
                    for cp in Coupling::ALL {
                        if ui.selectable_label(c.coupling == *cp, cp.scpi()).clicked() {
                            self.set_channel(
                                n,
                                ChannelUpdate {
                                    coupling: Some(*cp),
                                    ..Default::default()
                                },
                            );
                        }
                    }
                    ui.separator();
                    for (imp, label) in [(Impedance::OneMeg, "1MΩ"), (Impedance::Fifty, "50Ω")] {
                        if ui.selectable_label(c.impedance == imp, label).clicked() {
                            self.set_channel(
                                n,
                                ChannelUpdate {
                                    impedance: Some(imp),
                                    ..Default::default()
                                },
                            );
                        }
                    }
                });
                ui.horizontal(|ui| {
                    ui.label("BWL");
                    for bwl in ["OFF", "20M", "250M"] {
                        if ui
                            .selectable_label(c.bw_limit.eq_ignore_ascii_case(bwl), bwl)
                            .clicked()
                        {
                            self.set_channel(
                                n,
                                ChannelUpdate {
                                    bw_limit: Some(bwl.into()),
                                    ..Default::default()
                                },
                            );
                        }
                    }
                    ui.separator();
                    for p in [1.0, 10.0, 100.0] {
                        if ui
                            .selectable_label((c.probe - p).abs() < 1e-9, format!("{p}x"))
                            .clicked()
                        {
                            self.set_channel(
                                n,
                                ChannelUpdate {
                                    probe: Some(p),
                                    ..Default::default()
                                },
                            );
                        }
                    }
                    let mut inv = c.invert;
                    if ui.checkbox(&mut inv, "inv").changed() {
                        self.set_channel(
                            n,
                            ChannelUpdate {
                                invert: Some(inv),
                                ..Default::default()
                            },
                        );
                    }
                });
            });
    }

    fn measurements_panel(
        &mut self,
        ui: &mut egui::Ui,
        s: &Snapshot,
        local: &[(String, Measurements)],
    ) {
        let opt =
            |v: Option<f64>, u: &str| v.map(|v| format_si(v, u)).unwrap_or_else(|| "---".into());
        egui::Grid::new("local_meas")
            .striped(true)
            .spacing([18.0, 2.0])
            .show(ui, |ui| {
                for h in [
                    "", "Vpp", "Vmax", "Vmin", "Vavg", "Vrms", "Freq", "Period", "Duty", "Rise",
                    "Fall",
                ] {
                    ui.label(RichText::new(h).strong());
                }
                ui.end_row();
                for (src, m) in local {
                    ui.label(RichText::new(src).color(channel_color(src)).strong());
                    if m.samples == 0 {
                        ui.label(
                            RichText::new("no data (scope reports a constant full-scale code)")
                                .weak(),
                        );
                        ui.end_row();
                        continue;
                    }
                    for v in [
                        format_si(m.vpp, "V"),
                        format_si(m.vmax, "V"),
                        format_si(m.vmin, "V"),
                        format_si(m.vavg, "V"),
                        format_si(m.vrms, "V"),
                        opt(m.frequency, "Hz"),
                        opt(m.period, "s"),
                        opt(m.duty, "%"),
                        opt(m.rise_time, "s"),
                        opt(m.fall_time, "s"),
                    ] {
                        ui.label(RichText::new(v).monospace());
                    }
                    ui.end_row();
                }
            });

        ui.horizontal_wrapped(|ui| {
            ui.label(RichText::new("Scope:").strong());
            if let (Some(m), Some(live)) = (&s.measurements, &s.live) {
                for v in &m.values {
                    let txt = format!("{} {} = {}", v.source, v.item, opt(v.value, &v.unit));
                    ui.label(
                        RichText::new(txt)
                            .monospace()
                            .color(channel_color(&v.source)),
                    );
                    if ui.small_button("x").on_hover_text("remove").clicked() {
                        // Values may skip invalid specs, so match by item and source, not index.
                        let mut cfg = live.clone();
                        let pos = cfg.measurements.iter().position(|spec| {
                            find_item(&spec.item).is_ok_and(|it| it.long == v.item)
                                && normalize_source(&spec.source).is_ok_and(|s| s == v.source)
                        });
                        if let Some(pos) = pos {
                            cfg.measurements.remove(pos);
                        }
                        self.set_live(cfg);
                    }
                    ui.separator();
                }
            }
            egui::ComboBox::from_id_salt("meas_item")
                .selected_text(&self.new_meas_item)
                .width(80.0)
                .show_ui(ui, |ui| {
                    for it in MEASURE_ITEMS.iter().filter(|i| !i.dual_source) {
                        ui.selectable_value(&mut self.new_meas_item, it.token.to_string(), it.long)
                            .on_hover_text(it.description);
                    }
                });
            egui::ComboBox::from_id_salt("meas_src")
                .selected_text(&self.new_meas_source)
                .width(70.0)
                .show_ui(ui, |ui| {
                    for n in 1..=CHANNELS {
                        let src = format!("CHAN{n}");
                        ui.selectable_value(&mut self.new_meas_source, src.clone(), &src);
                    }
                });
            if let Some(live) = &s.live
                && ui.button("+ add").clicked()
            {
                let mut cfg = live.clone();
                cfg.measurements.push(MeasureSpec {
                    item: self.new_meas_item.clone(),
                    source: self.new_meas_source.clone(),
                });
                self.set_live(cfg);
            }
            if let Some(live) = &s.live
                && !live.measurements.is_empty()
            {
                ui.separator();
                if ui
                    .button("statistics")
                    .on_hover_text("read min/max/avg/σ/count of the scope measurements")
                    .clicked()
                {
                    self.load_stats(live);
                }
                if ui
                    .small_button("reset")
                    .on_hover_text("reset statistics")
                    .clicked()
                {
                    self.reset_stats();
                }
            }
        });
        self.stats_table(ui, s);
    }

    fn scope_plot(&mut self, ui: &mut egui::Ui, s: &Snapshot, height: f32) {
        let Some(frame) = &s.frame else {
            ui.centered_and_justified(|ui| ui.label("no data yet"));
            return;
        };
        let status = s.status.as_ref();
        let chan_state = |src: &str| -> Option<ChannelState> {
            let n = channel_of(src)?;
            status
                .and_then(|st| st.channels.get((n - 1) as usize))
                .cloned()
        };
        let y_mode = if status.is_some() {
            self.y_mode
        } else {
            YMode::Volts
        };

        let (t0, t1) = frame
            .traces
            .first()
            .map(|t| {
                (
                    t.preamble.time(0),
                    t.preamble.time(t.raw.len().saturating_sub(1)),
                )
            })
            .unwrap_or((0.0, 1.0));
        let tdiv = status
            .map(|s| s.timebase.scale)
            .unwrap_or((t1 - t0) / H_DIVS);

        // A cursor under the pointer is dragged instead of panning the view. While the
        // button is down the press position counts: egui reports the drag only after the
        // pointer has moved a few pixels, by then it may be off the cursor line.
        let near = self.view.drag.or_else(|| {
            let pos = ui.input(|i| {
                if i.pointer.primary_down() {
                    i.pointer.press_origin()
                } else {
                    i.pointer.hover_pos()
                }
            })?;
            let tr = self.plot_transform?;
            if !tr.frame().contains(pos) {
                return None;
            }
            self.view.handle_near(
                [pos.x, pos.y],
                |x, y| {
                    let p = tr.position_from_point(&egui_plot::PlotPoint::new(x, y));
                    [p.x, p.y]
                },
                8.0,
            )
        });

        let mut plot = Plot::new("scope")
            .height(height)
            .legend(Legend::default())
            .show_background(true)
            .allow_double_click_reset(true)
            .allow_drag(near.is_none())
            .x_axis_formatter(move |m, _| {
                format_si(
                    if m.value.abs() < tdiv * 1e-6 {
                        0.0
                    } else {
                        m.value
                    },
                    "s",
                )
            })
            .x_grid_spacer(move |input| {
                let (lo, hi) = input.bounds;
                // One mark per division; when zoomed in far, a finer 1-2-5 step.
                let span = hi - lo;
                let step = if span < tdiv * 2.0 {
                    let raw = span / 10.0;
                    let e = 10f64.powf(raw.log10().floor());
                    [1.0, 2.0, 5.0, 10.0]
                        .iter()
                        .map(|m| m * e)
                        .find(|s| *s >= raw)
                        .unwrap_or(10.0 * e)
                } else {
                    tdiv
                };
                let start = t0 + ((lo - t0) / step).floor() * step;
                let mut marks = Vec::new();
                let mut x = start;
                while x <= hi && marks.len() < 200 {
                    marks.push(GridMark {
                        value: x,
                        step_size: step,
                    });
                    x += step;
                }
                marks
            });
        plot = match y_mode {
            YMode::Divisions => plot
                .y_axis_formatter(|m, _| format!("{:+.1}", m.value))
                .y_grid_spacer(|input| {
                    let (lo, hi) = input.bounds;
                    let step = if hi - lo < 2.0 { 0.2 } else { 1.0 };
                    let first = (lo / step).floor() as i64;
                    let last = (hi / step).ceil() as i64;
                    (first..=last)
                        .map(|v| GridMark {
                            value: v as f64 * step,
                            step_size: step,
                        })
                        .collect()
                })
                .default_y_bounds(-V_DIVS / 2.0, V_DIVS / 2.0),
            YMode::Volts => plot.y_axis_formatter(|m, _| format_si(m.value, "V")),
        };
        plot = plot.default_x_bounds(t0, t1);

        let new_range = self.last_x_range != Some((t0, t1));
        self.last_x_range = Some((t0, t1));
        let pending = self.view.pending.take();
        let traces = &frame.traces;
        let trig = status.map(|s| s.trigger.clone());
        let view = &self.view;
        let math_scale = &s.math_scale;
        // Decoded events as labels along the top edge.
        let marks: Vec<(f64, String)> = match (&s.decode, self.show_decoder) {
            (Some((_, t)), true) => t
                .rows
                .iter()
                .filter_map(|r| Some((r.time?, r.cells.get(1)?.clone())))
                .take(MAX_DECODE_MARKS)
                .collect(),
            _ => Vec::new(),
        };

        let resp = plot.show(ui, |pui| {
            let default_y = |pui: &egui_plot::PlotUi| match y_mode {
                YMode::Divisions => (-V_DIVS / 2.0, V_DIVS / 2.0),
                YMode::Volts => (pui.plot_bounds().min()[1], pui.plot_bounds().max()[1]),
            };
            if new_range || pending == Some(ViewAction::Reset) {
                let y = default_y(pui);
                pui.set_plot_bounds(PlotBounds::from_min_max([t0, y.0], [t1, y.1]));
                if y_mode == YMode::Volts {
                    pui.set_auto_bounds([false, true]);
                }
            }
            match pending {
                Some(ViewAction::Zoom { x, y, center }) => {
                    let b = pui.plot_bounds();
                    let c = center.unwrap_or([
                        (b.min()[0] + b.max()[0]) / 2.0,
                        (b.min()[1] + b.max()[1]) / 2.0,
                    ]);
                    let (x0, x1) = zoom_range(b.min()[0], b.max()[0], x, c[0]);
                    let (y0, y1) = zoom_range(b.min()[1], b.max()[1], y, c[1]);
                    pui.set_plot_bounds(PlotBounds::from_min_max([x0, y0], [x1, y1]));
                    pui.set_auto_bounds([false, false]);
                }
                Some(ViewAction::ShowX(x0, x1)) => {
                    pui.set_plot_bounds_x(x0..=x1);
                    pui.set_auto_bounds([false, false]);
                }
                _ => {}
            }
            let visible = pui.plot_bounds();
            let (vx0, vx1) = (visible.min()[0], visible.max()[0]);
            let mut fit: Option<(f64, f64)> = None;
            for w in traces.iter().filter(|w| w.has_data()) {
                let cs = chan_state(&w.source);
                let pts: Vec<[f64; 2]> = (0..w.raw.len())
                    .map(|i| {
                        let v = w.preamble.volts(w.raw[i]);
                        let y = match (y_mode, &cs) {
                            (YMode::Divisions, Some(c)) => (v + c.offset) / c.scale,
                            (YMode::Divisions, None) => match math_scale.get(&w.source) {
                                Some((scale, offset)) => (v + offset) / scale,
                                None => v,
                            },
                            _ => v,
                        };
                        [w.preamble.time(i), y]
                    })
                    .collect();
                for p in pts.iter().filter(|p| (vx0..=vx1).contains(&p[0])) {
                    fit = Some(fit.map_or((p[1], p[1]), |(lo, hi)| (lo.min(p[1]), hi.max(p[1]))));
                }
                pui.line(
                    Line::new(w.source.clone(), PlotPoints::from(pts))
                        .color(channel_color(&w.source))
                        .width(1.2),
                );
            }
            if pending == Some(ViewAction::FitY)
                && let Some((lo, hi)) = fit
            {
                let pad = ((hi - lo) * 0.08).max(1e-9);
                pui.set_plot_bounds_y(lo - pad..=hi + pad);
                pui.set_auto_bounds([false, false]);
            }
            if let Some((g0, g1)) = view.fft_gate {
                pui.span(
                    egui_plot::Span::new("FFT window", g0..=g1)
                        .fill(Color32::from_rgb(255, 150, 40).gamma_multiply(0.10))
                        .border_color(Color32::from_rgb(255, 150, 40).gamma_multiply(0.6)),
                );
            }
            if view.measure_gate
                && let Some((g0, g1)) = view.gate()
            {
                pui.span(
                    egui_plot::Span::new("A-B measured", g0..=g1)
                        .fill(Color32::WHITE.gamma_multiply(0.05))
                        .border_color(Color32::WHITE.gamma_multiply(0.3)),
                );
            }
            if !marks.is_empty() {
                let top = match y_mode {
                    YMode::Divisions => V_DIVS / 2.0 - 0.35,
                    YMode::Volts => {
                        let b = pui.plot_bounds();
                        b.max()[1] - 0.05 * (b.max()[1] - b.min()[1])
                    }
                };
                for (t, label) in &marks {
                    pui.vline(
                        VLine::new("decoder", *t)
                            .color(Color32::from_rgb(120, 220, 120).gamma_multiply(0.35))
                            .width(1.0),
                    );
                    pui.text(
                        egui_plot::Text::new(
                            "decoder",
                            egui_plot::PlotPoint::new(*t, top),
                            RichText::new(label)
                                .monospace()
                                .color(Color32::from_rgb(120, 220, 120)),
                        )
                        .anchor(egui::Align2::LEFT_CENTER),
                    );
                }
            }
            if let Some(t) = &trig
                && let Some(cs) = chan_state(&t.edge_source)
            {
                let y = match y_mode {
                    YMode::Divisions => (t.edge_level + cs.offset) / cs.scale,
                    YMode::Volts => t.edge_level,
                };
                pui.hline(
                    HLine::new("trig level", y)
                        .color(channel_color(&t.edge_source).gamma_multiply(0.6))
                        .style(LineStyle::dashed_loose()),
                );
            }
            pui.vline(
                VLine::new("T", 0.0)
                    .color(Color32::from_rgb(255, 140, 0))
                    .style(LineStyle::dotted_dense()),
            );
            if let Some(a) = view.a {
                pui.vline(VLine::new("cursor A", a).color(Color32::WHITE).width(1.2));
            }
            if let Some(b) = view.b {
                pui.vline(
                    VLine::new("cursor B", b)
                        .color(Color32::LIGHT_GRAY)
                        .style(LineStyle::dashed_dense())
                        .width(1.2),
                );
            }
            for (name, y) in [("cursor Y1", view.y1), ("cursor Y2", view.y2)] {
                if let Some(y) = y {
                    pui.hline(
                        HLine::new(name, y)
                            .color(Color32::from_rgb(200, 200, 255))
                            .style(if name.ends_with('2') {
                                LineStyle::dashed_dense()
                            } else {
                                LineStyle::Solid
                            }),
                    );
                }
            }
            (pui.pointer_coordinate(), (vx0, vx1))
        });
        let (pointer, view_x) = resp.inner;
        self.view.view_x = Some(view_x);
        self.plot_transform = Some(resp.transform);
        let r = &resp.response;
        if let Some(p) = pointer {
            if r.drag_started_by(egui::PointerButton::Primary) && near.is_some() {
                self.view.drag = near;
            }
            if let Some(h) = self.view.drag
                && r.dragged()
            {
                self.view.set(h, p.x, p.y);
            }
            if r.clicked() && near.is_none() {
                self.view.a = Some(p.x);
            }
            if r.secondary_clicked() {
                self.view.menu_at = Some([p.x, p.y]);
            }
        }
        if r.drag_stopped() {
            self.view.drag = None;
        }
        if near.is_some() {
            let icon = match near {
                Some(Handle::Y1 | Handle::Y2) => egui::CursorIcon::ResizeVertical,
                _ => egui::CursorIcon::ResizeHorizontal,
            };
            ui.ctx().set_cursor_icon(icon);
        }
        if r.middle_clicked() {
            self.view.clear_cursors();
        }
        let trig_cs = trig
            .as_ref()
            .and_then(|t| Some((t.edge_source.clone(), chan_state(&t.edge_source)?)));
        r.context_menu(|ui| self.plot_menu(ui, y_mode, trig_cs.as_ref()));
    }

    /// Right-click menu of the scope plot; acts at the clicked position.
    fn plot_menu(
        &mut self,
        ui: &mut egui::Ui,
        y_mode: YMode,
        trig: Option<&(String, ChannelState)>,
    ) {
        let Some([x, y]) = self.view.menu_at else {
            ui.close();
            return;
        };
        let done = |ui: &mut egui::Ui| ui.close();
        ui.label(RichText::new(format!("at {}", format_si(x, "s"))).weak());
        if ui.button("Cursor A here").clicked() {
            self.view.a = Some(x);
            done(ui);
        }
        if ui.button("Cursor B here").clicked() {
            self.view.b = Some(x);
            done(ui);
        }
        if ui.button("Level cursor Y1 here").clicked() {
            self.view.y1 = Some(y);
            done(ui);
        }
        if ui.button("Level cursor Y2 here").clicked() {
            self.view.y2 = Some(y);
            done(ui);
        }
        if ui.button("Clear cursors").clicked() {
            self.view.clear_cursors();
            done(ui);
        }
        ui.separator();
        let gate = self.view.gate();
        if ui
            .add_enabled(gate.is_some(), egui::Button::new("Zoom to A-B"))
            .clicked()
            && let Some((a, b)) = gate
        {
            self.view.pending = Some(ViewAction::ShowX(a, b));
            done(ui);
        }
        if ui.button("Zoom in here (2x)").clicked() {
            self.view.pending = Some(ViewAction::Zoom {
                x: 2.0,
                y: 1.0,
                center: Some([x, y]),
            });
            done(ui);
        }
        if ui.button("Zoom out (2x)").clicked() {
            self.view.pending = Some(ViewAction::Zoom {
                x: 0.5,
                y: 1.0,
                center: Some([x, y]),
            });
            done(ui);
        }
        if ui.button("Fit vertical").clicked() {
            self.view.pending = Some(ViewAction::FitY);
            done(ui);
        }
        if ui.button("Reset view").clicked() {
            self.view.pending = Some(ViewAction::Reset);
            done(ui);
        }
        ui.separator();
        if ui
            .add_enabled(
                gate.is_some(),
                egui::Checkbox::new(&mut self.view.measure_gate, "Measure A-B"),
            )
            .changed()
        {
            done(ui);
        }
        if ui
            .add_enabled(gate.is_some(), egui::Button::new("FFT of A-B"))
            .clicked()
        {
            self.view.fft_gate = gate;
            self.show_fft = true;
            done(ui);
        }
        if ui.button("FFT of the visible range").clicked() {
            self.view.fft_gate = self.view.view_x;
            self.show_fft = true;
            done(ui);
        }
        if self.view.fft_gate.is_some() && ui.button("FFT of the whole record").clicked() {
            self.view.fft_gate = None;
            done(ui);
        }
        ui.separator();
        ui.label(RichText::new("on the scope").weak());
        if let Some((src, cs)) = trig {
            let volts = match y_mode {
                YMode::Divisions => plot_tools::div_to_volts(y, cs.scale, cs.offset),
                YMode::Volts => y,
            };
            if ui
                .button(format!(
                    "Trigger level here ({src} {})",
                    format_si(volts, "V")
                ))
                .clicked()
            {
                self.set_trigger(TriggerUpdate {
                    edge_level: Some(volts),
                    ..Default::default()
                });
                done(ui);
            }
        }
        if ui.button("Center time here").clicked() {
            self.set_timebase(TimebaseUpdate {
                offset: Some(x),
                ..Default::default()
            });
            done(ui);
        }
        if let Some((x0, x1)) = self.view.view_x
            && ui
                .button("Apply visible range to the scope")
                .on_hover_text(
                    "sets timebase scale and offset so the scope acquires this range \
                     (more detail than zooming into the 1000 screen points)",
                )
                .clicked()
        {
            let (scale, offset) = plot_tools::timebase_for_view(x0, x1);
            self.set_timebase(TimebaseUpdate {
                scale: Some(scale),
                offset: Some(offset),
                ..Default::default()
            });
            done(ui);
        }
        if ui.button("Screenshot").clicked() {
            self.screenshot();
            done(ui);
        }
    }

    /// View buttons above the measurements.
    fn view_toolbar(&mut self, ui: &mut egui::Ui) {
        ui.label(RichText::new("view").weak());
        let mut act = |ui: &mut egui::Ui, label: &str, tip: &str, a: ViewAction| {
            if ui.small_button(label).on_hover_text(tip).clicked() {
                self.view.pending = Some(a);
            }
        };
        act(
            ui,
            "reset",
            "whole record, default vertical range",
            ViewAction::Reset,
        );
        act(
            ui,
            "fit Y",
            "vertical range fitted to the visible data",
            ViewAction::FitY,
        );
        let zoom = |x: f64, y: f64| ViewAction::Zoom { x, y, center: None };
        act(ui, "X+", "zoom in horizontally", zoom(2.0, 1.0));
        act(ui, "X-", "zoom out horizontally", zoom(0.5, 1.0));
        act(ui, "Y+", "zoom in vertically", zoom(1.0, 2.0));
        act(ui, "Y-", "zoom out vertically", zoom(1.0, 0.5));
    }

    fn cursor_readout(&self, ui: &mut egui::Ui, frame: &Frame, s: &Snapshot) {
        let v = &self.view;
        if v.a.is_none() && v.b.is_none() && v.y1.is_none() && v.y2.is_none() {
            ui.label(
                RichText::new(
                    "left-click: cursor A - drag cursors - right-click: menu - right-drag: \
                     zoom box - scroll: pan - ctrl+scroll: zoom - double-click: reset",
                )
                .small()
                .weak(),
            );
            return;
        }
        let value_at = |w: &Waveform, t: f64| -> Option<f64> {
            let p = &w.preamble;
            let x = (t - p.x_origin) / p.x_increment + p.x_reference;
            if x < 0.0 || x > (w.raw.len() - 1) as f64 {
                return None;
            }
            let i = x.floor() as usize;
            let j = (i + 1).min(w.raw.len() - 1);
            let f = x - i as f64;
            Some(p.volts(w.raw[i]) * (1.0 - f) + p.volts(w.raw[j]) * f)
        };
        ui.horizontal_wrapped(|ui| {
            for (name, c) in [("A", v.a), ("B", v.b)] {
                if let Some(t) = c {
                    ui.label(RichText::new(format!("{name}: {}", format_si(t, "s"))).strong());
                    for w in frame.traces.iter().filter(|w| w.has_data()) {
                        if let Some(val) = value_at(w, t) {
                            ui.label(
                                RichText::new(format!("{} {}", w.source, format_si(val, "V")))
                                    .color(channel_color(&w.source)),
                            );
                        }
                    }
                    ui.separator();
                }
            }
            if let (Some(a), Some(b)) = (v.a, v.b) {
                let dt = b - a;
                ui.label(
                    RichText::new(format!(
                        "dt {}  1/dt {}",
                        format_si(dt, "s"),
                        format_si(1.0 / dt.abs(), "Hz")
                    ))
                    .strong(),
                );
                for w in frame.traces.iter().filter(|w| w.has_data()) {
                    if let (Some(va), Some(vb)) = (value_at(w, a), value_at(w, b)) {
                        ui.label(
                            RichText::new(format!("d{} {}", w.source, format_si(vb - va, "V")))
                                .color(channel_color(&w.source)),
                        );
                    }
                }
                ui.separator();
            }
            // Level cursors: in the division view they mean a different voltage per channel.
            let volts_of = |y: f64, src: &str| -> Option<f64> {
                match self.y_mode {
                    YMode::Volts => Some(y),
                    YMode::Divisions => {
                        let n = channel_of(src)?;
                        let c = s.status.as_ref()?.channels.get((n - 1) as usize)?;
                        Some(plot_tools::div_to_volts(y, c.scale, c.offset))
                    }
                }
            };
            let sources: Vec<&str> = frame
                .traces
                .iter()
                .filter(|w| w.has_data())
                .map(|w| w.source.as_str())
                .collect();
            for (name, y) in [("Y1", v.y1), ("Y2", v.y2)] {
                if let Some(y) = y {
                    let unit = if self.y_mode == YMode::Divisions {
                        "div"
                    } else {
                        ""
                    };
                    ui.label(RichText::new(format!("{name}: {y:+.3} {unit}")).strong());
                    if self.y_mode == YMode::Divisions {
                        for src in &sources {
                            if let Some(volts) = volts_of(y, src) {
                                ui.label(
                                    RichText::new(format!("{src} {}", format_si(volts, "V")))
                                        .color(channel_color(src)),
                                );
                            }
                        }
                    }
                    ui.separator();
                }
            }
            if let (Some(y1), Some(y2)) = (v.y1, v.y2) {
                match self.y_mode {
                    YMode::Volts => {
                        ui.label(RichText::new(format!("dY {}", format_si(y2 - y1, "V"))).strong());
                    }
                    YMode::Divisions => {
                        ui.label(RichText::new(format!("dY {:+.3} div", y2 - y1)).strong());
                        for src in &sources {
                            if let (Some(a), Some(b)) = (volts_of(y1, src), volts_of(y2, src)) {
                                ui.label(
                                    RichText::new(format!("d{src} {}", format_si(b - a, "V")))
                                        .color(channel_color(src)),
                                );
                            }
                        }
                    }
                }
            }
        });
    }

    fn prefs(&self) -> Prefs {
        Prefs {
            volts_axis: self.y_mode == YMode::Volts,
            show_fft: self.show_fft,
            fft_source: self.fft_source.clone(),
            fft_window: self.fft_window,
            show_decoder: self.show_decoder,
            decoder_bus: self.decoder_bus,
            peaks_math: self.peaks_math,
            meas_item: self.new_meas_item.clone(),
            meas_source: self.new_meas_source.clone(),
            raw_source: self.raw_source.clone(),
            bode: self.bode.clone(),
            generator: self.generator.clone(),
        }
    }

    /// Writes changed preferences, at most every two seconds (and on exit).
    fn persist(&mut self, force: bool) {
        let p = self.prefs();
        if p != self.prefs_saved && (force || self.prefs_saved_at.elapsed().as_secs() >= 2) {
            prefs::save(&p);
            self.prefs_saved = p;
            self.prefs_saved_at = std::time::Instant::now();
        }
    }

    /// Reads scale/offset of displayed math traces (for the division view).
    fn track_math_scale(&mut self, s: &Snapshot) {
        let Some(frame) = &s.frame else { return };
        for t in &frame.traces {
            let Some(n) = t
                .source
                .strip_prefix("MATH")
                .and_then(|d| d.parse::<u8>().ok())
            else {
                continue;
            };
            if self
                .math_requested
                .get(&n)
                .is_some_and(|t| t.elapsed() < MATH_SCALE_REFRESH)
            {
                continue;
            }
            self.math_requested.insert(n, std::time::Instant::now());
            self.client.op(
                format!("math {n} scale"),
                move |api| async move { api.group_get("math".into(), Some(n)).await },
                move |sh: &mut Shared, m: ParamMap| {
                    let num = |k: &str| match m.get(k) {
                        Some(mho_driver::params::ParamValue::Real(x)) => Some(*x),
                        _ => None,
                    };
                    if let (Some(scale), Some(offset)) = (num("scale"), num("offset"))
                        && scale > 0.0
                    {
                        sh.math_scale.insert(format!("MATH{n}"), (scale, offset));
                    }
                },
            );
        }
    }

    fn read_scope_peaks(&self) {
        let n = self.peaks_math;
        self.client.op(
            format!("math {n} peaks"),
            move |api| async move { api.fft_peaks(n).await },
            move |sh: &mut Shared, p: Vec<mho_dsp::ScopePeak>| sh.scope_peaks = Some((n, p)),
        );
    }

    fn fft_plot(&mut self, ui: &mut egui::Ui, frame: &Frame, s: &Snapshot) {
        ui.horizontal_wrapped(|ui| {
            ui.label(RichText::new("Scope FFT").strong()).on_hover_text(
                "peak table of the scope's own FFT (math channel as FFT with peak search)",
            );
            for n in 1..=4u8 {
                ui.selectable_value(&mut self.peaks_math, n, format!("M{n}"));
            }
            if ui.button("Peaks").clicked() {
                self.read_scope_peaks();
            }
            if ui.button("Setup…").clicked() {
                self.open_settings("math.fft", Some(self.peaks_math), true);
            }
            if let Some((n, peaks)) = &s.scope_peaks {
                ui.label(
                    RichText::new(format!("MATH{n}:")).color(channel_color(&format!("MATH{n}"))),
                );
                if peaks.is_empty() {
                    ui.label("no peaks");
                }
                for p in peaks {
                    ui.label(
                        RichText::new(format!(
                            "{} {}",
                            format_si(p.frequency, "Hz"),
                            p.amplitude_text
                        ))
                        .monospace(),
                    );
                }
            }
        });
        ui.horizontal(|ui| {
            ui.label(RichText::new("FFT").strong());
            ui.checkbox(&mut self.fft_log_x, "log f")
                .on_hover_text("logarithmic frequency axis");
            ui.checkbox(&mut self.fft_auto_y, "auto Y")
                .on_hover_text("vertical range follows the spectrum (else -140..+10 dBV)");
            match self.view.fft_gate {
                Some((a, b)) => {
                    ui.label(
                        RichText::new(format!(
                            "window {} .. {}",
                            format_si(a, "s"),
                            format_si(b, "s")
                        ))
                        .color(Color32::from_rgb(255, 150, 40)),
                    );
                    if ui.small_button("whole record").clicked() {
                        self.view.fft_gate = None;
                    }
                }
                None => {
                    ui.label(RichText::new("whole record").weak());
                }
            }
            ui.separator();
            for w in frame.traces.iter().filter(|w| w.has_data()) {
                ui.selectable_value(
                    &mut self.fft_source,
                    w.source.clone(),
                    RichText::new(&w.source).color(channel_color(&w.source)),
                );
            }
            ui.separator();
            for (win, label) in [
                (Window::Rectangular, "Rect"),
                (Window::Hann, "Hann"),
                (Window::Blackman, "Blackman"),
                (Window::FlatTop, "FlatTop"),
            ] {
                ui.selectable_value(&mut self.fft_window, win, label);
            }
        });
        let Some(w) = frame
            .traces
            .iter()
            .filter(|t| t.has_data())
            .find(|t| t.source == self.fft_source)
            .or(frame.traces.iter().find(|t| t.has_data()))
        else {
            return;
        };
        // The FFT window (plot menu "FFT of A-B" / "of the visible range") selects samples.
        let volts = match self.view.fft_gate {
            Some((a, b)) => match gated_volts(w, a, b) {
                Some(v) if v.len() >= 16 => v,
                _ => {
                    ui.label(
                        RichText::new("FFT window holds too few samples; showing the whole record")
                            .color(Color32::YELLOW),
                    );
                    w.volts()
                }
            },
            None => w.volts(),
        };
        let spec = mho_dsp::spectrum(&volts, w.preamble.x_increment, self.fft_window);
        let log_x = self.fft_log_x;
        let fx = move |f: f64| if log_x { f.max(1e-12).log10() } else { f };
        let pts: PlotPoints = spec
            .dbv
            .iter()
            .enumerate()
            .skip(1)
            .map(|(k, &d)| [fx(spec.frequency(k)), d])
            .collect();
        let peaks = spec.peaks(3);
        let mut plot = Plot::new(("fft", log_x))
            .x_axis_formatter(move |m, _| {
                format_si(if log_x { 10f64.powf(m.value) } else { m.value }, "Hz")
            })
            .y_axis_formatter(|m, _| format!("{:.0} dBV", m.value));
        if !self.fft_auto_y {
            plot = plot.include_y(-140.0).include_y(10.0);
        }
        let resp = plot.show(ui, |pui| {
            pui.line(Line::new("spectrum", pts).color(channel_color(&w.source)));
            for p in &peaks {
                pui.text(egui_plot::Text::new(
                    "peak",
                    egui_plot::PlotPoint::new(fx(p.frequency), p.dbv + 3.0),
                    RichText::new(format!("{} {:.1} dBV", format_si(p.frequency, "Hz"), p.dbv))
                        .small(),
                ));
            }
            pui.pointer_coordinate()
        });
        let at = resp.inner;
        resp.response.context_menu(|ui| {
            if let Some(p) = at {
                let f = if log_x { 10f64.powf(p.x) } else { p.x };
                ui.label(RichText::new(format!("{} {:.1} dBV", format_si(f, "Hz"), p.y)).weak());
            }
            if ui
                .checkbox(&mut self.fft_log_x, "log frequency axis")
                .changed()
            {
                ui.close();
            }
            if ui
                .checkbox(&mut self.fft_auto_y, "auto vertical range")
                .changed()
            {
                ui.close();
            }
            if self.view.fft_gate.is_some() && ui.button("whole record").clicked() {
                self.view.fft_gate = None;
                ui.close();
            }
            ui.label(RichText::new("double-click: reset view").weak());
        });
    }
}

struct Snapshot {
    frame: Option<Arc<Frame>>,
    status: Option<ScopeStatus>,
    info: Option<mho_service::ServiceInfo>,
    live: Option<LiveConfig>,
    network: Option<NetworkState>,
    groups: Option<Vec<GroupInfo>>,
    group_values: Option<(String, u8, ParamMap)>,
    stats: Vec<MeasureStats>,
    decode: Option<(u8, Arc<DecodeTable>)>,
    math_scale: std::collections::HashMap<String, (f64, f64)>,
    scope_peaks: Option<(u8, Vec<mho_dsp::ScopePeak>)>,
    readouts: Option<Vec<mho_driver::ops::ReadoutInfo>>,
    bode: Option<Vec<mho_driver::BodePoint>>,
    bode_previous: Option<Vec<mho_driver::BodePoint>>,
    bode_running: bool,
    bode_live: Option<(u64, BodeRequest, usize)>,
    capabilities: Option<Capabilities>,
    generator_output: [Option<bool>; 2],
    options: Option<Vec<OptionInfo>>,
    commands: Option<Vec<mho_driver::ops::CommandInfo>>,
    readout_values: Option<(String, ParamMap)>,
    measurements: Option<Arc<mho_service::MeasureSnapshot>>,
    live_error: Option<String>,
    ws_connected: bool,
    progress: Option<mho_service::Progress>,
    busy: usize,
    log: Vec<(String, bool)>,
}

/// Points of a memory depth entry (`10k` → 1e4), `None` for AUTO.
fn depth_parse(d: &str) -> Option<f64> {
    parse_si(d)
}

/// Parses numbers with optional SI prefix and unit, e.g. `1.5m`, `200 mV`, `2us`.
pub(crate) fn parse_si(s: &str) -> Option<f64> {
    let s = s.trim().trim_end_matches(['V', 's', 'S', 'z', 'H']).trim();
    let (num, mult) = match s.chars().last()? {
        'G' => (&s[..s.len() - 1], 1e9),
        'M' => (&s[..s.len() - 1], 1e6),
        'k' => (&s[..s.len() - 1], 1e3),
        'm' => (&s[..s.len() - 1], 1e-3),
        'u' => (&s[..s.len() - 1], 1e-6),
        'µ' => (&s[..s.len() - 'µ'.len_utf8()], 1e-6),
        'n' => (&s[..s.len() - 1], 1e-9),
        'p' => (&s[..s.len() - 1], 1e-12),
        _ => (s, 1.0),
    };
    num.trim().parse::<f64>().ok().map(|v| v * mult)
}

impl eframe::App for ScopeApp {
    fn ui(&mut self, ui: &mut egui::Ui, _frame: &mut eframe::Frame) {
        let s = self.snapshot();
        self.load_capabilities(&s);
        self.track_math_scale(&s);
        self.persist(false);
        let mut local: Vec<(String, Measurements)> = s
            .frame
            .as_ref()
            .map(|f| {
                f.traces
                    .iter()
                    .map(|t| (t.source.clone(), t.analyze()))
                    .collect()
            })
            .unwrap_or_default();
        // Measurements of the region between the cursors ("Measure A-B" in the plot menu).
        if self.view.measure_gate
            && let (Some((a, b)), Some(f)) = (self.view.gate(), &s.frame)
        {
            for t in f.traces.iter().filter(|t| t.has_data()) {
                if let Some(v) = gated_volts(t, a, b) {
                    let m = mho_dsp::analyze(&v, t.preamble.x_increment);
                    local.push((format!("{} [A-B]", t.source), m));
                }
            }
        }

        egui::Panel::top("header").show(ui, |ui| self.header(ui, &s));
        egui::Panel::right("controls")
            .resizable(true)
            .default_size(320.0)
            .show(ui, |ui| {
                egui::ScrollArea::vertical().show(ui, |ui| self.controls(ui, &s));
            });
        egui::Panel::bottom("measurements")
            .resizable(true)
            .show(ui, |ui| {
                ui.horizontal(|ui| {
                    ui.selectable_value(&mut self.y_mode, YMode::Divisions, "div");
                    ui.selectable_value(&mut self.y_mode, YMode::Volts, "volts");
                    ui.separator();
                    ui.checkbox(&mut self.show_fft, "FFT");
                    ui.checkbox(&mut self.show_decoder, "Decoder");
                    ui.separator();
                    self.view_toolbar(ui);
                    ui.separator();
                    if let Some(f) = &s.frame {
                        self.cursor_readout(ui, f, &s);
                    }
                });
                ui.separator();
                self.measurements_panel(ui, &s, &local);
                if self.show_decoder {
                    ui.separator();
                    self.decoder_panel(ui, &s);
                }
            });
        // A sweep started anywhere (GUI, CLI, REST, MCP) opens the window.
        if std::mem::take(&mut self.client.lock().bode_new) {
            self.show_bode = true;
        }
        if self.show_bode
            && let Some(pts) = &s.bode
        {
            let mut open = true;
            egui::Window::new("Bode plot")
                .open(&mut open)
                .default_size([560.0, 440.0])
                .show(ui.ctx(), |ui| {
                    if let Some((id, req, total)) = &s.bode_live {
                        let follow = req
                            .follow
                            .map(|g| format!(", G{} in step", if g == 2 { "II" } else { "I" }))
                            .unwrap_or_default();
                        let state = if s.bode_running {
                            format!("running {}/{total}", pts.len())
                        } else {
                            format!("{} points", pts.len())
                        };
                        ui.label(
                            RichText::new(format!(
                                "sweep #{id}: {} ... {}, {} Vpp, G{} -> CH{}/CH{}{follow} - {state}",
                                format_si(req.start, "Hz"),
                                format_si(req.stop, "Hz"),
                                req.amplitude,
                                if req.generator == 2 { "II" } else { "I" },
                                req.input,
                                req.output
                            ))
                            .small(),
                        );
                    }
                    bode_plot(
                        ui,
                        pts,
                        s.bode_previous.as_deref(),
                        &self.client,
                        &self.capture_dir,
                    )
                });
            self.show_bode = open;
        }
        egui::CentralPanel::default().show(ui, |ui| {
            let total = ui.available_height();
            let main_h = if self.show_fft { total * 0.62 } else { total };
            self.scope_plot(ui, &s, main_h);
            if self.show_fft
                && let Some(f) = &s.frame
            {
                self.fft_plot(ui, f, &s);
            }
        });
    }

    fn on_exit(&mut self, _gl: Option<&eframe::glow::Context>) {
        self.persist(true);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn steps() {
        assert!((step_125(1.0, true) - 2.0).abs() < 1e-12);
        assert!((step_125(2.0, true) - 5.0).abs() < 1e-12);
        assert!((step_125(5.0, true) - 10.0).abs() < 1e-12);
        assert!((step_125(0.05, false) - 0.02).abs() < 1e-12);
        assert!((step_125(0.1, false) - 0.05).abs() < 1e-12);
        assert!((step_125(200e-6, true) - 500e-6).abs() < 1e-15);
    }

    #[test]
    fn si_parse() {
        assert_eq!(parse_si("1.5m"), Some(1.5e-3));
        assert_eq!(parse_si("200 mV"), Some(0.2));
        assert_eq!(parse_si("2us"), Some(2e-6));
        assert_eq!(parse_si("-3"), Some(-3.0));
    }
}
