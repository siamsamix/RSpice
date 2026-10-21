use circuitsim::circuit::{CURRENT_FAMILIES, Circuit as CsCircuit, ElementFamily, Pulse, Sine, Waveform};
use circuitsim::netlist::{emit as netlist_emit, emit_source_value, parse_source_value, ParseOptions, SourceValue};
use circuitsim::units::parse_value;
use circuitsim::{parse, run, AcResult, DcResult, DcSweepResult, SimulationResult, TranResult};
use egui;
use egui_plot::Plot;
use std::collections::HashMap;
use egui::{Color32, Pos2, Rect, Stroke, Vec2};

use crate::schematic::{
    AnalysisType, CompKind, Direction, GridPt, NodeAssignment, SchematicState, Tool,
    GRID_SIZE,
};
use crate::plot::{
    ac_current_series, ac_plot_frame, ac_voltage_series, dc_sweep_series, draw, plot_frame,
    tran_current_series, tran_diff_series, tran_voltage_series, AcCursor, AcMagScale,
    AcQuantity, PlotState,
};

/// Deck-spelling label for a node (`in`, `out`), falling back to its id.
/// A free function so UI closures can capture only the names slice.
fn node_label(names: &[String], i: usize) -> String {
    match names.get(i) {
        Some(s) if !s.is_empty() => s.clone(),
        _ => i.to_string(),
    }
}

/// Grid labels per source family — shared by the read and rebuild paths so
/// the two can never disagree about a field's meaning.
const DC_LABELS: &[&str] = &["Voltage"];
const AC_LABELS: &[&str] = &["Magnitude", "Phase"];
const SIN_LABELS: &[&str] = &["V_offset", "V_amp", "Freq", "T_delay", "Theta"];
const PULSE_LABELS: &[&str] = &[
    "V_initial", "V_on", "T_delay", "T_rise", "T_fall", "T_on", "T_period",
];

/// The dropdown family name for a parsed source value (`"DC"`, `"AC"`,
/// `"SIN"`, `"PULSE"`, or `"Params"` for families without a grid editor).
/// The classification reads the typed [`SourceValue`], not the raw text —
/// `starts_with` guessing is what let a `SFFM(...)` card be rewritten into
/// garbage here.
fn source_value_family(value: &SourceValue) -> &'static str {
    match value {
        SourceValue::Dc { .. } => "DC",
        SourceValue::Ac { .. } => "AC",
        SourceValue::Waveform(Waveform::Sine(_)) => "SIN",
        SourceValue::Waveform(Waveform::Pulse(_)) => "PULSE",
        SourceValue::Waveform(_) => "Params",
    }
}

/// One editable row per parameter: the grid labels plus one field string.
/// Values print through `f64` `Display` — plain decimal notation the value
/// grammar always re-parses, so the round trip never loses a number.
fn source_value_fields(value: &SourceValue) -> (&'static [&'static str], Vec<String>) {
    match value {
        SourceValue::Dc { value } => (DC_LABELS, vec![format!("{value}")]),
        SourceValue::Ac { magnitude, phase } => {
            (AC_LABELS, vec![format!("{magnitude}"), format!("{phase}")])
        }
        SourceValue::Waveform(Waveform::Sine(s)) => (
            SIN_LABELS,
            vec![
                format!("{}", s.vo), format!("{}", s.va), format!("{}", s.freq),
                format!("{}", s.td), format!("{}", s.theta),
            ],
        ),
        SourceValue::Waveform(Waveform::Pulse(p)) => (
            PULSE_LABELS,
            vec![
                format!("{}", p.v1), format!("{}", p.v2), format!("{}", p.td),
                format!("{}", p.tr), format!("{}", p.tf), format!("{}", p.pw),
                format!("{}", p.per),
            ],
        ),
        _ => (&["Params"], vec![]),
    }
}

/// Rebuild a typed [`SourceValue`] from the dropdown family and one string
/// per grid field. Every field re-enters through the shared value grammar,
/// so suffixes (`2u`) work exactly as they do on a deck card.
fn source_value_from_fields(family: &str, fields: &[String]) -> Result<SourceValue, String> {
    let field = |labels: &[&str], i: usize| -> Result<f64, String> {
        let label = labels.get(i).copied().unwrap_or("param");
        let text = fields.get(i).map(|s| s.as_str()).unwrap_or("0");
        let text = if text.is_empty() { "0" } else { text };
        parse_value(text).map_err(|e| format!("{label}: {e}"))
    };
    Ok(match family {
        "DC" => SourceValue::Dc { value: field(DC_LABELS, 0)? },
        "AC" => SourceValue::Ac {
            magnitude: field(AC_LABELS, 0)?,
            phase: field(AC_LABELS, 1)?,
        },
        "SIN" => SourceValue::Waveform(Waveform::Sine(Sine {
            vo: field(SIN_LABELS, 0)?,
            va: field(SIN_LABELS, 1)?,
            freq: field(SIN_LABELS, 2)?,
            td: field(SIN_LABELS, 3)?,
            theta: field(SIN_LABELS, 4)?,
        })),
        "PULSE" => SourceValue::Waveform(Waveform::Pulse(Pulse {
            v1: field(PULSE_LABELS, 0)?,
            v2: field(PULSE_LABELS, 1)?,
            td: field(PULSE_LABELS, 2)?,
            tr: field(PULSE_LABELS, 3)?,
            tf: field(PULSE_LABELS, 4)?,
            pw: field(PULSE_LABELS, 5)?,
            per: field(PULSE_LABELS, 6)?,
        })),
        _ => return Err("no grid editor for this family".into()),
    })
}

#[derive(PartialEq, Eq)]
enum EditorMode { Text, Schematic }


use crate::theme::{
    self, card_frame, code_frame, editor_frame, panel_frame, section_heading, status_chip,
    ACCENT, BG_PANEL, BG_SURFACE, STATUS_ERROR, STATUS_OK, TEXT_SECONDARY,
};

const DEFAULT_NETLIST: &str = r#"* RC charging — edit and press Run (F5)
V1 1 0 DC 5
R1 1 2 1k
C1 2 0 1u
.tran 10u 5m
.end
"#;

#[derive(Clone, Copy, PartialEq, Eq)]
enum Example {
    RcCharge,
    VoltageDivider,
    RlCircuit,
    RcLowPass,
}

impl Example {
    fn netlist(self) -> &'static str {
        match self {
            Example::RcCharge => {
                r#"* RC charging
                V1 1 0 DC 5
                R1 1 2 1k
                C1 2 0 1u
                .tran 10u 5m
                .end
                "#
            }
            Example::VoltageDivider => {
                r#"* Voltage divider
                V1 1 0 DC 10
                R1 1 2 1k
                R2 2 0 1k
                .op
                .end
                "#
            }
            Example::RlCircuit => {
                r#"* RL circuit
                V1 1 0 DC 12
                R1 1 2 100
                L1 2 0 1m
                .op
                .tran 1u 1m
                .end
                "#
            }
            Example::RcLowPass => {
                r#"* RC low-pass filter — frequency sweep
                * Corner frequency: fc = 1/(2π·R·C) ≈ 1.59 kHz
                V1 1 0 AC 1
                R1 1 2 1k
                C1 2 0 100n
                .ac dec 200 10 100k
                .end
                "#
            }
        }
    }

    fn label(self) -> &'static str {
        match self {
            Example::RcCharge => "RC charge",
            Example::VoltageDivider => "Voltage divider",
            Example::RlCircuit => "RL circuit",
            Example::RcLowPass  => "RC low-pass (.ac)",
        }
    }
}

#[derive(Default, Clone, Copy, PartialEq, Eq)]
enum ResultTab {
    #[default]
    Overview,
    Dc,
    Waveforms,
    FreqResponse,
}

#[derive(Clone, Copy, PartialEq, Eq, Default)]
enum TranSubTab {
    #[default]
    Voltage,
    Current,
    Differential,
}

pub struct CircuitSimApp {
    netlist: String,
    file_label: String,
    status: Option<(bool, String)>,
    circuit_summary: Option<String>,
    dc: Option<DcResult>,
    dc_sweep: Option<DcSweepResult>,
    tran: Option<TranResult>,
    ac: Option<AcResult>,
    /// Deck-spelling node names (indexed by node id) for trace labels.
    node_names: Vec<String>,
    /// The circuit the last run parsed, source of the honest element labels
    /// (`I(RLOAD)`); `None` before the first successful parse.
    circuit: Option<CsCircuit>,
    plot_nodes: Vec<bool>,
    dc_plot_nodes: Vec<bool>,
    ac_plot_nodes: Vec<bool>,
    ac_mag_scale: AcMagScale,
    result_tab: ResultTab,
    tran_sub_tab: TranSubTab,
    editor_mode: EditorMode,
    schematic: SchematicState,
    /// Axis scales, bounds, and apply-flags for every plot (one owner).
    plot: PlotState,

    // Differential voltage plot node selectors
    diff_node_a: usize,
    diff_node_b: usize,
}

impl Default for CircuitSimApp {
    fn default() -> Self {
        Self {
            netlist: DEFAULT_NETLIST.to_string(),
            file_label: "untitled.cir".to_string(),
            status: None,
            circuit_summary: None,
            dc: None,
            dc_sweep: None,
            tran: None,
            ac: None,
            node_names: vec!["0".to_string()],
            circuit: None,
            plot_nodes: vec![false; 16],
            dc_plot_nodes: vec![false; 16],
            ac_plot_nodes: vec![false; 16],
            ac_mag_scale: AcMagScale::default(),
            result_tab: ResultTab::Overview,
            tran_sub_tab: TranSubTab::default(),
            editor_mode: EditorMode::Text,
            schematic: SchematicState::default(),
            plot: PlotState::default(),

            diff_node_a: 1,
            diff_node_b: 2,
        }
    }
}

impl CircuitSimApp {
    pub fn new(cc: &eframe::CreationContext<'_>) -> Self {
        let mut fonts = egui::FontDefinitions::default();

        egui_extras::install_image_loaders(&cc.egui_ctx);

        // 1. Load the custom font data
        fonts.font_data.insert(
            "technical_symbols".to_owned(),
                               egui::FontData::from_static(include_bytes!("../fonts/DejaVuSans.ttf")),
        );

        // 2. Insert it as a fallback for Proportional fonts (UI text)
        // By pushing it to the end, egui will use its default font first,
        // and fall back to our custom font for missing symbols.
        if let Some(vec) = fonts.families.get_mut(&egui::FontFamily::Proportional) {
            vec.insert(0, "technical_symbols".to_owned());
        }

        // 3. Do the same for Monospace text
        if let Some(vec) = fonts.families.get_mut(&egui::FontFamily::Monospace) {
            vec.insert(0, "technical_symbols".to_owned());
        }

        // 4. Apply the new font definitions to the egui context
        cc.egui_ctx.set_fonts(fonts);

        Self::default()
    }
    fn generate_netlist_from_schematic(&mut self) {
        match self.schematic.build_netlist() {
            Ok(netlist) => {
                self.netlist = netlist_emit(&netlist);
                self.status = Some((true, "Generated netlist from schematic".into()));
            }
            Err(e) => {
                self.status = Some((false, format!("Cannot generate netlist: {e}")));
            }
        }
    }

    /// Parse the current netlist text and rebuild the schematic from it —
    /// the column/interval layout lives with the IR in [`crate::schematic`];
    /// this shell only reports the outcome.
    fn generate_schematic_from_netlist(&mut self) {
        match SchematicState::from_netlist(&self.netlist, &self.schematic) {
            Ok((schematic, status)) => {
                self.schematic = schematic;
                self.status = Some((true, status));
            }
            Err(e) => self.status = Some((false, e)),
        }
    }

    fn run_simulation(&mut self) {
        match parse(&self.netlist) {
            Ok(netlist) => {
                let mut summary = format!(
                    "{} nodes · {} R · {} C · {} L · {} V · {} I",
                    netlist.circuit.nodes,
                    netlist.circuit.resistors.len(),
                                      netlist.circuit.capacitors.len(),
                                      netlist.circuit.inductors.len(),
                                      netlist.circuit.voltage_sources.len(),
                                      netlist.circuit.current_sources.len()
                );
                if !netlist.warnings.is_empty() {
                    summary.push_str(&format!(" · {} import warning(s)", netlist.warnings.len()));
                }
                self.circuit_summary = Some(summary);
                self.node_names = netlist.circuit.node_names.clone();
                self.circuit = Some(netlist.circuit.clone());

                match run(&netlist) {
                    Ok(result) => {
                        self.apply_result(result);
                        self.status = Some((true, "Simulation finished".into()));
                    }
                    Err(e) => {
                        self.clear_results();
                        self.status = Some((false, e.to_string()));
                    }
                }
            }
            Err(e) => {
                self.clear_results();
                self.status = Some((false, e.to_string()));
            }
        }
    }

    fn apply_result(&mut self, result: SimulationResult) {
        self.dc = result.dc;
        self.dc_sweep = result.dc_sweep;
        self.tran = result.tran.clone();
        self.ac = result.ac.clone();

        // Node count is the deck's node table — one fact, one place, no
        // result-structure digging.
        let max_nodes = self.node_names.len();

        self.plot_nodes.resize(max_nodes, false);
        for i in 1..max_nodes.min(self.plot_nodes.len()) {
            self.plot_nodes[i] = i <= 3;
        }

        self.dc_plot_nodes.resize(max_nodes, false);
        for i in 1..max_nodes.min(self.dc_plot_nodes.len()) {
            self.dc_plot_nodes[i] = i <= 3;
        }

        self.ac_plot_nodes.resize(max_nodes, false);
        for i in 1..max_nodes.min(self.ac_plot_nodes.len()) {
            self.ac_plot_nodes[i] = i <= 3;
        }
        if self.ac.is_some() {
            self.result_tab = ResultTab::FreqResponse;
        } else if self.dc_sweep.is_some() && self.tran.is_none() {
            self.result_tab = ResultTab::Dc;
        }
    }

    fn clear_results(&mut self) {
        self.circuit_summary = None;
        self.dc = None;
        self.dc_sweep = None;
        self.tran = None;
        self.ac = None;
        self.node_names = vec!["0".to_string()];
    }

    fn load_example(&mut self, ex: Example) {
        self.netlist = ex.netlist().to_string();
        self.file_label = format!("{}.cir", ex.label().to_lowercase().replace(' ', "_"));
        self.status = Some((true, format!("Loaded example: {}", ex.label())));
        self.generate_schematic_from_netlist();
        self.clear_results();
    }

    fn open_file(&mut self) {
        #[cfg(not(target_arch = "wasm32"))] {
        if let Some(path) = rfd::FileDialog::new()
            .add_filter("Netlist", &["cir", "sp", "cir", "txt"])
            .pick_file()
            {
                match std::fs::read_to_string(&path) {
                    Ok(text) => {
                        self.netlist = text;
                        self.file_label = path
                        .file_name()
                        .map(|s| s.to_string_lossy().into_owned())
                        .unwrap_or_else(|| "circuit.cir".into());
                        self.status = Some((true, format!("Opened {}", self.file_label)));
                        self.clear_results();
                    }
                    Err(e) => self.status = Some((false, e.to_string())),
                }
            }
        }
    }

    fn save_file(&mut self) {
        #[cfg(not(target_arch = "wasm32"))] {
        if let Some(path) = rfd::FileDialog::new()
            .add_filter("Netlist", &["cir"])
            .set_file_name(&self.file_label)
            .save_file()
            {
                match std::fs::write(&path, &self.netlist) {
                    Ok(()) => {
                        self.file_label = path
                        .file_name()
                        .map(|s| s.to_string_lossy().into_owned())
                        .unwrap_or_else(|| "circuit.cir".into());
                        self.status = Some((true, format!("Saved {}", self.file_label)));
                    }
                    Err(e) => self.status = Some((false, e.to_string())),
                }
            }
        }
    }


}

impl eframe::App for CircuitSimApp {
    fn update(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        if ctx.input(|i| i.key_pressed(egui::Key::F5)) {
            self.run_simulation();
        }

        egui::TopBottomPanel::top("toolbar")
        .frame(panel_frame(BG_PANEL))
        .show(ctx, |ui| {
            self.toolbar(ui);
        });

        egui::TopBottomPanel::bottom("status")
        .frame(
            egui::Frame::none()
            .fill(BG_PANEL)
            .stroke(egui::Stroke::new(1.0, theme::BORDER))
            .inner_margin(egui::Margin::symmetric(16.0, 8.0)),
        )
        .show(ctx, |ui| {
            self.status_bar(ui);
        });

        egui::SidePanel::left("editor")
        .resizable(true)
        .default_width(420.0)
        .min_width(280.0)
        .frame(
            egui::Frame::none()
            .fill(BG_SURFACE)
            .inner_margin(egui::Margin::same(14.0)),
        )
        .show(ctx, |ui| {
            self.editor_panel(ui);
        });

        egui::CentralPanel::default()
        .frame(
            egui::Frame::none()
            .fill(BG_PANEL)
            .inner_margin(egui::Margin::same(14.0)),
        )
        .show(ctx, |ui| {
            self.results_panel(ui);
        });
    }
}

impl CircuitSimApp {
    fn toolbar(&mut self, ui: &mut egui::Ui) {
        ui.horizontal(|ui| {
            ui.label(
                egui::RichText::new("⚡ RSpice")
                .strong()
                .size(20.0)
                .color(ACCENT),
            );
            ui.add_space(12.0);

            let run = ui.add(
                egui::Button::new(egui::RichText::new("▶  Run").strong().color(egui::Color32::WHITE))
                .fill(ACCENT)
                .min_size(egui::vec2(100.0, 32.0)),
            );
            if run.clicked() {
                self.run_simulation();
            }
            if ui
                .add(egui::Button::new("Open"))
                .on_hover_text("Open netlist file")
                .clicked()
                {
                    self.open_file();
                }
                if ui
                    .add(egui::Button::new("Save"))
                    .on_hover_text("Save netlist")
                    .clicked()
                    {
                        self.save_file();
                    }

                    ui.separator();

                ui.menu_button("Examples", |ui| {
                    for ex in [
                        Example::RcCharge,
                        Example::VoltageDivider,
                        Example::RlCircuit,
                        Example::RcLowPass,
                    ] {
                        if ui.button(ex.label()).clicked() {
                            self.load_example(ex);
                            ui.close_menu();
                        }
                    }
                });

                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    ui.label(
                        egui::RichText::new("F5 run · SPICE-style netlist")
                        .color(TEXT_SECONDARY)
                        .size(12.0),
                    );
                });
        });
    }

    fn status_bar(&mut self, ui: &mut egui::Ui) {
        ui.horizontal(|ui| {
            if let Some((ok, msg)) = &self.status {
                let color = if *ok { STATUS_OK } else { STATUS_ERROR };
                status_chip(ui, msg, color);
            } else {
                ui.label(
                    egui::RichText::new("Ready")
                    .color(TEXT_SECONDARY)
                    .italics(),
                );
            }
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                ui.label(
                    egui::RichText::new(&self.file_label)
                    .color(TEXT_SECONDARY)
                    .family(egui::FontFamily::Monospace),
                );
            });
        });
    }

    fn editor_panel(&mut self, ui: &mut egui::Ui) {
        ui.horizontal(|ui| {
            section_heading(ui, "Editor");
            ui.add_space(16.0);
            ui.selectable_value(&mut self.editor_mode, EditorMode::Text, "Text");
            ui.selectable_value(&mut self.editor_mode, EditorMode::Schematic, "Schematic");
        });
        ui.add_space(6.0);

        if self.editor_mode == EditorMode::Text {
            ui.horizontal(|ui| {
                ui.label(egui::RichText::new("R · C · L · V  —  .op  .tran  .ac").color(TEXT_SECONDARY).size(12.0));
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    let import_btn = egui::Button::new(
                        egui::RichText::new("⟶ Import to Schematic").size(12.0).color(egui::Color32::WHITE)
                    )
                    .fill(egui::Color32::from_rgb(60, 100, 160))
                    .rounding(egui::Rounding::same(4.0))
                    .min_size(egui::vec2(0.0, 22.0));
                    if ui.add(import_btn).on_hover_text("Parse the netlist and render it as a schematic").clicked() {
                        self.generate_schematic_from_netlist();
                        self.editor_mode = EditorMode::Schematic;
                    }
                });
            });
            editor_frame().show(ui, |ui| {
                ui.add(
                    egui::TextEdit::multiline(&mut self.netlist)
                    .font(egui::TextStyle::Monospace)
                    .desired_width(f32::INFINITY)
                    .desired_rows(28)
                    .lock_focus(true)
                    .code_editor(),
                );
            });
        } else {
            self.schematic_panel(ui);
        }
    }

    fn schematic_panel(&mut self, ui: &mut egui::Ui) {
        // Handle keyboard shortcut for rotation (Spacebar)
        if ui.input(|i| i.key_pressed(egui::Key::Space)) {
            if self.schematic.selected_component.is_some() {
                self.schematic.rotate_selected();
            } else {
                // No component selected, update future placement direction
                self.schematic.advance_direction();
            }
        }

        // Toolbar — styled tool buttons with icons
        egui::Frame::none()
        .fill(theme::BG_SURFACE)
        .stroke(egui::Stroke::new(1.0, theme::BORDER))
        .rounding(egui::Rounding::same(6.0))
        .inner_margin(egui::Margin::symmetric(8.0, 4.0))
        .show(ui, |ui| {

            // FIX: Use horizontal_wrapped to naturally wrap components to a new row on small screens
            ui.horizontal_wrapped(|ui| {

                // 1. General Tools Array using include_image!
                let tools = [
                    // Assuming you put your SVGs in an "assets" folder next to "src"
                    (Tool::Select, egui::include_image!("../assets/select.svg"), "Select"),
                                  (Tool::Wire,   egui::include_image!("../assets/wire.svg"), "Wire"),
                ];

                for &(t, ref icon_source, label) in &tools {
                    let active = self.schematic.tool == t;
                    let color = if active { egui::Color32::WHITE } else { TEXT_SECONDARY };

                    // Create the SVG image instance, scale it, and tint it
                    let image = egui::Image::new(icon_source.clone())
                    .max_height(14.0)
                    .tint(color);

                    let btn_text = egui::RichText::new(label)
                    .size(12.0)
                    .color(color);

                    let btn = egui::Button::image_and_text(image, btn_text)
                    .fill(if active { ACCENT } else { egui::Color32::TRANSPARENT })
                    .rounding(egui::Rounding::same(4.0))
                    .min_size(egui::vec2(0.0, 24.0));

                    if ui.add(btn).clicked() {
                        self.schematic.select_tool(t);
                    }
                }

                ui.separator();

                // 2. Component Tools Array using include_image!
                let comp_tools = [
                    (Tool::R,   egui::include_image!("../assets/resistor.svg"), "R"),
                                  (Tool::C,   egui::include_image!("../assets/capacitor.svg"), "C"),
                                  (Tool::L,   egui::include_image!("../assets/inductor.svg"), "L"),
                                  (Tool::V,   egui::include_image!("../assets/voltage.svg"), "V"),
                                  (Tool::Gnd, egui::include_image!("../assets/ground.svg"), "GND"),
                ];

                for &(t, ref icon_source, label) in &comp_tools {
                    let active = self.schematic.tool == t;
                    let color = if active { egui::Color32::WHITE } else { TEXT_SECONDARY };

                    let image = egui::Image::new(icon_source.clone())
                    .max_height(14.0)
                    .tint(color);

                    let btn_text = egui::RichText::new(label)
                    .size(12.0)
                    .color(color);

                    let btn = egui::Button::image_and_text(image, btn_text)
                    .fill(if active { ACCENT.gamma_multiply(0.9) } else { egui::Color32::TRANSPARENT })
                    .rounding(egui::Rounding::same(4.0))
                    .min_size(egui::vec2(0.0, 24.0));

                    if ui.add(btn).clicked() {
                        self.schematic.select_tool(t);
                    }
                }

                ui.separator();

                // FIX: Replaced `right_to_left` block with Left-To-Right rendering.
                // This flows nicely into the wrapper when squished.
                let rot_dir = match self.schematic.current_dir {
                    Direction::Right => "→", Direction::Down => "↓",
                    Direction::Left => "←",  Direction::Up => "↑",
                };
                ui.label(egui::RichText::new(format!("Space: rotate {rot_dir}")).size(11.0).color(TEXT_SECONDARY));

                ui.add_space(4.0);

                let gen_btn = egui::Button::new(
                    egui::RichText::new("Generate Netlist").size(12.0).color(egui::Color32::WHITE)
                )
                .fill(egui::Color32::from_rgb(60, 130, 80))
                .rounding(egui::Rounding::same(4.0))
                .min_size(egui::vec2(0.0, 24.0));
                if ui.add(gen_btn).clicked() {
                    self.generate_netlist_from_schematic();
                    self.editor_mode = EditorMode::Text;
                }

                ui.separator();

                // Analysis type selector
                ui.label(egui::RichText::new("Analysis:").size(11.0).color(TEXT_SECONDARY));
                ui.selectable_value(&mut self.schematic.analysis_type, AnalysisType::Ac, ".ac");
                ui.selectable_value(&mut self.schematic.analysis_type, AnalysisType::Tran, ".tran");

                ui.separator();

                // Analysis params (Reordered left-to-right logic)
                match self.schematic.analysis_type {
                    AnalysisType::Tran => {
                        ui.label(egui::RichText::new("step:").size(10.0).color(TEXT_SECONDARY));
                        ui.add(egui::TextEdit::singleline(&mut self.schematic.tran_step)
                        .desired_width(42.0).font(egui::TextStyle::Small));
                        ui.label(egui::RichText::new("stop:").size(10.0).color(TEXT_SECONDARY));
                        ui.add(egui::TextEdit::singleline(&mut self.schematic.tran_stop)
                        .desired_width(42.0).font(egui::TextStyle::Small));
                    }
                    AnalysisType::Ac => {
                        egui::ComboBox::from_id_salt("ac_var")
                        .selected_text(&self.schematic.ac_variation)
                        .width(46.0)
                        .show_ui(ui, |ui| {
                            let mut v = self.schematic.ac_variation.clone();
                            if ui.selectable_value(&mut v, "dec".to_string(), "dec").clicked() {
                                self.schematic.ac_variation = v.clone();
                            }
                            if ui.selectable_value(&mut v, "oct".to_string(), "oct").clicked() {
                                self.schematic.ac_variation = v.clone();
                            }
                            if ui.selectable_value(&mut v, "lin".to_string(), "lin").clicked() {
                                self.schematic.ac_variation = v;
                            }
                        });

                        ui.label(egui::RichText::new("pts:").size(10.0).color(TEXT_SECONDARY));
                        ui.add(egui::TextEdit::singleline(&mut self.schematic.ac_points)
                        .desired_width(30.0).font(egui::TextStyle::Small));

                        ui.label(egui::RichText::new("fstart:").size(10.0).color(TEXT_SECONDARY));
                        ui.add(egui::TextEdit::singleline(&mut self.schematic.ac_fstart)
                        .desired_width(42.0).font(egui::TextStyle::Small));

                        ui.label(egui::RichText::new("fstop:").size(10.0).color(TEXT_SECONDARY));
                        ui.add(egui::TextEdit::singleline(&mut self.schematic.ac_fstop)
                        .desired_width(50.0).font(egui::TextStyle::Small));
                    }
                }
            });
        });

        ui.add_space(4.0);

        // Canvas Drawing
        let (response, painter) = ui.allocate_painter(ui.available_size(), egui::Sense::click_and_drag());
        let rect = response.rect;

        // --- NEW PAN & ZOOM LOGIC ---
        // Handle Zoom (Ctrl + Scroll or Pinch)
        let zoom_delta = ui.input(|i| i.zoom_delta());
        if response.hovered() && zoom_delta != 1.0 {
            if let Some(mouse_pos) = ui.input(|i| i.pointer.hover_pos()) {
                let mouse_in_canvas = mouse_pos - rect.left_top();
                let old_zoom = self.schematic.zoom;
                let new_zoom = (old_zoom * zoom_delta).clamp(0.2, 5.0);

                let pos_in_grid = (mouse_in_canvas - self.schematic.pan) / old_zoom;
                self.schematic.pan = mouse_in_canvas - pos_in_grid * new_zoom;
                self.schematic.zoom = new_zoom;
            }
        }

        // Handle Pan (Middle Click Drag or Right Click Drag)
        if response.dragged_by(egui::PointerButton::Middle) || response.dragged_by(egui::PointerButton::Secondary) {
            self.schematic.pan += response.drag_delta();
        }

        let z = self.schematic.zoom;
        let pan = self.schematic.pan;

        // Draw dot grid (Dynamic based on visible bounds)
        let dot_color = theme::BORDER.gamma_multiply(0.6);
        let bounds_min = -pan / z;
        let bounds_max = (rect.size() - pan) / z;

        let start_x = (bounds_min.x / GRID_SIZE).floor() as i32;
        let end_x = (bounds_max.x / GRID_SIZE).ceil() as i32;
        let start_y = (bounds_min.y / GRID_SIZE).floor() as i32;
        let end_y = (bounds_max.y / GRID_SIZE).ceil() as i32;

        for xi in start_x..=end_x {
            for yi in start_y..=end_y {
                let pos = rect.left_top() + pan + Vec2::new(xi as f32 * GRID_SIZE, yi as f32 * GRID_SIZE) * z;
                painter.circle_filled(pos, 0.8 * z.max(0.5), dot_color);
            }
        }

        let to_screen = |grid_pt: GridPt| rect.left_top() + pan + (grid_pt.to_pos().to_vec2() * z);
        // --- END PAN & ZOOM LOGIC ---

        // Draw existing wires
        let wire_color = Color32::from_rgb(80, 200, 120);
        let wire_selected_color = Color32::from_rgb(255, 100, 80);
        for (wi, w) in self.schematic.wires.iter().enumerate() {
            let color = if self.schematic.selected_wire == Some(wi) { wire_selected_color } else { wire_color };
            painter.line_segment([to_screen(w.p1), to_screen(w.p2)], Stroke::new(if self.schematic.selected_wire == Some(wi) { 3.0 * z } else { 2.0 * z }, color));
            painter.circle_filled(to_screen(w.p1), 2.5 * z, color);
            painter.circle_filled(to_screen(w.p2), 2.5 * z, color);
        }

        // Draw active wire preview
        if let Some(hover_pos) = response.hover_pos() {
            // Apply reverse zoom/pan to find the actual grid point
            let hover_grid = GridPt::from_pos(Pos2::new(
                (hover_pos.x - rect.left_top().x - pan.x) / z,
                                                        (hover_pos.y - rect.left_top().y - pan.y) / z
            ));

            if self.schematic.tool == Tool::Wire {
                if let Some(start_grid) = self.schematic.active_wire_start {
                    painter.line_segment([to_screen(start_grid), to_screen(hover_grid)], Stroke::new(2.0 * z, theme::ACCENT));
                }
                painter.circle_filled(to_screen(hover_grid), 4.0 * z, theme::ACCENT);
            } else if matches!(self.schematic.tool, Tool::R | Tool::C | Tool::L | Tool::V) {
                // Ghost component preview
                let (dx, dy) = self.schematic.current_dir.offset(3);
                let p2_grid = GridPt(hover_grid.0 + dx, hover_grid.1 + dy);

                painter.line_segment(
                    [to_screen(hover_grid), to_screen(p2_grid)],
                                     Stroke::new(1.5 * z, theme::TEXT_SECONDARY.gamma_multiply(0.4))
                );
                // Ghost body box at center of preview
                let ghost_center = to_screen(hover_grid) + (to_screen(p2_grid) - to_screen(hover_grid)) / 2.0;
                let is_h = hover_grid.1 == p2_grid.1;
                let ghost_rect = Rect::from_center_size(ghost_center,
                                                        if is_h { Vec2::new(28.0 * z, 16.0 * z) } else { Vec2::new(16.0 * z, 28.0 * z) });
                painter.rect_stroke(ghost_rect, 3.0 * z, Stroke::new(1.5 * z, theme::ACCENT.gamma_multiply(0.5)));
                painter.circle_filled(to_screen(hover_grid), 3.5 * z, theme::ACCENT.gamma_multiply(0.9));
            }
        }

        // Draw Components with proper electronic symbols
        let comp_stroke = Stroke::new(2.0 * z, Color32::from_rgb(220, 225, 235));
        let selected_stroke = Stroke::new(2.0 * z, theme::ACCENT);

        for (i, c) in self.schematic.components.iter().enumerate() {
            let p1 = to_screen(c.p1);
            let p2 = to_screen(c.p2);
            let center = p1 + (p2 - p1) / 2.0;
            let is_horizontal = c.p1.1 == c.p2.1;
            let is_selected = self.schematic.selected_component == Some(i);

            // Lead wires from terminal to component body
            let lead_frac = 0.28_f32;
            let body_p1 = p1 + (p2 - p1) * lead_frac;
            let body_p2 = p1 + (p2 - p1) * (1.0 - lead_frac);

            match c.kind {
                CompKind::Resistor => {
                    // Lead wires
                    painter.line_segment([p1, body_p1], comp_stroke);
                    painter.line_segment([body_p2, p2], comp_stroke);

                    // Zigzag resistor body (IEC rectangular or ANSI zigzag)
                    let len = (body_p2 - body_p1).length();
                    let dir = (body_p2 - body_p1) / len;
                    let perp = if is_horizontal { Vec2::new(0.0, 1.0) } else { Vec2::new(1.0, 0.0) };
                    let segs = 6i32;
                    let seg_len = len / segs as f32;
                    let amp = 5.0_f32;
                    let mut pts = Vec::new();
                    pts.push(body_p1);
                    for k in 0..segs {
                        let t = k as f32 + 0.5;
                        let mid = body_p1 + dir * (t * seg_len);
                        let sign = if k % 2 == 0 { 1.0 } else { -1.0 };
                        pts.push(mid + perp * (amp * sign));
                    }
                    pts.push(body_p2);
                    for w in pts.windows(2) {
                        painter.line_segment([w[0], w[1]], comp_stroke);
                    }

                    // Selection glow
                    if is_selected {
                        let sel_rect = Rect::from_center_size(center,
                                                              if is_horizontal { Vec2::new(len + 4.0, 18.0) } else { Vec2::new(18.0, len + 4.0) });
                        painter.rect_stroke(sel_rect, 3.0, selected_stroke);
                    }

                    // Value label
                    let off = if is_horizontal { Vec2::new(0.0, 14.0) } else { Vec2::new(18.0, 0.0) };
                    painter.text(center + off, egui::Align2::CENTER_CENTER, &c.val,
                                 egui::FontId::proportional(10.0), TEXT_SECONDARY);
                    painter.text(center - off * 0.8, egui::Align2::CENTER_CENTER, "R",
                                 egui::FontId::proportional(9.0), ACCENT);
                }

                CompKind::Capacitor => {
                    // Lead wires
                    painter.line_segment([p1, body_p1], comp_stroke);
                    painter.line_segment([body_p2, p2], comp_stroke);

                    // Two parallel plates
                    let plate_half = 9.0_f32;
                    let gap = 4.0_f32;
                    let perp = if is_horizontal { Vec2::new(0.0, 1.0) } else { Vec2::new(1.0, 0.0) };
                    let dir = if is_horizontal { Vec2::new(1.0, 0.0) } else { Vec2::new(0.0, 1.0) };

                    let plate1_c = center - dir * (gap / 2.0);
                    let plate2_c = center + dir * (gap / 2.0);

                    painter.line_segment([plate1_c - perp * plate_half, plate1_c + perp * plate_half],
                                         Stroke::new(2.5, Color32::from_rgb(220, 225, 235)));
                    painter.line_segment([plate2_c - perp * plate_half, plate2_c + perp * plate_half],
                                         Stroke::new(2.5, Color32::from_rgb(220, 225, 235)));

                    if is_selected {
                        let sel_rect = Rect::from_center_size(center,
                                                              if is_horizontal { Vec2::new(24.0, plate_half * 2.0 + 4.0) }
                                                              else { Vec2::new(plate_half * 2.0 + 4.0, 24.0) });
                        painter.rect_stroke(sel_rect, 3.0, selected_stroke);
                    }

                    let off = if is_horizontal { Vec2::new(0.0, 16.0) } else { Vec2::new(20.0, 0.0) };
                    painter.text(center + off, egui::Align2::CENTER_CENTER, &c.val,
                                 egui::FontId::proportional(10.0), TEXT_SECONDARY);
                    painter.text(center - off * 0.8, egui::Align2::CENTER_CENTER, "C",
                                 egui::FontId::proportional(9.0), ACCENT);
                }

                CompKind::Inductor => {
                    // Lead wires
                    painter.line_segment([p1, body_p1], comp_stroke);
                    painter.line_segment([body_p2, p2], comp_stroke);

                    // Coil arcs — draw 3 bumps
                    let len = (body_p2 - body_p1).length();
                    let bumps = 3i32;
                    let bump_len = len / bumps as f32;
                    let bump_r = bump_len / 2.0;
                    let dir = (body_p2 - body_p1) / len;
                    let perp = if is_horizontal { Vec2::new(0.0, 1.0) } else { Vec2::new(1.0, 0.0) };

                    for b in 0..bumps {
                        let bump_start = body_p1 + dir * (b as f32 * bump_len);
                        let bump_center = bump_start + dir * bump_r;
                        // Approximate arc with line segments
                        let arc_pts: Vec<Pos2> = (0..=8).map(|k| {
                            let angle = std::f32::consts::PI * k as f32 / 8.0;
                            let (sin, cos) = angle.sin_cos();
                            bump_center - dir * (bump_r * cos) + perp * (bump_r * sin * -1.0)
                        }).collect();
                        for w in arc_pts.windows(2) {
                            painter.line_segment([w[0], w[1]], comp_stroke);
                        }
                    }

                    if is_selected {
                        let sel_rect = Rect::from_center_size(center,
                                                              if is_horizontal { Vec2::new(len + 4.0, 18.0) } else { Vec2::new(18.0, len + 4.0) });
                        painter.rect_stroke(sel_rect, 3.0, selected_stroke);
                    }

                    let off = if is_horizontal { Vec2::new(0.0, 14.0) } else { Vec2::new(18.0, 0.0) };
                    painter.text(center + off, egui::Align2::CENTER_CENTER, &c.val,
                                 egui::FontId::proportional(10.0), TEXT_SECONDARY);
                    painter.text(center - off * 0.8, egui::Align2::CENTER_CENTER, "L",
                                 egui::FontId::proportional(9.0), ACCENT);
                }

                CompKind::Voltage => {
                    // Lead wires
                    painter.line_segment([p1, body_p1], comp_stroke);
                    painter.line_segment([body_p2, p2], comp_stroke);

                    // Circle body
                    let r = 11.0_f32;
                    painter.circle_stroke(center, r,
                                          Stroke::new(2.0, Color32::from_rgb(255, 200, 80)));

                    // + and - polarity symbols inside
                    let dir = (p2 - p1).normalized();
                    let plus_c = center + dir * (r * 0.45);
                    let minus_c = center - dir * (r * 0.45);
                    let ps = 3.0_f32;
                    painter.line_segment([plus_c - dir * ps, plus_c + dir * ps],
                                         Stroke::new(1.5, Color32::from_rgb(255, 200, 80)));
                    let perp = Vec2::new(-dir.y, dir.x);
                    painter.line_segment([plus_c - perp * ps, plus_c + perp * ps],
                                         Stroke::new(1.5, Color32::from_rgb(255, 200, 80)));
                    painter.line_segment([minus_c - dir * ps, minus_c + dir * ps],
                                         Stroke::new(1.5, Color32::from_rgb(200, 200, 200)));

                    if is_selected {
                        painter.circle_stroke(center, r + 4.0, selected_stroke);
                    }

                    let off = if is_horizontal { Vec2::new(0.0, 16.0) } else { Vec2::new(20.0, 0.0) };
                    painter.text(center + off, egui::Align2::CENTER_CENTER, &c.val,
                                 egui::FontId::proportional(10.0), TEXT_SECONDARY);
                    painter.text(center - off * 0.8, egui::Align2::CENTER_CENTER, "V",
                                 egui::FontId::proportional(9.0), Color32::from_rgb(255, 200, 80));
                }

                CompKind::Ground => {
                    // Vertical line down from pin
                    let gnd_color = Color32::from_rgb(160, 170, 185);
                    let pin = p1;
                    let bar_y1 = pin + Vec2::new(0.0, 10.0);
                    painter.line_segment([pin, bar_y1], Stroke::new(2.0, gnd_color));
                    // Three decreasing horizontal bars
                    for (k, half) in [(0, 10.0_f32), (1, 7.0_f32), (2, 4.0_f32)] {
                        let y = bar_y1.y + k as f32 * 4.0;
                        painter.line_segment(
                            [Pos2::new(pin.x - half, y), Pos2::new(pin.x + half, y)],
                                             Stroke::new(2.0, gnd_color),
                        );
                    }
                    // "GND" label
                    painter.text(pin + Vec2::new(0.0, 26.0), egui::Align2::CENTER_CENTER,
                                 "GND", egui::FontId::proportional(9.0), gnd_color);

                    if is_selected {
                        let sel_rect = Rect::from_center_size(pin + Vec2::new(0.0, 12.0), Vec2::new(26.0, 28.0));
                        painter.rect_stroke(sel_rect, 3.0, selected_stroke);
                    }
                }
            }
        }

        // ── Compute and draw node number labels ─────────────────────────
        // The same NodeAssignment the netlist writer uses, so the on-canvas
        // labels can never drift away from the emitted netlist.
        {
            let assignment = NodeAssignment::of(&self.schematic.components, &self.schematic.wires);

            // Collect one representative screen position per node number.
            //    Pick the point with the smallest (x, y) grid coords for stability —
            //    deterministic and never flickers regardless of mouse position.
            let mut node_positions: HashMap<usize, (Pos2, GridPt)> = HashMap::new();

            for pt in &assignment.pt_list {
                let node_num = assignment.node_of(*pt);
                let screen_pos = to_screen(*pt);
                let entry = node_positions.entry(node_num).or_insert((screen_pos, *pt));
                // Keep the point with the smallest grid coords (already sorted, so
                // the first insertion wins — but guard explicitly for clarity).
                if (pt.0, pt.1) < (entry.1.0, entry.1.1) {
                    *entry = (screen_pos, *pt);
                }
            }

            // 6. Draw node labels.
            for (node_num, (pos, _grid_pt)) in &node_positions {
                let label = format!("{node_num}");
                let bg_color = if *node_num == 0 {
                    Color32::from_rgba_unmultiplied(40, 40, 50, 200)
                } else {
                    Color32::from_rgba_unmultiplied(20, 50, 80, 210)
                };
                let text_color = if *node_num == 0 {
                    Color32::from_rgb(160, 170, 185)
                } else {
                    Color32::from_rgb(100, 210, 255)
                };

                // Small filled pill background for legibility.
                let font_id = egui::FontId::proportional((10.0 * z).clamp(8.0, 14.0));
                let galley = painter.layout_no_wrap(label.clone(), font_id.clone(), text_color);
                let text_size = galley.size();
                let pad = Vec2::new(3.0, 1.5);
                let label_offset = Vec2::new(7.0 * z, -7.0 * z);
                let pill_rect = Rect::from_min_size(
                    *pos + label_offset - pad,
                    text_size + pad * 2.0,
                );
                painter.rect_filled(pill_rect, 3.0, bg_color);
                painter.rect_stroke(pill_rect, 3.0, egui::Stroke::new(0.5, text_color.gamma_multiply(0.5)));
                painter.text(
                    *pos + label_offset,
                    egui::Align2::LEFT_TOP,
                    label,
                    font_id,
                    text_color,
                );
            }
        }
        // ── End node labels ──────────────────────────────────────────────
        if self.schematic.tool == Tool::Select {
            // 1. Detect drag start and lock onto the target component
            if response.drag_started() {
                if let Some(interact_pos) = response.interact_pointer_pos() {
                    let mut hit = None;
                    for (i, c) in self.schematic.components.iter().enumerate() {
                        let p1 = to_screen(c.p1);
                        let p2 = to_screen(c.p2);
                        let center = p1 + (p2 - p1) / 2.0;
                        let is_horizontal = c.p1.1 == c.p2.1;
                        let box_size = if is_horizontal { Vec2::new(24.0, 16.0) } else { Vec2::new(16.0, 24.0) };
                        let click_rect = Rect::from_center_size(center, box_size + Vec2::new(12.0, 12.0));
                        if click_rect.contains(interact_pos) {
                            hit = Some(i);
                            break;
                        }
                    }

                    if hit.is_some() {
                        self.schematic.selected_component = hit;
                        self.schematic.selected_wire = None;
                        self.schematic.drag_last_grid = Some(GridPt::from_pos(interact_pos - rect.left_top().to_vec2()));
                    } else {
                        self.schematic.drag_last_grid = None;
                    }
                }
            }

            // 2. Continuously process movement
            if response.dragged() {
                if let (Some(idx), Some(interact_pos), Some(last_grid)) = (
                    self.schematic.selected_component,
                    response.interact_pointer_pos(),
                                                                           self.schematic.drag_last_grid,
                ) {
                    let current_grid = GridPt::from_pos(Pos2::new(
                        (interact_pos.x - rect.left_top().x - pan.x) / z,
                                                                  (interact_pos.y - rect.left_top().y - pan.y) / z
                    ));
                    if current_grid != last_grid {
                        let dx = current_grid.0 - last_grid.0;
                        let dy = current_grid.1 - last_grid.1;
                        self.schematic.drag_component(idx, dx, dy);
                        self.schematic.drag_last_grid = Some(current_grid);
                    }
                }
            }
        }
        // --- END DRAGGING BLOCK ---

        // Handle Interactions

        if response.clicked() {
            if let Some(interact_pos) = response.interact_pointer_pos() {
                let grid_pt = GridPt::from_pos(Pos2::new(
                    (interact_pos.x - rect.left_top().x - pan.x) / z,
                                                         (interact_pos.y - rect.left_top().y - pan.y) / z
                ));

                match self.schematic.tool {
                    Tool::Wire => self.schematic.wire_click(grid_pt),
                    Tool::R | Tool::C | Tool::L | Tool::V | Tool::Gnd => {
                        self.schematic.place_at(grid_pt);
                    }
                    Tool::Select => {
                        let mut selected = None;
                        for (i, c) in self.schematic.components.iter().enumerate() {
                            let p1 = to_screen(c.p1);
                            let p2 = to_screen(c.p2);
                            let center = p1 + (p2 - p1) / 2.0;
                            let is_horizontal = c.p1.1 == c.p2.1;
                            let box_size = if is_horizontal { Vec2::new(24.0, 16.0) } else { Vec2::new(16.0, 24.0) };

                            // Make click area slightly larger than visual box
                            let click_rect = Rect::from_center_size(center, box_size + Vec2::new(12.0, 12.0));
                            if click_rect.contains(interact_pos) {
                                selected = Some(i);
                                break;
                            }
                        }
                        // If no component hit, check wires; the hit-test
                        // result goes through the model so the selection
                        // fields can never disagree.
                        let mut hit_wire = None;
                        if selected.is_none() {
                            for (wi, w) in self.schematic.wires.iter().enumerate() {
                                let a = to_screen(w.p1);
                                let b = to_screen(w.p2);
                                // Point-to-segment distance
                                let ab = b - a;
                                let ap = interact_pos - a;
                                let ab_len_sq = ab.x * ab.x + ab.y * ab.y;
                                let dist_sq = if ab_len_sq < 1.0 {
                                    ap.x * ap.x + ap.y * ap.y
                                } else {
                                    let t = ((ap.x * ab.x + ap.y * ab.y) / ab_len_sq).clamp(0.0, 1.0);
                                    let closest = a + ab * t;
                                    let d = interact_pos - closest;
                                    d.x * d.x + d.y * d.y
                                };
                                if dist_sq < (36.0 * z * z) { // 6px radius
                                    hit_wire = Some(wi);
                                    break;
                                }
                            }
                        }
                        self.schematic.select(selected, hit_wire);
                    }
                }

                // Clear selection if we click canvas while placing components or wiring
                if self.schematic.tool != Tool::Select {
                    self.schematic.selected_component = None;
                }
            }
        }

        // Cancel wire dragging on Right Click
        if response.secondary_clicked() {
            self.schematic.cancel_gesture();
        }

        // Delete key removes selected wire or component
        if ui.input(|i| i.key_pressed(egui::Key::Delete)) {
            if let Some(wi) = self.schematic.selected_wire {
                self.schematic.wires.remove(wi);
                self.schematic.selected_wire = None;
            } else if let Some(ci) = self.schematic.selected_component {
                self.schematic.components.remove(ci);
                self.schematic.selected_component = None;
            }
        }

        // Wire selection popup — shows near the wire midpoint
        let mut delete_wire_idx = None;
        if let Some(wi) = self.schematic.selected_wire {
            if let Some(w) = self.schematic.wires.get(wi) {
                let mid = to_screen(w.p1) + (to_screen(w.p2) - to_screen(w.p1)) / 2.0;
                let mut is_open = true;
                egui::Window::new("")
                .id(egui::Id::new("edit_wire_window"))
                .title_bar(false)
                .fixed_pos(mid + Vec2::new(10.0, 10.0))
                .collapsible(false)
                .resizable(false)
                .open(&mut is_open)
                .frame(
                    egui::Frame::none()
                    .fill(theme::BG_SURFACE)
                    .stroke(egui::Stroke::new(1.5, Color32::from_rgb(255, 100, 80).gamma_multiply(0.6)))
                    .rounding(egui::Rounding::same(8.0))
                    .inner_margin(egui::Margin::same(10.0))
                )
                .show(ui.ctx(), |ui| {
                    ui.horizontal(|ui| {
                        ui.label(egui::RichText::new("Wire").strong().size(12.0).color(Color32::from_rgb(80, 200, 120)));
                        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                            ui.label(egui::RichText::new("✕ close").size(10.0).color(TEXT_SECONDARY));
                        });
                    });
                    ui.add_space(4.0);
                    ui.separator();
                    ui.add_space(4.0);
                    ui.label(egui::RichText::new("Delete: key or button").size(10.0).color(TEXT_SECONDARY));
                    ui.add_space(4.0);
                    if ui.add(
                        egui::Button::new(egui::RichText::new("🗑 Delete Wire").size(11.0).color(egui::Color32::WHITE))
                        .fill(egui::Color32::from_rgb(180, 50, 50))
                        .rounding(egui::Rounding::same(4.0))
                        .min_size(egui::vec2(100.0, 22.0))
                    ).clicked() {
                        delete_wire_idx = Some(wi);
                    }
                });
                if !is_open {
                    self.schematic.selected_wire = None;
                }
            }
        }
        if let Some(wi) = delete_wire_idx {
            self.schematic.delete_wire(wi);
        }

        // --- ADD THIS AT THE END OF schematic_panel ---
        // Show Editor Window for selected component
        let mut delete_idx = None;
        if let Some(idx) = self.schematic.selected_component {
            if let Some(c) = self.schematic.components.get_mut(idx) {
                let p1 = to_screen(c.p1);
                let p2 = to_screen(c.p2);
                let center = p1 + (p2 - p1) / 2.0;

                let mut is_open = true;

                // Component kind display name and color
                let (kind_label, kind_color) = match c.kind {
                    CompKind::Resistor  => ("Resistor",  Color32::from_rgb(100, 180, 255)),
                    CompKind::Capacitor => ("Capacitor", Color32::from_rgb(180, 130, 255)),
                    CompKind::Inductor  => ("Inductor",  Color32::from_rgb(255, 180, 80)),
                    CompKind::Voltage   => ("Voltage Source", Color32::from_rgb(255, 200, 80)),
                    CompKind::Ground    => ("Ground",    Color32::from_rgb(160, 170, 185)),
                };

                egui::Window::new("")
                .id(egui::Id::new("edit_comp_window"))
                .title_bar(false)
                .fixed_pos(center + Vec2::new(20.0, 20.0))
                .collapsible(false)
                .resizable(false)
                .open(&mut is_open)
                .frame(
                    egui::Frame::none()
                    .fill(theme::BG_SURFACE)
                    .stroke(egui::Stroke::new(1.5, kind_color.gamma_multiply(0.6)))
                    .rounding(egui::Rounding::same(8.0))
                    .inner_margin(egui::Margin::same(12.0))
                )
                .show(ui.ctx(), |ui| {
                    // Header row with kind badge + close hint
                    ui.horizontal(|ui| {
                        ui.label(
                            egui::RichText::new(kind_label)
                            .strong()
                            .size(13.0)
                            .color(kind_color)
                        );
                        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                            ui.label(egui::RichText::new("✕ close").size(10.0).color(TEXT_SECONDARY));
                        });
                    });
                    ui.add_space(6.0);
                    ui.separator();
                    ui.add_space(6.0);
                    if c.kind == CompKind::Voltage {
                        // The deck grammar owns source values: parse once to
                        // classify for the dropdown, edit the typed fields,
                        // re-emit. Hand string surgery is gone from here.
                        let parse_opts = ParseOptions::default();
                        let current_type = parse_source_value(&c.val, &parse_opts)
                            .map(|v| source_value_family(&v))
                            .unwrap_or("DC"); // unparseable text: dropdown only

                        let mut new_type = current_type;
                        let mem_id = ui.id().with("v_params").with(idx);

                        ui.horizontal(|ui| {
                            ui.label(egui::RichText::new("Type").size(11.0).color(TEXT_SECONDARY));
                            ui.add_space(8.0);

                            egui::ComboBox::from_id_salt("v_type")
                            .selected_text(current_type)
                            .width(90.0)
                            .show_ui(ui, |ui| {
                                ui.selectable_value(&mut new_type, "DC", "DC");
                                ui.selectable_value(&mut new_type, "AC", "AC");
                                ui.selectable_value(&mut new_type, "SIN", "Sine");
                                ui.selectable_value(&mut new_type, "PULSE", "Pulse");
                            });

                            // Auto-fill a template if the user changes the
                            // dropdown type; the grammar's writer emits it, so
                            // every card value in the deck is round-trippable.
                            if new_type != current_type {
                                let value = match new_type {
                                    "DC" => SourceValue::Dc { value: 5.0 },
                                    "AC" => SourceValue::Ac { magnitude: 1.0, phase: 0.0 },
                                    "SIN" => SourceValue::Waveform(Waveform::Sine(Sine {
                                        vo: 0.0, va: 5.0, freq: 1e3, td: 0.0, theta: 0.0,
                                    })),
                                    "PULSE" => SourceValue::Waveform(Waveform::Pulse(Pulse {
                                        v1: 0.0, v2: 5.0, td: 1e-3, tr: 1e-6, tf: 1e-6,
                                        pw: 5e-3, per: 1e-2,
                                    })),
                                    _ => return,
                                };
                                c.val = emit_source_value(&value);
                            }
                        });

                        ui.add_space(6.0);

                        // The grid only exists for families the editor knows
                        // (DC / AC / SIN / PULSE). Other families keep their
                        // canonical text until a template rewrite applies.
                        let editable: Option<(&'static [&'static str], Vec<String>)> =
                            match parse_source_value(&c.val, &parse_opts) {
                                Ok(value) if source_value_family(&value) != "Params" => {
                                    Some(source_value_fields(&value))
                                }
                                _ => None,
                            };

                        // Cache strings in UI memory so text cursors survive
                        // immediate mode updates; fields re-derive from the
                        // typed parse whenever the card text changed outside
                        // this grid.
                        let mut cache: (String, Vec<String>) = ui.data_mut(|d| {
                            let mut cache = d
                                .get_temp::<(String, Vec<String>)>(mem_id)
                                .unwrap_or_else(|| (String::new(), vec![]));

                            if cache.0 != c.val {
                                if let Some((_, fields)) = &editable {
                                    cache.1 = fields.clone();
                                }
                                cache.0 = c.val.clone();
                            }
                            cache
                        });

                        if let Some((labels, _)) = &editable {
                            let mut changed = false;

                            // Display parameters in an organized vertical grid
                            egui::Grid::new("v_params_grid").num_columns(2).spacing([8.0, 6.0]).show(ui, |ui| {
                                for (i, label) in labels.iter().enumerate() {
                                    ui.label(egui::RichText::new(*label).size(11.0).color(TEXT_SECONDARY));
                                    if ui.add(egui::TextEdit::singleline(&mut cache.1[i]).desired_width(70.0)).changed() {
                                        changed = true;
                                    }
                                    ui.end_row();
                                }
                            });

                            // Reconstruct the card text seamlessly on edit —
                            // each field re-enters through the value grammar
                            // and the writer owns the family spelling. A field
                            // that does not parse keeps the card's previous
                            // value instead of smuggling raw text into the
                            // netlist.
                            if changed {
                                if let Ok(value) = source_value_from_fields(new_type, &cache.1) {
                                    c.val = emit_source_value(&value);
                                }
                                cache.0 = c.val.clone();
                            }
                        } else {
                            ui.horizontal(|ui| {
                                ui.label(egui::RichText::new("Value").size(11.0).color(TEXT_SECONDARY));
                                ui.add_space(4.0);
                                ui.label(egui::RichText::new(&c.val).size(11.0).color(TEXT_SECONDARY));
                            });
                        }

                        ui.data_mut(|d| d.insert_temp(mem_id, cache));

                                        } else if c.kind != CompKind::Ground {
                        ui.horizontal(|ui| {
                            ui.label(egui::RichText::new("Value").size(11.0).color(TEXT_SECONDARY));
                            ui.add_space(4.0);
                            ui.add(egui::TextEdit::singleline(&mut c.val).desired_width(110.0));
                        });
                    } else {
                        ui.label(egui::RichText::new("Reference ground node (0)").size(11.0).color(TEXT_SECONDARY));
                    }

                    ui.add_space(8.0);
                    ui.separator();
                    ui.add_space(4.0);
                    if ui.add(
                        egui::Button::new(
                            egui::RichText::new("🗑 Delete").size(11.0).color(egui::Color32::WHITE)
                        )
                        .fill(egui::Color32::from_rgb(180, 50, 50))
                        .rounding(egui::Rounding::same(4.0))
                        .min_size(egui::vec2(80.0, 22.0))
                    ).clicked() {
                        delete_idx = Some(idx);
                    }
                });

                if !is_open {
                    self.schematic.selected_component = None;
                }
            } else {
                self.schematic.selected_component = None;
            }
        }

        // Apply deletion if requested
        if let Some(idx) = delete_idx {
            self.schematic.delete_component(idx);
        }

    }

    fn results_panel(&mut self, ui: &mut egui::Ui) {
        ui.horizontal(|ui| {
            section_heading(ui, "Results");
            ui.add_space(8.0);
            ui.selectable_value(&mut self.result_tab, ResultTab::Overview, "Overview");
            ui.selectable_value(&mut self.result_tab, ResultTab::Dc, "DC");
            ui.selectable_value(&mut self.result_tab, ResultTab::Waveforms, "Waveforms");
            ui.selectable_value(&mut self.result_tab, ResultTab::FreqResponse, "Frequency");
        });
        ui.add_space(8.0);

        match self.result_tab {
            ResultTab::Overview    => self.overview_tab(ui),
            ResultTab::Dc          => self.dc_tab(ui),
            ResultTab::Waveforms   => self.waveforms_tab(ui),
            ResultTab::FreqResponse => self.freq_response_tab(ui),
        }
    }

    fn overview_tab(&mut self, ui: &mut egui::Ui) {
        if self.dc.is_none() && self.dc_sweep.is_none() && self.tran.is_none() && self.ac.is_none() {
            ui.vertical_centered(|ui| {
                ui.add_space(80.0);
                ui.label(egui::RichText::new("⚡").size(48.0).color(ACCENT.gamma_multiply(0.4)));
                ui.add_space(12.0);
                ui.label(
                    egui::RichText::new("No results yet")
                    .size(18.0)
                    .color(TEXT_SECONDARY),
                );
                ui.label(
                    egui::RichText::new("Edit the netlist and press Run (F5)")
                    .color(TEXT_SECONDARY),
                );
            });
            return;
        }

        ui.columns(2, |cols| {
            cols[0].vertical(|ui| {
                card_frame().show(ui, |ui| {
                    section_heading(ui, "Circuit");
                    if let Some(s) = &self.circuit_summary {
                        ui.label(s);
                    }
                    if let Some(dc) = &self.dc {
                        ui.add_space(8.0);
                        ui.label(
                            egui::RichText::new("DC operating point")
                            .color(STATUS_OK)
                            .strong(),
                        );
                        for (i, v) in dc.node_voltages.iter().enumerate() {
                            ui.horizontal(|ui| {
                                ui.label(format!("V({})", node_label(&self.node_names, i)));
                                ui.with_layout(
                                    egui::Layout::right_to_left(egui::Align::Center),
                                    |ui| {
                                        ui.label(
                                            egui::RichText::new(format!("{v:.4e} V"))
                                            .family(egui::FontFamily::Monospace),
                                        );
                                    },
                                );
                            });
                        }
                    }
                });
            });

            cols[1].vertical(|ui| {
                if let Some(tran) = &self.tran {
                    card_frame().show(ui, |ui| {
                        section_heading(ui, "Transient preview");
                        ui.label(
                            egui::RichText::new(format!(
                                "{} points · open Waveforms for full plot",
                                tran.points.len()
                            ))
                            .color(TEXT_SECONDARY)
                            .size(12.0),
                        );
                        ui.add_space(6.0);
                        self.mini_plot(ui, tran);
                    });
                }
            });
        });
    }

    fn mini_plot(&self, ui: &mut egui::Ui, tran: &TranResult) {
        let plot = Plot::new("mini")
        .height(200.0)
        .allow_drag(false)
        .allow_zoom(false)
        .allow_scroll(false)
        .show_axes([true, true])
        .show_background(true);

        let series = tran_voltage_series(tran, &self.plot_nodes, &self.node_names, 1.0, 1.0);
        plot.show(ui, |plot_ui| {
            draw(plot_ui, &series, 1.5);
        });
    }

    /// One row of the DC branch-current grid (shared by every element family).
    fn dc_branch_row(ui: &mut egui::Ui, label: String, cur: f64) {
        ui.label(label);
        ui.label(
            egui::RichText::new(format!("{cur:.6e} A"))
            .family(egui::FontFamily::Monospace)
            .color(ACCENT),
        );
        ui.end_row();
    }

    /// Deck element names of one family from the run's circuit; empty when
    /// nothing has parsed yet (labels fall back to the family letter).
    fn circuit_element_names(&self, family: ElementFamily) -> Vec<String> {
        self.circuit
            .as_ref()
            .map(|c| c.element_names(family))
            .unwrap_or_default()
    }

    fn dc_tab(&mut self, ui: &mut egui::Ui) {
        if self.dc.is_none() && self.dc_sweep.is_none() {
            ui.label(
                egui::RichText::new("No DC analysis in netlist (.op / .dc)")
                .color(TEXT_SECONDARY),
            );
            return;
        }

        if let Some(dc) = &self.dc {
        card_frame().show(ui, |ui| {
            // ── Node voltages ────────────────────────────────────────────
            section_heading(ui, "Node voltages");
            egui::Grid::new("dc_node_grid")
            .num_columns(2)
            .spacing([24.0, 8.0])
            .striped(true)
            .show(ui, |ui| {
                ui.label(egui::RichText::new("Node").strong());
                ui.label(egui::RichText::new("Voltage").strong());
                ui.end_row();
                for (i, v) in dc.node_voltages.iter().enumerate() {
                    ui.label(format!("V({})", node_label(&self.node_names, i)));
                    ui.label(
                        egui::RichText::new(format!("{v:.6e} V"))
                        .family(egui::FontFamily::Monospace)
                        .color(ACCENT),
                    );
                    ui.end_row();
                }
            });

            // ── Branch currents ──────────────────────────────────────────
            ui.add_space(16.0);
            section_heading(ui, "Branch currents");
            egui::Grid::new("dc_branch_grid")
            .num_columns(2)
            .spacing([24.0, 8.0])
            .striped(true)
            .show(ui, |ui| {
                ui.label(egui::RichText::new("Element").strong());
                ui.label(egui::RichText::new("Current (n1→n2)").strong());
                ui.end_row();

                let b = &dc.branch_currents;

                // Canonical family walk through the named lookup: labels are
                // the deck's element names, and Q/J/B show like every other
                // family instead of being silently skipped.
                for family in CURRENT_FAMILIES {
                    let values = b.slice(family).filter(|v| !v.is_empty());
                    if values.is_none() {
                        continue;
                    }
                    let names = self.circuit_element_names(family);
                    for (i, &cur) in values.unwrap().iter().enumerate() {
                        let label = names.get(i).cloned()
                            .unwrap_or_else(|| format!("{}{}", family.letter(), i + 1));
                        Self::dc_branch_row(ui, format!("{}({label})", family.probe_prefix()), cur);
                    }
                }
            });
        });
        }

        // ── Sweep ────────────────────────────────────────────────────────────
        if let Some(sweep) = &self.dc_sweep {
        ui.add_space(16.0);
        card_frame().show(ui, |ui| {
            section_heading(ui, "DC sweep");
            ui.label(
                egui::RichText::new(format!(
                    "{} points · targets: {}",
                    sweep.points.len(),
                    sweep.targets.join(", ")
                ))
                .color(TEXT_SECONDARY)
                .size(12.0),
            );

            let n_nodes = self.node_names.len();

            if sweep.targets.len() == 1 {
                // Flat sweep: a plot with one V(node) trace per enabled node.
                ui.horizontal_wrapped(|ui| {
                    ui.label(egui::RichText::new("Nodes:").color(TEXT_SECONDARY));
                    for i in 1..n_nodes.min(self.dc_plot_nodes.len()) {
                        let mut on = self.dc_plot_nodes[i];
                        if ui.checkbox(&mut on, format!("V({})", node_label(&self.node_names, i))).changed() {
                            self.dc_plot_nodes[i] = on;
                        }
                    }
                    ui.separator();
                    if ui.button("Auto Fit").clicked() {
                        self.plot.dc_bounds.reset();
                        self.plot.dc_apply = true;
                    }
                });
                ui.add_space(6.0);

                let apply = self.plot.dc_apply.then(|| self.plot.dc_bounds);
                self.plot.dc_apply = false;
                let x_label = sweep.targets.first().cloned().unwrap_or_else(|| "sweep".into());
                let series = dc_sweep_series(sweep, &self.dc_plot_nodes, &self.node_names, 1.0, 1.0);
                plot_frame(
                    "dc_sweep_plot", 260.0, &x_label, "Voltage (V)",
                    &series, apply, ui,
                );
            } else {
                // Nested sweep: a table — `${outer} • ${inner}` pairs don't line
                // up into a single curve, so the numbers are shown directly.
                egui::Grid::new("dc_sweep_grid")
                .num_columns(sweep.targets.len() + n_nodes)
                .spacing([24.0, 6.0])
                .striped(true)
                .show(ui, |ui| {
                    for t in &sweep.targets {
                        ui.label(egui::RichText::new(t.clone()).strong());
                    }
                    for n in 0..n_nodes {
                        ui.label(egui::RichText::new(format!("V({})", node_label(&self.node_names, n))).strong());
                    }
                    ui.end_row();

                    const MAX_SWEEP_ROWS: usize = 500;
                    for pt in sweep.points.iter().take(MAX_SWEEP_ROWS) {
                        for &v in &pt.values {
                            ui.label(
                                egui::RichText::new(format!("{v:.4e}"))
                                .family(egui::FontFamily::Monospace),
                            );
                        }
                        for v in pt.node_voltages.iter() {
                            ui.label(
                                egui::RichText::new(format!("{v:.4e}"))
                                .family(egui::FontFamily::Monospace),
                            );
                        }
                        ui.end_row();
                    }
                    if sweep.points.len() > MAX_SWEEP_ROWS {
                        ui.label(
                            egui::RichText::new(format!(
                                "… ({} further points not shown)",
                                sweep.points.len() - MAX_SWEEP_ROWS
                            ))
                            .color(TEXT_SECONDARY),
                        );
                    }
                });
            }
        });
        }
    }

    fn waveforms_tab(&mut self, ui: &mut egui::Ui) {
        let Some(tran) = &self.tran else {
            ui.label(
                egui::RichText::new("No transient analysis (.tran) in netlist")
                .color(TEXT_SECONDARY),
            );
            return;
        };

        // ── Sub-tab bar ───────────────────────────────────────────────────────
        ui.horizontal(|ui| {
            ui.selectable_value(&mut self.tran_sub_tab, TranSubTab::Voltage, "Voltage");
            ui.selectable_value(&mut self.tran_sub_tab, TranSubTab::Current, "Current");
            ui.selectable_value(&mut self.tran_sub_tab, TranSubTab::Differential, "Differential");
        });
        ui.separator();

        let x_mult = self.plot.tran_x;
        let y_mult = self.plot.tran_y;

        match self.tran_sub_tab {
            TranSubTab::Voltage => {
                ui.horizontal_wrapped(|ui| {
                    if ui.button("Auto Fit").clicked() {
                        self.plot.tran_bounds.reset();
                        self.plot.tran_apply = true;
                    }
                });
                ui.add_space(8.0);

                code_frame().show(ui, |ui| {
                    let adjusted_height = ui.available_height();
                    let series = tran_voltage_series(tran, &self.plot_nodes, &self.node_names, x_mult, y_mult);
                    let apply = self.plot.tran_apply.then(|| self.plot.tran_bounds);
                    self.plot.tran_apply = false;
                    plot_frame(
                        "waveforms", adjusted_height, "time (s)", "Voltage (V)",
                        &series, apply, ui,
                    );
                });
            }

            TranSubTab::Current => {
                ui.horizontal_wrapped(|ui| {
                    if ui.button("Auto Fit").clicked() {
                        self.plot.tran_bounds.reset();
                        self.plot.tran_apply = true;
                    }
                });
                ui.add_space(8.0);
                code_frame().show(ui, |ui| {
                    let adjusted_height = ui.available_height();
                    let series = tran_current_series(
                        tran,
                        self.circuit.as_ref().unwrap_or(&CsCircuit::default()),
                        x_mult,
                        y_mult,
                    );
                    let apply = self.plot.tran_apply.then(|| self.plot.tran_bounds);
                    self.plot.tran_apply = false;
                    plot_frame(
                        "waveforms_current", adjusted_height, "time (s)", "Current (A)",
                        &series, apply, ui,
                    );
                });
            }

            TranSubTab::Differential => {
                // ── Node selector controls ────────────────────────────────────
                let n_nodes = self.node_names.len();

                // Clamp stored indices to valid range whenever n_nodes changes
                self.diff_node_a = self.diff_node_a.clamp(0, n_nodes.saturating_sub(1));
                self.diff_node_b = self.diff_node_b.clamp(0, n_nodes.saturating_sub(1));

                // Own a names copy here: the combos borrow diff_node_a/b while
                // formatting labels, so a whole-`self` borrow would collide.
                let node_names = self.node_names.clone();

                ui.horizontal_wrapped(|ui| {
                    if ui.button("Auto Fit").clicked() {
                        self.plot.tran_bounds.reset();
                        self.plot.tran_apply = true;
                    }

                    ui.separator();

                    ui.label(egui::RichText::new("V(A) − V(B)  →  A:").color(TEXT_SECONDARY));

                    egui::ComboBox::from_id_salt("diff_node_a")
                        .selected_text(format!("V({})", node_label(&node_names, self.diff_node_a)))
                        .width(80.0)
                        .show_ui(ui, |ui| {
                            for n in 0..n_nodes {
                                ui.selectable_value(&mut self.diff_node_a, n, format!("V({})", node_label(&node_names, n)));
                            }
                        });

                    ui.label(egui::RichText::new("B:").color(TEXT_SECONDARY));

                    egui::ComboBox::from_id_salt("diff_node_b")
                        .selected_text(format!("V({})", node_label(&node_names, self.diff_node_b)))
                        .width(80.0)
                        .show_ui(ui, |ui| {
                            for n in 0..n_nodes {
                                ui.selectable_value(&mut self.diff_node_b, n, format!("V({})", node_label(&node_names, n)));
                            }
                        });
                });

                ui.add_space(8.0);

                let node_a = self.diff_node_a;
                let node_b = self.diff_node_b;

                code_frame().show(ui, |ui| {
                    let adjusted_height = ui.available_height();
                    let series = tran_diff_series(tran, node_a, node_b, &self.node_names, x_mult, y_mult);
                    let apply = self.plot.tran_apply.then(|| self.plot.tran_bounds);
                    self.plot.tran_apply = false;
                    plot_frame(
                        "waveforms_diff", adjusted_height, "time (s)",
                        &format!("V({}) − V({})  (V)", node_label(&self.node_names, node_a), node_label(&self.node_names, node_b)),
                        &series, apply, ui,
                    );
                });
            }
        }
    }
    fn freq_response_tab(&mut self, ui: &mut egui::Ui) {
        let Some(ac) = &self.ac else {
            ui.vertical_centered(|ui| {
                ui.add_space(60.0);
                ui.label(
                    egui::RichText::new("No AC analysis in netlist")
                    .size(16.0)
                    .color(TEXT_SECONDARY),
                );
                ui.label(
                    egui::RichText::new("Add  .ac dec 200 1 100k  to your netlist and run")
                    .color(TEXT_SECONDARY)
                    .size(12.0),
                );
            });
            return;
        };

        let n_nodes = self.node_names.len();

        // ── Controls row ────────────────────────────────────────────────────
        ui.horizontal_wrapped(|ui| {
            ui.label(egui::RichText::new("Nodes:").color(TEXT_SECONDARY));
            for i in 1..n_nodes.min(self.ac_plot_nodes.len()) {
                let mut on = self.ac_plot_nodes[i];
                if ui.checkbox(&mut on, format!("V({})", node_label(&self.node_names, i))).changed() {
                    self.ac_plot_nodes[i] = on;
                }
            }
            ui.separator();
            ui.label(egui::RichText::new("Scale:").color(TEXT_SECONDARY));
            ui.selectable_value(&mut self.ac_mag_scale, AcMagScale::Db,     "dB");
            ui.selectable_value(&mut self.ac_mag_scale, AcMagScale::Linear, "Linear");
        });

        ui.add_space(4.0);

        ui.horizontal_wrapped(|ui| {

            if ui.button("Auto Fit").clicked() {
                self.plot.ac_mag_bounds.reset();
                self.plot.ac_phase_bounds.reset();
                self.plot.ac_apply = true;
            }
        });

        ui.add_space(6.0);

        let log_x = matches!(self.ac_mag_scale, AcMagScale::Db);
        let available_h = ui.available_height();
        let half_h = (available_h - 24.0) / 2.0;

        let mag_qty = match self.ac_mag_scale {
            AcMagScale::Db => AcQuantity::MagnitudeDb,
            AcMagScale::Linear => AcQuantity::MagnitudeLinear,
        };
        let ac_plot_nodes = self.ac_plot_nodes.clone();
        let node_names = self.node_names.clone();
        let circuit = self.circuit.clone().unwrap_or_default();

        // All series are built up front by the one plot adapter; the show
        // closures only draw. (The old code cloned the whole result per frame
        // into a snapshot so its in-closure loops could hold it.)
        let node_mag_series = ac_voltage_series(ac, &ac_plot_nodes, &node_names, mag_qty, self.plot.ac_x, self.plot.ac_mag_y, log_x);
        let node_phase_series = ac_voltage_series(ac, &ac_plot_nodes, &node_names, AcQuantity::Phase, self.plot.ac_x, self.plot.ac_phase_y, log_x);
        let cur_mag_series = ac_current_series(ac, &circuit, mag_qty, self.plot.ac_x, self.plot.ac_mag_y, log_x);
        let cur_phase_series = ac_current_series(ac, &circuit, AcQuantity::Phase, self.plot.ac_x, self.plot.ac_phase_y, log_x);

        let apply_b = self.plot.ac_apply;
        let mag_bounds = self.plot.ac_mag_bounds.x_through_log10();
        let phase_bounds = self.plot.ac_phase_bounds.x_through_log10();

        // ── Magnitude plot ───────────────────────────────────────────────────
        let mag_scale = self.ac_mag_scale;
        let mag_label = match mag_scale {
            AcMagScale::Db     => "magnitude (dB)",
            AcMagScale::Linear => "magnitude (V)",
        };

        code_frame().show(ui, |ui| {
            let apply = apply_b.then(|| mag_bounds);
            ac_plot_frame(
                "ac_magnitude", half_h.max(120.0), mag_scale, AcCursor::Quantity(mag_label.to_string()),
                &mag_label, &node_mag_series, apply, ui,
            );
        });

        ui.add_space(8.0);

        // ── Phase plot ───────────────────────────────────────────────────────
        code_frame().show(ui, |ui| {
            let apply = apply_b.then(|| phase_bounds);
            ac_plot_frame(
                "ac_phase", half_h.max(120.0), mag_scale, AcCursor::Phase,
                "phase (°)", &node_phase_series, apply, ui,
            );
        });

        ui.add_space(8.0);

        // ── Branch current magnitude plot ─────────────────────────────────────
        code_frame().show(ui, |ui| {
            let cur_mag_label = match mag_scale {
                AcMagScale::Db     => "current magnitude (dB·A)",
                AcMagScale::Linear => "current magnitude (A)",
            };
            let apply = apply_b.then(|| mag_bounds);
            ac_plot_frame(
                "ac_branch_current_mag", half_h.max(120.0), mag_scale,
                AcCursor::Quantity(cur_mag_label.to_string()), cur_mag_label,
                &cur_mag_series, apply, ui,
            );
        });

        ui.add_space(8.0);

        // ── Branch current phase plot ─────────────────────────────────────────
        code_frame().show(ui, |ui| {
            let apply = apply_b.then(|| phase_bounds);
            ac_plot_frame(
                "ac_branch_current_phase", half_h.max(120.0), mag_scale,
                AcCursor::Phase, "current phase (°)",
                &cur_phase_series, apply, ui,
            );
        });

        // Toggle off the bounds application after all plots have processed it
        self.plot.ac_apply = false;
    }

}
