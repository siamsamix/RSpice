//! LTspice-style component property dialogs (Classic theme).
//!
//! Right-clicking a part in the schematic opens "Resistor - R1",
//! "Capacitor - C1", "Inductor - L1" or "Independent Voltage Source - V1".
//! The dialog edits a *copy*; nothing reaches the schematic until OK passes
//! validation, so Cancel (or Esc) leaves the part untouched.
//!
//! Stored as: the main value in [`Component::val`] (for a source the whole
//! SPICE source card), everything else in [`Component::attrs`] — see the
//! docs there for which keys are simulated parasitics and which are
//! annotations.
#![allow(dead_code)]

use circuitsim::netlist::{parse_source_card, ParseOptions};
use circuitsim::units::parse_value;
use egui::{Align2, Color32, Context};

use crate::schematic::{CompKind, Component};

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum PropsOutcome {
    Open,
    Cancel,
    Ok,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Func {
    None,
    Pulse,
    Sine,
    Exp,
    Sffm,
    Am,
    Pwl,
}

impl Func {
    /// SPICE keyword the card is written with.
    fn keyword(self) -> &'static str {
        match self {
            Func::None => "",
            Func::Pulse => "PULSE",
            Func::Sine => "SIN",
            Func::Exp => "EXP",
            Func::Sffm => "SFFM",
            Func::Am => "AM",
            Func::Pwl => "PWL",
        }
    }

    /// (label, required) for each parameter, in card order.
    fn params(self) -> &'static [(&'static str, bool)] {
        match self {
            Func::Pulse => &[
                ("Vinitial[V]:", true),
                ("Von[V]:", true),
                ("Tdelay[s]:", true),
                ("Trise[s]:", true),
                ("Tfall[s]:", true),
                ("Ton[s]:", true),
                ("Tperiod[s]:", true),
            ],
            Func::Sine => &[
                ("DC offset[V]:", true),
                ("Amplitude[V]:", true),
                ("Freq[Hz]:", true),
                ("Tdelay[s]:", false),
                ("Theta[1/s]:", false),
            ],
            Func::Exp => &[
                ("Vinitial[V]:", true),
                ("Vpulsed[V]:", true),
                ("Rise Delay[s]:", true),
                ("Rise Tau[s]:", true),
                ("Fall Delay[s]:", true),
                ("Fall Tau[s]:", true),
            ],
            Func::Sffm => &[
                ("DC offset[V]:", true),
                ("Amplitude[V]:", true),
                ("Carrier Freq[Hz]:", true),
                ("Modulation Index:", true),
                ("Signal Freq[Hz]:", true),
                ("Tdelay[s]:", false),
            ],
            Func::Am => &[
                ("Amplitude[V]:", true),
                ("DC offset[V]:", true),
                ("Modulating Freq[Hz]:", true),
                ("Carrier Freq[Hz]:", true),
                ("Tdelay[s]:", false),
            ],
            Func::None | Func::Pwl => &[],
        }
    }

    fn from_keyword(word: &str) -> Option<Func> {
        Some(match word.to_ascii_lowercase().as_str() {
            "pulse" => Func::Pulse,
            "sin" | "sine" => Func::Sine,
            "exp" => Func::Exp,
            "sffm" => Func::Sffm,
            "am" => Func::Am,
            "pwl" => Func::Pwl,
            _ => return None,
        })
    }
}

/// A source card split into the text the dialog fields show.
#[derive(Clone, Debug, Default, PartialEq)]
pub(crate) struct SourceText {
    pub dc: String,
    pub ac_amp: String,
    pub ac_phase: String,
    pub func: Option<Func>,
    pub params: Vec<String>,
}

/// Split a source card without interpreting numbers, so the fields keep the
/// user's own spelling (`1k`, `5m`). Unknown text lands in `dc` untouched and
/// is reported by validation rather than lost.
pub(crate) fn split_source_text(card: &str) -> SourceText {
    let mut out = SourceText::default();
    let text = card.trim();
    // Find the waveform group: keyword followed by `(`, at a word start.
    let lower = text.to_ascii_lowercase();
    let mut group: Option<(usize, usize, Func)> = None;
    for (i, _) in lower.char_indices() {
        if i > 0 && !lower.as_bytes()[i - 1].is_ascii_whitespace() {
            continue;
        }
        let rest = &lower[i..];
        let word_len = rest.chars().take_while(|c| c.is_ascii_alphabetic()).count();
        if word_len == 0 {
            continue;
        }
        let Some(f) = Func::from_keyword(&rest[..word_len]) else { continue };
        if !rest[word_len..].trim_start().starts_with('(') {
            continue;
        }
        let open = i + word_len + rest[word_len..].find('(').unwrap_or(0);
        let close = lower[open..].find(')').map_or(lower.len(), |c| open + c + 1);
        group = Some((i, close, f));
        break;
    }
    let mut remainder = text.to_string();
    if let Some((start, end, f)) = group {
        let inner = text[start..end]
            .trim_start_matches(|c: char| c.is_ascii_alphabetic())
            .trim()
            .trim_start_matches('(')
            .trim_end_matches(')');
        out.func = Some(f);
        out.params = inner
            .split(|c: char| c.is_whitespace() || c == ',')
            .filter(|s| !s.is_empty())
            .map(str::to_string)
            .collect();
        if f == Func::Pwl {
            out.params = vec![out.params.join(" ")];
        }
        remainder.replace_range(start..end, " ");
    }
    let tokens: Vec<&str> = remainder.split_whitespace().collect();
    let mut i = 0;
    while i < tokens.len() {
        let t = tokens[i].to_ascii_lowercase();
        if t == "dc" && i + 1 < tokens.len() {
            out.dc = tokens[i + 1].to_string();
            i += 2;
        } else if t == "ac" && i + 1 < tokens.len() {
            out.ac_amp = tokens[i + 1].to_string();
            i += 2;
            if i < tokens.len() && parse_value(tokens[i]).is_ok() {
                out.ac_phase = tokens[i].to_string();
                i += 1;
            }
        } else if out.dc.is_empty() {
            out.dc = tokens[i].to_string();
            i += 1;
        } else {
            i += 1;
        }
    }
    out
}

/// Assemble and validate a source card from the dialog fields. A card that
/// is only a DC level is stored as the bare number, like the importer does.
pub(crate) fn build_source_text(s: &SourceText) -> Result<String, String> {
    let mut parts: Vec<String> = Vec::new();
    let dc = s.dc.trim();
    let amp = s.ac_amp.trim();
    if !dc.is_empty() {
        parts.push(format!("DC {dc}"));
    }
    if !amp.is_empty() {
        let phase = s.ac_phase.trim();
        parts.push(if phase.is_empty() { format!("AC {amp}") } else { format!("AC {amp} {phase}") });
    }
    match s.func {
        None | Some(Func::None) => {}
        Some(Func::Pwl) => {
            let pts: Vec<&str> = s
                .params
                .first()
                .map(|p| p.split(|c: char| c.is_whitespace() || c == ',').filter(|t| !t.is_empty()).collect())
                .unwrap_or_default();
            if pts.len() < 2 || pts.len() % 2 != 0 {
                return Err("PWL needs time/value pairs, e.g. 0 0 1m 5".into());
            }
            parts.push(format!("PWL({})", pts.join(" ")));
        }
        Some(f) => {
            let spec = f.params();
            let mut vals: Vec<String> =
                (0..spec.len()).map(|i| s.params.get(i).map(|v| v.trim().to_string()).unwrap_or_default()).collect();
            while vals.last().map_or(false, |v| v.is_empty()) && !spec[vals.len() - 1].1 {
                vals.pop();
            }
            for (i, v) in vals.iter_mut().enumerate() {
                if v.is_empty() {
                    if spec[i].1 {
                        let label = spec[i].0.trim_end_matches(':');
                        return Err(format!("{} needs a value for {label}", f.keyword()));
                    }
                    *v = "0".into();
                }
            }
            parts.push(format!("{}({})", f.keyword(), vals.join(" ")));
        }
    }
    if parts.is_empty() {
        parts.push("DC 0".into());
    }
    let card = parts.join(" ");
    parse_source_card(&card, &ParseOptions::default()).map_err(|e| e.to_string())?;
    // Plain DC levels keep the bare-number form.
    if parts.len() == 1 && !dc.is_empty() && amp.is_empty() && matches!(s.func, None | Some(Func::None)) {
        return Ok(dc.to_string());
    }
    Ok(card)
}

// ─── The dialog ─────────────────────────────────────────────────────────────

/// One text row of the dialog (`attrs` key, label, whether it is simulated).
struct Row {
    key: &'static str,
    label: &'static str,
    /// Must parse as a non-negative number when non-empty.
    numeric: bool,
    required: bool,
}

const fn row(key: &'static str, label: &'static str, numeric: bool, required: bool) -> Row {
    Row { key, label, numeric, required }
}

fn rows_for(kind: CompKind) -> Vec<Row> {
    match kind {
        CompKind::Resistor => vec![
            row("val", "Resistance[Ω]:", true, true),
            row("tolerance", "Tolerance[%]:", false, false),
            row("power", "Power Rating[W]:", false, false),
        ],
        CompKind::Capacitor => vec![
            row("val", "Capacitance[F]:", true, true),
            row("voltage_rating", "Voltage Rating[V]:", false, false),
            row("rms_current", "RMS Current Rating[A]:", false, false),
            row("rser", "Equiv. Series Resistance (ESR)[Ω]:", true, false),
            row("lser", "Equiv. Series Inductance (ESL)[H]:", true, false),
            row("rpar", "Equiv. Parallel Resistance (EPR)[Ω]:", true, false),
            row("cpar", "Parallel Capacitance[F]:", true, false),
        ],
        CompKind::Inductor => vec![
            row("val", "Inductance[H]:", true, true),
            row("current_rating", "Current Rating[A]:", false, false),
            row("rser", "Series Resistance[Ω]:", true, false),
            row("rpar", "Parallel Resistance[Ω]:", true, false),
            row("cpar", "Parallel Capacitance[F]:", true, false),
        ],
        CompKind::Voltage | CompKind::Ground | CompKind::Part => vec![],
    }
}

fn title_for(kind: CompKind) -> &'static str {
    match kind {
        CompKind::Resistor => "Resistor",
        CompKind::Capacitor => "Capacitor",
        CompKind::Inductor => "Inductor",
        CompKind::Voltage => "Independent Voltage Source",
        CompKind::Ground => "Ground",
        CompKind::Part => "Part",
    }
}

pub struct PropsDialog {
    idx: usize,
    name: String,
    kind: CompKind,
    /// `val` plus the attrs keys, as typed.
    fields: std::collections::BTreeMap<String, String>,
    /// Voltage-source card pieces.
    src: SourceText,
    /// Keys with a validation error (drawn red).
    bad: Vec<String>,
    error: Option<String>,
}

impl PropsDialog {
    /// Open the dialog for component `idx`; `None` for parts without one.
    pub fn open(idx: usize, comp: &Component, name: &str) -> Option<Self> {
        if matches!(comp.kind, CompKind::Ground | CompKind::Part) {
            return None;
        }
        let mut fields = comp.attrs.clone();
        fields.insert("val".into(), comp.val.clone());
        let src = if comp.kind == CompKind::Voltage {
            let mut s = split_source_text(&comp.val);
            if s.func == Some(Func::Pwl) && s.params.len() > 1 {
                s.params = vec![s.params.join(" ")];
            }
            s
        } else {
            SourceText::default()
        };
        Some(Self { idx, name: name.to_string(), kind: comp.kind, fields, src, bad: Vec::new(), error: None })
    }

    pub fn index(&self) -> usize {
        self.idx
    }

    fn is_bad(&self, key: &str) -> bool {
        self.bad.iter().any(|k| k == key)
    }

    fn field(&self, key: &str) -> &str {
        self.fields.get(key).map_or("", String::as_str)
    }

    /// Validate every field; fills `bad`/`error` and reports success.
    fn validate(&mut self) -> bool {
        self.bad.clear();
        self.error = None;
        if self.kind == CompKind::Voltage {
            for (key, text) in [("rser", self.field("rser").to_string()), ("cpar", self.field("cpar").to_string())] {
                if let Some(msg) = numeric_problem(&text) {
                    self.bad.push(key.into());
                    self.error.get_or_insert(format!("{}: {msg}", if key == "rser" { "Series Resistance" } else { "Parallel Capacitance" }));
                }
            }
            if let Err(e) = build_source_text(&self.src) {
                self.bad.push("source".into());
                self.error.get_or_insert(e);
            }
            return self.error.is_none();
        }
        for r in rows_for(self.kind) {
            let text = self.field(r.key).trim();
            if text.is_empty() {
                if r.required {
                    self.bad.push(r.key.into());
                    self.error.get_or_insert(format!("{} is required", r.label.trim_end_matches(':')));
                }
                continue;
            }
            if r.numeric {
                if let Some(msg) = numeric_problem(text) {
                    self.bad.push(r.key.into());
                    self.error.get_or_insert(format!("{}: {msg}", r.label.trim_end_matches(':')));
                }
            }
        }
        self.error.is_none()
    }

    /// Write the validated fields into the component. Call after
    /// [`PropsOutcome::Ok`].
    pub fn apply(&self, comp: &mut Component) {
        let mut attrs = std::collections::BTreeMap::new();
        for (k, v) in &self.fields {
            let v = v.trim();
            if k != "val" && !v.is_empty() {
                attrs.insert(k.clone(), v.to_string());
            }
        }
        if self.kind == CompKind::Voltage {
            if let Ok(card) = build_source_text(&self.src) {
                comp.val = card;
            }
        } else {
            comp.val = self.field("val").trim().to_string();
        }
        comp.attrs = attrs;
    }

    pub fn show(&mut self, ctx: &Context) -> PropsOutcome {
        let mut outcome = PropsOutcome::Open;
        let title = format!("{} - {}", title_for(self.kind), self.name);
        egui::Window::new(title)
            .id(egui::Id::new(("lt_props", self.idx)))
            .collapsible(false)
            .resizable(false)
            .pivot(Align2::CENTER_CENTER)
            .default_pos(ctx.screen_rect().center())
            .show(ctx, |ui| {
                ui.set_min_width(if self.kind == CompKind::Voltage { 520.0 } else { 400.0 });
                if self.kind == CompKind::Voltage {
                    self.source_body(ui);
                } else {
                    self.part_body(ui);
                }
                ui.add_space(6.0);
                if let Some(e) = &self.error {
                    ui.colored_label(Color32::from_rgb(190, 0, 0), e);
                }
                ui.horizontal(|ui| {
                    if ui.button("  OK  ").clicked() {
                        outcome = PropsOutcome::Ok;
                    }
                    if ui.button("Cancel").clicked() {
                        outcome = PropsOutcome::Cancel;
                    }
                });
                let (enter, esc) =
                    ui.input(|i| (i.key_pressed(egui::Key::Enter), i.key_pressed(egui::Key::Escape)));
                if enter {
                    outcome = PropsOutcome::Ok;
                }
                if esc {
                    outcome = PropsOutcome::Cancel;
                }
            });
        if outcome == PropsOutcome::Ok && !self.validate() {
            return PropsOutcome::Open;
        }
        outcome
    }

    fn text(&mut self, ui: &mut egui::Ui, key: &str, width: f32) {
        let bad = self.is_bad(key);
        let entry = self.fields.entry(key.to_string()).or_default();
        let edit = egui::TextEdit::singleline(entry).desired_width(width).text_color(if bad {
            Color32::from_rgb(190, 0, 0)
        } else {
            Color32::BLACK
        });
        ui.add(edit);
    }

    fn part_body(&mut self, ui: &mut egui::Ui) {
        let (select_label, manufacturer) = match self.kind {
            CompKind::Resistor => (Some("Select Resistor"), true),
            CompKind::Capacitor => (Some("Select Capacitor"), true),
            _ => (None, false),
        };
        egui::Grid::new("lt_props_grid").num_columns(2).spacing([10.0, 5.0]).show(ui, |ui| {
            for r in rows_for(self.kind) {
                ui.label(r.label);
                self.text(ui, r.key, 150.0);
                ui.end_row();
            }
            if manufacturer {
                ui.label("Manufacturer:");
                self.text(ui, "manufacturer", 150.0);
                ui.end_row();
                ui.label("Part No.:");
                self.text(ui, "part", 150.0);
                ui.end_row();
            }
        });
        if let Some(label) = select_label {
            ui.add_space(4.0);
            ui.add_enabled(false, egui::Button::new(label))
                .on_disabled_hover_text("Part libraries are not available in RSpice");
        }
    }

    fn source_body(&mut self, ui: &mut egui::Ui) {
        ui.label("DC value / Functions:");
        let mut func = self.src.func.unwrap_or(Func::None);
        ui.horizontal_wrapped(|ui| {
            for (f, label) in [
                (Func::None, "(none)"),
                (Func::Pulse, "PULSE(V1 V2 Tdelay Trise Tfall Ton Tperiod)"),
            ] {
                ui.radio_value(&mut func, f, label);
            }
        });
        ui.horizontal_wrapped(|ui| {
            for (f, label) in [
                (Func::Sine, "SINE(Voffset Vamp Freq Td Theta)"),
                (Func::Exp, "EXP(V1 V2 Td1 Tau1 Td2 Tau2)"),
            ] {
                ui.radio_value(&mut func, f, label);
            }
        });
        ui.horizontal_wrapped(|ui| {
            for (f, label) in [
                (Func::Sffm, "SFFM(Voff Vamp Fcar MDI Fsig)"),
                (Func::Am, "AM(Vamp Voff Fmod Fcar Td)"),
                (Func::Pwl, "PWL(t1 v1 t2 v2 ...)"),
            ] {
                ui.radio_value(&mut func, f, label);
            }
            ui.add_enabled(false, egui::RadioButton::new(false, "PWL FILE"))
                .on_disabled_hover_text("PWL files are not supported yet");
        });
        let previous = self.src.func.unwrap_or(Func::None);
        if func != previous {
            self.src.func = (func != Func::None).then_some(func);
            self.src.params = vec![String::new(); func.params().len().max(usize::from(func == Func::Pwl))];
        }
        ui.add_space(4.0);

        let bad_src = self.is_bad("source");
        let red = Color32::from_rgb(190, 0, 0);
        fn edit_box(s: &mut String, w: f32, bad: bool) -> egui::TextEdit<'_> {
            egui::TextEdit::singleline(s)
                .desired_width(w)
                .text_color(if bad { Color32::from_rgb(190, 0, 0) } else { Color32::BLACK })
        }
        let _ = red;
        if func == Func::Pwl {
            ui.horizontal(|ui| {
                ui.label("Time/value pairs:");
                if self.src.params.is_empty() {
                    self.src.params.push(String::new());
                }
                ui.add(edit_box(&mut self.src.params[0], 340.0, bad_src));
            });
        } else if func != Func::None {
            let spec = func.params();
            self.src.params.resize(spec.len(), String::new());
            egui::Grid::new("lt_src_params").num_columns(4).spacing([10.0, 4.0]).show(ui, |ui| {
                for (i, (label, _)) in spec.iter().enumerate() {
                    ui.label(*label);
                    ui.add(edit_box(&mut self.src.params[i], 110.0, bad_src));
                    if i % 2 == 1 {
                        ui.end_row();
                    }
                }
            });
        }

        ui.add_space(6.0);
        ui.group(|ui| {
            egui::Grid::new("lt_src_levels").num_columns(3).spacing([10.0, 5.0]).show(ui, |ui| {
                ui.label("DC Value[V]:");
                ui.add(edit_box(&mut self.src.dc, 110.0, bad_src));
                self.visible_box(ui, "vis_dc");
                ui.end_row();
                ui.label("Small signal AC analysis(.AC):");
                ui.label("");
                self.visible_box(ui, "vis_ac");
                ui.end_row();
                ui.label("      AC Amplitude:");
                ui.add(edit_box(&mut self.src.ac_amp, 110.0, bad_src));
                ui.label("");
                ui.end_row();
                ui.label("      AC Phase[°]:");
                ui.add(edit_box(&mut self.src.ac_phase, 110.0, bad_src));
                ui.label("");
                ui.end_row();
            });
        });
        ui.add_space(4.0);
        ui.group(|ui| {
            ui.label("Parasitic Properties");
            egui::Grid::new("lt_src_par").num_columns(3).spacing([10.0, 5.0]).show(ui, |ui| {
                ui.label("Series Resistance[Ω]:");
                self.text(ui, "rser", 110.0);
                self.visible_box(ui, "vis_par");
                ui.end_row();
                ui.label("Parallel Capacitance[F]:");
                self.text(ui, "cpar", 110.0);
                ui.label("");
                ui.end_row();
            });
        });
        ui.label(egui::RichText::new("Make this information visible on schematic: ☑").size(10.5));
    }

    fn visible_box(&mut self, ui: &mut egui::Ui, key: &str) {
        let mut on = self.field(key) == "1";
        if ui.checkbox(&mut on, "").changed() {
            self.fields.insert(key.to_string(), if on { "1".into() } else { String::new() });
        }
    }
}

/// `None` when `text` is empty or a finite, non-negative value.
fn numeric_problem(text: &str) -> Option<String> {
    let t = text.trim();
    if t.is_empty() {
        return None;
    }
    match parse_value(t) {
        Ok(v) if v.is_finite() && v >= 0.0 => None,
        Ok(_) => Some(format!("'{t}' must be zero or positive")),
        Err(e) => Some(format!("'{t}': {e}")),
    }
}

// ─── Tests ──────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use crate::schematic::GridPt;

    fn comp(kind: CompKind, val: &str) -> Component {
        Component {
            kind,
            p1: GridPt(0, 0),
            p2: GridPt(0, 4),
            val: val.into(),
            attrs: Default::default(),
            part: None,
        }
    }

    #[test]
    fn splits_plain_dc_and_keeps_user_spelling() {
        let s = split_source_text("5");
        assert_eq!(s.dc, "5");
        assert!(s.func.is_none() && s.ac_amp.is_empty());
        let s = split_source_text("DC 1k AC 1 90");
        assert_eq!((s.dc.as_str(), s.ac_amp.as_str(), s.ac_phase.as_str()), ("1k", "1", "90"));
    }

    #[test]
    fn splits_waveforms_with_commas_and_case() {
        let s = split_source_text("DC 0 AC 1 pulse(0, 5, 0 1n 1n 1m 2m)");
        assert_eq!(s.func, Some(Func::Pulse));
        assert_eq!(s.params, vec!["0", "5", "0", "1n", "1n", "1m", "2m"]);
        assert_eq!(s.ac_amp, "1");
        assert_eq!(s.dc, "0");
        let s = split_source_text("SIN(0 1 1k)");
        assert_eq!(s.func, Some(Func::Sine));
        assert_eq!(s.params.len(), 3);
        // `sine` is accepted on input.
        assert_eq!(split_source_text("SINE(0 1 50)").func, Some(Func::Sine));
        // A node-like word is not mistaken for a keyword.
        assert!(split_source_text("DC 3").func.is_none());
    }

    #[test]
    fn builds_cards_that_the_core_parser_accepts() {
        let mut s = SourceText { dc: "5".into(), ..Default::default() };
        assert_eq!(build_source_text(&s).unwrap(), "5");
        s.ac_amp = "1".into();
        assert_eq!(build_source_text(&s).unwrap(), "DC 5 AC 1");
        s.ac_phase = "45".into();
        assert_eq!(build_source_text(&s).unwrap(), "DC 5 AC 1 45");
        let empty = SourceText::default();
        assert_eq!(build_source_text(&empty).unwrap(), "DC 0");
    }

    #[test]
    fn waveform_cards_round_trip_through_the_dialog_text() {
        for card in [
            "PULSE(0 5 0 1n 1n 1m 2m)",
            "DC 0 AC 1 SIN(0 1 1k)",
            "SIN(0 1 1k 1m 10)",
            "EXP(0 5 1m 1m 5m 2m)",
            "SFFM(0 1 10k 5 1k)",
            "AM(1 0 100 10k)",
            "PWL(0 0 1m 5 2m 0)",
        ] {
            let split = split_source_text(card);
            let rebuilt = build_source_text(&split).unwrap_or_else(|e| panic!("{card}: {e}"));
            let a = parse_source_card(card, &ParseOptions::default()).unwrap();
            let b = parse_source_card(&rebuilt, &ParseOptions::default()).unwrap();
            assert_eq!(format!("{a:?}"), format!("{b:?}"), "{card} → {rebuilt}");
        }
    }

    #[test]
    fn optional_parameters_may_be_blank_but_required_ones_may_not() {
        let mut s = SourceText {
            func: Some(Func::Sine),
            params: vec!["0".into(), "1".into(), "1k".into(), "".into(), "".into()],
            ..Default::default()
        };
        assert_eq!(build_source_text(&s).unwrap(), "DC 0 SIN(0 1 1k)".replace("DC 0 ", ""));
        s.params[3] = "".into();
        s.params[4] = "100".into();
        assert_eq!(build_source_text(&s).unwrap(), "SIN(0 1 1k 0 100)");
        s.params[2] = "".into();
        assert!(build_source_text(&s).unwrap_err().contains("Freq"));
    }

    #[test]
    fn bad_numbers_and_pwl_shapes_are_rejected() {
        let s = SourceText { dc: "abc".into(), ..Default::default() };
        assert!(build_source_text(&s).is_err());
        let s = SourceText { func: Some(Func::Pwl), params: vec!["0 0 1m".into()], ..Default::default() };
        assert!(build_source_text(&s).unwrap_err().contains("pairs"));
        let s = SourceText { func: Some(Func::Pwl), params: vec!["".into()], ..Default::default() };
        assert!(build_source_text(&s).is_err());
    }

    #[test]
    fn resistor_dialog_validates_and_applies() {
        let mut c = comp(CompKind::Resistor, "10k");
        let mut d = PropsDialog::open(3, &c, "R1").unwrap();
        assert_eq!(d.index(), 3);
        d.fields.insert("val".into(), "4.7k".into());
        d.fields.insert("tolerance".into(), "5%".into());
        d.fields.insert("part".into(), "  ".into());
        assert!(d.validate());
        d.apply(&mut c);
        assert_eq!(c.val, "4.7k");
        assert_eq!(c.attrs.get("tolerance").map(String::as_str), Some("5%"));
        assert!(!c.attrs.contains_key("part"), "blank extras are dropped");

        d.fields.insert("val".into(), "".into());
        assert!(!d.validate());
        assert!(d.is_bad("val"));
        d.fields.insert("val".into(), "-3".into());
        assert!(!d.validate());
        d.fields.insert("val".into(), "zzz".into());
        assert!(!d.validate());
    }

    #[test]
    fn capacitor_parasitics_are_numeric_and_annotations_are_free_text() {
        let mut c = comp(CompKind::Capacitor, "100n");
        let mut d = PropsDialog::open(0, &c, "C1").unwrap();
        d.fields.insert("rser".into(), "50m".into());
        d.fields.insert("lser".into(), "2n".into());
        d.fields.insert("voltage_rating".into(), "50V".into());
        assert!(d.validate());
        d.apply(&mut c);
        assert_eq!(c.attrs["rser"], "50m");
        assert_eq!(c.attrs["voltage_rating"], "50V");
        d.fields.insert("cpar".into(), "x".into());
        assert!(!d.validate());
        assert!(d.is_bad("cpar") && !d.is_bad("rser"));
    }

    #[test]
    fn inductor_and_source_dialogs_round_trip() {
        let mut l = comp(CompKind::Inductor, "1m");
        let mut d = PropsDialog::open(1, &l, "L1").unwrap();
        d.fields.insert("rser".into(), "0.1".into());
        assert!(d.validate());
        d.apply(&mut l);
        assert_eq!(l.attrs["rser"], "0.1");

        let mut v = comp(CompKind::Voltage, "DC 0 AC 1 SIN(0 1 1k)");
        let mut d = PropsDialog::open(2, &v, "V1").unwrap();
        assert_eq!(d.src.func, Some(Func::Sine));
        d.src.ac_amp = "2".into();
        d.fields.insert("rser".into(), "10".into());
        d.fields.insert("vis_dc".into(), "1".into());
        assert!(d.validate());
        d.apply(&mut v);
        assert_eq!(v.val, "DC 0 AC 2 SIN(0 1 1k)");
        assert_eq!(v.attrs["rser"], "10");
        assert_eq!(v.attrs["vis_dc"], "1");

        d.src.params[2] = "".into();
        assert!(!d.validate());
        assert!(d.is_bad("source"));
    }

    #[test]
    fn ground_has_no_dialog() {
        assert!(PropsDialog::open(0, &comp(CompKind::Ground, ""), "GND").is_none());
    }

    #[test]
    fn dialogs_render_headless_in_every_shape() {
        let ctx = Context::default();
        crate::theme::install_fonts(&ctx);
        for (kind, val) in [
            (CompKind::Resistor, "1k"),
            (CompKind::Capacitor, "1u"),
            (CompKind::Inductor, "1m"),
            (CompKind::Voltage, "PULSE(0 5 0 1n 1n 1m 2m)"),
            (CompKind::Voltage, "PWL(0 0 1m 5)"),
            (CompKind::Voltage, "5"),
        ] {
            let c = comp(kind, val);
            let mut d = PropsDialog::open(0, &c, "X1").unwrap();
            for _ in 0..2 {
                let _ = ctx.run(egui::RawInput::default(), |ctx| {
                    assert_eq!(d.show(ctx), PropsOutcome::Open);
                });
            }
        }
    }
}
