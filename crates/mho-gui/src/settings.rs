//! Generic editor for the table-driven settings groups (trigger types, measurement setup,
//! counter, voltmeter, decoders, …). Widgets are derived from the group catalogue.

use std::collections::BTreeMap;

use eframe::egui::{self, Color32, RichText};
use mho_driver::params::{GroupInfo, ParamInfo, ParamMap, ParamValue, short_form};
use mho_service::ScopeApi;

use crate::app::parse_si;
use crate::state::{Client, Shared};

#[derive(Default)]
pub struct SettingsPanel {
    pub selected: Option<String>,
    instance: u8,
    /// Group/instance the fields were loaded for.
    loaded_for: Option<(String, u8)>,
    fields: BTreeMap<String, String>,
    original: BTreeMap<String, String>,
    activate: bool,
    /// Group requested before the catalogue arrived: `(id, instance, activate)`.
    pending: Option<(String, Option<u8>, bool)>,
}

fn text_of(v: &ParamValue) -> String {
    match v {
        ParamValue::Bool(b) => b.to_string(),
        ParamValue::Int(i) => i.to_string(),
        ParamValue::Real(x) => format!("{x}"),
        ParamValue::Text(t) => t.clone(),
    }
}

fn to_value(p: &ParamInfo, text: &str) -> Result<ParamValue, String> {
    match p.kind.as_str() {
        "bool" => Ok(ParamValue::Bool(text == "true")),
        "int" => text
            .trim()
            .parse::<u64>()
            .map(ParamValue::Int)
            .map_err(|_| format!("{}: integer expected", p.name)),
        "real" | "source_level" => parse_si(text)
            .map(ParamValue::Real)
            .ok_or_else(|| format!("{}: number expected", p.name)),
        _ => Ok(ParamValue::Text(text.trim().to_string())),
    }
}

impl SettingsPanel {
    /// Selects a group and requests its values (verified groups only; unverified ones would
    /// cost the scope's unknown-query budget). `activate` preselects "make active".
    pub fn open(&mut self, client: &Client, group: &GroupInfo, activate: bool) {
        self.open_at(client, group, None, activate);
    }

    /// Opens group `id` (instance `instance`) as soon as the catalogue is available.
    pub fn request(&mut self, id: &str, instance: Option<u8>, activate: bool) {
        self.selected = Some(id.to_string());
        self.pending = Some((id.to_string(), instance, activate));
    }

    fn open_at(
        &mut self,
        client: &Client,
        group: &GroupInfo,
        instance: Option<u8>,
        activate: bool,
    ) {
        self.selected = Some(group.id.clone());
        self.instance = match (group.instances, instance) {
            (Some((lo, hi)), Some(n)) if (lo..=hi).contains(&n) => n,
            (Some((lo, _)), _) => lo,
            (None, _) => 1,
        };
        self.loaded_for = None;
        self.fields.clear();
        self.original.clear();
        self.activate = activate;
        self.load(client, group);
    }

    fn load(&mut self, client: &Client, group: &GroupInfo) {
        if !group.verified {
            return;
        }
        let id = group.id.clone();
        let n = self.instance;
        let inst = group.instances.map(|_| n);
        client.op(
            format!("read {id}"),
            move |api| async move {
                let values = api.group_get(id.clone(), inst).await?;
                Ok((id, n, values))
            },
            |sh: &mut Shared, v: (String, u8, ParamMap)| sh.group_values = Some(v),
        );
    }

    pub fn ui(
        &mut self,
        ui: &mut egui::Ui,
        client: &Client,
        groups: Option<&[GroupInfo]>,
        values: Option<&(String, u8, ParamMap)>,
    ) {
        let Some(groups) = groups else {
            ui.label("loading settings catalogue…");
            return;
        };
        if let Some((id, n, activate)) = self.pending.take() {
            match groups.iter().find(|g| g.id == id) {
                Some(g) => self.open_at(client, g, n, activate),
                None => client
                    .lock()
                    .log(format!("unknown settings group {id}"), true),
            }
        }
        let current = self
            .selected
            .clone()
            .unwrap_or_else(|| "select a group".into());
        egui::ComboBox::from_id_salt("settings_group")
            .selected_text(&current)
            .width(220.0)
            .show_ui(ui, |ui| {
                for g in groups.iter().filter(|g| g.available) {
                    let label =
                        format!("{}  {}", g.id, if g.verified { "" } else { "(unverified)" });
                    if ui
                        .selectable_label(self.selected.as_deref() == Some(g.id.as_str()), label)
                        .on_hover_text(&g.title)
                        .clicked()
                    {
                        self.open(client, g, g.id.starts_with("trigger."));
                    }
                }
            });
        let Some(group) = self
            .selected
            .as_ref()
            .and_then(|id| groups.iter().find(|g| &g.id == id))
        else {
            return;
        };
        ui.label(RichText::new(&group.title).small().weak());
        if let Some((lo, hi)) = group.instances {
            ui.horizontal(|ui| {
                ui.label("instance");
                for n in lo..=hi {
                    if ui
                        .selectable_label(self.instance == n, n.to_string())
                        .clicked()
                    {
                        self.instance = n;
                        self.loaded_for = None;
                        self.load(client, group);
                    }
                }
            });
        }

        // Take over freshly read values.
        if let Some((id, n, map)) = values
            && id == &group.id
            && *n == self.instance
            && self.loaded_for.as_ref() != Some(&(id.clone(), *n))
        {
            self.fields = map.iter().map(|(k, v)| (k.clone(), text_of(v))).collect();
            self.original = self.fields.clone();
            self.loaded_for = Some((id.clone(), *n));
        }
        if !group.verified {
            ui.label(
                RichText::new(
                    "not yet verified on this scope: values cannot be read, only applied",
                )
                .small()
                .color(Color32::YELLOW),
            );
        }

        egui::Grid::new(("settings_grid", &group.id))
            .num_columns(2)
            .striped(true)
            .show(ui, |ui| {
                for p in &group.params {
                    ui.label(&p.name).on_hover_text(&p.help);
                    let field = self.fields.entry(p.name.clone()).or_default();
                    match p.kind.as_str() {
                        "bool" => {
                            let mut b = field == "true";
                            if ui.checkbox(&mut b, "").changed() {
                                *field = b.to_string();
                            }
                        }
                        "enum" | "source" => {
                            let shown = p
                                .values
                                .iter()
                                .find(|v| short_form(v) == *field || v.as_str() == field)
                                .cloned()
                                .unwrap_or_else(|| field.clone());
                            egui::ComboBox::from_id_salt(("param", &group.id, &p.name))
                                .selected_text(shown)
                                .width(130.0)
                                .show_ui(ui, |ui| {
                                    for v in &p.values {
                                        let token = if p.kind == "enum" {
                                            short_form(v)
                                        } else {
                                            v.clone()
                                        };
                                        if ui.selectable_label(*field == token, v).clicked() {
                                            *field = token;
                                        }
                                    }
                                });
                        }
                        _ => {
                            let hint = match (p.min, p.max, p.unit.as_deref()) {
                                (Some(a), Some(b), u) => format!("{a} … {b} {}", u.unwrap_or("")),
                                _ => String::new(),
                            };
                            ui.add(
                                egui::TextEdit::singleline(field)
                                    .desired_width(130.0)
                                    .hint_text(hint),
                            );
                        }
                    }
                    ui.end_row();
                }
            });

        ui.horizontal(|ui| {
            ui.checkbox(&mut self.activate, "activate")
                .on_hover_text("make this the active mode (e.g. trigger type)");
            let changed: Vec<&ParamInfo> = group
                .params
                .iter()
                .filter(|p| self.fields.get(&p.name) != self.original.get(&p.name))
                .collect();
            let label = if changed.is_empty() {
                "Apply".to_string()
            } else {
                format!("Apply ({})", changed.len())
            };
            if ui.button(label).clicked() {
                let mut map = ParamMap::new();
                let mut error = None;
                for p in &changed {
                    let text = self.fields.get(&p.name).cloned().unwrap_or_default();
                    if text.is_empty() {
                        continue;
                    }
                    match to_value(p, &text) {
                        Ok(v) => {
                            map.insert(p.name.clone(), v);
                        }
                        Err(e) => error = Some(e),
                    }
                    // A level belongs to a source: send the source with it.
                    if p.kind == "source_level"
                        && let Some(src) = group.params.iter().find(|q| q.kind == "source")
                        && let Some(t) = self.fields.get(&src.name)
                    {
                        map.insert(src.name.clone(), ParamValue::Text(t.clone()));
                    }
                }
                if let Some(e) = error {
                    client.lock().log(e, true);
                } else {
                    let id = group.id.clone();
                    let inst = group.instances.map(|_| self.instance);
                    let activate = self.activate;
                    let n = self.instance;
                    client.op(
                        format!("apply {id}"),
                        move |api| async move {
                            let r = api.group_set(id.clone(), inst, map, activate).await?;
                            Ok((id, n, r))
                        },
                        |sh: &mut Shared, (id, n, r): (String, u8, Option<ParamMap>)| {
                            sh.log(format!("{id} applied"), false);
                            sh.status_dirty = true;
                            if let Some(map) = r {
                                sh.group_values = Some((id, n, map));
                            }
                        },
                    );
                    self.loaded_for = None;
                    self.original = self.fields.clone();
                }
            }
            if group.verified && ui.button("Reload").clicked() {
                self.loaded_for = None;
                self.load(client, group);
            }
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn info(kind: &str) -> ParamInfo {
        ParamInfo {
            name: "x".into(),
            node: ":X".into(),
            kind: kind.into(),
            min: None,
            max: None,
            unit: None,
            values: vec![],
            help: String::new(),
        }
    }

    #[test]
    fn values_from_text() {
        assert_eq!(
            to_value(&info("real"), "2u").unwrap(),
            ParamValue::Real(2e-6)
        );
        assert_eq!(to_value(&info("int"), "12").unwrap(), ParamValue::Int(12));
        assert!(to_value(&info("int"), "1.5").is_err());
        assert!(to_value(&info("real"), "abc").is_err());
        assert_eq!(
            to_value(&info("bool"), "true").unwrap(),
            ParamValue::Bool(true)
        );
        assert_eq!(
            to_value(&info("enum"), " POS ").unwrap(),
            ParamValue::Text("POS".into())
        );
    }
}
