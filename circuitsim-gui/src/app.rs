use circuitsim::{parse, run, AcResult, DcResult, SimulationResult, TranResult};
use egui;
use egui_plot::{Legend, Line, Plot, PlotPoints};

use std::collections::{HashMap, HashSet};
use egui::{Color32, Pos2, Rect, Stroke, Vec2};

const GRID_SIZE: f32 = 20.0;

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Direction { Right, Down, Left, Up }

impl Direction {
    fn next(self) -> Self {
        match self {
            Direction::Right => Direction::Down,
            Direction::Down => Direction::Left,
            Direction::Left => Direction::Up,
            Direction::Up => Direction::Right,
        }
    }

    fn offset(self, length: i32) -> (i32, i32) {
        match self {
            Direction::Right => (length, 0),
            Direction::Down => (0, length),
            Direction::Left => (-length, 0),
            Direction::Up => (0, -length),
        }
    }
}

#[derive(Clone, Copy, PartialEq, Eq, Hash)]
struct GridPt(i32, i32);

impl GridPt {
    fn from_pos(pos: Pos2) -> Self {
        Self((pos.x / GRID_SIZE).round() as i32, (pos.y / GRID_SIZE).round() as i32)
    }
    fn to_pos(self) -> Pos2 {
        Pos2::new(self.0 as f32 * GRID_SIZE, self.1 as f32 * GRID_SIZE)
    }
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum CompKind { Resistor, Capacitor, Inductor, Voltage, Ground }

#[derive(Clone)]
struct Component {
    kind: CompKind,
    p1: GridPt,
    p2: GridPt,
    val: String,
}

#[derive(Clone)]
struct Wire {
    p1: GridPt,
    p2: GridPt,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Tool { Select, Wire, R, C, L, V, Gnd }

#[derive(Clone, Copy, PartialEq, Eq)]
enum AnalysisType { Tran, Ac }

impl Default for AnalysisType {
    fn default() -> Self { AnalysisType::Tran }
}

struct SchematicState {
    tool: Tool,
    components: Vec<Component>,
    wires: Vec<Wire>,
    active_wire_start: Option<GridPt>,
    current_dir: Direction,
    selected_component: Option<usize>,
    selected_wire: Option<usize>,
    analysis_type: AnalysisType,
    tran_step: String,
    tran_stop: String,
    ac_variation: String,
    ac_points: String,
    ac_fstart: String,
    ac_fstop: String,
}

impl Default for SchematicState {
    fn default() -> Self {
        Self {
            tool: Tool::Select,
            components: vec![],
            wires: vec![],
            active_wire_start: None,
            current_dir: Direction::Down,
            selected_component: None,
            selected_wire: None,
            analysis_type: AnalysisType::Tran,
            tran_step: "1u".to_string(),
            tran_stop: "5m".to_string(),
            ac_variation: "dec".to_string(),
            ac_points: "20".to_string(),
            ac_fstart: "1".to_string(),
            ac_fstop: "10k".to_string(),
        }
    }
}

#[derive(PartialEq, Eq)]
enum EditorMode { Text, Schematic }

// --- Node Extraction (Union-Find) ---
struct Dsu {
    parent: Vec<usize>,
}

impl Dsu {
    fn new(size: usize) -> Self {
        Self { parent: (0..size).collect() }
    }
    fn find(&mut self, i: usize) -> usize {
        if self.parent[i] == i {
            i
        } else {
            let p = self.parent[i];
            let root = self.find(p);
            self.parent[i] = root;
            root
        }
    }
    fn union(&mut self, i: usize, j: usize) {
        let root_i = self.find(i);
        let root_j = self.find(j);
        if root_i != root_j {
            self.parent[root_i] = root_j;
        }
    }
}


use crate::theme::{
    self, card_frame, code_frame, editor_frame, panel_frame, section_heading, status_chip,
    ACCENT, BG_PANEL, BG_SURFACE, PLOT_COLORS, STATUS_ERROR, STATUS_OK, TEXT_SECONDARY,
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
V1 1 0 DC 0
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

/// Which quantity to show on the frequency-response magnitude plot.
#[derive(Clone, Copy, PartialEq, Eq)]
enum AcMagScale { Db, Linear }

impl Default for AcMagScale {
    fn default() -> Self { AcMagScale::Db }
}

pub struct CircuitSimApp {
    netlist: String,
    file_label: String,
    status: Option<(bool, String)>,
    circuit_summary: Option<String>,
    dc: Option<DcResult>,
    tran: Option<TranResult>,
    ac: Option<AcResult>,
    plot_nodes: Vec<bool>,
    ac_plot_nodes: Vec<bool>,
    ac_mag_scale: AcMagScale,
    result_tab: ResultTab,
    editor_mode: EditorMode,
    schematic: SchematicState,
    plot_x_scale: String,
    plot_y_scale: String,
    plot_x_min: String,
    plot_x_max: String,
    plot_y_min: String,
    plot_y_max: String,
    apply_plot_bounds: bool,

    ac_plot_x_scale: String,
    ac_mag_y_scale: String,
    ac_phase_y_scale: String,
    ac_plot_x_min: String,
    ac_plot_x_max: String,
    ac_mag_y_min: String,
    ac_mag_y_max: String,
    ac_phase_y_min: String,
    ac_phase_y_max: String,
    ac_apply_plot_bounds: bool,
}

impl Default for CircuitSimApp {
    fn default() -> Self {
        Self {
            netlist: DEFAULT_NETLIST.to_string(),
            file_label: "untitled.cir".to_string(),
            status: None,
            circuit_summary: None,
            dc: None,
            tran: None,
            ac: None,
            plot_nodes: vec![false; 16],
            ac_plot_nodes: vec![false; 16],
            ac_mag_scale: AcMagScale::default(),
            result_tab: ResultTab::Overview,
            editor_mode: EditorMode::Text,
            schematic: SchematicState::default(),
            plot_x_scale: "1.0".to_string(),
            plot_y_scale: "1.0".to_string(),
            plot_x_min: "".to_string(),
            plot_x_max: "".to_string(),
            plot_y_min: "".to_string(),
            plot_y_max: "".to_string(),
            apply_plot_bounds: false,

            ac_plot_x_scale: "1.0".to_string(),
            ac_mag_y_scale: "1.0".to_string(),
            ac_phase_y_scale: "1.0".to_string(),
            ac_plot_x_min: "".to_string(),
            ac_plot_x_max: "".to_string(),
            ac_mag_y_min: "".to_string(),
            ac_mag_y_max: "".to_string(),
            ac_phase_y_min: "".to_string(),
            ac_phase_y_max: "".to_string(),
            ac_apply_plot_bounds: false,
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
        let mut points = HashSet::new();

        // Collect all unique grid points
        for c in &self.schematic.components {
            points.insert(c.p1);
            points.insert(c.p2);
        }
        for w in &self.schematic.wires {
            points.insert(w.p1);
            points.insert(w.p2);
        }

        let pt_list: Vec<GridPt> = points.into_iter().collect();
        let mut pt_to_idx: HashMap<GridPt, usize> = HashMap::new();
        for (i, &pt) in pt_list.iter().enumerate() {
            pt_to_idx.insert(pt, i);
        }

        let mut dsu = Dsu::new(pt_list.len());

        // Wires connect nodes
        for w in &self.schematic.wires {
            let i1 = pt_to_idx[&w.p1];
            let i2 = pt_to_idx[&w.p2];
            dsu.union(i1, i2);
        }

        // Identify ground components to force Node 0
        let mut ground_roots = HashSet::new();
        for c in &self.schematic.components {
            if c.kind == CompKind::Ground {
                let root = dsu.find(pt_to_idx[&c.p1]);
                ground_roots.insert(root);
            }
        }

        // Map DSU roots to SPICE node numbers
        let mut root_to_spicenode: HashMap<usize, usize> = HashMap::new();
        let mut next_node_id = 1;

        for i in 0..pt_list.len() {
            let root = dsu.find(i);
            if !root_to_spicenode.contains_key(&root) {
                if ground_roots.contains(&root) {
                    root_to_spicenode.insert(root, 0);
                } else {
                    root_to_spicenode.insert(root, next_node_id);
                    next_node_id += 1;
                }
            }
        }

        // Generate Text
        let mut out = String::from("* Auto-generated from Schematic\n");
        let mut r_idx = 1; let mut c_idx = 1; let mut l_idx = 1; let mut v_idx = 1;

        for c in &self.schematic.components {
            if c.kind == CompKind::Ground { continue; }

            let n1 = root_to_spicenode[&dsu.find(pt_to_idx[&c.p1])];
            let n2 = root_to_spicenode[&dsu.find(pt_to_idx[&c.p2])];

            match c.kind {
                CompKind::Resistor => { out.push_str(&format!("R{} {} {} {}\n", r_idx, n1, n2, c.val)); r_idx += 1; },
                CompKind::Capacitor => { out.push_str(&format!("C{} {} {} {}\n", c_idx, n1, n2, c.val)); c_idx += 1; },
                CompKind::Inductor => { out.push_str(&format!("L{} {} {} {}\n", l_idx, n1, n2, c.val)); l_idx += 1; },
                CompKind::Voltage => { out.push_str(&format!("V{} {} {} {}\n", v_idx, n1, n2, c.val)); v_idx += 1; },
                CompKind::Ground => unreachable!(),
            }
        }

        out.push_str(".op\n");
        match self.schematic.analysis_type {
            AnalysisType::Tran => {
                out.push_str(&format!(".tran {} {}\n", self.schematic.tran_step, self.schematic.tran_stop));
            }
            AnalysisType::Ac => {
                out.push_str(&format!(".ac {} {} {} {}\n",
                    self.schematic.ac_variation,
                    self.schematic.ac_points,
                    self.schematic.ac_fstart,
                    self.schematic.ac_fstop,
                ));
            }
        }
        out.push_str(".end\n");
        self.netlist = out;
        self.status = Some((true, "Generated netlist from schematic".into()));
    }
    fn run_simulation(&mut self) {
        match parse(&self.netlist) {
            Ok(netlist) => {
                let summary = format!(
                    "{} nodes · {} R · {} C · {} L · {} V",
                    netlist.circuit.nodes,
                    netlist.circuit.resistors.len(),
                    netlist.circuit.capacitors.len(),
                    netlist.circuit.inductors.len(),
                    netlist.circuit.voltage_sources.len()
                );
                self.circuit_summary = Some(summary);

                match run(&netlist.circuit, &netlist.analysis) {
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
        self.dc   = result.dc;
        self.tran = result.tran.clone();
        self.ac   = result.ac.clone();

        let max_nodes = self
            .tran
            .as_ref()
            .and_then(|t| t.points.first())
            .map(|p| p.node_voltages.len())
            .or_else(|| self.dc.as_ref().map(|d| d.node_voltages.len()))
            .or_else(|| {
                self.ac.as_ref()
                    .and_then(|a| a.points.first())
                    .map(|p| p.node_voltages.len())
            })
            .unwrap_or(8);

        self.plot_nodes.resize(max_nodes, false);
        for i in 1..max_nodes.min(self.plot_nodes.len()) {
            self.plot_nodes[i] = i <= 3;
        }

        // Default: enable same nodes for AC plots, auto-switch tab if .ac present
        self.ac_plot_nodes.resize(max_nodes, false);
        for i in 1..max_nodes.min(self.ac_plot_nodes.len()) {
            self.ac_plot_nodes[i] = i <= 3;
        }
        if self.ac.is_some() {
            self.result_tab = ResultTab::FreqResponse;
        }
    }

    fn clear_results(&mut self) {
        self.circuit_summary = None;
        self.dc   = None;
        self.tran = None;
        self.ac   = None;
    }

    fn load_example(&mut self, ex: Example) {
        self.netlist = ex.netlist().to_string();
        self.file_label = format!("{}.cir", ex.label().to_lowercase().replace(' ', "_"));
        self.status = Some((true, format!("Loaded example: {}", ex.label())));
        self.clear_results();
    }

    fn open_file(&mut self) {
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

    fn save_file(&mut self) {
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
            ui.label(egui::RichText::new("R · C · L · V  —  .op  .tran  .ac").color(TEXT_SECONDARY).size(12.0));
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
            self.schematic.current_dir = self.schematic.current_dir.next();
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
                        self.schematic.tool = t;
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
                        self.schematic.tool = t;
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

        // Draw dot grid for a clean PCB/schematic look
        let dot_color = theme::BORDER.gamma_multiply(0.6);
        for xi in 0..=(rect.width() as i32 / GRID_SIZE as i32) {
            for yi in 0..=(rect.height() as i32 / GRID_SIZE as i32) {
                let pos = rect.left_top() + Vec2::new(xi as f32 * GRID_SIZE, yi as f32 * GRID_SIZE);
                painter.circle_filled(pos, 0.8, dot_color);
            }
        }

        let to_screen = |grid_pt: GridPt| rect.left_top() + grid_pt.to_pos().to_vec2();

        // Draw existing wires — crisp, clean lines with a slight glow tint
        let wire_color = Color32::from_rgb(80, 200, 120); // professional green
        let wire_selected_color = Color32::from_rgb(255, 100, 80);
        for (wi, w) in self.schematic.wires.iter().enumerate() {
            let color = if self.schematic.selected_wire == Some(wi) { wire_selected_color } else { wire_color };
            painter.line_segment([to_screen(w.p1), to_screen(w.p2)], Stroke::new(if self.schematic.selected_wire == Some(wi) { 3.0 } else { 2.0 }, color));
            // Junction dots at endpoints for clarity
            painter.circle_filled(to_screen(w.p1), 2.5, color);
            painter.circle_filled(to_screen(w.p2), 2.5, color);
        }

        // Draw active wire preview
        if let Some(hover_pos) = response.hover_pos() {
            let hover_grid = GridPt::from_pos(hover_pos - rect.left_top().to_vec2());

            if self.schematic.tool == Tool::Wire {
                if let Some(start_grid) = self.schematic.active_wire_start {
                    painter.line_segment([to_screen(start_grid), to_screen(hover_grid)], Stroke::new(2.0, theme::ACCENT));
                }
                painter.circle_filled(to_screen(hover_grid), 4.0, theme::ACCENT);
            } else if matches!(self.schematic.tool, Tool::R | Tool::C | Tool::L | Tool::V) {
                // Ghost component preview
                let (dx, dy) = self.schematic.current_dir.offset(3);
                let p2_grid = GridPt(hover_grid.0 + dx, hover_grid.1 + dy);

                painter.line_segment(
                    [to_screen(hover_grid), to_screen(p2_grid)],
                    Stroke::new(1.5, theme::TEXT_SECONDARY.gamma_multiply(0.4))
                );
                // Ghost body box at center of preview
                let ghost_center = to_screen(hover_grid) + (to_screen(p2_grid) - to_screen(hover_grid)) / 2.0;
                let is_h = hover_grid.1 == p2_grid.1;
                let ghost_rect = Rect::from_center_size(ghost_center,
                    if is_h { Vec2::new(28.0, 16.0) } else { Vec2::new(16.0, 28.0) });
                painter.rect_stroke(ghost_rect, 3.0, Stroke::new(1.5, theme::ACCENT.gamma_multiply(0.5)));
                // Mark Pin 1 so polarity is obvious (useful for Voltage sources)
                painter.circle_filled(to_screen(hover_grid), 3.5, theme::ACCENT.gamma_multiply(0.9));
            }
        }

        // Draw Components with proper electronic symbols
        let comp_stroke = Stroke::new(2.0, Color32::from_rgb(220, 225, 235));
        let selected_stroke = Stroke::new(2.0, theme::ACCENT);

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

        // Handle Interactions
        if response.clicked() {
            if let Some(interact_pos) = response.interact_pointer_pos() {
                let grid_pt = GridPt::from_pos(interact_pos - rect.left_top().to_vec2());

                match self.schematic.tool {
                    Tool::Wire => {
                        if let Some(start) = self.schematic.active_wire_start {
                            if start != grid_pt {
                                self.schematic.wires.push(Wire { p1: start, p2: grid_pt });
                            }
                            self.schematic.active_wire_start = None;
                        } else {
                            self.schematic.active_wire_start = Some(grid_pt);
                        }
                    }
                    Tool::R | Tool::C | Tool::L | Tool::V => {
                        let kind = match self.schematic.tool {
                            Tool::R => CompKind::Resistor, Tool::C => CompKind::Capacitor,
                            Tool::L => CompKind::Inductor, Tool::V => CompKind::Voltage,
                            _ => unreachable!(),
                        };
                        let val = match kind {
                            CompKind::Resistor => "1k", CompKind::Capacitor => "1u",
                            CompKind::Inductor => "1m", CompKind::Voltage => "DC 5",
                            _ => "",
                        }.to_string();

                        // Calculate P2 based on current rotation direction
                        let (dx, dy) = self.schematic.current_dir.offset(3);
                        self.schematic.components.push(Component {
                            kind,
                            p1: grid_pt,
                            p2: GridPt(grid_pt.0 + dx, grid_pt.1 + dy),
                                                       val
                        });
                    }
                    Tool::Gnd => {
                        self.schematic.components.push(Component {
                            kind: CompKind::Ground, p1: grid_pt, p2: grid_pt, val: "".into()
                        });
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
                        self.schematic.selected_component = selected;

                        // If no component hit, check wires
                        if selected.is_none() {
                            let mut hit_wire = None;
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
                                if dist_sq < 36.0 { // 6px radius
                                    hit_wire = Some(wi);
                                    break;
                                }
                            }
                            self.schematic.selected_wire = hit_wire;
                        } else {
                            self.schematic.selected_wire = None;
                        }
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
            self.schematic.active_wire_start = None;
            self.schematic.selected_component = None; // Deselect on right click
            self.schematic.selected_wire = None;
        }

        // Cancel wire dragging on Right Click
        if response.secondary_clicked() {
            self.schematic.active_wire_start = None;
            self.schematic.selected_component = None; // Deselect on right click
            self.schematic.selected_wire = None;
        }

        // Delete key removes selected wire or component
        if ui.input(|i| i.key_pressed(egui::Key::Delete) || i.key_pressed(egui::Key::Backspace)) {
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
            self.schematic.wires.remove(wi);
            self.schematic.selected_wire = None;
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
                    CompKind::Voltage   => ("Voltage Src", Color32::from_rgb(255, 200, 80)),
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
                        ui.horizontal(|ui| {
                            ui.label(egui::RichText::new("Type").size(11.0).color(TEXT_SECONDARY));
                            ui.add_space(8.0);

                            // Determine current type based on string prefix
                            let current_type = if c.val.starts_with("SIN") { "SIN" }
                            else if c.val.starts_with("PULSE") { "PULSE" }
                            else if c.val.starts_with("AC") { "AC" }
                            else { "DC" };

                            let mut new_type = current_type;

                            egui::ComboBox::from_id_salt("v_type")
                            .selected_text(current_type)
                            .width(90.0)
                            .show_ui(ui, |ui| {
                                ui.selectable_value(&mut new_type, "DC", "DC");
                                ui.selectable_value(&mut new_type, "AC", "AC");
                                ui.selectable_value(&mut new_type, "SIN", "Sine");
                                ui.selectable_value(&mut new_type, "PULSE", "Pulse");
                            });

                            // Auto-fill a template if the user changes the dropdown type
                            if new_type != current_type {
                                c.val = match new_type {
                                    "DC" => "DC 5".to_string(),
                                      "AC" => "AC 1".to_string(),
                                      "SIN" => "SIN(0 5 1k 0 0)".to_string(),
                                      "PULSE" => "PULSE(0 5 1m 1u 1u 5m 10m)".to_string(),
                                      _ => c.val.clone(),
                                };
                            }
                        });
                        ui.add_space(6.0);
                        ui.horizontal(|ui| {
                            ui.label(egui::RichText::new("Params").size(11.0).color(TEXT_SECONDARY));
                            ui.add_space(4.0);
                            ui.add(egui::TextEdit::singleline(&mut c.val).desired_width(140.0));
                        });

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
            self.schematic.components.remove(idx);
            self.schematic.selected_component = None;
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
        if self.dc.is_none() && self.tran.is_none() && self.ac.is_none() {
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
                                    ui.label(format!("V({i})"));
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

        plot.show(ui, |plot_ui| {
            for (node, &enabled) in self.plot_nodes.iter().enumerate() {
                if !enabled || node == 0 {
                    continue;
                }
                let points: PlotPoints = tran
                    .points
                    .iter()
                    .map(|p| {
                        [
                            p.time as f64,
                            p.node_voltages.get(node).copied().unwrap_or(0.0) as f64,
                        ]
                    })
                    .collect();
                let color = PLOT_COLORS[node % PLOT_COLORS.len()];
                plot_ui.line(
                    Line::new(points)
                        .name(format!("V({node})"))
                        .color(color)
                        .width(1.5),
                );
            }
        });
    }

    fn dc_tab(&mut self, ui: &mut egui::Ui) {
        let Some(dc) = &self.dc else {
            ui.label(egui::RichText::new("No DC analysis in netlist (.op)").color(TEXT_SECONDARY));
            return;
        };

        card_frame().show(ui, |ui| {
                egui::Grid::new("dc_grid")
                    .num_columns(2)
                    .spacing([24.0, 8.0])
                    .striped(true)
                    .show(ui, |ui| {
                        ui.label(egui::RichText::new("Node").strong());
                        ui.label(egui::RichText::new("Voltage").strong());
                        ui.end_row();
                        for (i, v) in dc.node_voltages.iter().enumerate() {
                            ui.label(format!("V({i})"));
                            ui.label(
                                egui::RichText::new(format!("{v:.6e} V"))
                                    .family(egui::FontFamily::Monospace)
                                    .color(ACCENT),
                            );
                            ui.end_row();
                        }
                    });

                if !dc.source_currents.is_empty() {
                    ui.add_space(16.0);
                    section_heading(ui, "Source currents");
                    egui::Grid::new("src_grid")
                        .num_columns(2)
                        .spacing([24.0, 8.0])
                        .show(ui, |ui| {
                            for (i, &amp) in dc.source_currents.iter().enumerate() {
                                ui.label(format!("I(V{i})"));
                                ui.label(
                                    egui::RichText::new(format!("{amp:.6e} A"))
                                        .family(egui::FontFamily::Monospace),
                                );
                                ui.end_row();
                            }
                        });
                }
            });
    }

    fn waveforms_tab(&mut self, ui: &mut egui::Ui) {
        let Some(tran) = &self.tran else {
            ui.label(
                egui::RichText::new("No transient analysis (.tran) in netlist")
                    .color(TEXT_SECONDARY),
            );
            return;
        };

        ui.horizontal_wrapped(|ui| {
            ui.label(egui::RichText::new("Plot nodes:").color(TEXT_SECONDARY));
            for i in 1..self.plot_nodes.len() {
                let mut on = self.plot_nodes[i];
                if ui.checkbox(&mut on, format!("V({i})")).changed() {
                    self.plot_nodes[i] = on;
                }
            }
        });

        // NEW: Scaling and Limits Controls
        ui.horizontal_wrapped(|ui| {
            /*ui.label(egui::RichText::new("Mult X:").color(TEXT_SECONDARY));
            ui.add(egui::TextEdit::singleline(&mut self.plot_x_scale).desired_width(40.0));
            ui.label(egui::RichText::new("Mult Y:").color(TEXT_SECONDARY));
            ui.add(egui::TextEdit::singleline(&mut self.plot_y_scale).desired_width(40.0));

            ui.separator();

            ui.label(egui::RichText::new("Limits X [").color(TEXT_SECONDARY));
            ui.add(egui::TextEdit::singleline(&mut self.plot_x_min).desired_width(40.0));
            ui.label(",");
            ui.add(egui::TextEdit::singleline(&mut self.plot_x_max).desired_width(40.0));
            ui.label("]");

            ui.label(egui::RichText::new("Y [").color(TEXT_SECONDARY));
            ui.add(egui::TextEdit::singleline(&mut self.plot_y_min).desired_width(40.0));
            ui.label(",");
            ui.add(egui::TextEdit::singleline(&mut self.plot_y_max).desired_width(40.0));
            ui.label("]");

            if ui.button("Apply").clicked() {
                self.apply_plot_bounds = true;
            }*/
            if ui.button("Auto Fit").clicked() {
                self.plot_x_min.clear();
                self.plot_x_max.clear();
                self.plot_y_min.clear();
                self.plot_y_max.clear();
                self.apply_plot_bounds = true;
            }
        });

        ui.add_space(8.0);

        let x_mult = self.plot_x_scale.parse::<f64>().unwrap_or(1.0);
        let y_mult = self.plot_y_scale.parse::<f64>().unwrap_or(1.0);

        code_frame()
            .show(ui, |ui| {
                let plot = Plot::new("waveforms")
                    .height(ui.available_height() - 8.0)
                    .x_axis_label("time (s)")
                    .y_axis_label("voltage (V)")
                    .legend(Legend::default().position(egui_plot::Corner::RightTop))
                    .show_background(true)
                    .allow_scroll(true)
                    .allow_drag(true)
                    .allow_boxed_zoom(true);

                plot.show(ui, |plot_ui| {

                    // NEW: Apply Manual View Bounds
                    // NEW: Apply Manual View Bounds
                    if self.apply_plot_bounds {
                        // Check if user cleared all inputs to trigger an auto-fit reset
                        if self.plot_x_min.is_empty() && self.plot_x_max.is_empty()
                            && self.plot_y_min.is_empty() && self.plot_y_max.is_empty() {
                                plot_ui.set_auto_bounds(egui::Vec2b::new(true, true));
                            } else {
                                // TURN OFF AUTO BOUNDS so manual bounds aren't immediately overridden
                                plot_ui.set_auto_bounds(egui::Vec2b::new(false, false));

                                let current = plot_ui.plot_bounds();
                                let mut x_min = current.min()[0];
                                let mut x_max = current.max()[0];
                                let mut y_min = current.min()[1];
                                let mut y_max = current.max()[1];

                                // Override only the fields the user has typed into
                                if let Ok(v) = self.plot_x_min.parse::<f64>() { x_min = v; }
                                if let Ok(v) = self.plot_x_max.parse::<f64>() { x_max = v; }
                                if let Ok(v) = self.plot_y_min.parse::<f64>() { y_min = v; }
                                if let Ok(v) = self.plot_y_max.parse::<f64>() { y_max = v; }

                                // Prevent panics if the user types min >= max
                                let safe_x_min = x_min.min(x_max);
                                let safe_x_max = if x_min == x_max { x_max + 1e-6 } else { x_min.max(x_max) };
                                let safe_y_min = y_min.min(y_max);
                                let safe_y_max = if y_min == y_max { y_max + 1e-6 } else { y_min.max(y_max) };

                                plot_ui.set_plot_bounds(egui_plot::PlotBounds::from_min_max(
                                    [safe_x_min, safe_y_min],
                                    [safe_x_max, safe_y_max],
                                ));
                            }
                            self.apply_plot_bounds = false; // Execute once per click
                    }

                    for (node, &enabled) in self.plot_nodes.iter().enumerate() {
                        if !enabled || node == 0 {
                            continue;
                        }
                        let points: PlotPoints = tran
                            .points
                            .iter()
                            .map(|p| {
                                [
                                    (p.time as f64) * x_mult,
                                 (p.node_voltages.get(node).copied().unwrap_or(0.0) as f64) * y_mult,
                                ]
                            })
                            .collect();
                        let color = PLOT_COLORS[node % PLOT_COLORS.len()];
                        plot_ui.line(
                            Line::new(points)
                                .name(format!("V({node})"))
                                .color(color)
                                .width(2.0),
                        );
                    }
                });
            });
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

        let n_nodes = ac.points.first().map(|p| p.node_voltages.len()).unwrap_or(1);

        // ── Controls row ────────────────────────────────────────────────────
        ui.horizontal_wrapped(|ui| {
            ui.label(egui::RichText::new("Nodes:").color(TEXT_SECONDARY));
            for i in 1..n_nodes.min(self.ac_plot_nodes.len()) {
                let mut on = self.ac_plot_nodes[i];
                if ui.checkbox(&mut on, format!("V({i})")).changed() {
                    self.ac_plot_nodes[i] = on;
                }
            }
            ui.separator();
            ui.label(egui::RichText::new("Scale:").color(TEXT_SECONDARY));
            ui.selectable_value(&mut self.ac_mag_scale, AcMagScale::Db,     "dB");
            ui.selectable_value(&mut self.ac_mag_scale, AcMagScale::Linear, "Linear");
        });

        ui.add_space(4.0);

        // NEW: Scaling and Limits Controls for AC
        ui.horizontal_wrapped(|ui| {
            /*ui.label(egui::RichText::new("Mult X:").color(TEXT_SECONDARY));
            ui.add(egui::TextEdit::singleline(&mut self.ac_plot_x_scale).desired_width(40.0));
            ui.label(egui::RichText::new("Mult Y(Mag):").color(TEXT_SECONDARY));
            ui.add(egui::TextEdit::singleline(&mut self.ac_mag_y_scale).desired_width(40.0));
            ui.label(egui::RichText::new("Mult Y(Phs):").color(TEXT_SECONDARY));
            ui.add(egui::TextEdit::singleline(&mut self.ac_phase_y_scale).desired_width(40.0));

            ui.separator();

            ui.label(egui::RichText::new("Limits X [").color(TEXT_SECONDARY));
            ui.add(egui::TextEdit::singleline(&mut self.ac_plot_x_min).desired_width(40.0));
            ui.label(",");
            ui.add(egui::TextEdit::singleline(&mut self.ac_plot_x_max).desired_width(40.0));
            ui.label("]");

            ui.label(egui::RichText::new("Y(Mag) [").color(TEXT_SECONDARY));
            ui.add(egui::TextEdit::singleline(&mut self.ac_mag_y_min).desired_width(40.0));
            ui.label(",");
            ui.add(egui::TextEdit::singleline(&mut self.ac_mag_y_max).desired_width(40.0));
            ui.label("]");

            ui.label(egui::RichText::new("Y(Phs) [").color(TEXT_SECONDARY));
            ui.add(egui::TextEdit::singleline(&mut self.ac_phase_y_min).desired_width(40.0));
            ui.label(",");
            ui.add(egui::TextEdit::singleline(&mut self.ac_phase_y_max).desired_width(40.0));
            ui.label("]");

            if ui.button("Apply").clicked() {
                self.ac_apply_plot_bounds = true;
            }*/
            if ui.button("Auto Fit").clicked() {
                self.ac_plot_x_min.clear();
                self.ac_plot_x_max.clear();
                self.ac_mag_y_min.clear();
                self.ac_mag_y_max.clear();
                self.ac_phase_y_min.clear();
                self.ac_phase_y_max.clear();
                self.ac_apply_plot_bounds = true;
            }
        });

        ui.add_space(6.0);

        // Take a snapshot of what we need so we can release the borrow on self.ac
        // before calling self methods.
        let points_snapshot: Vec<_> = ac.points.iter().map(|p| {
            (p.freq, p.node_voltages.clone())
        }).collect();

        let mag_scale = self.ac_mag_scale;
        let ac_plot_nodes = self.ac_plot_nodes.clone();

        let available_h = ui.available_height();
        let half_h = (available_h - 24.0) / 2.0;

        let x_mult = self.ac_plot_x_scale.parse::<f64>().unwrap_or(1.0);
        let mag_y_mult = self.ac_mag_y_scale.parse::<f64>().unwrap_or(1.0);
        let phase_y_mult = self.ac_phase_y_scale.parse::<f64>().unwrap_or(1.0);

        // ── Magnitude plot ───────────────────────────────────────────────────
        let mag_label = match mag_scale {
            AcMagScale::Db     => "magnitude (dB)",
            AcMagScale::Linear => "magnitude (V/V)",
        };

        code_frame().show(ui, |ui| {
            let mut plot = Plot::new("ac_magnitude")
            .height(half_h.max(120.0))
            .x_axis_label("frequency (Hz)")
            .y_axis_label(mag_label)
            .legend(Legend::default().position(egui_plot::Corner::RightTop))
            .show_background(true)
            .allow_scroll(true)
            .allow_drag(true)
            .allow_boxed_zoom(true);

            plot = match mag_scale {
                AcMagScale::Db => plot
                .x_grid_spacer(egui_plot::log_grid_spacer(10))
                .label_formatter(move |name, val| {
                    if name.is_empty() { return String::new(); }
                    let f = 10.0_f64.powf(val.x); // convert log10(x) back to real freq for the tooltip
                    format!("{}\nf = {:.3e} Hz\n{} = {:.3e}", name, f, mag_label, val.y)
                }),
                AcMagScale::Linear => plot
                .label_formatter(move |name, val| {
                    if name.is_empty() { return String::new(); }
                    format!("{}\nf = {:.3e} Hz\n{} = {:.3e}", name, val.x, mag_label, val.y)
                }),
            };

            plot.show(ui, |plot_ui| {

                // NEW: Apply bounds to Magnitude Plot
                if self.ac_apply_plot_bounds {
                    if self.ac_plot_x_min.is_empty() && self.ac_plot_x_max.is_empty()
                        && self.ac_mag_y_min.is_empty() && self.ac_mag_y_max.is_empty() {
                            plot_ui.set_auto_bounds(egui::Vec2b::new(true, true));
                        } else {
                            // TURN OFF AUTO BOUNDS
                            plot_ui.set_auto_bounds(egui::Vec2b::new(false, false));

                            let current = plot_ui.plot_bounds();
                            let mut x_min = current.min()[0];
                            let mut x_max = current.max()[0];
                            let mut y_min = current.min()[1];
                            let mut y_max = current.max()[1];

                            // Note: If using Db scale, internal X axis is actually plotted in log10
                            if let Ok(v) = self.ac_plot_x_min.parse::<f64>() {
                                x_min = if matches!(mag_scale, AcMagScale::Db) && v > 0.0 { v.log10() } else { v };
                            }
                            if let Ok(v) = self.ac_plot_x_max.parse::<f64>() {
                                x_max = if matches!(mag_scale, AcMagScale::Db) && v > 0.0 { v.log10() } else { v };
                            }
                            if let Ok(v) = self.ac_mag_y_min.parse::<f64>() { y_min = v; }
                            if let Ok(v) = self.ac_mag_y_max.parse::<f64>() { y_max = v; }

                            let safe_x_min = x_min.min(x_max);
                            let safe_x_max = if x_min == x_max { x_max + 1e-6 } else { x_min.max(x_max) };
                            let safe_y_min = y_min.min(y_max);
                            let safe_y_max = if y_min == y_max { y_max + 1e-6 } else { y_min.max(y_max) };

                            plot_ui.set_plot_bounds(egui_plot::PlotBounds::from_min_max(
                                [safe_x_min, safe_y_min],
                                [safe_x_max, safe_y_max],
                            ));
                        }
                }

                for node in 1..n_nodes.min(ac_plot_nodes.len()) {
                    if !ac_plot_nodes[node] { continue; }
                    let pts: PlotPoints = points_snapshot.iter().map(|(freq, voltages)| {
                        let v = voltages.get(node).copied().unwrap_or_default();
                        let mag = v.norm();

                        let y = match mag_scale {
                            AcMagScale::Db     => if mag > 0.0 { 20.0 * mag.log10() } else { -200.0 },
                                                                     AcMagScale::Linear => mag,
                        } * mag_y_mult; // Scale applied here

                        let scaled_freq = freq * x_mult;
                        let x = match mag_scale {
                            AcMagScale::Db     => scaled_freq.log10(),
                                                                     AcMagScale::Linear => scaled_freq,
                        };
                        [x, y]
                    }).collect();
                    let color = PLOT_COLORS[node % PLOT_COLORS.len()];
                    plot_ui.line(
                        Line::new(pts)
                        .name(format!("V({node})"))
                        .color(color)
                        .width(2.0),
                    );
                }
            });
        });

        ui.add_space(8.0);

        // ── Phase plot ───────────────────────────────────────────────────────
        code_frame().show(ui, |ui| {
            let mut plot = Plot::new("ac_phase")
            .height(half_h.max(120.0))
            .x_axis_label("frequency (Hz)")
            .y_axis_label("phase (°)")
            .legend(Legend::default().position(egui_plot::Corner::RightTop))
            .show_background(true)
            .allow_scroll(true)
            .allow_drag(true)
            .allow_boxed_zoom(true);

            plot = match mag_scale {
                AcMagScale::Db => plot
                .x_grid_spacer(egui_plot::log_grid_spacer(10))
                .label_formatter(|name, val| {
                    if name.is_empty() { return String::new(); }
                    let f = 10.0_f64.powf(val.x); // convert log10(x) back to real freq for the tooltip
                    format!("{}\nf = {:.3e} Hz\nphase = {:.2}°", name, f, val.y)
                }),
                AcMagScale::Linear => plot
                .label_formatter(|name, val| {
                    if name.is_empty() { return String::new(); }
                    format!("{}\nf = {:.3e} Hz\nphase = {:.2}°", name, val.x, val.y)
                }),
            };

            plot.show(ui, |plot_ui| {

                // NEW: Apply bounds to Phase Plot
                if self.ac_apply_plot_bounds {
                    if self.ac_plot_x_min.is_empty() && self.ac_plot_x_max.is_empty()
                        && self.ac_phase_y_min.is_empty() && self.ac_phase_y_max.is_empty() {
                            plot_ui.set_auto_bounds(egui::Vec2b::new(true, true));
                        } else {
                            // TURN OFF AUTO BOUNDS
                            plot_ui.set_auto_bounds(egui::Vec2b::new(false, false));

                            let current = plot_ui.plot_bounds();
                            let mut x_min = current.min()[0];
                            let mut x_max = current.max()[0];
                            let mut y_min = current.min()[1];
                            let mut y_max = current.max()[1];

                            if let Ok(v) = self.ac_plot_x_min.parse::<f64>() {
                                x_min = if matches!(mag_scale, AcMagScale::Db) && v > 0.0 { v.log10() } else { v };
                            }
                            if let Ok(v) = self.ac_plot_x_max.parse::<f64>() {
                                x_max = if matches!(mag_scale, AcMagScale::Db) && v > 0.0 { v.log10() } else { v };
                            }
                            if let Ok(v) = self.ac_phase_y_min.parse::<f64>() { y_min = v; }
                            if let Ok(v) = self.ac_phase_y_max.parse::<f64>() { y_max = v; }

                            let safe_x_min = x_min.min(x_max);
                            let safe_x_max = if x_min == x_max { x_max + 1e-6 } else { x_min.max(x_max) };
                            let safe_y_min = y_min.min(y_max);
                            let safe_y_max = if y_min == y_max { y_max + 1e-6 } else { y_min.max(y_max) };

                            plot_ui.set_plot_bounds(egui_plot::PlotBounds::from_min_max(
                                [safe_x_min, safe_y_min],
                                [safe_x_max, safe_y_max],
                            ));
                        }
                }

                for node in 1..n_nodes.min(ac_plot_nodes.len()) {
                    if !ac_plot_nodes[node] { continue; }
                    let pts: PlotPoints = points_snapshot.iter().map(|(freq, voltages)| {
                        let v = voltages.get(node).copied().unwrap_or_default();

                        let phase_deg = v.arg().to_degrees() * phase_y_mult; // Scale applied here
                        let scaled_freq = freq * x_mult;

                        let x = match mag_scale {
                            AcMagScale::Db     => scaled_freq.log10(),
                                                                     AcMagScale::Linear => scaled_freq,
                        };
                        [x, phase_deg]
                    }).collect();
                    let color = PLOT_COLORS[node % PLOT_COLORS.len()];
                    plot_ui.line(
                        Line::new(pts)
                        .name(format!("V({node}) phase"))
                        .color(color)
                        .width(2.0),
                    );
                }
            });
        });

        // Toggle off the bounds application after both plots have processed it
        self.ac_apply_plot_bounds = false;
    }

}
