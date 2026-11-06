use circuitsim::circuit::{CURRENT_FAMILIES, Circuit as CsCircuit, ElementFamily, Pulse, Sine, Waveform};
use circuitsim::netlist::{
    emit as netlist_emit, emit_source_value, parse_source_value, parse_with_options, ParseOptions,
    SourceValue,
};
use circuitsim::units::parse_value;
use circuitsim::{run, AcResult, DcResult, DcSweepResult, SimulationResult, TranResult};
use egui;
use egui_plot::Plot;
use std::collections::HashMap;
use egui::{Color32, Pos2, Rect, Stroke, Vec2};

use crate::symbols;
use crate::chrome;
use crate::icons::{icon_button, Icon};
use crate::plot::{
    ac_current_series, ac_plot_frame, ac_voltage_series, current_slots, dc_sweep_series, draw,
    first_transient_plot, plot_frame, plot_frame_hover, tran_current_series, tran_diff_series,
    tran_voltage_series, AcCursor, AcMagScale, AcQuantity, ExpressionTraces, PlotState,
};
use crate::schematic::{
    instance_names, AnalysisType, CompKind, Component, GridPt, NodeAssignment, Note, SchematicState, Tool,
    GRID_SIZE,
};

/// Centres of the `+` and `-` marks inside a voltage source's circle.
///
/// SPICE `V n1 n2` puts the positive terminal on `n1`, and the editor's `p1`
/// is `n1`, so `+` goes on the `p1` side of the centre and `-` on the `p2`
/// side. (They used to be swapped.)
fn source_polarity_marks(center: Pos2, p1: Pos2, p2: Pos2, radius: f32) -> (Pos2, Pos2) {
    let dir = (p2 - p1).normalized();
    (center - dir * (radius * 0.45), center + dir * (radius * 0.45))
}

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

#[derive(Clone, Copy, PartialEq, Eq)]
enum EditorMode { Text, Schematic }

/// The two child windows of the workspace: the netlist/schematic editor and
/// the waveform/results viewer.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Pane { Editor, Results }

/// How the workspace arranges its child windows (Window menu).
#[derive(Clone, Copy, PartialEq, Eq)]
enum Tiling {
    /// One window fills the workspace — the active one.
    Single,
    /// Stacked: waveforms above the editor, as in the reference.
    Horizontal,
    /// Side by side: editor left, waveforms right.
    Vertical,
}

/// Schematic tools as the toolbar and the Edit menu present them.
const TOOLS: [(Tool, Icon, &str); 7] = [
    (Tool::Select, Icon::Select, "Select"),
    (Tool::Wire, Icon::Wire, "Draw Wire"),
    (Tool::Gnd, Icon::Ground, "Ground"),
    (Tool::R, Icon::Resistor, "Resistor"),
    (Tool::C, Icon::Capacitor, "Capacitor"),
    (Tool::L, Icon::Inductor, "Inductor"),
    (Tool::V, Icon::Voltage, "Voltage Source"),
];


use crate::theme::{
    self, bar_frame, card_frame, code_frame, section_heading, ACCENT, ACCENT_DIM, LT_STRIP,
    LT_WHITE, LT_WORKSPACE, STATUS_ERROR, STATUS_OK, TEXT_DISABLED, TEXT_SECONDARY,
};

const DEFAULT_NETLIST: &str = r#"* RC charging — edit and press Run (F5)
* UIC starts C1 uncharged (0 V); without it the run begins at the DC operating point
V1 1 0 DC 5
R1 1 2 1k
C1 2 0 1u
.tran 10u 5m UIC
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
    const ALL: [Example; 4] = [
        Example::RcCharge,
        Example::VoltageDivider,
        Example::RlCircuit,
        Example::RcLowPass,
    ];

    fn netlist(self) -> &'static str {
        match self {
            Example::RcCharge => {
                r#"* RC charging
                * UIC starts C1 uncharged (0 V); without it the run begins at the DC operating point
                V1 1 0 DC 5
                R1 1 2 1k
                C1 2 0 1u
                .tran 10u 5m UIC
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
                * UIC starts L1 with no current; tau = L/R = 10 us, so 100 us is ten time constants
                V1 1 0 DC 12
                R1 1 2 100
                L1 2 0 1m
                .op
                .tran 1u 100u UIC
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

#[derive(Default, Clone, Copy, PartialEq, Eq, Debug)]
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
    /// Waveform arithmetic: typed expression traces (`V(out)*I(V1)`).
    Expression,
}

/// The Label Net / SPICE Directive dialog: the text being typed, and whether
/// it is the multi-line directive flavor. OK arms the schematic's matching
/// tool; the next canvas click drops the text.
struct NoteDialog {
    directive: bool,
    text: String,
    /// True until the first frame has put the cursor in the text field.
    fresh: bool,
}

/// What the schematic panel highlights because a Waveforms hover reached it
/// across the probe plumbing ("hover a trace, see its source"): a node from a
/// `V(...)` trace, or a family's element slot from an `I(...)` trace.
#[derive(Clone, Copy, PartialEq, Eq)]
enum ProbeFlash {
    Node(usize),
    Element(ElementFamily, usize),
}

/// The schematic object a cross-probe click or hover preview landed on.
#[derive(Clone, Copy, PartialEq, Eq)]
enum ProbeTarget {
    /// Index into the schematic's component list (two-terminal parts only).
    Part(usize),
    /// Index into the schematic's wire list.
    Wire(usize),
}

/// The printed family a canvas part kind reports its current under; `Ground`
/// carries none.
fn canvas_family(kind: CompKind) -> Option<ElementFamily> {
    match kind {
        CompKind::Resistor => Some(ElementFamily::Resistors),
        CompKind::Capacitor => Some(ElementFamily::Capacitors),
        CompKind::Inductor => Some(ElementFamily::Inductors),
        CompKind::Voltage => Some(ElementFamily::VoltageSources),
        CompKind::Diode => Some(ElementFamily::Diodes),
        CompKind::Nmos | CompKind::Pmos => Some(ElementFamily::Mosfets),
        CompKind::Npn | CompKind::Pnp => Some(ElementFamily::Bjts),
        CompKind::Njf | CompKind::Pjf => Some(ElementFamily::Jfets),
        CompKind::Current => Some(ElementFamily::CurrentSources),
        CompKind::Behavioral => Some(ElementFamily::Behavioral),
        CompKind::Opamp => Some(ElementFamily::OpAmps),
        CompKind::Tline => Some(ElementFamily::Tlines),
        // Dependent sources, switches and the transformer print no branch
        // current of their own family on the canvas.
        CompKind::Ground
        | CompKind::Vcvs
        | CompKind::Vccs
        | CompKind::Ccvs
        | CompKind::Cccs
        | CompKind::Switch
        | CompKind::CSwitch
        | CompKind::Transformer => None,
    }
}

/// The screen rectangle that counts as "on" a part for clicks, drags and
/// probes. The original two-terminal parts keep their fixed body box around
/// the midpoint; a multi-pin symbol is hit anywhere on its drawing's bounds.
fn part_hit_rect(c: &Component, to_screen: &impl Fn(GridPt) -> Pos2) -> Rect {
    let p1 = to_screen(c.p1);
    let p2 = to_screen(c.p2);
    if c.pins().len() <= 2 {
        let center = p1 + (p2 - p1) / 2.0;
        let box_size = if c.p1.1 == c.p2.1 { Vec2::new(24.0, 16.0) } else { Vec2::new(16.0, 24.0) };
        return Rect::from_center_size(center, box_size + Vec2::new(12.0, 12.0));
    }
    let len = ((c.p2.0 - c.p1.0).abs() + (c.p2.1 - c.p1.1).abs()) as f32;
    let (lo, hi) = symbols::local_bounds(c.kind, len);
    let (u, v) = symbols::frame(c.p1, c.p2);
    let su = to_screen(GridPt(c.p1.0 + u.0, c.p1.1 + u.1)) - p1;
    let sv = to_screen(GridPt(c.p1.0 + v.0, c.p1.1 + v.1)) - p1;
    let mut rect = Rect::NOTHING;
    for (a, b) in [(lo.0, lo.1), (lo.0, hi.1), (hi.0, lo.1), (hi.0, hi.1)] {
        rect.extend_with(p1 + su * a + sv * b);
    }
    rect.expand(6.0)
}

/// The emitted order of the branch-current walk, expanded to one entry per
/// series: slot k → (family, index within the family). One owner of that
/// mapping instead of the Current tab and the probe clicks each re-deriving
/// it from [`current_slots`].
fn current_slot_targets(tran: &TranResult) -> Vec<(ElementFamily, usize)> {
    let mut out = Vec::new();
    for (family, count) in current_slots(tran) {
        out.extend(
            std::iter::repeat_n(family, count)
                .enumerate()
                .map(|(i, f)| (f, i)),
        );
    }
    out
}

/// How the deck spells schematic net `node`: its label when it has one, else
/// its number — the same rule the netlist writer follows. The schematic's
/// node numbers are *not* the run's node ids once a net is labeled (the
/// parser gives numeric nodes their own number and named nodes ids above
/// them), so probes and highlights compare spellings, never raw numbers.
fn net_spelling(assignment: &NodeAssignment, node: usize) -> String {
    assignment.name_of(node).map_or_else(|| node.to_string(), str::to_string)
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
    /// One flag per emitted branch-current series (slot k = series k, the
    /// [`current_slots`] order); the Current tab renders only the slots that
    /// are on, so the probe clicks can address the same position.
    plot_currents: Vec<bool>,
    ac_mag_scale: AcMagScale,
    result_tab: ResultTab,
    tran_sub_tab: TranSubTab,
    editor_mode: EditorMode,
    schematic: SchematicState,
    /// Axis scales, bounds, and apply-flags for every plot (one owner).
    plot: PlotState,

    /// Directory of the open deck file, where relative `.include`/`.lib`
    /// paths are tried first. `None` for pasted text, built-in examples and
    /// unsaved documents (the bundled libraries still resolve for those).
    deck_dir: Option<std::path::PathBuf>,

    // Differential voltage plot node selectors
    diff_node_a: usize,
    diff_node_b: usize,

    // Waveform arithmetic: the expression traces, the text being typed, and
    // the last add's error.
    expressions: ExpressionTraces,
    expr_input: String,
    expr_error: Option<String>,

    // Workspace: which child window has focus and how they are arranged.
    active_pane: Pane,
    tiling: Tiling,
    show_about: bool,
    /// Dot grid on the schematic canvas (off by default, as in the reference).
    schematic_grid: bool,
    /// Grid lines in the waveform viewer (off by default).
    plot_grid: bool,
    /// Last painted schematic canvas, in screen space — the zoom buttons
    /// zoom about its center.
    canvas_rect: Rect,
    /// The open Label Net / SPICE Directive dialog, if any.
    note_dialog: Option<NoteDialog>,
    /// The popup's edit buffer for the selected annotation (which one, text).
    note_edit: Option<(Note, String)>,
    /// The deck text the shown results were simulated from. Wire probing
    /// trusts the schematic's nets only while the schematic still generates
    /// exactly this text.
    run_text: Option<String>,

    // Cross-probing: the Waveforms hover writes this highlight target and the
    // schematic panel draws it. No frame bookkeeping needed — the plot and
    // the schematic paint inside the same synchronous frame, so the only
    // question is whether a Waveforms plot is even on screen (see
    // [`Self::waveforms_plot_visible`]).
    probe_flash: Option<ProbeFlash>,
    /// The schematic object the previous probe click landed on: clicking the
    /// same probe twice erases the other traces (the reference's "plotted by
    /// itself"), so the second click must know it repeats the first.
    last_probe_click: Option<ProbeTarget>,
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
            plot_currents: Vec::new(),
            ac_mag_scale: AcMagScale::default(),
            result_tab: ResultTab::Overview,
            tran_sub_tab: TranSubTab::default(),
            editor_mode: EditorMode::Text,
            schematic: SchematicState::default(),
            plot: PlotState::default(),
            deck_dir: None,

            diff_node_a: 1,
            diff_node_b: 2,

            expressions: ExpressionTraces::default(),
            expr_input: String::new(),
            expr_error: None,

            active_pane: Pane::Editor,
            tiling: Tiling::Single,
            show_about: false,
            schematic_grid: false,
            plot_grid: false,
            canvas_rect: Rect::NOTHING,
            note_dialog: None,
            note_edit: None,
            run_text: None,

            probe_flash: None,
            last_probe_click: None,
        }
    }
}

impl CircuitSimApp {
    pub fn new(cc: &eframe::CreationContext<'_>) -> Self {
        egui_extras::install_image_loaders(&cc.egui_ctx);
        theme::install_fonts(&cc.egui_ctx);

        Self::default()
    }
    /// Rebuild the netlist text from the schematic. Returns whether it worked:
    /// on failure the netlist is untouched and the status says why, so callers
    /// switch to the netlist only when this returns `true`.
    fn generate_netlist_from_schematic(&mut self) -> bool {
        match self.schematic.deck_text() {
            Ok(text) => {
                self.netlist = text;
                self.status = Some((true, "Generated netlist from schematic".into()));
                true
            }
            Err(e) => {
                self.status = Some((false, format!("Cannot generate netlist: {e}")));
                false
            }
        }
    }

    /// Parse the current netlist text and rebuild the schematic from it —
    /// the column/interval layout lives with the IR in [`crate::schematic`].
    /// `Ok` carries the import summary; on `Err` the schematic is untouched.
    fn import_schematic(&mut self) -> Result<String, String> {
        let options = self.parse_options();
        let (schematic, status) =
            SchematicState::from_netlist_with_options(&self.netlist, &self.schematic, &options)?;
        self.schematic = schematic;
        Ok(status)
    }

    /// [`Self::import_schematic`] reporting through the status line. Returns
    /// whether it worked, so callers switch to the schematic only then.
    fn generate_schematic_from_netlist(&mut self) -> bool {
        match self.import_schematic() {
            Ok(status) => {
                self.status = Some((true, status));
                true
            }
            Err(e) => {
                self.status = Some((false, e));
                false
            }
        }
    }

    /// How the open deck resolves `.include`/`.lib`: its own directory, then
    /// `RSPICE_LIB` and the bundled `libs/` — the same policy as the CLI.
    fn parse_options(&self) -> ParseOptions {
        ParseOptions::for_deck(self.deck_dir.clone(), &[])
    }

    fn run_simulation(&mut self) {
        match parse_with_options(&self.netlist, &self.parse_options()) {
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
                self.run_text = Some(self.netlist.clone());
                self.node_names = netlist.circuit.node_names.clone();
                self.circuit = Some(netlist.circuit.clone());

                match run(&netlist) {
                    Ok(result) => {
                        // The expression traces compute over the first run's
                        // transient plot as `.raw` columns; built before the
                        // result is taken apart.
                        let raw = circuitsim::rawwave::build("* gui", &netlist, &result);
                        self.expressions
                            .rebase(first_transient_plot(raw), netlist.param_env());
                        self.expr_error = None;
                        self.apply_result(result);
                        self.show_results_after_run();
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

        // Both Voltage and Current traces show the first few slots on a
        // fresh run; LTspice puts nothing up and lets the probe add them.
        // With nothing flagged the plot still draws every series (the
        // Voltage pass treats a missing/none flag as off only by position),
        // so keep the historical behavior here.
        self.plot_currents = match &self.tran {
            // One flag per plotted series (per element), the same
            // count `current_slot_targets` walks — sized by family it ran
            // short, and a probe on any element past the family count
            // flagged nothing.
            Some(tran) => vec![true; current_slot_targets(tran).len()],
            None => Vec::new(),
        };
        self.last_probe_click = None;

        self.result_tab = self.tab_for_new_results();
    }

    /// The tab a freshly applied result opens on. AC and DC-sweep runs jump
    /// to their own view; otherwise the current tab stays only if the new
    /// result has something to show there. Without that, an AC run followed
    /// by a transient one would keep an empty Frequency tab open.
    fn tab_for_new_results(&self) -> ResultTab {
        if self.ac.is_some() {
            return ResultTab::FreqResponse;
        }
        if self.dc_sweep.is_some() && self.tran.is_none() {
            return ResultTab::Dc;
        }
        let has_content = match self.result_tab {
            ResultTab::Overview => true,
            ResultTab::Dc => self.dc.is_some() || self.dc_sweep.is_some(),
            ResultTab::Waveforms => self.tran.is_some(),
            // `self.ac` is `None` here, so there is no frequency response.
            ResultTab::FreqResponse => false,
        };
        if has_content { self.result_tab } else { ResultTab::Overview }
    }

    fn clear_results(&mut self) {
        self.run_text = None;
        self.circuit_summary = None;
        self.dc = None;
        self.dc_sweep = None;
        self.tran = None;
        self.ac = None;
        self.node_names = vec!["0".to_string()];
        self.plot_currents = Vec::new();
        self.probe_flash = None;
        self.last_probe_click = None;
        self.expressions.rebase(None, Default::default());
        self.expr_error = None;
    }

    fn load_example(&mut self, ex: Example) {
        self.netlist = ex.netlist().to_string();
        self.deck_dir = None; // built-in text: no file behind it
        self.file_label = format!("{}.cir", ex.label().to_lowercase().replace(' ', "_"));
        self.status = Some((true, format!("Loaded example: {}", ex.label())));
        if !self.generate_schematic_from_netlist() {
            // The schematic could not follow the deck; show the deck itself
            // rather than a circuit that no longer belongs to it.
            self.show_netlist();
        }
        self.clear_results();
    }

    /// Make `text` the open document. The results belong to the old deck and
    /// are cleared; the schematic follows the new deck, and a deck it cannot
    /// draw still opens — as text, over an empty canvas, never over the
    /// previous document's circuit. The netlist is what the user sees next.
    #[cfg_attr(target_arch = "wasm32", allow(dead_code))]
    fn adopt_document(&mut self, text: String, label: String) {
        self.netlist = text;
        self.file_label = label;
        self.clear_results();
        self.status = Some(match self.import_schematic() {
            Ok(_) => (true, format!("Opened {}", self.file_label)),
            Err(e) => {
                self.schematic = SchematicState::default();
                (true, format!("Opened {} (schematic not drawn: {e})", self.file_label))
            }
        });
        self.show_netlist();
    }

    fn open_file(&mut self) {
        #[cfg(not(target_arch = "wasm32"))] {
        if let Some(path) = rfd::FileDialog::new()
            .add_filter("Netlist", &["cir", "sp", "cir", "txt"])
            .pick_file()
            {
                match std::fs::read_to_string(&path) {
                    Ok(text) => {
                        let label = path
                        .file_name()
                        .map(|s| s.to_string_lossy().into_owned())
                        .unwrap_or_else(|| "circuit.cir".into());
                        // Set before adopting: the schematic import parses the
                        // deck and must resolve its `.include`s from here.
                        self.deck_dir = path.parent().map(std::path::Path::to_path_buf);
                        self.adopt_document(text, label);
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
                        // Relative `.include`s now resolve from the saved location.
                        self.deck_dir = path.parent().map(std::path::Path::to_path_buf);
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
        self.handle_shortcuts(ctx);
        crate::plot::set_grid(ctx, self.plot_grid);

        // LTspice's frame: menu bar, icon toolbar, document tabs, the gray
        // MDI workspace, status bar.
        egui::TopBottomPanel::top("menubar")
        .frame(bar_frame(LT_WHITE, egui::Margin::symmetric(4.0, 0.0)))
        .show(ctx, |ui| {
            self.menu_bar(ui);
        });

        egui::TopBottomPanel::top("toolbar")
        .frame(bar_frame(LT_WHITE, egui::Margin::symmetric(6.0, 4.0)))
        .show(ctx, |ui| {
            self.toolbar(ui);
        });

        egui::TopBottomPanel::top("tabs")
        .frame(bar_frame(
            LT_STRIP,
            egui::Margin { left: 4.0, right: 4.0, top: 4.0, bottom: 0.0 },
        ))
        .show(ctx, |ui| {
            self.tab_strip(ui);
        });

        egui::TopBottomPanel::bottom("status")
        .frame(bar_frame(LT_STRIP, egui::Margin::symmetric(8.0, 3.0)))
        .show(ctx, |ui| {
            self.status_bar(ui);
        });

        egui::CentralPanel::default()
        .frame(
            egui::Frame::none()
            .fill(LT_WORKSPACE)
            .stroke(egui::Stroke::new(1.0, theme::LT_FRAME))
            .inner_margin(egui::Margin::same(3.0)),
        )
        .show(ctx, |ui| {
            self.workspace(ui);
        });

        self.about_window(ctx);
        self.note_dialog_window(ctx);
    }
}

impl CircuitSimApp {
    // ── Keyboard ────────────────────────────────────────────────────────────

    fn handle_shortcuts(&mut self, ctx: &egui::Context) {
        use egui::{Key, KeyboardShortcut, Modifiers};
        let (run, open, save, grid) = ctx.input_mut(|i| {
            (
                i.key_pressed(Key::F5),
                i.consume_shortcut(&KeyboardShortcut::new(Modifiers::COMMAND, Key::O)),
                i.consume_shortcut(&KeyboardShortcut::new(Modifiers::COMMAND, Key::S)),
                i.consume_shortcut(&KeyboardShortcut::new(Modifiers::COMMAND, Key::G)),
            )
        });
        // F4 / S open the annotation dialogs from the schematic window (S is a
        // plain letter, so it must never fire while a text field has focus or
        // a dialog is already collecting text).
        if self.schematic_active() && self.note_dialog.is_none() && !ctx.wants_keyboard_input() {
            let (label, directive) = ctx.input_mut(|i| {
                (
                    i.consume_key(Modifiers::NONE, Key::F4),
                    i.consume_key(Modifiers::NONE, Key::S),
                )
            });
            if label {
                self.open_note_dialog(false);
            }
            if directive {
                self.open_note_dialog(true);
            }
        }
        if run {
            self.run_simulation();
        }
        if open {
            self.open_file();
        }
        if save {
            self.save_file();
        }
        if grid {
            // Ctrl+G toggles the grid of whichever window is active.
            if self.active_pane == Pane::Results {
                self.plot_grid = !self.plot_grid;
            } else {
                self.schematic_grid = !self.schematic_grid;
            }
        }
    }

    // ── Cross-probing ───────────────────────────────────────────────────────

    /// The run's node id for the net under schematic wire `wire`, found by
    /// the net's deck spelling (its label, or its number). Refused unless the
    /// schematic still generates the very deck the results came from: after
    /// any edit — or a hand-edited netlist — the schematic's net names no
    /// longer say which trace is which, and a wrong trace is worse than none.
    fn probe_target(&self, wire: usize) -> Result<usize, String> {
        let Some(run_text) = self.run_text.as_deref() else {
            return Err("Run a simulation first, then click a wire to plot its voltage".into());
        };
        let w = self.schematic.wires.get(wire).ok_or("no such wire")?;
        let deck = self.schematic.deck_text()?;
        if deck != run_text {
            return Err("The schematic has changed since the last run — generate the netlist and run again to probe".into());
        }
        let a = NodeAssignment::of(
            &self.schematic.components,
            &self.schematic.wires,
            &self.schematic.labels,
        );
        let node = a.node_of(w.p1);
        if node == 0 {
            return Err("Ground is 0 V — nothing to plot".into());
        }
        let spelling = net_spelling(&a, node);
        self.node_names
            .iter()
            .position(|n| n.eq_ignore_ascii_case(&spelling))
            .ok_or_else(|| format!("net '{spelling}' is not in the simulated deck"))
    }

    /// Shift+click on a wire: add `V(net)` to the viewer and show it.
    fn probe_wire(&mut self, wire: usize) {
        match self.probe_target(wire) {
            Ok(id) => {
                for plotted in [&mut self.plot_nodes, &mut self.dc_plot_nodes, &mut self.ac_plot_nodes] {
                    if plotted.len() <= id {
                        plotted.resize(id + 1, false);
                    }
                    plotted[id] = true;
                }
                if self.ac.is_some() {
                    self.result_tab = ResultTab::FreqResponse;
                } else if self.tran.is_some() {
                    self.result_tab = ResultTab::Waveforms;
                    self.tran_sub_tab = TranSubTab::Voltage;
                } else if self.dc_sweep.is_some() {
                    self.result_tab = ResultTab::Dc;
                }
                self.status = Some((true, format!("Probing V({})", self.node_names[id])));
            }
            Err(e) => self.status = Some((false, e)),
        }
    }

    // ── Annotation dialogs ──────────────────────────────────────────────────

    /// Edit ▸ Label Net / SPICE Directive: open the text dialog.
    fn open_note_dialog(&mut self, directive: bool) {
        if !self.schematic_active() {
            return;
        }
        self.note_dialog = Some(NoteDialog { directive, text: String::new(), fresh: true });
    }

    /// The dialog itself. OK validates through the schematic (a bad net name
    /// or an `.end` keeps the dialog open with the reason in the status line)
    /// and arms the placement tool; the next canvas click drops the text.
    fn note_dialog_window(&mut self, ctx: &egui::Context) {
        let Some(mut dialog) = self.note_dialog.take() else {
            return;
        };
        let (mut commit, mut cancel) = (false, false);
        let title = if dialog.directive { "SPICE Directive" } else { "Label Net" };
        egui::Window::new(title)
            .collapsible(false)
            .resizable(false)
            .anchor(egui::Align2::CENTER_CENTER, [0.0, 0.0])
            .show(ctx, |ui| {
                if dialog.directive {
                    ui.label("SPICE text — written into the netlist as typed:");
                    let edit = ui.add(
                        egui::TextEdit::multiline(&mut dialog.text)
                            .font(egui::TextStyle::Monospace)
                            .desired_rows(4)
                            .desired_width(360.0),
                    );
                    if dialog.fresh {
                        edit.request_focus();
                    }
                } else {
                    ui.label("Net name:");
                    let edit = ui.add(egui::TextEdit::singleline(&mut dialog.text).desired_width(200.0));
                    if dialog.fresh {
                        edit.request_focus();
                    }
                    if edit.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter)) {
                        commit = true;
                    }
                }
                dialog.fresh = false;
                ui.horizontal(|ui| {
                    commit |= ui.button("OK").clicked();
                    cancel = ui.button("Cancel").clicked();
                });
            });
        if ctx.input(|i| i.key_pressed(egui::Key::Escape)) {
            cancel = true;
        }
        if cancel {
            return;
        }
        if commit {
            let armed = if dialog.directive {
                self.schematic.arm_directive(&dialog.text)
            } else {
                self.schematic.arm_label(&dialog.text)
            };
            match armed {
                Ok(()) => {
                    let what = if dialog.directive { "directive" } else { "label" };
                    self.status = Some((true, format!("Click the schematic to place the {what}")));
                    self.show_schematic();
                    return;
                }
                Err(e) => self.status = Some((false, e)),
            }
        }
        self.note_dialog = Some(dialog);
    }

    // ── Document state ──────────────────────────────────────────────────────

    fn has_results(&self) -> bool {
        self.dc.is_some() || self.dc_sweep.is_some() || self.tran.is_some() || self.ac.is_some()
    }

    /// Whether a Waveforms plot is painted on screen right now, derived from
    /// state alone. The schematic paints before or after the results pane
    /// depending on the tiling, so its cross-probe highlight can't ask the
    /// plot "did you just paint a hover?" — it asks this instead, and the
    /// Waveforms branches clear/replace [`Self::probe_flash`] every frame,
    /// making the highlight expire the instant the plot is not shown.
    fn waveforms_plot_visible(&self) -> bool {
        self.tran.is_some()
            && self.result_tab == ResultTab::Waveforms
            && (self.tiling != Tiling::Single || self.active_pane == Pane::Results)
    }

    /// The open document's name without its extension (`untitled`, `rc_charge`).
    fn doc_stem(&self) -> String {
        std::path::Path::new(&self.file_label)
            .file_stem()
            .map(|s| s.to_string_lossy().into_owned())
            .filter(|s| !s.is_empty())
            .unwrap_or_else(|| "untitled".to_string())
    }

    /// Title and icon of a child window, LTspice-style: the editor is the
    /// `.asc` schematic or the `.cir` netlist, the results window the `.raw`.
    fn pane_title(&self, pane: Pane) -> (String, Icon) {
        let stem = self.doc_stem();
        match pane {
            Pane::Editor => match self.editor_mode {
                EditorMode::Schematic => (format!("{stem}.asc"), Icon::Schematic),
                EditorMode::Text => (format!("{stem}.cir"), Icon::Netlist),
            },
            Pane::Results => (format!("{stem}.raw"), Icon::Waveform),
        }
    }

    fn show_schematic(&mut self) {
        self.editor_mode = EditorMode::Schematic;
        self.active_pane = Pane::Editor;
    }

    fn show_netlist(&mut self) {
        self.editor_mode = EditorMode::Text;
        self.active_pane = Pane::Editor;
    }

    /// Tools ▸ Import Netlist to Schematic. Switches to the schematic only if
    /// the import worked; after a failure the user stays with the netlist, so
    /// the error and its cause are side by side rather than the old circuit
    /// being shown as if it were the result.
    fn import_and_show_schematic(&mut self) {
        if self.generate_schematic_from_netlist() {
            self.show_schematic();
        } else {
            self.show_netlist();
        }
    }

    /// Tools ▸ Generate Netlist from Schematic; the mirror of
    /// [`Self::import_and_show_schematic`].
    fn generate_and_show_netlist(&mut self) {
        if self.generate_netlist_from_schematic() {
            self.show_netlist();
        } else {
            self.show_schematic();
        }
    }

    /// Pick a schematic tool, opening the schematic window if needed.
    fn pick_tool(&mut self, tool: Tool) {
        self.schematic.select_tool(tool);
        self.show_schematic();
    }

    /// The symbol library as a menu: one entry per placeable device family
    /// (grouped by a separator between families), arming the Part tool.
    fn part_picker(&mut self, ui: &mut egui::Ui) {
        let armed = match self.schematic.tool {
            Tool::Part(kind) => Some(kind),
            _ => None,
        };
        let mut prev: Option<char> = None;
        for &kind in symbols::PICKER {
            let spec = symbols::spec_of(kind);
            if prev.is_some_and(|p| p != spec.letter) && !matches!(spec.letter, 'M' | 'Q' | 'J') {
                ui.separator();
            }
            prev = Some(spec.letter);
            if ui.selectable_label(armed == Some(kind), spec.label).clicked() {
                self.pick_tool(Tool::Part(kind));
                ui.close_menu();
            }
        }
    }

    /// Rotate the selected part (or the next placement). Schematic edits act
    /// only while the schematic window is the active one: with Results
    /// focused, Space or the Edit menu must not change a circuit that is
    /// merely visible beside it.
    fn rotate_selection(&mut self) {
        if self.schematic_active() {
            self.schematic.rotate_or_advance();
        }
    }

    /// Delete the selected wire or part — only from the active schematic
    /// window (see [`Self::rotate_selection`]).
    fn delete_selection(&mut self) {
        if self.schematic_active() {
            self.schematic.delete_selection();
            // Renumbering invalidates "clicked the same probe twice": the
            // deck's slot identities moved.
            self.last_probe_click = None;
        }
    }

    /// After a successful run the waveform window opens beside the editor
    /// and takes focus, as LTspice's does.
    ///
    /// Only a run started from the maximised editor re-tiles: that is the
    /// moment the waveform window appears. A Results window the user has
    /// maximised themselves stays maximised on a re-run — the editor is one
    /// tab away, and un-maximising it would undo their layout choice.
    fn show_results_after_run(&mut self) {
        if self.has_results() {
            if self.tiling == Tiling::Single && self.active_pane == Pane::Editor {
                self.tiling = Tiling::Horizontal;
            }
            self.active_pane = Pane::Results;
        }
    }

    // ── Cross-probing ───────────────────────────────────────────────────────

    /// The probe click target at a screen position on the schematic canvas:
    /// two-terminal parts first (a Ground body stays a selection click), then
    /// wires. Answers `None` anywhere else, which the caller reads as plain
    /// canvas — the ordinary selection click.
    fn probe_target_at(
        &self,
        pos: Pos2,
        to_screen: impl Fn(GridPt) -> Pos2,
        z: f32,
    ) -> Option<ProbeTarget> {
        for (i, c) in self.schematic.components.iter().enumerate() {
            if canvas_family(c.kind).is_none() {
                continue;
            }
            let click_rect = part_hit_rect(c, &to_screen);
            if click_rect.contains(pos) {
                return Some(ProbeTarget::Part(i));
            }
        }
        for (wi, w) in self.schematic.wires.iter().enumerate() {
            let a = to_screen(w.p1);
            let b = to_screen(w.p2);
            let ab = b - a;
            let ap = pos - a;
            let ab_len_sq = ab.x * ab.x + ab.y * ab.y;
            let dist_sq = if ab_len_sq < 1.0 {
                ap.x * ap.x + ap.y * ap.y
            } else {
                let t = ((ap.x * ab.x + ap.y * ab.y) / ab_len_sq).clamp(0.0, 1.0);
                let closest = a + ab * t;
                let d = pos - closest;
                d.x * d.x + d.y * d.y
            };
            if dist_sq < (36.0 * z * z) {
                return Some(ProbeTarget::Wire(wi));
            }
        }
        None
    }

    /// Probe a part — the canvas component at `ci` — as the reference does:
    /// clicking the body adds its branch current. `isolate` is the
    /// double-click: the trace alone replaces whatever else was plotted.
    /// Returns whether a trace was added, so the caller falls back to the
    /// ordinary selection click when the family carries no report.
    fn probe_part_for(&mut self, ci: usize, isolate: bool) -> bool {
        // The (family, family-index) pair each plotted slot carries, taken
        // while `tran` still borrows.
        let slot_targets = match self.tran.as_ref() {
            Some(tran) => current_slot_targets(tran),
            None => return false,
        };
        let Some(c) = self.schematic.components.get(ci).cloned() else {
            return false;
        };
        let Some(family) = canvas_family(c.kind) else {
            return false;
        };
        // Family order on canvas == deck order == report order (`R1` is the
        // first reported resistor), because the netlist writer numbers the
        // same component walk these labels come from.
        let within = self.schematic.components[..ci]
            .iter()
            .filter(|o| canvas_family(o.kind) == Some(family))
            .count();
        let Some(slot) = slot_targets
            .iter()
            .position(|&(f, i)| f == family && i == within)
        else {
            return false;
        };
        if isolate {
            self.plot_currents = vec![false; self.plot_currents.len()];
        }
        if let Some(flag) = self.plot_currents.get_mut(slot) {
            *flag = true;
        }
        self.result_tab = ResultTab::Waveforms;
        self.tran_sub_tab = TranSubTab::Current;
        self.show_results_after_run();
        true
    }

    /// Probe a wire — the wire at `wi` — by adding the voltage of the node it
    /// carries. `isolate` takes the double-click role from
    /// [`Self::probe_part_for`] / [`Self::probe_wire_for`]. Returns whether a
    /// trace was added: node 0 is ground (always 0 V), and a node without a
    /// slot (import mismatch) probes nothing.
    fn probe_wire_for(&mut self, wi: usize, isolate: bool) -> bool {
        // The run's node id for the wire's net, by deck spelling and only
        // while the schematic still generates the deck the traces came from
        // ([`Self::probe_target`]); a refusal says why and the click falls
        // back to selection.
        let node = match self.probe_target(wi) {
            Ok(id) => id,
            Err(e) => {
                self.status = Some((false, e));
                return false;
            }
        };
        if node == 0 || node >= self.plot_nodes.len() {
            return false;
        }
        if isolate {
            self.plot_nodes = vec![false; self.plot_nodes.len()];
        }
        if let Some(flag) = self.plot_nodes.get_mut(node) {
            *flag = true;
        }
        self.result_tab = ResultTab::Waveforms;
        self.tran_sub_tab = TranSubTab::Voltage;
        self.show_results_after_run();
        true
    }

    // ── View ────────────────────────────────────────────────────────────────

    fn schematic_active(&self) -> bool {
        self.active_pane == Pane::Editor && self.editor_mode == EditorMode::Schematic
    }

    fn zoom_schematic(&mut self, factor: f32) {
        let anchor = if self.canvas_rect.is_positive() {
            self.canvas_rect.size() / 2.0
        } else {
            Vec2::ZERO
        };
        self.schematic.zoom_about(factor, anchor);
    }

    /// Zoom to fit: the schematic's content, or the plots' auto-fit.
    fn zoom_fit(&mut self) {
        if self.active_pane == Pane::Results {
            self.plot.tran_bounds.reset();
            self.plot.ac_mag_bounds.reset();
            self.plot.ac_phase_bounds.reset();
            self.plot.dc_bounds.reset();
            self.plot.tran_apply = true;
            self.plot.ac_apply = true;
            self.plot.dc_apply = true;
        } else if self.editor_mode == EditorMode::Schematic {
            let size = if self.canvas_rect.is_positive() {
                self.canvas_rect.size()
            } else {
                Vec2::new(800.0, 500.0)
            };
            self.schematic.fit_view(size);
        }
    }

    // ── Menu bar ────────────────────────────────────────────────────────────

    fn menu_bar(&mut self, ui: &mut egui::Ui) {
        egui::menu::bar(ui, |ui| {
            ui.spacing_mut().button_padding = egui::vec2(6.0, 2.0);
            ui.spacing_mut().item_spacing.x = 2.0;

            ui.menu_button("File", |ui| {
                if ui.add(egui::Button::new("Open…").shortcut_text("Ctrl+O")).clicked() {
                    self.open_file();
                    ui.close_menu();
                }
                if ui.add(egui::Button::new("Save As…").shortcut_text("Ctrl+S")).clicked() {
                    self.save_file();
                    ui.close_menu();
                }
                ui.separator();
                ui.menu_button("Examples", |ui| {
                    for ex in Example::ALL {
                        if ui.button(ex.label()).clicked() {
                            self.load_example(ex);
                            ui.close_menu();
                        }
                    }
                });
                #[cfg(not(target_arch = "wasm32"))]
                {
                    ui.separator();
                    if ui.button("Exit").clicked() {
                        ui.ctx().send_viewport_cmd(egui::ViewportCommand::Close);
                    }
                }
            });

            ui.menu_button("Edit", |ui| {
                let can_edit = self.schematic_active();
                let has_component = self.schematic.selected_component.is_some();
                let has_selection = has_component
                    || self.schematic.selected_wire.is_some()
                    || self.schematic.selected_note.is_some();
                if ui.add_enabled(can_edit && has_component, egui::Button::new("Rotate").shortcut_text("Space")).clicked() {
                    self.rotate_selection();
                    ui.close_menu();
                }
                if ui.add_enabled(can_edit && has_selection, egui::Button::new("Delete").shortcut_text("Del")).clicked() {
                    self.delete_selection();
                    ui.close_menu();
                }
                ui.separator();
                for (tool, _, label) in TOOLS {
                    if ui.button(label).clicked() {
                        self.pick_tool(tool);
                        ui.close_menu();
                    }
                }
                ui.menu_button("Component", |ui| self.part_picker(ui));
                ui.separator();
                if ui.add(egui::Button::new("Label Net…").shortcut_text("F4")).clicked() {
                    self.open_note_dialog(false);
                    ui.close_menu();
                }
                if ui.add(egui::Button::new("SPICE Directive…").shortcut_text("S")).clicked() {
                    self.open_note_dialog(true);
                    ui.close_menu();
                }
            });

            ui.menu_button("Hierarchy", |ui| {
                ui.add_enabled(false, egui::Button::new("Hierarchical schematics are not supported yet"));
            });

            ui.menu_button("View", |ui| {
                let schematic = self.schematic_active();
                if ui.add_enabled(schematic, egui::Button::new("Zoom In")).clicked() {
                    self.zoom_schematic(1.25);
                    ui.close_menu();
                }
                if ui.add_enabled(schematic, egui::Button::new("Zoom Out")).clicked() {
                    self.zoom_schematic(0.8);
                    ui.close_menu();
                }
                if ui.button("Zoom to Fit").clicked() {
                    self.zoom_fit();
                    ui.close_menu();
                }
                ui.separator();
                ui.checkbox(&mut self.schematic_grid, "Schematic Grid");
                ui.checkbox(&mut self.plot_grid, "Plot Grid");
            });

            ui.menu_button("Simulate", |ui| {
                if ui.add(egui::Button::new("Run").shortcut_text("F5")).clicked() {
                    self.run_simulation();
                    ui.close_menu();
                }
                ui.add_enabled(false, egui::Button::new("Stop"));
                ui.separator();
                if ui.button("Edit Simulation Command…").clicked() {
                    self.show_schematic();
                    ui.close_menu();
                }
            });

            ui.menu_button("Tools", |ui| {
                if ui.button("Import Netlist to Schematic").clicked() {
                    self.import_and_show_schematic();
                    ui.close_menu();
                }
                if ui.button("Generate Netlist from Schematic").clicked() {
                    self.generate_and_show_netlist();
                    ui.close_menu();
                }
            });

            ui.menu_button("Window", |ui| {
                if ui.button("Tile Horizontally").clicked() {
                    self.tiling = Tiling::Horizontal;
                    ui.close_menu();
                }
                if ui.button("Tile Vertically").clicked() {
                    self.tiling = Tiling::Vertical;
                    ui.close_menu();
                }
                ui.separator();
                for pane in [Pane::Editor, Pane::Results] {
                    if pane == Pane::Results && !self.has_results() {
                        continue;
                    }
                    let (title, _) = self.pane_title(pane);
                    if ui.selectable_label(self.active_pane == pane, title).clicked() {
                        self.active_pane = pane;
                        ui.close_menu();
                    }
                }
            });

            ui.menu_button("Help", |ui| {
                if ui.button("About RSpice").clicked() {
                    self.show_about = true;
                    ui.close_menu();
                }
            });
        });
    }

    fn about_window(&mut self, ctx: &egui::Context) {
        let mut open = self.show_about;
        egui::Window::new("About RSpice")
        .open(&mut open)
        .collapsible(false)
        .resizable(false)
        .show(ctx, |ui| {
            ui.label(egui::RichText::new(format!("RSpice {}", env!("CARGO_PKG_VERSION"))).size(14.0));
            ui.label("A SPICE-style circuit simulator.");
            ui.label(egui::RichText::new("Interface modeled on LTspice.").color(TEXT_SECONDARY));
        });
        self.show_about = open;
    }

    // ── Toolbar ─────────────────────────────────────────────────────────────

    fn toolbar(&mut self, ui: &mut egui::Ui) {
        ui.horizontal_wrapped(|ui| {
            ui.spacing_mut().item_spacing = egui::vec2(2.0, 2.0);
            let gap = 10.0;

            if icon_button(ui, Icon::Open, "Open netlist (Ctrl+O)", false, true).clicked() {
                self.open_file();
            }
            if icon_button(ui, Icon::Save, "Save netlist (Ctrl+S)", false, true).clicked() {
                self.save_file();
            }
            ui.add_space(gap);

            let analysis = match self.schematic.analysis_type {
                AnalysisType::Tran => ".tran",
                AnalysisType::Ac => ".ac",
            };
            if icon_button(ui, Icon::Analysis(analysis), "Edit simulation command", false, true).clicked() {
                self.show_schematic();
            }
            if icon_button(ui, Icon::Run, "Run (F5)", false, true).clicked() {
                self.run_simulation();
            }
            icon_button(ui, Icon::Stop, "Stop (a run always finishes)", false, false);
            ui.add_space(gap);

            if icon_button(ui, Icon::TileVertical, "Tile windows vertically", self.tiling == Tiling::Vertical, true).clicked() {
                self.tiling = Tiling::Vertical;
            }
            if icon_button(ui, Icon::TileHorizontal, "Tile windows horizontally", self.tiling == Tiling::Horizontal, true).clicked() {
                self.tiling = Tiling::Horizontal;
            }
            ui.add_space(gap);

            let schematic = self.schematic_active();
            if icon_button(ui, Icon::ZoomIn, "Zoom in", false, schematic).clicked() {
                self.zoom_schematic(1.25);
            }
            if icon_button(ui, Icon::ZoomOut, "Zoom out", false, schematic).clicked() {
                self.zoom_schematic(0.8);
            }
            let can_fit = schematic || self.active_pane == Pane::Results;
            if icon_button(ui, Icon::ZoomFit, "Zoom to fit", false, can_fit).clicked() {
                self.zoom_fit();
            }
            ui.add_space(gap);

            for (tool, icon, tip) in TOOLS {
                let selected = self.editor_mode == EditorMode::Schematic && self.schematic.tool == tool;
                if icon_button(ui, icon, tip, selected, true).clicked() {
                    self.pick_tool(tool);
                }
            }
            // Every other device family: diodes, transistors, dependent
            // sources, switches, transformer, line, op-amp.
            ui.menu_button("Parts ▾", |ui| self.part_picker(ui))
                .response
                .on_hover_text("Place a diode, transistor, source, switch, transformer, line or op-amp");
            ui.add_space(gap);

            let has_component = self.schematic.selected_component.is_some();
            let has_selection = has_component || self.schematic.selected_wire.is_some();
            if icon_button(ui, Icon::Rotate, "Rotate (Space)", false, schematic).clicked() {
                self.rotate_selection();
            }
            if icon_button(ui, Icon::Delete, "Delete selection (Del)", false, schematic && has_selection).clicked() {
                self.delete_selection();
            }
        });
    }

    // ── Document tabs ───────────────────────────────────────────────────────

    fn tab_strip(&mut self, ui: &mut egui::Ui) {
        let stem = self.doc_stem();
        ui.horizontal(|ui| {
            ui.spacing_mut().item_spacing.x = 2.0;
            let editor_active = self.active_pane == Pane::Editor;
            let schematic_tab = chrome::tab(
                ui,
                &format!("{stem}.asc"),
                Icon::Schematic,
                editor_active && self.editor_mode == EditorMode::Schematic,
            );
            if schematic_tab.clicked() {
                self.show_schematic();
            }
            let netlist_tab = chrome::tab(
                ui,
                &format!("{stem}.cir"),
                Icon::Netlist,
                editor_active && self.editor_mode == EditorMode::Text,
            );
            if netlist_tab.clicked() {
                self.show_netlist();
            }
            if self.has_results() {
                let raw_tab = chrome::tab(
                    ui,
                    &format!("{stem}.raw"),
                    Icon::Waveform,
                    self.active_pane == Pane::Results,
                );
                if raw_tab.clicked() {
                    self.active_pane = Pane::Results;
                }
            }
        });
    }

    // ── Workspace (MDI) ─────────────────────────────────────────────────────

    fn workspace(&mut self, ui: &mut egui::Ui) {
        let full = ui.available_rect_before_wrap();
        ui.allocate_rect(full, egui::Sense::hover());

        // The waveform window only exists once there is something to show.
        let results = self.has_results();
        if !results {
            self.active_pane = Pane::Editor;
        }

        let gap = 4.0;
        let layout: Vec<(Pane, Rect)> = if !results || self.tiling == Tiling::Single {
            vec![(self.active_pane, full)]
        } else if self.tiling == Tiling::Horizontal {
            let h = ((full.height() - gap) / 2.0).max(0.0);
            vec![
                (Pane::Results, Rect::from_min_size(full.min, Vec2::new(full.width(), h))),
                (
                    Pane::Editor,
                    Rect::from_min_size(egui::pos2(full.min.x, full.min.y + h + gap), Vec2::new(full.width(), h)),
                ),
            ]
        } else {
            let w = ((full.width() - gap) / 2.0).max(0.0);
            vec![
                (Pane::Editor, Rect::from_min_size(full.min, Vec2::new(w, full.height()))),
                (
                    Pane::Results,
                    Rect::from_min_size(egui::pos2(full.min.x + w + gap, full.min.y), Vec2::new(w, full.height())),
                ),
            ]
        };
        let single = layout.len() == 1;

        let mut events = Vec::new();
        for (pane, rect) in layout {
            let (title, icon) = self.pane_title(pane);
            let active = self.active_pane == pane;
            let body_fill = match (pane, &self.editor_mode) {
                (Pane::Editor, EditorMode::Text) => LT_WHITE,
                _ => LT_WORKSPACE,
            };
            let id = egui::Id::new(match pane {
                Pane::Editor => "pane_editor",
                Pane::Results => "pane_results",
            });
            let ev = chrome::child_window(
                ui,
                id,
                rect,
                &title,
                icon,
                active,
                single,
                pane == Pane::Results,
                body_fill,
                |ui| match pane {
                    Pane::Editor => self.editor_body(ui),
                    Pane::Results => self.results_body(ui),
                },
            );
            events.push((pane, ev));
        }

        for (pane, ev) in events {
            if ev.activated {
                self.active_pane = pane;
            }
            if ev.minimize {
                self.active_pane = if pane == Pane::Editor { Pane::Results } else { Pane::Editor };
                self.tiling = Tiling::Single;
            }
            if ev.maximize {
                if single {
                    self.tiling = Tiling::Horizontal;
                } else {
                    self.active_pane = pane;
                    self.tiling = Tiling::Single;
                }
            }
            if ev.close && pane == Pane::Results {
                self.clear_results();
                self.active_pane = Pane::Editor;
            }
        }
    }

    fn editor_body(&mut self, ui: &mut egui::Ui) {
        match self.editor_mode {
            EditorMode::Text => {
                let line_h = ui.text_style_height(&egui::TextStyle::Monospace);
                let rows = (ui.available_height() / line_h).floor().max(4.0) as usize;
                egui::ScrollArea::vertical()
                .auto_shrink([false, false])
                .show(ui, |ui| {
                    ui.add(
                        egui::TextEdit::multiline(&mut self.netlist)
                        .font(egui::TextStyle::Monospace)
                        .desired_width(f32::INFINITY)
                        .desired_rows(rows)
                        .lock_focus(true)
                        .code_editor()
                        .frame(false)
                        .margin(egui::Margin::same(6.0)),
                    );
                });
            }
            EditorMode::Schematic => self.schematic_panel(ui),
        }
    }

    fn results_body(&mut self, ui: &mut egui::Ui) {
        egui::Frame::none()
        .inner_margin(egui::Margin::same(6.0))
        .show(ui, |ui| {
            self.results_panel(ui);
        });
    }

    // ── Status bar ──────────────────────────────────────────────────────────

    fn status_bar(&mut self, ui: &mut egui::Ui) {
        ui.horizontal(|ui| {
            match &self.status {
                Some((true, msg)) => {
                    ui.label(msg);
                }
                Some((false, msg)) => {
                    ui.label(egui::RichText::new(msg).color(STATUS_ERROR));
                }
                None => {
                    ui.label(egui::RichText::new("Ready").color(TEXT_SECONDARY));
                }
            }
            if let Some(summary) = &self.circuit_summary {
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    ui.label(egui::RichText::new(summary).color(TEXT_SECONDARY));
                });
            }
        });
    }

    fn schematic_panel(&mut self, ui: &mut egui::Ui) {
        // Spacebar rotates the selection (or the next placement) — unless a
        // text field has focus, where Space is just a character.
        if !ui.ctx().wants_keyboard_input() && ui.input(|i| i.key_pressed(egui::Key::Space)) {
            self.rotate_selection();
        }

        // Simulation command strip (LTspice's "Edit Simulation Command"): the
        // analysis the Run button compiles. The drawing tools live in the
        // main toolbar.
        egui::Frame::none()
        .fill(theme::LT_STRIP)
        .stroke(egui::Stroke::new(1.0, theme::BORDER_SUBTLE))
        .inner_margin(egui::Margin::symmetric(8.0, 3.0))
        .show(ui, |ui| {
            ui.set_width(ui.available_width());
            ui.horizontal_wrapped(|ui| {
                if ui.button("Generate Netlist").clicked() && self.generate_netlist_from_schematic() {
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

        // Canvas Drawing
        let (response, painter) = ui.allocate_painter(ui.available_size(), egui::Sense::click_and_drag());
        let rect = response.rect;
        self.canvas_rect = rect;
        painter.rect_filled(rect, 0.0, theme::LT_WORKSPACE);

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
        let dot_color = Color32::from_rgb(96, 96, 96);
        let bounds_min = -pan / z;
        let bounds_max = (rect.size() - pan) / z;

        let start_x = (bounds_min.x / GRID_SIZE).floor() as i32;
        let end_x = (bounds_max.x / GRID_SIZE).ceil() as i32;
        let start_y = (bounds_min.y / GRID_SIZE).floor() as i32;
        let end_y = (bounds_max.y / GRID_SIZE).ceil() as i32;

        if self.schematic_grid {
            for xi in start_x..=end_x {
                for yi in start_y..=end_y {
                    let pos = rect.left_top() + pan + Vec2::new(xi as f32 * GRID_SIZE, yi as f32 * GRID_SIZE) * z;
                    painter.circle_filled(pos, 0.8 * z.max(0.5), dot_color);
                }
            }
        }

        let to_screen = |grid_pt: GridPt| rect.left_top() + pan + (grid_pt.to_pos().to_vec2() * z);
        // --- END PAN & ZOOM LOGIC ---

        // Draw existing wires — thin blue, with a dot where three or more
        // wire ends / pins meet (a junction), as in the reference.
        let wire_color = theme::LT_WIRE;
        let wire_selected_color = Color32::from_rgb(230, 80, 40);
        for (wi, w) in self.schematic.wires.iter().enumerate() {
            let selected = self.schematic.selected_wire == Some(wi);
            let color = if selected { wire_selected_color } else { wire_color };
            let width = if selected { 2.5 * z } else { 1.5 * z };
            painter.line_segment([to_screen(w.p1), to_screen(w.p2)], Stroke::new(width, color));
        }
        {
            let mut degree: HashMap<GridPt, usize> = HashMap::new();
            for w in &self.schematic.wires {
                *degree.entry(w.p1).or_default() += 1;
                *degree.entry(w.p2).or_default() += 1;
            }
            for c in &self.schematic.components {
                for pin in c.pins() {
                    *degree.entry(pin).or_default() += 1;
                }
            }
            for (pt, n) in degree {
                if n >= 3 {
                    let dot = Rect::from_center_size(to_screen(pt), Vec2::splat(5.0 * z.max(0.6)));
                    painter.rect_filled(dot, 0.0, wire_color);
                }
            }
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
                    painter.line_segment([to_screen(start_grid), to_screen(hover_grid)], Stroke::new(1.5 * z, theme::LT_WIRE));
                }
                painter.circle_filled(to_screen(hover_grid), 3.0 * z, theme::LT_WIRE);
            } else if let Tool::Part(kind) = self.schematic.tool {
                // Ghost of the library symbol itself, in the armed facing.
                let (dx, dy) = self.schematic.current_dir.offset(symbols::spec_of(kind).axis_len);
                let ghost = Component {
                    kind,
                    p1: hover_grid,
                    p2: GridPt(hover_grid.0 + dx, hover_grid.1 + dy),
                    val: String::new(),
                };
                symbols::paint(
                    &painter,
                    &ghost,
                    &to_screen,
                    Stroke::new(1.0 * z, theme::LT_WIRE.gamma_multiply(0.5)),
                    theme::LT_WIRE.gamma_multiply(0.5),
                );
                painter.circle_filled(to_screen(hover_grid), 3.0 * z, theme::LT_WIRE);
            } else if matches!(self.schematic.tool, Tool::R | Tool::C | Tool::L | Tool::V) {
                // Ghost component preview
                let (dx, dy) = self.schematic.current_dir.offset(3);
                let p2_grid = GridPt(hover_grid.0 + dx, hover_grid.1 + dy);

                painter.line_segment(
                    [to_screen(hover_grid), to_screen(p2_grid)],
                                     Stroke::new(1.0 * z, theme::LT_WIRE.gamma_multiply(0.5))
                );
                // Ghost body box at center of preview
                let ghost_center = to_screen(hover_grid) + (to_screen(p2_grid) - to_screen(hover_grid)) / 2.0;
                let is_h = hover_grid.1 == p2_grid.1;
                let ghost_rect = Rect::from_center_size(ghost_center,
                                                        if is_h { Vec2::new(28.0 * z, 16.0 * z) } else { Vec2::new(16.0 * z, 28.0 * z) });
                painter.rect_stroke(ghost_rect, 0.0, Stroke::new(1.0 * z, theme::LT_WIRE.gamma_multiply(0.5)));
                painter.circle_filled(to_screen(hover_grid), 3.0 * z, theme::LT_WIRE);
            }
        }

        // Draw Components with proper electronic symbols
        let comp_stroke = Stroke::new(1.5 * z, theme::LT_WIRE);
        let selected_stroke = Stroke::new(1.5 * z, Color32::from_rgb(230, 80, 40));
        let names = instance_names(&self.schematic.components);
        let label_font = theme::bold(10.0);

        for (i, c) in self.schematic.components.iter().enumerate() {
            let p1 = to_screen(c.p1);
            let p2 = to_screen(c.p2);
            let center = p1 + (p2 - p1) / 2.0;
            let is_horizontal = c.p1.1 == c.p2.1;
            let is_selected = self.schematic.selected_component == Some(i);

            // Horizontal parts label above/below; vertical parts put the name
            // on the left and the value on the right of the symbol, as the
            // reference does — long values never run across the plates.
            let (name_anchor, val_anchor) = if is_horizontal {
                (egui::Align2::CENTER_CENTER, egui::Align2::CENTER_CENTER)
            } else {
                (egui::Align2::RIGHT_CENTER, egui::Align2::LEFT_CENTER)
            };

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
                    painter.text(center + off, val_anchor, &c.val,
                                 label_font.clone(), theme::LT_SCHEM_TEXT);
                    painter.text(center - off * 0.8, name_anchor, &names[i],
                                 label_font.clone(), theme::LT_SCHEM_TEXT);
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
                                         Stroke::new(2.0, theme::LT_WIRE));
                    painter.line_segment([plate2_c - perp * plate_half, plate2_c + perp * plate_half],
                                         Stroke::new(2.0, theme::LT_WIRE));

                    if is_selected {
                        let sel_rect = Rect::from_center_size(center,
                                                              if is_horizontal { Vec2::new(24.0, plate_half * 2.0 + 4.0) }
                                                              else { Vec2::new(plate_half * 2.0 + 4.0, 24.0) });
                        painter.rect_stroke(sel_rect, 3.0, selected_stroke);
                    }

                    let off = if is_horizontal { Vec2::new(0.0, 16.0) } else { Vec2::new(20.0, 0.0) };
                    painter.text(center + off, val_anchor, &c.val,
                                 label_font.clone(), theme::LT_SCHEM_TEXT);
                    painter.text(center - off * 0.8, name_anchor, &names[i],
                                 label_font.clone(), theme::LT_SCHEM_TEXT);
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
                    painter.text(center + off, val_anchor, &c.val,
                                 label_font.clone(), theme::LT_SCHEM_TEXT);
                    painter.text(center - off * 0.8, name_anchor, &names[i],
                                 label_font.clone(), theme::LT_SCHEM_TEXT);
                }

                CompKind::Voltage => {
                    // Lead wires
                    painter.line_segment([p1, body_p1], comp_stroke);
                    painter.line_segment([body_p2, p2], comp_stroke);

                    // Circle body
                    let r = 11.0_f32;
                    painter.circle_stroke(center, r, Stroke::new(1.5, theme::LT_WIRE));

                    // + and - polarity symbols inside. Both stay upright
                    // whatever the rotation; `+` sits toward p1, the SPICE
                    // positive node.
                    let (plus_c, minus_c) = source_polarity_marks(center, p1, p2, r);
                    let ps = 3.0_f32;
                    let mark = Stroke::new(1.0, theme::LT_SCHEM_TEXT);
                    painter.line_segment([plus_c - Vec2::X * ps, plus_c + Vec2::X * ps], mark);
                    painter.line_segment([plus_c - Vec2::Y * ps, plus_c + Vec2::Y * ps], mark);
                    painter.line_segment([minus_c - Vec2::X * ps, minus_c + Vec2::X * ps], mark);

                    if is_selected {
                        painter.circle_stroke(center, r + 4.0, selected_stroke);
                    }

                    let off = if is_horizontal { Vec2::new(0.0, 16.0) } else { Vec2::new(20.0, 0.0) };
                    painter.text(center + off, val_anchor, &c.val,
                                 label_font.clone(), theme::LT_SCHEM_TEXT);
                    painter.text(center - off * 0.8, name_anchor, &names[i],
                                 label_font.clone(), theme::LT_SCHEM_TEXT);
                }

                _ if symbols::is_library_kind(c.kind) => {
                    let ink = if is_selected { selected_stroke.color } else { theme::LT_WIRE };
                    let stroke = if is_selected { selected_stroke } else { comp_stroke };
                    let bounds = symbols::paint(&painter, c, &to_screen, stroke, ink);
                    if is_selected {
                        painter.rect_stroke(bounds.expand(4.0), 3.0, selected_stroke);
                    }
                    // Name over value, parked right of the symbol so the
                    // leads and any wire on the left stay clear.
                    let at = Pos2::new(bounds.right() + 6.0, bounds.center().y);
                    painter.text(at - Vec2::new(0.0, 7.0), egui::Align2::LEFT_CENTER, &names[i],
                                 label_font.clone(), theme::LT_SCHEM_TEXT);
                    painter.text(at + Vec2::new(0.0, 7.0), egui::Align2::LEFT_CENTER, &c.val,
                                 label_font.clone(), theme::LT_SCHEM_TEXT);
                }

                CompKind::Ground => {
                    // Stem down from the pin into an open, downward triangle.
                    let pin = p1;
                    let stem = pin + Vec2::new(0.0, 12.0);
                    painter.line_segment([pin, stem], comp_stroke);
                    painter.add(egui::Shape::closed_line(
                        vec![
                            stem + Vec2::new(-9.0, 0.0),
                            stem + Vec2::new(9.0, 0.0),
                            stem + Vec2::new(0.0, 11.0),
                        ],
                        comp_stroke,
                    ));

                    if is_selected {
                        let sel_rect = Rect::from_center_size(pin + Vec2::new(0.0, 12.0), Vec2::new(26.0, 28.0));
                        painter.rect_stroke(sel_rect, 0.0, selected_stroke);
                    }
                }
                // Library kinds were drawn by the arm above.
                _ => {}
            }
        }

        // ── Compute and draw node number labels ─────────────────────────
        // The same NodeAssignment the netlist writer uses, so the on-canvas
        // labels can never drift away from the emitted netlist.
        {
            let assignment = NodeAssignment::of(&self.schematic.components, &self.schematic.wires, &self.schematic.labels);

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
                // A labeled net shows its label (drawn below), not a number.
                if assignment.name_of(*node_num).is_some() {
                    continue;
                }
                let label = format!("{node_num}");
                let bg_color = Color32::from_rgb(255, 255, 225);
                let text_color = if *node_num == 0 {
                    Color32::from_gray(70)
                } else {
                    Color32::from_rgb(0, 0, 128)
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
                painter.rect_filled(pill_rect, 0.0, bg_color);
                painter.rect_stroke(pill_rect, 0.0, egui::Stroke::new(0.5, theme::LT_FRAME));
                painter.text(
                    *pos + label_offset,
                    egui::Align2::LEFT_TOP,
                    label,
                    font_id,
                    text_color,
                );
            }

            // ── Cross-probe highlight: a hover in the Waveforms plot marks
            //    its owning object here — the wire(s) its node runs through,
            //    or the part body its branch current comes from. The
            //    Waveforms branch refreshes `probe_flash` on every painted
            //    frame; with no Waveforms plot showing, it expires at once.
            let flash = if self.waveforms_plot_visible() {
                self.probe_flash
            } else {
                None
            };
            match flash {
                Some(ProbeFlash::Node(node)) => {
                    let stroke = Stroke::new(3.0 * z, Color32::from_rgb(230, 80, 40));
                    // `node` is a run node id; wires are matched by the
                    // deck spelling of their net (a labeled net's id is not
                    // its schematic number).
                    let spelling = node_label(&self.node_names, node);
                    for w in &self.schematic.wires {
                        let wire_net = net_spelling(&assignment, assignment.node_of(w.p1));
                        if wire_net.eq_ignore_ascii_case(&spelling) {
                            painter.line_segment([to_screen(w.p1), to_screen(w.p2)], stroke);
                        }
                    }
                }
                Some(ProbeFlash::Element(family, within)) => {
                    // Only the R/C/L/V families exist on canvas; a Current
                    // tab series from another family keeps its trace but
                    // gets no schematic flash.
                    {
                        // The within-family order is the deck's card order,
                        // which is the schematic's component order.
                        let mut seen = 0usize;
                        for c in &self.schematic.components {
                            if canvas_family(c.kind) == Some(family) {
                                if seen == within {
                                    let rect = if c.pins().len() <= 2 {
                                        let p1 = to_screen(c.p1);
                                        let p2 = to_screen(c.p2);
                                        let center = p1 + (p2 - p1) / 2.0;
                                        let horizontal = c.p1.1 == c.p2.1;
                                        Rect::from_center_size(
                                            center,
                                            if horizontal {
                                                Vec2::new(40.0, 22.0)
                                            } else {
                                                Vec2::new(22.0, 40.0)
                                            } * z,
                                        )
                                    } else {
                                        part_hit_rect(c, &to_screen)
                                    };
                                    painter.rect_stroke(
                                        rect,
                                        2.0,
                                        Stroke::new(2.0_f32, Color32::from_rgb(230, 80, 40)),
                                    );
                                    break;
                                }
                                seen += 1;
                            }
                        }
                    }
                }
                None => {}
            }
        }
        // ── End node labels ──────────────────────────────────────────────

        // ── Annotations: net labels and SPICE directives ─────────────────
        // Each one records its screen rectangle for the Select tool's
        // hit-test below.
        let mut note_rects: Vec<(Note, Rect)> = Vec::new();
        {
            let selected_color = Color32::from_rgb(230, 80, 40);
            let label_font_id = egui::FontId::proportional((11.0 * z).clamp(8.0, 18.0));
            for (i, l) in self.schematic.labels.iter().enumerate() {
                let note = Note::Label(i);
                let at = to_screen(l.at);
                let color = if self.schematic.selected_note == Some(note) {
                    selected_color
                } else {
                    theme::LT_SCHEM_TEXT
                };
                let galley = painter.layout_no_wrap(l.name.clone(), label_font_id.clone(), color);
                let size = galley.size();
                // The name rides a short leader running right from the pin.
                let text_pos = at + Vec2::new(3.0 * z, -size.y - 1.0 * z);
                painter.line_segment(
                    [at, at + Vec2::new(size.x + 6.0 * z, 0.0)],
                    Stroke::new(1.0 * z, color),
                );
                painter.circle_filled(at, 2.5 * z.max(0.6), color);
                painter.galley(text_pos, galley, color);
                note_rects.push((
                    note,
                    Rect::from_min_size(text_pos, size).union(Rect::from_center_size(at, Vec2::splat(8.0))).expand(3.0),
                ));
            }
            let mono = egui::FontId::monospace((11.0 * z).clamp(7.0, 18.0));
            for (i, d) in self.schematic.directives.iter().enumerate() {
                let note = Note::Directive(i);
                let at = to_screen(d.at);
                let selected = self.schematic.selected_note == Some(note);
                let color = if selected { selected_color } else { theme::LT_SCHEM_TEXT };
                let galley = painter.layout(d.text.clone(), mono.clone(), color, f32::INFINITY);
                let text_rect = Rect::from_min_size(at, galley.size());
                painter.galley(at, galley, color);
                if selected {
                    painter.rect_stroke(text_rect.expand(2.0), 0.0, Stroke::new(1.0, selected_color));
                }
                note_rects.push((note, text_rect.expand(3.0)));
            }
            // Ghost of the armed text under the cursor, so the user sees what
            // the next click will drop.
            if matches!(self.schematic.tool, Tool::Label | Tool::Directive)
                && !self.schematic.pending_text.is_empty()
            {
                if let Some(hover_pos) = response.hover_pos() {
                    let snapped = to_screen(GridPt::from_pos(Pos2::new(
                        (hover_pos.x - rect.left_top().x - pan.x) / z,
                        (hover_pos.y - rect.left_top().y - pan.y) / z,
                    )));
                    let font = if self.schematic.tool == Tool::Directive { mono.clone() } else { label_font_id.clone() };
                    painter.text(
                        snapped,
                        egui::Align2::LEFT_BOTTOM,
                        &self.schematic.pending_text,
                        font,
                        theme::LT_SCHEM_TEXT.gamma_multiply(0.55),
                    );
                    painter.circle_filled(snapped, 3.0 * z, theme::LT_WIRE);
                }
            }
        }
        // ── End annotations ──────────────────────────────────────────────
        if self.schematic.tool == Tool::Select {
            // 1. Detect drag start and lock onto the target component
            if response.drag_started() {
                if let Some(interact_pos) = response.interact_pointer_pos() {
                    let mut hit = None;
                    for (i, c) in self.schematic.components.iter().enumerate() {
                        let click_rect = part_hit_rect(c, &to_screen);
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
                    Tool::R | Tool::C | Tool::L | Tool::V | Tool::Gnd | Tool::Part(_) => {
                        self.schematic.place_at(grid_pt);
                    }
                    Tool::Label | Tool::Directive => {
                        self.schematic.place_note(grid_pt);
                    }
                    Tool::Select => {
                        let mut selected = None;
                        for (i, c) in self.schematic.components.iter().enumerate() {
                            // Click area slightly larger than the visual box
                            let click_rect = part_hit_rect(c, &to_screen);
                            if click_rect.contains(interact_pos) {
                                selected = Some(i);
                                break;
                            }
                        }
                        // If no component hit, check wires; the hit-test
                        // result goes through the model so the selection
                        // fields can never disagree.
                        // Annotations sit above wires: their text is the
                        // click target, so a label on a wire stays pickable.
                        let hit_note = if selected.is_none() {
                            note_rects.iter().find(|(_, r)| r.contains(interact_pos)).map(|(n, _)| *n)
                        } else {
                            None
                        };
                        let mut hit_wire = None;
                        if selected.is_none() && hit_note.is_none() {
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
                        // With a transient run to apply it to, a plain click
                        // probes: a part adds its branch current, a wire adds
                        // the voltage its node carries, and the click skips
                        // selection entirely — the reference's probe
                        // semantics. Ground bodies and empty canvas still
                        // behave as ordinary selection clicks. Clicking the
                        // same probe again erases the rest ("plotted by
                        // itself"). A click on an annotation's text selects
                        // it instead, so a label sitting on a wire stays
                        // pickable.
                        let probed = if self.tran.is_some() && hit_note.is_none() {
                            match self.probe_target_at(interact_pos, to_screen, z) {
                                Some(target) => {
                                    let isolate = self.last_probe_click == Some(target);
                                    let probed = match target {
                                        ProbeTarget::Part(ci) => self.probe_part_for(ci, isolate),
                                        ProbeTarget::Wire(wi) => self.probe_wire_for(wi, isolate),
                                    };
                                    self.last_probe_click = probed.then_some(target);
                                    probed
                                }
                                None => {
                                    self.last_probe_click = None;
                                    false
                                }
                            }
                        } else {
                            self.last_probe_click = None;
                            false
                        };
                        if !probed {
                            match hit_note {
                                Some(note) => self.schematic.select_note(Some(note)),
                                None => self.schematic.select(selected, hit_wire),
                            }
                            // Shift+click probes a wire's voltage after any
                            // analysis (plain click covers transient runs
                            // above); same freshness rule, same spelling.
                            if let (Some(wi), true) = (hit_wire, ui.input(|i| i.modifiers.shift)) {
                                self.probe_wire(wi);
                            }
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
            self.schematic.cancel_gesture();
        }

        // ── Cross-probe preview: the LTspice dot reads the probe at the
        //    cursor — hovering a part shows `I(R1)`-style, a wire `V(node)`-.
        //    Only when a click would actually land a trace; otherwise the
        //    pointer behaves (and reads) as a selection cursor.
        if self.tran.is_some()
            && self.schematic.tool == Tool::Select
            && response.hovered()
            && !response.dragged()
        {
            if let Some(hover_pos) = response.hover_pos() {
                let preview = match self.probe_target_at(hover_pos, to_screen, z) {
                    Some(ProbeTarget::Wire(wi)) => self
                        .probe_target(wi)
                        .ok()
                        .filter(|&node| node > 0 && node < self.plot_nodes.len())
                        .map(|node| format!("V({})", node_label(&self.node_names, node))),
                    Some(ProbeTarget::Part(ci)) => self
                        .schematic
                        .components
                        .get(ci)
                        .and_then(|c| canvas_family(c.kind).map(|family| (family, ci)))
                        .map(|(family, ci)| {
                            let within = self.schematic.components[..ci]
                                .iter()
                                .filter(|o| o.kind == self.schematic.components[ci].kind)
                                .count();
                            (
                                family,
                                current_slot_targets(self.tran.as_ref().unwrap())
                                    .iter()
                                    .any(|&(f, i)| f == family && i == within),
                                within,
                            )
                        })
                        .and_then(|(family, has_slot, within)| {
                            if !has_slot {
                                return None;
                            }
                            let name = self
                                .circuit
                                .as_ref()
                                .and_then(|c| c.element_names(family).get(within).cloned())
                                .unwrap_or_else(|| format!("{}{}", family.letter(), within + 1));
                            Some(format!("I({name})"))
                        }),
                    None => None,
                };
                if let Some(text) = preview {
                    let galley = painter.layout_no_wrap(
                        text,
                        theme::bold(11.0),
                        Color32::from_rgb(200, 30, 30),
                    );
                    let pad = Vec2::new(5.0, 3.0);
                    let pill = Rect::from_min_size(
                        hover_pos + Vec2::new(14.0, 10.0) - pad,
                        galley.size() + pad * 2.0,
                    );
                    painter.rect_filled(pill, 3.0, Color32::WHITE);
                    painter.rect_stroke(
                        pill,
                        3.0,
                        Stroke::new(1.0_f32, Color32::from_rgb(200, 30, 30)),
                    );
                    painter.galley(pill.min + pad, galley, Color32::from_rgb(200, 30, 30));
                }
            }
        }

        // Delete key removes selected wire or component
        // (Skipped while a text field has focus: Delete there edits the text.)
        if !ui.ctx().wants_keyboard_input() && ui.input(|i| i.key_pressed(egui::Key::Delete)) {
            self.delete_selection();
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
                    .stroke(egui::Stroke::new(1.0, theme::LT_FRAME))
                    .rounding(egui::Rounding::same(0.0))
                    .inner_margin(egui::Margin::same(10.0))
                )
                .show(ui.ctx(), |ui| {
                    ui.horizontal(|ui| {
                        ui.label(egui::RichText::new("Wire").strong().size(12.0).color(theme::LT_WIRE));
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

        // Annotation popup: edit the selected label's name or directive's
        // text in place, or delete it.
        if let Some(note) = self.schematic.selected_note {
            let current = match note {
                Note::Label(i) => self.schematic.labels.get(i).map(|l| (l.at, l.name.clone())),
                Note::Directive(i) => self.schematic.directives.get(i).map(|d| (d.at, d.text.clone())),
            };
            if let Some((at, text)) = current {
                if self.note_edit.as_ref().map(|(n, _)| *n) != Some(note) {
                    self.note_edit = Some((note, text));
                }
                let multiline = matches!(note, Note::Directive(_));
                let heading = if multiline { "SPICE directive" } else { "Net label" };
                let (mut apply, mut delete, mut is_open) = (false, false, true);
                egui::Window::new("")
                .id(egui::Id::new("edit_note_window"))
                .title_bar(false)
                .fixed_pos(to_screen(at) + Vec2::new(10.0, 14.0))
                .collapsible(false)
                .resizable(false)
                .open(&mut is_open)
                .frame(
                    egui::Frame::none()
                    .fill(theme::BG_SURFACE)
                    .stroke(egui::Stroke::new(1.0, theme::LT_FRAME))
                    .rounding(egui::Rounding::same(0.0))
                    .inner_margin(egui::Margin::same(10.0))
                )
                .show(ui.ctx(), |ui| {
                    ui.label(egui::RichText::new(heading).strong().size(12.0).color(theme::LT_WIRE));
                    ui.add_space(4.0);
                    if let Some((_, buf)) = self.note_edit.as_mut() {
                        if multiline {
                            ui.add(egui::TextEdit::multiline(buf)
                                .font(egui::TextStyle::Monospace)
                                .desired_rows(3)
                                .desired_width(280.0));
                        } else {
                            ui.add(egui::TextEdit::singleline(buf).desired_width(160.0));
                        }
                    }
                    ui.add_space(4.0);
                    ui.horizontal(|ui| {
                        apply = ui.button("Apply").clicked();
                        delete = ui.button("Delete").clicked();
                    });
                });
                if !is_open {
                    self.schematic.select_note(None);
                }
                if apply {
                    if let Some((n, buf)) = self.note_edit.clone() {
                        self.status = Some(match self.schematic.edit_note(n, &buf) {
                            Ok(()) => (true, format!("Updated {}", heading.to_lowercase())),
                            Err(e) => (false, e),
                        });
                    }
                }
                if delete {
                    self.schematic.delete_note(note);
                }
            } else {
                self.schematic.select_note(None);
            }
        }
        if self.schematic.selected_note.is_none() {
            self.note_edit = None;
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
                    CompKind::Resistor  => ("Resistor",  Color32::from_rgb(0, 70, 170)),
                    CompKind::Capacitor => ("Capacitor", Color32::from_rgb(100, 40, 170)),
                    CompKind::Inductor  => ("Inductor",  Color32::from_rgb(170, 90, 0)),
                    CompKind::Voltage   => ("Voltage Source", Color32::from_rgb(150, 100, 0)),
                    CompKind::Ground    => ("Ground",    Color32::from_rgb(70, 70, 70)),
                    other => (symbols::spec_of(other).label, Color32::from_rgb(20, 110, 90)),
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
                    .stroke(egui::Stroke::new(1.0, theme::LT_FRAME))
                    .rounding(egui::Rounding::same(0.0))
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
                            let wide = symbols::is_library_kind(c.kind);
                            ui.add(egui::TextEdit::singleline(&mut c.val).desired_width(if wide { 200.0 } else { 110.0 }));
                        });
                        if symbols::is_library_kind(c.kind) {
                            // What the text means, and which pin is which —
                            // the symbol alone does not say (e.g. an
                            // op-amp's four pins, a switch's control pair).
                            let spec = symbols::spec_of(c.kind);
                            ui.add_space(2.0);
                            ui.label(egui::RichText::new(spec.val_hint).size(10.0).color(TEXT_SECONDARY));
                            ui.label(
                                egui::RichText::new(format!("Pins: {}", spec.pin_names.join(", ")))
                                    .size(10.0)
                                    .color(TEXT_SECONDARY),
                            );
                        }
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
            ui.selectable_value(&mut self.result_tab, ResultTab::Overview, "Overview");
            ui.selectable_value(&mut self.result_tab, ResultTab::Dc, "DC");
            ui.selectable_value(&mut self.result_tab, ResultTab::Waveforms, "Waveforms");
            ui.selectable_value(&mut self.result_tab, ResultTab::FreqResponse, "Frequency");
        });
        ui.add_space(6.0);

        // The card tabs scroll; the plot tabs size their plots to the window.
        match self.result_tab {
            ResultTab::Overview => {
                egui::ScrollArea::vertical().auto_shrink([false, false]).show(ui, |ui| {
                    self.overview_tab(ui);
                });
            }
            ResultTab::Dc => {
                egui::ScrollArea::vertical().auto_shrink([false, false]).show(ui, |ui| {
                    self.dc_tab(ui);
                });
            }
            ResultTab::Waveforms    => self.waveforms_tab(ui),
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
        .show_grid(crate::plot::grid_on(ui))
        .allow_drag(false)
        .allow_zoom(false)
        .allow_scroll(false)
        .show_axes([true, true])
        .show_background(true);

        let series = tran_voltage_series(tran, &self.plot_nodes, &self.node_names, 1.0, 1.0);
        // The black frame carries the axis labels too; without it they sit in
        // light gray (the viewer's text colour) on the white card.
        crate::plot::viewer_scope(ui, |ui| {
            code_frame().show(ui, |ui| {
                plot.show(ui, |plot_ui| {
                    draw(plot_ui, &series, 1.5);
                });
            });
        });
    }

    /// One row of the DC branch-current grid (shared by every element family).
    fn dc_branch_row(ui: &mut egui::Ui, label: String, cur: f64) {
        ui.label(label);
        ui.label(
            egui::RichText::new(format!("{cur:.6e} A"))
            .family(egui::FontFamily::Monospace)
            .color(ACCENT_DIM),
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
                        .color(ACCENT_DIM),
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
            ui.selectable_value(&mut self.tran_sub_tab, TranSubTab::Expression, "Expression");
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
                    let series = tran_voltage_series(
                        tran,
                        &self.plot_nodes,
                        &self.node_names,
                        x_mult,
                        y_mult,
                    );
                    let apply = self.plot.tran_apply.then(|| self.plot.tran_bounds);
                    self.plot.tran_apply = false;
                    // Emitted order: enabled nodes above ground —
                    // `tran_voltage_series` skips node 0 and any flag off —
                    // so the hover index maps back to its node.
                    let visible_nodes: Vec<usize> = self
                        .plot_nodes
                        .iter()
                        .enumerate()
                        .filter(|&(node, &on)| node > 0 && on)
                        .map(|(node, _)| node)
                        .collect();
                    let hover = plot_frame_hover(
                        "waveforms",
                        adjusted_height,
                        "time (s)",
                        "Voltage (V)",
                        &series,
                        apply,
                        ui,
                    );
                    self.probe_flash = hover
                        .and_then(|slot| visible_nodes.get(slot).copied())
                        .map(ProbeFlash::Node);
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
                    let slot_targets = current_slot_targets(tran);
                    let series = tran_current_series(
                        tran,
                        self.circuit.as_ref().unwrap_or(&CsCircuit::default()),
                        &self.plot_currents,
                        x_mult,
                        y_mult,
                    );
                    let apply = self.plot.tran_apply.then(|| self.plot.tran_bounds);
                    self.plot.tran_apply = false;
                    let hover = plot_frame_hover(
                        "waveforms_current",
                        adjusted_height,
                        "time (s)",
                        "Current (A)",
                        &series,
                        apply,
                        ui,
                    );
                    self.probe_flash = hover
                        .and_then(|k| slot_targets.get(k).copied())
                        .map(|(family, at)| ProbeFlash::Element(family, at));
                });
            }

            TranSubTab::Differential => {
                // The differential readout is plotted explicitly — no owning
                // wire, so nothing on the schematic can answer the hover.
                self.probe_flash = None;
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

            TranSubTab::Expression => {
                // Typed expressions have no single owning object; no hover
                // highlight either.
                self.probe_flash = None;
                // ── Waveform arithmetic: type an expression, plot the result ──
                let mut submit = false;
                ui.horizontal_wrapped(|ui| {
                    if ui.button("Auto Fit").clicked() {
                        self.plot.tran_bounds.reset();
                        self.plot.tran_apply = true;
                    }

                    ui.separator();

                    ui.label(egui::RichText::new("Expression:").color(TEXT_SECONDARY));
                    let edit = ui.add(
                        egui::TextEdit::singleline(&mut self.expr_input)
                            .hint_text("V(out)*I(V1)")
                            .desired_width(240.0),
                    );
                    let enter = edit.lost_focus()
                        && ui.input(|i| i.key_pressed(egui::Key::Enter));
                    if ui.button("Add").clicked() || enter {
                        submit = true;
                    }
                    if ui.button("Clear").clicked() {
                        self.expressions.clear();
                        self.expr_error = None;
                    }
                });

                if submit {
                    match self.expressions.add(&self.expr_input) {
                        Ok(()) => {
                            self.expr_input.clear();
                            self.expr_error = None;
                        }
                        Err(e) => self.expr_error = Some(e),
                    }
                    // A new trace refits the view, like the Auto Fit button.
                    self.plot.tran_bounds.reset();
                    self.plot.tran_apply = true;
                }

                if let Some(error) = &self.expr_error {
                    ui.label(egui::RichText::new(error).color(STATUS_ERROR));
                }

                let entries = self.expressions.entries();
                if entries.is_empty() {
                    ui.label(
                        egui::RichText::new(
                            "Plot arithmetic over the traces: V(out)*I(V1) (power), 2*V(out)+1, \
                             V(a,b), db(V(out)), d(V(out)).",
                        )
                        .color(TEXT_SECONDARY),
                    );
                } else {
                    let mut remove = None;
                    ui.horizontal_wrapped(|ui| {
                        for (i, (name, live)) in entries.iter().enumerate() {
                            // A dormant expression (the current run has no
                            // data for it) stays listed, dimmed, and draws
                            // again when a run can compute it.
                            let label = egui::RichText::new(format!("{name} ×"));
                            let label = if *live { label } else { label.color(TEXT_DISABLED) };
                            let chip = ui.add(egui::Button::new(label).small());
                            let chip = if *live {
                                chip
                            } else {
                                chip.on_hover_text("No data for this expression in the current run")
                            };
                            if chip.clicked() {
                                remove = Some(i);
                            }
                        }
                    });
                    if let Some(i) = remove {
                        self.expressions.remove(i);
                    }
                }

                ui.add_space(8.0);

                code_frame().show(ui, |ui| {
                    let adjusted_height = ui.available_height();
                    let series = self.expressions.series(x_mult, y_mult);
                    let apply = self.plot.tran_apply.then(|| self.plot.tran_bounds);
                    self.plot.tran_apply = false;
                    plot_frame(
                        "waveforms_expr", adjusted_height, "time (s)", "Expression",
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
        // Four plots stack in a scroll area; each takes half the window.
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

        egui::ScrollArea::vertical().auto_shrink([false, false]).show(ui, |ui| {
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
        });

        // Toggle off the bounds application after all plots have processed it
        self.plot.ac_apply = false;
    }

}

#[cfg(test)]
mod tests {

    #[test]
    fn source_plus_sits_toward_p1_in_every_orientation() {
        let c = Pos2::new(100.0, 100.0);
        for (p1, p2) in [
            (Pos2::new(100.0, 60.0), Pos2::new(100.0, 140.0)),  // p1 above
            (Pos2::new(100.0, 140.0), Pos2::new(100.0, 60.0)),  // p1 below
            (Pos2::new(60.0, 100.0), Pos2::new(140.0, 100.0)),  // p1 left
            (Pos2::new(140.0, 100.0), Pos2::new(60.0, 100.0)),  // p1 right
        ] {
            let (plus, minus) = source_polarity_marks(c, p1, p2, 11.0);
            assert!(plus.distance(p1) < minus.distance(p1), "+ must be nearer p1 ({p1:?})");
            assert!(minus.distance(p2) < plus.distance(p2), "- must be nearer p2 ({p2:?})");
        }
    }
    use super::*;
    use circuitsim::netlist::parse;

    /// Every shipped `examples/*.cir` must open and run in the GUI, the ones
    /// that `.include` a bundled library (`standard.lib`, `opamps.lib`)
    /// included: the deck's directory plus the bundled `libs/` tree resolve
    /// them, not the process directory.
    #[test]
    fn shipped_example_decks_open_and_run() {
        let dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../examples");
        let mut checked = 0;
        for entry in std::fs::read_dir(&dir).expect("examples directory") {
            let path = entry.expect("dir entry").path();
            if path.extension().and_then(|e| e.to_str()) != Some("cir") {
                continue;
            }
            let text = std::fs::read_to_string(&path).expect("example deck");
            let mut app = CircuitSimApp::default();
            // What `open_file` does, minus the file dialog.
            app.deck_dir = path.parent().map(std::path::Path::to_path_buf);
            app.adopt_document(text, path.file_name().unwrap().to_string_lossy().into_owned());
            match &app.status {
                Some((true, msg)) => {
                    assert!(!msg.contains("not found"), "{}: {msg}", path.display())
                }
                other => panic!("{}: unexpected open status {other:?}", path.display()),
            }
            app.run_simulation();
            assert!(
                matches!(&app.status, Some((true, m)) if m == "Simulation finished"),
                "{}: {:?}",
                path.display(),
                app.status
            );
            checked += 1;
        }
        assert!(checked >= 10, "only {checked} example decks found");
    }

    /// Pasted text has no directory behind it, yet the bundled libraries
    /// still resolve for it.
    #[test]
    fn pasted_deck_includes_a_bundled_library() {
        let mut app = CircuitSimApp::default();
        assert!(app.deck_dir.is_none());
        app.netlist = "* rectifier\n.include standard.lib\nV1 in 0 DC 1\n\
                       D1 in out D1N4148\nR1 out 0 1k\n.op\n.end\n"
            .into();
        app.run_simulation();
        assert!(
            matches!(&app.status, Some((true, m)) if m == "Simulation finished"),
            "{:?}",
            app.status
        );
    }

    /// V(2) of the RC-charging decks must be a real 0 V → 5 V charge. Started
    /// from the DC operating point instead, the capacitor is already charged,
    /// the trace sits at 5 V and autoscale stretches ~5 nV of gmin relaxation
    /// across the whole y axis.
    fn assert_rc_charges_from_zero(netlist: &str) {
        let parsed = parse(netlist).expect("deck parses");
        let tran = run(&parsed).expect("deck runs").tran.expect("transient result");
        let v2: Vec<f64> = tran.points.iter().map(|p| p.node_voltages[2]).collect();
        let first = *v2.first().unwrap();
        let last = *v2.last().unwrap();
        assert!(first.abs() < 1e-3, "V(2) should start at 0 V, got {first}");
        assert!(last > 4.5 && last <= 5.0, "V(2) should charge toward 5 V, got {last}");
    }

    #[test]
    fn default_netlist_charges_from_zero() {
        assert_rc_charges_from_zero(DEFAULT_NETLIST);
    }

    #[test]
    fn rc_charge_example_charges_from_zero() {
        assert_rc_charges_from_zero(Example::RcCharge.netlist());
    }

    /// The RL example must energise L1 from no current: V(2) = L·di/dt starts
    /// near the 12 V source and decays to ~0 as the current reaches 120 mA.
    /// Started from the DC operating point the inductor already shorts node 2
    /// to ground, so V(2) is a flat 0 V line.
    #[test]
    fn rl_example_energises_from_zero() {
        let parsed = parse(Example::RlCircuit.netlist()).expect("deck parses");
        let tran = run(&parsed).expect("deck runs").tran.expect("transient result");
        let first = tran.points.first().unwrap();
        let last = tran.points.last().unwrap();
        assert!(
            first.branch_currents.inductors[0].abs() < 1e-6,
            "L1 should start with no current, got {}",
            first.branch_currents.inductors[0]
        );
        let tau = 1e-3 / 100.0;
        let expected = 0.12 * (1.0 - (-last.time / tau).exp());
        assert!(
            (last.branch_currents.inductors[0] - expected).abs() < 1e-3 * 0.12,
            "I(L1) at {} s should be {expected}, got {}",
            last.time,
            last.branch_currents.inductors[0]
        );
        let peak = tran.points.iter().map(|p| p.node_voltages[2]).fold(0.0_f64, f64::max);
        assert!(peak > 5.0, "V(2) should spike toward the source voltage, peak {peak}");
    }

    // ── Editor / pane handling ──────────────────────────────────────────────

    use crate::schematic::{CompKind, Component, GridPt};

    const RC_DECK: &str = "V1 1 0 DC 5\nR1 1 2 1k\nC1 2 0 1u\n.tran 10u 5m UIC\n.end\n";
    /// A resistor card with a missing value: neither parseable nor drawable.
    const BAD_DECK: &str = "R1 1\n.end\n";

    fn resistor(val: &str) -> Component {
        Component { kind: CompKind::Resistor, p1: GridPt(2, 2), p2: GridPt(5, 2), val: val.into() }
    }

    fn status_is_error(app: &CircuitSimApp) -> bool {
        matches!(app.status, Some((false, _)))
    }

    /// With Results active, a schematic that is merely visible beside it must
    /// not be rotated or deleted by Space, Delete or the Edit menu.
    #[test]
    fn schematic_edits_ignore_a_hidden_schematic() {
        let mut app = CircuitSimApp::default();
        app.schematic.components = vec![resistor("1k")];
        app.schematic.selected_component = Some(0);
        app.editor_mode = EditorMode::Schematic;
        app.active_pane = Pane::Results;

        let before = app.schematic.components[0].p2;
        app.rotate_selection();
        app.delete_selection();
        assert!(app.schematic.components[0].p2 == before, "rotated while Results was active");
        assert_eq!(app.schematic.components.len(), 1, "deleted while Results was active");

        // The same calls act once the schematic window is the active one.
        app.active_pane = Pane::Editor;
        app.rotate_selection();
        assert!(app.schematic.components[0].p2 != before, "rotation ignored in the active window");
        app.delete_selection();
        assert!(app.schematic.components.is_empty(), "delete ignored in the active window");
    }

    #[test]
    fn schematic_edits_ignore_the_text_editor() {
        let mut app = CircuitSimApp::default();
        app.schematic.components = vec![resistor("1k")];
        app.schematic.selected_component = Some(0);
        app.show_netlist();
        app.delete_selection();
        assert_eq!(app.schematic.components.len(), 1);
    }

    #[test]
    fn failed_import_leaves_the_user_on_the_netlist() {
        assert!(parse(BAD_DECK).is_err(), "fixture must not parse");
        let mut app = CircuitSimApp::default();
        app.schematic.components = vec![resistor("1k")];
        app.netlist = BAD_DECK.into();
        app.show_schematic();
        app.import_and_show_schematic();
        assert!(app.editor_mode == EditorMode::Text, "switched to a stale schematic");
        assert_eq!(app.schematic.components.len(), 1, "schematic modified by a failed import");
        assert!(status_is_error(&app));
    }

    #[test]
    fn failed_generation_leaves_the_user_on_the_schematic() {
        let mut app = CircuitSimApp::default();
        app.schematic.components = vec![resistor("abc")];
        assert!(app.schematic.build_netlist().is_err(), "fixture must not build");
        app.netlist = "original".into();
        app.show_netlist();
        app.generate_and_show_netlist();
        assert!(app.editor_mode == EditorMode::Schematic, "switched to a stale netlist");
        assert_eq!(app.netlist, "original");
        assert!(status_is_error(&app));
    }

    #[test]
    fn successful_conversions_switch_to_the_result() {
        let mut app = CircuitSimApp::default();
        app.netlist = RC_DECK.into();
        app.show_netlist();
        app.import_and_show_schematic();
        assert!(app.editor_mode == EditorMode::Schematic);
        assert!(app.schematic.components.iter().any(|c| c.kind == CompKind::Capacitor));
        app.generate_and_show_netlist();
        assert!(app.editor_mode == EditorMode::Text);
        assert!(app.netlist.contains("C1"), "{}", app.netlist);
    }

    /// Opening a deck while the schematic is showing must not leave the old
    /// circuit on screen under the new document's `.asc` title.
    #[test]
    fn opening_a_deck_replaces_the_schematic_and_shows_the_netlist() {
        let mut app = CircuitSimApp::default();
        app.schematic.components = vec![resistor("1k")];
        app.show_schematic();
        app.adopt_document(RC_DECK.into(), "rc.cir".into());
        assert!(app.editor_mode == EditorMode::Text);
        assert_eq!(app.file_label, "rc.cir");
        assert_eq!(app.netlist, RC_DECK);
        assert!(
            app.schematic.components.iter().any(|c| c.kind == CompKind::Capacitor),
            "schematic still shows the previous document"
        );
        assert!(matches!(app.status, Some((true, _))));
    }

    #[test]
    fn opening_an_undrawable_deck_opens_it_over_an_empty_schematic() {
        let mut app = CircuitSimApp::default();
        app.schematic.components = vec![resistor("1k")];
        app.show_schematic();
        app.adopt_document(BAD_DECK.into(), "bad.cir".into());
        assert!(app.editor_mode == EditorMode::Text);
        assert_eq!(app.netlist, BAD_DECK);
        assert!(app.schematic.components.is_empty(), "previous document's circuit left behind");
        match &app.status {
            Some((true, msg)) => assert!(msg.contains("schematic not drawn"), "{msg}"),
            other => panic!("expected an Opened status, got {other:?}"),
        }
    }

    // ── Result tab across runs ──────────────────────────────────────────────

    #[test]
    fn result_tab_follows_the_new_run() {
        let mut app = CircuitSimApp::default();
        app.netlist = Example::RcLowPass.netlist().into();
        app.run_simulation();
        assert_eq!(app.result_tab, ResultTab::FreqResponse);

        // A transient run has no frequency response: the tab must not stay there.
        app.netlist = Example::RcCharge.netlist().into();
        app.run_simulation();
        assert_eq!(app.result_tab, ResultTab::Overview);

        // A tab the new result can fill is kept…
        app.result_tab = ResultTab::Waveforms;
        app.run_simulation();
        assert_eq!(app.result_tab, ResultTab::Waveforms);

        // …and one it cannot is not.
        app.netlist = Example::VoltageDivider.netlist().into();
        app.run_simulation();
        assert_eq!(app.result_tab, ResultTab::Overview);
    }

    /// A V source, a divider and a labeled middle net, drawn and generated.
    fn probe_app() -> CircuitSimApp {
        let mut app = CircuitSimApp::default();
        let comp = |kind, p1: (i32, i32), p2: (i32, i32), val: &str| Component {
            kind,
            p1: GridPt(p1.0, p1.1),
            p2: GridPt(p2.0, p2.1),
            val: val.to_string(),
        };
        app.schematic.components = vec![
            comp(CompKind::Voltage, (0, 0), (0, 3), "5"),
            comp(CompKind::Resistor, (4, 0), (4, 3), "1k"),
            comp(CompKind::Resistor, (8, 3), (8, 6), "1k"),
            comp(CompKind::Ground, (0, 3), (0, 3), ""),
            comp(CompKind::Ground, (8, 6), (8, 6), ""),
        ];
        app.schematic.wires = vec![
            crate::schematic::Wire { p1: GridPt(0, 0), p2: GridPt(4, 0) },
            crate::schematic::Wire { p1: GridPt(4, 3), p2: GridPt(8, 3) },
        ];
        app.schematic.labels = vec![crate::schematic::NetLabel { at: GridPt(4, 3), name: "mid".into() }];
        app.schematic.tran_step = "1m".into();
        app.schematic.tran_stop = "5m".into();
        assert!(app.generate_netlist_from_schematic());
        app
    }

    /// A device part is probed by its place in its *family*, not its kind: a
    /// P-channel MOSFET drawn after an N-channel one is the deck's second `M`.
    #[test]
    fn probing_a_pmos_counts_it_in_the_mosfet_family() {
        let mut app = CircuitSimApp::default();
        let mut place = |app: &mut CircuitSimApp, tool: Tool, at: (i32, i32), val: Option<&str>| {
            app.schematic.tool = tool;
            app.schematic.place_at(GridPt(at.0, at.1));
            let c = app.schematic.components.last_mut().unwrap();
            if let Some(v) = val {
                c.val = v.into();
            }
            c.pins()
        };
        let tie = |app: &mut CircuitSimApp, pin: GridPt, name: &str| {
            app.schematic.labels.push(crate::schematic::NetLabel { at: pin, name: name.into() });
        };
        let v = place(&mut app, Tool::V, (0, 0), Some("5"));
        let n = place(&mut app, Tool::Part(CompKind::Nmos), (10, 0), None);
        let p = place(&mut app, Tool::Part(CompKind::Pmos), (20, 0), None);
        tie(&mut app, v[0], "vdd");
        tie(&mut app, v[1], "0");
        // N: drain=vdd, source=0, gate=vdd.   P: source=vdd, drain=0, gate=0.
        for (pin, net) in n.iter().zip(["vdd", "0", "vdd"]) {
            tie(&mut app, *pin, net);
        }
        for (pin, net) in p.iter().zip(["vdd", "0", "0"]) {
            tie(&mut app, *pin, net);
        }
        app.schematic.tran_step = "1m".into();
        app.schematic.tran_stop = "5m".into();
        assert!(app.generate_netlist_from_schematic(), "{:?}", app.status);
        app.run_simulation();
        assert!(app.tran.is_some(), "{:?}", app.status);
        app.plot_currents.iter_mut().for_each(|b| *b = false);
        let pmos = app.schematic.components.len() - 1;
        assert!(app.probe_part_for(pmos, false), "{:?}", app.status);
        assert_eq!(app.plot_currents.iter().filter(|b| **b).count(), 1);
        let slot = current_slot_targets(app.tran.as_ref().unwrap())
            .iter()
            .position(|&(f, i)| f == ElementFamily::Mosfets && i == 1)
            .expect("second MOSFET slot");
        assert!(app.plot_currents[slot], "the P-channel part plots the deck's second M");
    }

    #[test]
    fn probing_before_a_run_explains_itself() {
        let app = probe_app();
        assert!(app.probe_target(0).unwrap_err().contains("Run a simulation"));
    }

    #[test]
    fn shift_click_probe_plots_the_labeled_net() {
        let mut app = probe_app();
        app.run_simulation();
        assert!(app.tran.is_some(), "{:?}", app.status);
        for v in [&mut app.plot_nodes, &mut app.dc_plot_nodes, &mut app.ac_plot_nodes] {
            v.iter_mut().for_each(|b| *b = false);
        }
        let id = app.probe_target(1).expect("wire 1 is the mid net");
        assert_eq!(app.node_names[id], "mid");
        app.probe_wire(1);
        assert!(app.plot_nodes[id]);
        assert_eq!(app.result_tab, ResultTab::Waveforms);
        // The unlabeled top net resolves by its number.
        let top = app.probe_target(0).expect("wire 0 is the V1 net");
        assert_ne!(top, id);
        assert!(!app.plot_nodes[top], "other nets stay unplotted");
    }

    #[test]
    fn probing_refuses_after_the_schematic_drifts_from_the_run() {
        let mut app = probe_app();
        app.run_simulation();
        app.schematic.components[1].val = "2k".into();
        let err = app.probe_target(1).unwrap_err();
        assert!(err.contains("changed since the last run"), "{err}");
        app.probe_wire(1);
        assert!(matches!(app.status, Some((false, _))));
    }

    #[test]
    fn probing_a_ground_wire_is_refused() {
        let mut app = probe_app();
        app.schematic.wires.push(crate::schematic::Wire { p1: GridPt(0, 3), p2: GridPt(2, 3) });
        assert!(app.generate_netlist_from_schematic());
        app.run_simulation();
        let err = app.probe_target(2).unwrap_err();
        assert!(err.contains("Ground"), "{err}");
    }

    /// The plain-click probe finds a labeled net by its deck name: the run's
    /// id for `mid` is not the schematic's node number for it.
    #[test]
    fn click_probe_plots_the_labeled_net_by_its_deck_name() {
        let mut app = probe_app();
        app.run_simulation();
        assert!(app.tran.is_some(), "{:?}", app.status);
        let id = app.probe_target(1).expect("wire 1 is the mid net");
        assert_eq!(app.node_names[id], "mid");
        app.plot_nodes.iter_mut().for_each(|b| *b = false);
        assert!(app.probe_wire_for(1, false));
        assert!(app.plot_nodes[id]);
        assert_eq!(app.plot_nodes.iter().filter(|b| **b).count(), 1);
        assert!(app.tran_sub_tab == TranSubTab::Voltage);
        // Probing the same net again isolates it.
        assert!(app.probe_wire_for(1, true));
        assert_eq!(app.plot_nodes.iter().filter(|b| **b).count(), 1);
    }

    #[test]
    fn click_probe_refuses_a_drifted_schematic_and_says_why() {
        let mut app = probe_app();
        app.run_simulation();
        app.schematic.components[1].val = "2k".into();
        assert!(!app.probe_wire_for(1, false));
        match &app.status {
            Some((false, msg)) => assert!(msg.contains("changed since the last run"), "{msg}"),
            other => panic!("expected a refusal, got {other:?}"),
        }
    }
}
