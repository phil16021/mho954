//! Readouts (cursor values, mask/search/histogram results) and one-shot scope commands
//! (save/load on the scope, resets, recording).

use eframe::egui::{self, Color32, RichText};
use mho_driver::ops::{CommandInfo, ReadoutInfo};
use mho_driver::params::{ParamMap, ParamValue};
use mho_dsp::format_si;
use mho_service::{CommandRequest, ScopeApi};

use crate::state::{Client, Shared};

#[derive(Default)]
pub struct ToolsPanel {
    requested: bool,
    readout: String,
    /// Switch the readout's mode on if it is off (histogram, mask, search, cursor).
    enable_mode: bool,
    command: String,
    path: String,
    /// The path was typed by the user (kept when the command changes).
    path_edited: bool,
    index: u8,
    confirm: bool,
}

fn show(v: &ParamValue, unit: &str) -> String {
    match v {
        ParamValue::Real(x) if !unit.is_empty() => format_si(*x, unit),
        ParamValue::Real(x) => format!("{x:.6e}"),
        ParamValue::Int(i) => i.to_string(),
        ParamValue::Bool(b) => b.to_string(),
        ParamValue::Text(t) => t.clone(),
    }
}

/// Default file for a command: internal memory C:/, which always exists (D:/ needs a USB
/// stick in the scope).
fn default_path(ext: &str) -> String {
    format!("C:/mho954.{ext}")
}

fn read(client: &Client, id: String, enable: bool) {
    let failed_id = id.clone();
    client.op_or(
        format!("read {id}"),
        move |api| async move {
            let v = api.readout(id.clone(), enable).await?;
            Ok((id, v))
        },
        |sh: &mut Shared, v: (String, ParamMap)| {
            sh.readout_error = None;
            sh.readout_values = Some(v);
        },
        move |sh: &mut Shared, e| sh.readout_error = Some((failed_id, e.message.clone())),
    );
}

impl ToolsPanel {
    /// Loads both catalogues once.
    pub fn load(&mut self, client: &Client) {
        if self.requested {
            return;
        }
        self.requested = true;
        client.op(
            "readout catalogue",
            |api| async move { Ok((api.readouts().await?, api.commands().await?)) },
            |sh: &mut Shared, (r, c): (Vec<ReadoutInfo>, Vec<CommandInfo>)| {
                sh.readouts = Some(r);
                sh.commands = Some(c);
            },
        );
    }

    /// Returns a settings group to open (the readout's mode switch) if the user asked for it.
    pub fn ui(
        &mut self,
        ui: &mut egui::Ui,
        client: &Client,
        readouts: Option<&[ReadoutInfo]>,
        commands: Option<&[CommandInfo]>,
        values: Option<&(String, ParamMap)>,
        error: Option<&(String, String)>,
    ) -> Option<String> {
        let (Some(readouts), Some(commands)) = (readouts, commands) else {
            ui.label("loading...");
            return None;
        };
        let mut open_group = None;

        ui.label(RichText::new("Read values").strong());
        ui.horizontal(|ui| {
            egui::ComboBox::from_id_salt("readout_sel")
                .selected_text(if self.readout.is_empty() {
                    "select"
                } else {
                    &self.readout
                })
                .width(150.0)
                .show_ui(ui, |ui| {
                    for r in readouts.iter().filter(|r| r.available) {
                        let label = if r.verified {
                            r.id.clone()
                        } else {
                            format!("{} (unverified)", r.id)
                        };
                        ui.selectable_value(&mut self.readout, r.id.clone(), label)
                            .on_hover_text(&r.title);
                    }
                });
            let current = readouts.iter().find(|r| r.id == self.readout);
            if ui
                .add_enabled(
                    current.is_some_and(|r| r.verified && r.available),
                    egui::Button::new("Read"),
                )
                .on_disabled_hover_text("not yet verified on this scope (mho954 verify)")
                .clicked()
            {
                read(client, self.readout.clone(), self.enable_mode);
            }
            if current.is_some_and(|r| r.can_enable) {
                ui.checkbox(&mut self.enable_mode, "switch on if off")
                    .on_hover_text("switches the mode on the scope on before reading");
            }
        });
        let current = readouts.iter().find(|r| r.id == self.readout);
        if let (Some((id, msg)), Some(r)) = (error, current)
            && *id == r.id
        {
            ui.label(RichText::new(msg).small().color(Color32::YELLOW));
            ui.horizontal(|ui| {
                if r.can_enable
                    && ui
                        .button("Enable and read")
                        .on_hover_text("switches the mode on the scope on, then reads")
                        .clicked()
                {
                    read(client, r.id.clone(), true);
                }
                if let Some(g) = &r.enable_group
                    && ui
                        .button("Settings...")
                        .on_hover_text(format!("open group {g}"))
                        .clicked()
                {
                    open_group = Some(g.clone());
                }
            });
        }
        if let Some((id, map)) = values
            && let Some(info) = readouts.iter().find(|r| &r.id == id)
        {
            egui::Grid::new("readout_values")
                .num_columns(2)
                .striped(true)
                .show(ui, |ui| {
                    for (k, v) in map {
                        let unit = info
                            .items
                            .iter()
                            .find(|(n, _)| n == k)
                            .map(|(_, u)| u.as_str())
                            .unwrap_or("");
                        ui.label(k);
                        ui.label(RichText::new(show(v, unit)).monospace());
                        ui.end_row();
                    }
                });
        }

        ui.separator();
        ui.label(RichText::new("Scope commands").strong());
        ui.horizontal(|ui| {
            egui::ComboBox::from_id_salt("command_sel")
                .selected_text(if self.command.is_empty() {
                    "select"
                } else {
                    &self.command
                })
                .width(150.0)
                .show_ui(ui, |ui| {
                    for c in commands.iter().filter(|c| c.available) {
                        if ui
                            .selectable_value(&mut self.command, c.id.clone(), &c.id)
                            .on_hover_text(&c.title)
                            .clicked()
                        {
                            self.confirm = false;
                            // A default path follows the command's file type; a typed one stays.
                            if let Some(ext) = c.path_extensions.as_ref().and_then(|e| e.first())
                                && (self.path.is_empty() || !self.path_edited)
                            {
                                self.path = default_path(ext);
                                self.path_edited = false;
                            }
                        }
                    }
                });
        });
        let Some(cmd) = commands.iter().find(|c| c.id == self.command) else {
            return open_group;
        };
        ui.label(RichText::new(&cmd.title).small().weak());
        if let Some(pre) = &cmd.precondition {
            ui.label(
                RichText::new(format!("needs: {pre}"))
                    .small()
                    .color(Color32::LIGHT_YELLOW),
            );
        }
        if let Some(ext) = &cmd.path_extensions {
            ui.horizontal(|ui| {
                ui.label("file");
                if ui
                    .add(
                        egui::TextEdit::singleline(&mut self.path)
                            .desired_width(170.0)
                            .hint_text(format!("C:/ or D:/name.{}", ext.join("|"))),
                    )
                    .changed()
                {
                    self.path_edited = true;
                }
            })
            .response
            .on_hover_text("C:/ = internal memory, D:/ = USB stick in the scope");
        }
        if let Some((lo, hi)) = cmd.index {
            self.index = self.index.clamp(lo, hi);
            ui.horizontal(|ui| {
                ui.label("number");
                ui.add(egui::DragValue::new(&mut self.index).range(lo..=hi));
            });
        }
        ui.horizontal(|ui| {
            if cmd.confirm {
                ui.checkbox(
                    &mut self.confirm,
                    RichText::new("I really want this").color(Color32::LIGHT_RED),
                );
            }
            if ui
                .add_enabled(!cmd.confirm || self.confirm, egui::Button::new("Run"))
                .clicked()
            {
                let id = cmd.id.clone();
                let request = CommandRequest {
                    path: match (&cmd.path_extensions, cmd.index) {
                        (Some(_), _) => Some(self.path.clone()),
                        (None, Some(_)) => Some(self.index.to_string()),
                        (None, None) => None,
                    },
                    confirm: self.confirm,
                };
                self.confirm = false;
                client.op(
                    format!("run {id}"),
                    move |api| async move {
                        api.run_command(id.clone(), request).await?;
                        Ok(id)
                    },
                    |sh: &mut Shared, id: String| {
                        sh.log(format!("{id}: done"), false);
                        sh.status_dirty = true;
                    },
                );
            }
        });
        open_group
    }
}
