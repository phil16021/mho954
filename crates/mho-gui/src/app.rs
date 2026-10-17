use std::path::PathBuf;
use std::sync::Arc;

use eframe::egui::{self, Color32, RichText};
use egui_plot::{GridMark, HLine, Legend, Line, LineStyle, Plot, PlotBounds, PlotPoints, VLine};
use mho_driver::params::{GroupInfo, ParamMap};
use mho_driver::*;
use mho_dsp::{Measurements, Window, format_si};
use mho_service::{Frame, LiveConfig, MeasureSpec, ScopeApi, StatsRequest};

use crate::settings::SettingsPanel;
use crate::state::{Client, Shared};

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

fn channel_color(source: &str) -> Color32 {
    channel_of(source)
        .map(|n| CH_COLORS[(n - 1) as usize])
        .unwrap_or(Color32::LIGHT_GRAY)
}

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
    fft_source: String,
    fft_window: Window,
    cursor_a: Option<f64>,
    cursor_b: Option<f64>,
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
        ScopeApp {
            client: Client::new(&url, token, rt, cc.egui_ctx.clone()),
            capture_dir,
            y_mode: YMode::Divisions,
            show_fft: false,
            fft_source: "CHAN1".into(),
            fft_window: Window::Hann,
            cursor_a: None,
            cursor_b: None,
            offset_edit: [None; 4],
            tb_offset_edit: None,
            trig_level_edit: None,
            new_meas_item: "VRMS".into(),
            new_meas_source: "CHAN1".into(),
            raw_source: "CHAN1".into(),
            last_x_range: None,
            net_form: NetForm::default(),
            settings: SettingsPanel::default(),
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
            measurements: sh.measurements.clone(),
            live_error: sh.live_error.clone(),
            ws_connected: sh.ws_connected,
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
                sh.log(format!("RAW {n} pts → {}", p.display()), false)
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
        self.trigger_type(ui, s, &st.trigger.mode);
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
        ui.horizontal(|ui| {
            egui::ComboBox::from_id_salt("mdepth")
                .selected_text("depth")
                .width(70.0)
                .show_ui(ui, |ui| {
                    for d in DEPTHS {
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
        egui::CollapsingHeader::new(RichText::new("Network (RJ45)").strong())
            .default_open(false)
            .show(ui, |ui| {
                self.network_controls(ui, s);
            });

        ui.separator();
        for (msg, err) in &s.log {
            ui.label(RichText::new(msg).small().color(if *err {
                Color32::LIGHT_RED
            } else {
                Color32::GRAY
            }));
        }
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
    fn trigger_type(&mut self, ui: &mut egui::Ui, s: &Snapshot, mode: &str) {
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
                            self.open_settings(s, id, true);
                        }
                    }
                });
            if let Some((_, id)) = types.iter().find(|(t, _)| matches(t))
                && ui.small_button("edit…").clicked()
            {
                self.open_settings(s, id, false);
            }
        });
    }

    /// Opens the settings panel at group `id` (the catalogue is loaded on demand).
    fn open_settings(&mut self, s: &Snapshot, id: &str, activate: bool) {
        self.load_groups();
        self.settings_open = Some(true);
        match s
            .groups
            .as_ref()
            .and_then(|g| g.iter().find(|g| g.id == id))
        {
            Some(g) => self.settings.open(&self.client, g, activate),
            None => self.settings.selected = Some(id.to_string()),
        }
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

        let mut plot = Plot::new("scope")
            .height(height)
            .legend(Legend::default())
            .show_background(true)
            .allow_double_click_reset(true)
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
                let start = t0 + ((lo - t0) / tdiv).floor() * tdiv;
                let mut marks = Vec::new();
                let mut x = start;
                while x <= hi && marks.len() < 200 {
                    marks.push(GridMark {
                        value: x,
                        step_size: tdiv,
                    });
                    x += tdiv;
                }
                marks
            });
        plot = match y_mode {
            YMode::Divisions => plot
                .y_axis_formatter(|m, _| format!("{:+.0}", m.value))
                .y_grid_spacer(|input| {
                    let (lo, hi) = input.bounds;
                    (lo.floor() as i64..=hi.ceil() as i64)
                        .map(|v| GridMark {
                            value: v as f64,
                            step_size: 1.0,
                        })
                        .collect()
                })
                .default_y_bounds(-V_DIVS / 2.0, V_DIVS / 2.0),
            YMode::Volts => plot.y_axis_formatter(|m, _| format_si(m.value, "V")),
        };
        plot = plot.default_x_bounds(t0, t1);

        let reset = self.last_x_range != Some((t0, t1));
        self.last_x_range = Some((t0, t1));
        let traces = &frame.traces;
        let trig = status.map(|s| s.trigger.clone());
        let (cursor_a, cursor_b) = (self.cursor_a, self.cursor_b);

        let resp = plot.show(ui, |pui| {
            if reset {
                let y = match y_mode {
                    YMode::Divisions => (-V_DIVS / 2.0, V_DIVS / 2.0),
                    YMode::Volts => (pui.plot_bounds().min()[1], pui.plot_bounds().max()[1]),
                };
                pui.set_plot_bounds(PlotBounds::from_min_max([t0, y.0], [t1, y.1]));
                if y_mode == YMode::Volts {
                    pui.set_auto_bounds([false, true]);
                }
            }
            for w in traces.iter().filter(|w| w.has_data()) {
                let cs = chan_state(&w.source);
                let pts: PlotPoints = (0..w.raw.len())
                    .map(|i| {
                        let v = w.preamble.volts(w.raw[i]);
                        let y = match (y_mode, &cs) {
                            (YMode::Divisions, Some(c)) => (v + c.offset) / c.scale,
                            _ => v,
                        };
                        [w.preamble.time(i), y]
                    })
                    .collect();
                pui.line(
                    Line::new(w.source.clone(), pts)
                        .color(channel_color(&w.source))
                        .width(1.2),
                );
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
            if let Some(a) = cursor_a {
                pui.vline(VLine::new("cursor A", a).color(Color32::WHITE));
            }
            if let Some(b) = cursor_b {
                pui.vline(
                    VLine::new("cursor B", b)
                        .color(Color32::LIGHT_GRAY)
                        .style(LineStyle::dashed_dense()),
                );
            }
            let pointer = pui.pointer_coordinate();
            (
                pointer,
                pui.response().clicked(),
                pui.response().secondary_clicked(),
                pui.response().middle_clicked(),
            )
        });
        let (pointer, left, right, middle) = resp.inner;
        if let Some(p) = pointer {
            if left {
                self.cursor_a = Some(p.x);
            }
            if right {
                self.cursor_b = Some(p.x);
            }
        }
        if middle {
            self.cursor_a = None;
            self.cursor_b = None;
        }
    }

    fn cursor_readout(&self, ui: &mut egui::Ui, frame: &Frame) {
        if self.cursor_a.is_none() && self.cursor_b.is_none() {
            ui.label(RichText::new("left-click: cursor A · right-click: cursor B · middle-click: clear · double-click: reset view").small().weak());
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
            for (name, c) in [("A", self.cursor_a), ("B", self.cursor_b)] {
                if let Some(t) = c {
                    ui.label(RichText::new(format!("{name}: {}", format_si(t, "s"))).strong());
                    for w in frame.traces.iter().filter(|w| w.has_data()) {
                        if let Some(v) = value_at(w, t) {
                            ui.label(
                                RichText::new(format!("{} {}", w.source, format_si(v, "V")))
                                    .color(channel_color(&w.source)),
                            );
                        }
                    }
                    ui.separator();
                }
            }
            if let (Some(a), Some(b)) = (self.cursor_a, self.cursor_b) {
                let dt = b - a;
                ui.label(
                    RichText::new(format!(
                        "Δt {}  1/Δt {}",
                        format_si(dt, "s"),
                        format_si(1.0 / dt.abs(), "Hz")
                    ))
                    .strong(),
                );
                for w in frame.traces.iter().filter(|w| w.has_data()) {
                    if let (Some(va), Some(vb)) = (value_at(w, a), value_at(w, b)) {
                        ui.label(
                            RichText::new(format!("Δ{} {}", w.source, format_si(vb - va, "V")))
                                .color(channel_color(&w.source)),
                        );
                    }
                }
            }
        });
    }

    fn fft_plot(&mut self, ui: &mut egui::Ui, frame: &Frame) {
        ui.horizontal(|ui| {
            ui.label(RichText::new("FFT").strong());
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
        let spec = mho_dsp::spectrum(&w.volts(), w.preamble.x_increment, self.fft_window);
        let pts: PlotPoints = spec
            .dbv
            .iter()
            .enumerate()
            .skip(1)
            .map(|(k, &d)| [spec.frequency(k), d])
            .collect();
        let peaks = spec.peaks(3);
        Plot::new("fft")
            .include_y(-140.0)
            .include_y(10.0)
            .x_axis_formatter(|m, _| format_si(m.value, "Hz"))
            .y_axis_formatter(|m, _| format!("{:.0} dBV", m.value))
            .show(ui, |pui| {
                pui.line(Line::new("spectrum", pts).color(channel_color(&w.source)));
                for p in peaks {
                    pui.text(egui_plot::Text::new(
                        "peak",
                        egui_plot::PlotPoint::new(p.frequency, p.dbv + 3.0),
                        RichText::new(format!("{} {:.1} dBV", format_si(p.frequency, "Hz"), p.dbv))
                            .small(),
                    ));
                }
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
    measurements: Option<Arc<mho_service::MeasureSnapshot>>,
    live_error: Option<String>,
    ws_connected: bool,
    busy: usize,
    log: Vec<(String, bool)>,
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
        let local: Vec<(String, Measurements)> = s
            .frame
            .as_ref()
            .map(|f| {
                f.traces
                    .iter()
                    .map(|t| (t.source.clone(), t.analyze()))
                    .collect()
            })
            .unwrap_or_default();

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
                    ui.separator();
                    if let Some(f) = &s.frame {
                        self.cursor_readout(ui, f);
                    }
                });
                ui.separator();
                self.measurements_panel(ui, &s, &local);
            });
        egui::CentralPanel::default().show(ui, |ui| {
            let total = ui.available_height();
            let main_h = if self.show_fft { total * 0.62 } else { total };
            self.scope_plot(ui, &s, main_h);
            if self.show_fft
                && let Some(f) = &s.frame
            {
                self.fft_plot(ui, f);
            }
        });
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
