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
    command: String,
    path: String,
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

    pub fn ui(
        &mut self,
        ui: &mut egui::Ui,
        client: &Client,
        readouts: Option<&[ReadoutInfo]>,
        commands: Option<&[CommandInfo]>,
        values: Option<&(String, ParamMap)>,
    ) {
        let (Some(readouts), Some(commands)) = (readouts, commands) else {
            ui.label("loading…");
            return;
        };

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
                    for r in readouts {
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
                    current.is_some_and(|r| r.verified),
                    egui::Button::new("Read"),
                )
                .on_disabled_hover_text("not yet verified on this scope (mho954 verify)")
                .clicked()
            {
                let id = self.readout.clone();
                client.op(
                    format!("read {id}"),
                    move |api| async move {
                        let v = api.readout(id.clone()).await?;
                        Ok((id, v))
                    },
                    |sh: &mut Shared, v: (String, ParamMap)| sh.readout_values = Some(v),
                );
            }
        });
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
                    for c in commands {
                        if ui
                            .selectable_value(&mut self.command, c.id.clone(), &c.id)
                            .on_hover_text(&c.title)
                            .clicked()
                        {
                            self.confirm = false;
                            if let Some(ext) = c.path_extensions.as_ref().and_then(|e| e.first())
                                && self.path.is_empty()
                            {
                                self.path = format!("D:/mho954.{ext}");
                            }
                        }
                    }
                });
        });
        let Some(cmd) = commands.iter().find(|c| c.id == self.command) else {
            return;
        };
        ui.label(RichText::new(&cmd.title).small().weak());
        if let Some(ext) = &cmd.path_extensions {
            ui.horizontal(|ui| {
                ui.label("file");
                ui.add(
                    egui::TextEdit::singleline(&mut self.path)
                        .desired_width(170.0)
                        .hint_text(format!("C:/ or D:/name.{}", ext.join("|"))),
                );
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
                    path: cmd.path_extensions.as_ref().map(|_| self.path.clone()),
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
    }
}
