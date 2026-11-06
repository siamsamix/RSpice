//! The schematic's data layer: the element IR the editor manipulates, the
//! union-find node assignment shared by the netlist writer and the on-canvas
//! node labels, and the editor state machine — placement carpet, tool
//! gestures, model mutations, and both deck conversions. A schematic and a
//! netlist are one round trip apart, and both directions live in this module.

use circuitsim::circuit::{
    Ac as CsAc, BehavioralKind, Capacitor as CsCapacitor, Circuit as CsCircuit, ControlVar,
    Inductor as CsInductor, NodeId, Resistor as CsResistor, SwitchControl,
    VoltageSource as CsVoltageSource,
};
use circuitsim::model::{BjtType, JfetType, MosfetType};
use circuitsim::netlist::{
    deck_directives, emit as netlist_emit, emit_source_value, parse, parse_source_value,
    parse_with_options, AcCommand, AcScale, AnalysisCommands, Netlist, ParseOptions, SourceValue,
};
use circuitsim::units::parse_value;
use crate::autolayout::{ORIGIN_X, ORIGIN_Y};
use egui::{Pos2, Vec2};
use crate::symbols;
use std::collections::{HashMap, HashSet};

/// Canvas grid pitch in screen pixels.
pub const GRID_SIZE: f32 = 20.0;

/// Canvas zoom limits (shared by the wheel, the zoom buttons and zoom-to-fit).
pub const MIN_ZOOM: f32 = 0.2;
pub const MAX_ZOOM: f32 = 5.0;

/// Pixels kept clear on every side when zooming to fit.
const FIT_MARGIN: f32 = 60.0;
/// Zoom-to-fit never magnifies past this: a lone part (or a single point)
/// would otherwise "fit" at the zoom limit.
const FIT_MAX_ZOOM: f32 = 2.0;

#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub struct GridPt(pub i32, pub i32);

impl GridPt {
    pub fn from_pos(pos: Pos2) -> Self {
        Self((pos.x / GRID_SIZE).round() as i32, (pos.y / GRID_SIZE).round() as i32)
    }

    pub fn to_pos(self) -> Pos2 {
        Pos2::new(self.0 as f32 * GRID_SIZE, self.1 as f32 * GRID_SIZE)
    }
}

/// Every part the editor can place. R/C/L/V/Ground are the original
/// two-terminal set; the rest come from the symbol library
/// ([`crate::symbols`]), which owns their pins, drawings and card layout.
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub enum CompKind {
    Resistor,
    Capacitor,
    Inductor,
    Voltage,
    Ground,
    Diode,
    Current,
    Nmos,
    Pmos,
    Npn,
    Pnp,
    Njf,
    Pjf,
    /// `E` — voltage-controlled voltage source.
    Vcvs,
    /// `G` — voltage-controlled current source.
    Vccs,
    /// `H` — current-controlled voltage source.
    Ccvs,
    /// `F` — current-controlled current source.
    Cccs,
    /// `B` — behavioral source.
    Behavioral,
    /// `S` — voltage-controlled switch.
    Switch,
    /// `W` — current-controlled switch.
    CSwitch,
    /// Two coupled inductors (`L`, `L`, `K`).
    Transformer,
    /// `T` — lossless transmission line.
    Tline,
    /// `U` — op-amp.
    Opamp,
}

impl CompKind {
    /// Whether the element is written as a text card beside the typed
    /// R/C/L/V netlist (every library device is).
    pub fn is_device(self) -> bool {
        !matches!(
            self,
            CompKind::Resistor
                | CompKind::Capacitor
                | CompKind::Inductor
                | CompKind::Voltage
                | CompKind::Ground
        )
    }
}

#[derive(Clone)]
pub struct Component {
    pub kind: CompKind,
    pub p1: GridPt,
    pub p2: GridPt,
    pub val: String,
}

impl Component {
    /// Every pin, in the symbol's pin order (`p1`, `p2`, then the kind's
    /// extra pins). A ground has the one pin.
    pub fn pins(&self) -> Vec<GridPt> {
        symbols::pins(self.kind, self.p1, self.p2)
    }
}

/// A value in SPICE engineering notation (`1k`, `4.7u`, `100n`) when that
/// spelling parses back to exactly `v`; otherwise plain decimal, which
/// `parse_value` always re-parses.
pub fn fmt_si(v: f64) -> String {
    let plain = format!("{v}");
    if v == 0.0 || !v.is_finite() {
        return plain;
    }
    const SCALES: [(f64, &str); 10] = [
        (1e12, "T"), (1e9, "G"), (1e6, "Meg"), (1e3, "k"), (1.0, ""),
        (1e-3, "m"), (1e-6, "u"), (1e-9, "n"), (1e-12, "p"), (1e-15, "f"),
    ];
    let magnitude = v.abs();
    let (scale, suffix) = SCALES
        .iter()
        .copied()
        .find(|&(s, _)| magnitude >= s * (1.0 - 1e-9))
        .unwrap_or(SCALES[SCALES.len() - 1]);
    let mut mantissa = format!("{:.9}", v / scale);
    if mantissa.contains('.') {
        mantissa = mantissa.trim_end_matches('0').trim_end_matches('.').to_string();
    }
    let candidate = format!("{mantissa}{suffix}");
    match parse_value(&candidate) {
        Ok(back) if back == v => candidate,
        _ => plain,
    }
}

/// Instance names in component order — the exact `R1`, `C1`, `L1`, `V1`
/// numbering [`SchematicState::build_netlist`] assigns (counted per letter), so
/// the canvas can label parts with the names the netlist will carry. Ground
/// symbols are not elements and get an empty name.
pub fn instance_names(components: &[Component]) -> Vec<String> {
    // Counted per SPICE letter: an N- and a P-channel MOSFET share `M`,
    // exactly as their cards do.
    let mut counts: HashMap<char, usize> = HashMap::new();
    components
        .iter()
        .map(|comp| {
            if comp.kind == CompKind::Ground {
                return String::new();
            }
            let letter = symbols::spec_of(comp.kind).letter;
            let n = counts.entry(letter).or_insert(0);
            *n += 1;
            format!("{letter}{n}")
        })
        .collect()
}

#[derive(Clone)]
pub struct Wire {
    pub p1: GridPt,
    pub p2: GridPt,
}

/// A net label: the name of the net its anchor point sits on. Labels that
/// spell the same name (case-insensitively, as SPICE nodes are) are one net
/// even with no wire between them; `0` and `gnd` label ground.
#[derive(Clone, PartialEq, Debug)]
pub struct NetLabel {
    pub at: GridPt,
    pub name: String,
}

/// SPICE text placed on the canvas — a `.meas`, `.model`, `.step`, `.include`
/// or a whole `.subckt` block — written into the generated deck verbatim.
/// `text` may span several lines.
#[derive(Clone, PartialEq, Debug)]
pub struct Directive {
    pub at: GridPt,
    pub text: String,
}

/// One selected annotation (label or directive), by index.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Note {
    Label(usize),
    Directive(usize),
}

/// Whether `name` is a ground alias (`0`, `gnd`).
fn is_ground_name(name: &str) -> bool {
    name == "0" || name.eq_ignore_ascii_case("gnd")
}

/// Validate and normalize a net label: trimmed, one token, nothing the deck
/// grammar would read as structure, and no bare number other than `0` — a
/// numeric name would fight the numbered nodes of unlabeled nets.
pub fn check_net_name(raw: &str) -> Result<String, String> {
    let name = raw.trim();
    if name.is_empty() {
        return Err("net name is empty".into());
    }
    if let Some(bad) = name
        .chars()
        .find(|c| c.is_whitespace() || "{}()[],;=*\"'".contains(*c))
    {
        return Err(format!("net name '{name}' cannot contain '{bad}'"));
    }
    if name.starts_with('.') || name.starts_with('+') {
        return Err(format!("net name '{name}' cannot start with '{}'", &name[..1]));
    }
    if name.chars().all(|c| c.is_ascii_digit()) && name != "0" {
        return Err(format!(
            "net name '{name}' is a bare number; numbers are reserved for unlabeled nets (use 0 for ground)"
        ));
    }
    Ok(name.to_string())
}

/// The first word of every dot line in a directive's text, lowercased.
fn directive_words(text: &str) -> impl Iterator<Item = String> + '_ {
    text.lines().filter_map(|line| {
        let word = line.split_whitespace().next()?;
        word.starts_with('.').then(|| word.to_ascii_lowercase())
    })
}

/// Validate a directive's text: non-blank, and no `.end` (the writer owns the
/// deck's terminator; a stray one would cut the deck short).
pub fn check_directive(raw: &str) -> Result<String, String> {
    let text = raw.trim();
    if text.is_empty() {
        return Err("directive text is empty".into());
    }
    if directive_words(text).any(|w| w == ".end") {
        return Err("`.end` is added to the deck automatically — remove it from the directive".into());
    }
    Ok(text.to_string())
}

/// SPICE node assignment over the schematic: every grid point maps to a node
/// number (ground roots = 0, all other components' nodes numbered 1..n in
/// sorted-grid order, deterministic frame to frame).
///
/// The union-find itself stays private — the interface is the resulting map.
pub struct NodeAssignment {
    /// The unique grid points, in the canonical sorted order used for
    /// numbering. (The painter iterates this to place one label per node.)
    pub pt_list: Vec<GridPt>,
    pt_to_node: HashMap<GridPt, usize>,
    /// Every distinct label name on a node, in label order (first spelling
    /// per case-insensitive name). Ground's `0`/`gnd` aliases are not names.
    names: HashMap<usize, Vec<String>>,
}

impl NodeAssignment {
    /// Assign SPICE node numbers to the schematic's points. Labels join the
    /// point set, merge every net that shares a name, and ground the nets
    /// labeled `0`/`gnd`; numbering of the remaining nets stays first-seen in
    /// sorted point order, so a schematic without labels numbers exactly as
    /// it always did.
    pub fn of(components: &[Component], wires: &[Wire], labels: &[NetLabel]) -> Self {
        // 1. Collect all unique grid points touched by components or wires
        //    (a ground's p2 equals its p1 everywhere the editor creates them;
        //    including it here matches the historical writer exactly).
        let mut points = HashSet::new();
        for c in components {
            points.insert(c.p1);
            points.insert(c.p2);
            points.extend(c.pins());
        }
        for w in wires {
            points.insert(w.p1);
            points.insert(w.p2);
        }
        for l in labels {
            points.insert(l.at);
        }

        let mut pt_list: Vec<GridPt> = points.into_iter().collect();
        // Sort deterministically so DSU root assignment never changes.
        pt_list.sort_by_key(|p| (p.0, p.1));
        let pt_to_idx: HashMap<GridPt, usize> =
            pt_list.iter().enumerate().map(|(i, &p)| (p, i)).collect();

        // 2. Wires connect nodes.
        let mut dsu = Dsu::new(pt_list.len());
        for w in wires {
            let i1 = pt_to_idx[&w.p1];
            let i2 = pt_to_idx[&w.p2];
            dsu.union(i1, i2);
        }

        // 2b. One name, one net: labels spelling the same name merge.
        let mut first_with_name: HashMap<String, usize> = HashMap::new();
        for l in labels {
            let idx = pt_to_idx[&l.at];
            match first_with_name.entry(l.name.to_ascii_lowercase()) {
                std::collections::hash_map::Entry::Occupied(e) => dsu.union(*e.get(), idx),
                std::collections::hash_map::Entry::Vacant(e) => {
                    e.insert(idx);
                }
            }
        }

        // 3. Ground roots are forced to node 0 — by a ground symbol or a
        //    `0`/`gnd` label.
        let mut ground_roots = HashSet::new();
        for c in components {
            if c.kind == CompKind::Ground {
                if let Some(&idx) = pt_to_idx.get(&c.p1) {
                    ground_roots.insert(dsu.find(idx));
                }
            }
        }
        for l in labels {
            if is_ground_name(&l.name) {
                ground_roots.insert(dsu.find(pt_to_idx[&l.at]));
            }
        }

        // 4. Number the remaining roots 1..n in first-seen (sorted) order.
        let mut root_to_node: HashMap<usize, usize> = HashMap::new();
        let mut next_id = 1usize;
        for i in 0..pt_list.len() {
            let root = dsu.find(i);
            root_to_node.entry(root).or_insert_with(|| {
                if ground_roots.contains(&root) {
                    0
                } else {
                    let id = next_id;
                    next_id += 1;
                    id
                }
            });
        }

        let pt_to_node: HashMap<GridPt, usize> = pt_list
            .iter()
            .enumerate()
            .map(|(i, &pt)| {
                let root = root_to_node[&dsu.find(i)];
                (pt, root)
            })
            .collect();

        // 5. Collect each node's distinct label names.
        let mut names: HashMap<usize, Vec<String>> = HashMap::new();
        for l in labels {
            if is_ground_name(&l.name) {
                continue;
            }
            let entry = names.entry(pt_to_node[&l.at]).or_default();
            if !entry.iter().any(|n| n.eq_ignore_ascii_case(&l.name)) {
                entry.push(l.name.clone());
            }
        }
        Self { pt_list, pt_to_node, names }
    }

    /// SPICE node id at a grid point.
    pub fn node_of(&self, pt: GridPt) -> usize {
        self.pt_to_node[&pt]
    }

    /// How the deck spells a node in a text card: its label, else its number
    /// (`0` for ground) — the same spelling the typed netlist writer prints.
    pub fn spelling(&self, node: usize) -> String {
        match self.name_of(node) {
            Some(name) if node != 0 => name.to_string(),
            _ => node.to_string(),
        }
    }

    /// The label name of a node (its first one, if two disagree — see
    /// [`Self::conflicts`]); `None` for unlabeled nets and for ground.
    pub fn name_of(&self, node: usize) -> Option<&str> {
        self.names.get(&node).and_then(|v| v.first()).map(String::as_str)
    }

    /// Nets that cannot be named consistently, one sentence each: a net
    /// carrying two different labels, or ground carrying any name besides
    /// its aliases. The deck would have to pick one silently, so the netlist
    /// writer refuses instead.
    pub fn conflicts(&self) -> Vec<String> {
        let mut nodes: Vec<usize> = self.names.keys().copied().collect();
        nodes.sort_unstable();
        let mut out = Vec::new();
        for node in nodes {
            let names = &self.names[&node];
            if node == 0 {
                out.push(format!(
                    "ground is also labeled '{}' — a net cannot be both ground and named",
                    names[0]
                ));
            } else if names.len() > 1 {
                out.push(format!(
                    "one net carries two labels, '{}' and '{}'",
                    names[0], names[1]
                ));
            }
        }
        out
    }
}

/// Union-find over point indices (path-shortening find, union by root).
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

/// A transformer's `Lp=… Ls=… k=…` value text as the three card tokens,
/// defaults filling what the text omits; each number is checked now, so a bad
/// literal names its part instead of surfacing as a parse error far away.
fn transformer_params(name: &str, val: &str) -> Result<(String, String, String), String> {
    let mut lp = "10m".to_string();
    let mut ls = "10m".to_string();
    let mut k = "0.99".to_string();
    for word in val.split_whitespace() {
        let Some((key, value)) = word.split_once('=') else {
            return Err(format!("{name} value '{val}': expected Lp=… Ls=… k=…, got '{word}'"));
        };
        match key.to_ascii_lowercase().as_str() {
            "lp" => lp = value.to_string(),
            "ls" => ls = value.to_string(),
            "k" => k = value.to_string(),
            other => return Err(format!("{name} value '{val}': unknown parameter '{other}' (use Lp, Ls, k)")),
        }
    }
    for (what, text) in [("Lp", &lp), ("Ls", &ls), ("k", &k)] {
        parse_value(text).map_err(|e| format!("{name} {what} '{text}': {e}"))?;
    }
    Ok((lp, ls, k))
}

// ─── Editor state machine ───────────────────────────────────────────────────

/// Placement carpet: the facing a fresh component drops in, stepping
/// clockwise on every spacebar press.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Direction { Right, Down, Left, Up }

impl Direction {
    pub fn next(self) -> Self {
        match self {
            Direction::Right => Direction::Down,
            Direction::Down => Direction::Left,
            Direction::Left => Direction::Up,
            Direction::Up => Direction::Right,
        }
    }

    /// Grid delta from a start point to a pin `length` cells away.
    pub fn offset(self, length: i32) -> (i32, i32) {
        match self {
            Direction::Right => (length, 0),
            Direction::Down => (0, length),
            Direction::Left => (-length, 0),
            Direction::Up => (0, -length),
        }
    }
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Tool { Select, Wire, R, C, L, V, Gnd, Label, Directive, Part(CompKind) }

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum AnalysisType { Tran, Ac }

impl Default for AnalysisType {
    fn default() -> Self { AnalysisType::Tran }
}

/// The schematic editor's state: the element IR, the in-progress tool
/// gestures, and the analysis text fields the Run button compiles. The
/// model mutations (place / move / rotate / wire) are methods here, so the
/// canvas code only computes pointer geometry and asks the module to mutate.
pub struct SchematicState {
    pub tool: Tool,
    pub components: Vec<Component>,
    pub wires: Vec<Wire>,
    pub labels: Vec<NetLabel>,
    pub directives: Vec<Directive>,
    /// The annotation under the Select tool's cursor, by index.
    pub selected_note: Option<Note>,
    /// Text armed for the Label / Directive tool: the next canvas click
    /// drops it (see [`SchematicState::place_note`]).
    pub pending_text: String,
    pub active_wire_start: Option<GridPt>,
    pub current_dir: Direction,
    pub selected_component: Option<usize>,
    pub selected_wire: Option<usize>,
    pub drag_last_grid: Option<GridPt>,
    pub pan: Vec2,
    pub zoom: f32,
    pub analysis_type: AnalysisType,
    pub tran_step: String,
    pub tran_stop: String,
    pub ac_variation: String,
    pub ac_points: String,
    pub ac_fstart: String,
    pub ac_fstop: String,
}

impl Default for SchematicState {
    fn default() -> Self {
        Self {
            tool: Tool::Select,
            components: vec![],
            wires: vec![],
            labels: vec![],
            directives: vec![],
            selected_note: None,
            pending_text: String::new(),
            active_wire_start: None,
            current_dir: Direction::Down,
            selected_component: None,
            selected_wire: None,
            drag_last_grid: None,
            pan: Vec2::ZERO,
            zoom: 1.0,
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

impl SchematicState {
    pub fn select_tool(&mut self, tool: Tool) {
        self.tool = tool;
    }

    /// Space with nothing selected: rotate the next placement's facing.
    pub fn advance_direction(&mut self) {
        self.current_dir = self.current_dir.next();
    }

    /// Rotate the selected component 90 degrees around `p1`, re-attaching
    /// the wires that touch its rotated pin. True if something rotated.
    pub fn rotate_selected(&mut self) -> bool {
        let Some((old_pins, new_pins)) = self
            .selected_component
            .and_then(|idx| self.components.get_mut(idx))
            .map(|c| {
                let old_pins = c.pins();
                let dx = c.p2.0 - c.p1.0;
                let dy = c.p2.1 - c.p1.1;
                // 90-degree rotation formula: (x, y) -> (-y, x)
                c.p2 = GridPt(c.p1.0 - dy, c.p1.1 + dx);
                (old_pins, c.pins())
            })
        else {
            return false;
        };
        // Keep wires and labels attached to every pin that moved (all but
        // p1, the centre of the turn). One snapshot, one pass: no endpoint
        // is moved twice.
        let moved: Vec<(GridPt, GridPt)> = old_pins
            .iter()
            .zip(&new_pins)
            .skip(1)
            .filter(|(o, n)| o != n)
            .map(|(&o, &n)| (o, n))
            .collect();
        let remap = |pt: GridPt| moved.iter().find(|(o, _)| *o == pt).map_or(pt, |&(_, n)| n);
        for w in self.wires.iter_mut() {
            w.p1 = remap(w.p1);
            w.p2 = remap(w.p2);
        }
        for l in self.labels.iter_mut() {
            l.at = remap(l.at);
        }
        true
    }

    /// Drag a selected component by a grid delta; wires attached at either
    /// of its pins stretch by the same delta. True if the component moved.
    pub fn drag_component(&mut self, idx: usize, dx: i32, dy: i32) -> bool {
        let Some(pins) = self.components.get_mut(idx).map(|c| {
            let pins = c.pins();
            // Move component endpoints
            c.p1.0 += dx;
            c.p1.1 += dy;
            c.p2.0 += dx;
            c.p2.1 += dy;
            pins
        }) else {
            return false;
        };
        // Stretch wires attached to the component's pins
        for w in self.wires.iter_mut() {
            if pins.contains(&w.p1) {
                w.p1.0 += dx;
                w.p1.1 += dy;
            }
            if pins.contains(&w.p2) {
                w.p2.0 += dx;
                w.p2.1 += dy;
            }
        }
        // A label sitting on a pin names that pin's net: it travels with it.
        for l in self.labels.iter_mut() {
            if pins.contains(&l.at) {
                l.at.0 += dx;
                l.at.1 += dy;
            }
        }
        true
    }

    /// One click of the wire tool: opens a wire or closes the in-flight
    /// one; clicking the origin point again cancels.
    pub fn wire_click(&mut self, at: GridPt) {
        match self.active_wire_start.take() {
            Some(start) if start != at => self.wires.push(Wire { p1: start, p2: at }),
            Some(_) => {} // same point: cancel
            None => self.active_wire_start = Some(at),
        }
    }

    /// Drop a fresh component of the active tool at a grid point — the
    /// tool's default value, oriented by the placement direction (grounds
    /// are pins, not directed elements, so both points coincide).
    pub fn place_at(&mut self, at: GridPt) {
        match self.tool {
            Tool::Gnd => {
                self.components.push(Component {
                    kind: CompKind::Ground, p1: at, p2: at, val: "".into(),
                });
                return;
            }
            Tool::R | Tool::C | Tool::L | Tool::V | Tool::Part(_) => {}
            _ => return,
        }
        let kind = match self.tool {
            Tool::R => CompKind::Resistor, Tool::C => CompKind::Capacitor,
            Tool::L => CompKind::Inductor, Tool::Part(kind) => kind,
            _ => CompKind::Voltage,
        };
        // Every kind's value and axis length come from its spec (the
        // original R/C/L/V rows read 1k / 1u / 1m / DC 5 over 3 cells).
        let spec = symbols::spec_of(kind);
        let val = spec.default_val.to_string();

        // Calculate P2 based on current rotation direction
        let (dx, dy) = self.current_dir.offset(spec.axis_len);
        self.components.push(Component {
            kind,
            p1: at,
            p2: GridPt(at.0 + dx, at.1 + dy),
            val,
        });
    }

    /// Apply a selection computed by the canvas hit-test.
    pub fn select(&mut self, component: Option<usize>, wire: Option<usize>) {
        self.selected_component = component;
        self.selected_wire = wire;
        self.selected_note = None;
    }

    /// Select one annotation (and nothing else), or none.
    pub fn select_note(&mut self, note: Option<Note>) {
        self.selected_component = None;
        self.selected_wire = None;
        self.selected_note = note;
    }

    /// Deselect everything.
    pub fn clear_selection(&mut self) {
        self.selected_component = None;
        self.selected_wire = None;
        self.selected_note = None;
    }

    /// Right click: cancel the in-flight wire and drop any selection.
    pub fn cancel_gesture(&mut self) {
        self.active_wire_start = None;
        self.clear_selection();
    }

    // ── Annotations: net labels and SPICE directives ────────────────────────

    /// Arm the Label tool with `name` (validated now, so a bad name is
    /// refused at the dialog, not after the click).
    pub fn arm_label(&mut self, name: &str) -> Result<(), String> {
        self.pending_text = check_net_name(name)?;
        self.tool = Tool::Label;
        Ok(())
    }

    /// Arm the Directive tool with `text` (validated now).
    pub fn arm_directive(&mut self, text: &str) -> Result<(), String> {
        self.pending_text = check_directive(text)?;
        self.tool = Tool::Directive;
        Ok(())
    }

    /// One click of the Label / Directive tool: drop the armed text at `at`
    /// and fall back to Select (one placement per arming, as in LTspice's
    /// dialog-then-click flow). A label dropped where one already sits
    /// renames it instead of stacking a second. True if something was placed.
    pub fn place_note(&mut self, at: GridPt) -> bool {
        let text = std::mem::take(&mut self.pending_text);
        if text.is_empty() {
            return false;
        }
        match self.tool {
            Tool::Label => {
                match self.labels.iter_mut().find(|l| l.at == at) {
                    Some(existing) => existing.name = text,
                    None => self.labels.push(NetLabel { at, name: text }),
                }
            }
            Tool::Directive => self.directives.push(Directive { at, text }),
            _ => return false,
        }
        self.tool = Tool::Select;
        true
    }

    /// Replace the text of the annotation `note` (the edit popup's OK).
    pub fn edit_note(&mut self, note: Note, text: &str) -> Result<(), String> {
        match note {
            Note::Label(i) => {
                let name = check_net_name(text)?;
                self.labels.get_mut(i).ok_or("no such label")?.name = name;
            }
            Note::Directive(i) => {
                let text = check_directive(text)?;
                self.directives.get_mut(i).ok_or("no such directive")?.text = text;
            }
        }
        Ok(())
    }

    /// Remove one annotation.
    pub fn delete_note(&mut self, note: Note) {
        match note {
            Note::Label(i) if i < self.labels.len() => {
                self.labels.remove(i);
            }
            Note::Directive(i) if i < self.directives.len() => {
                self.directives.remove(i);
            }
            _ => {}
        }
        self.selected_note = None;
    }

    /// Remove one component by index (the canvas popup's Delete).
    pub fn delete_component(&mut self, idx: usize) {
        self.components.remove(idx);
        self.clear_selection();
    }

    /// Remove one wire by index (the wire popup's Delete).
    pub fn delete_wire(&mut self, idx: usize) {
        self.wires.remove(idx);
        self.selected_wire = None;
    }

    /// Delete the selected wire, else the selected component — the one
    /// action behind the Delete key, the Edit menu and the toolbar button.
    /// True if something was removed.
    pub fn delete_selection(&mut self) -> bool {
        if let Some(note) = self.selected_note {
            let exists = match note {
                Note::Label(i) => i < self.labels.len(),
                Note::Directive(i) => i < self.directives.len(),
            };
            self.delete_note(note);
            if exists {
                return true;
            }
        }
        if let Some(wi) = self.selected_wire {
            if wi < self.wires.len() {
                self.delete_wire(wi);
                return true;
            }
            self.selected_wire = None;
        }
        if let Some(ci) = self.selected_component {
            if ci < self.components.len() {
                self.delete_component(ci);
                return true;
            }
            self.selected_component = None;
        }
        false
    }

    /// Space / Rotate: turn the selected component, or — with nothing
    /// selected — the facing of the next placement.
    pub fn rotate_or_advance(&mut self) {
        if self.selected_component.is_some() {
            self.rotate_selected();
        } else {
            self.advance_direction();
        }
    }

    /// Zoom by `factor`, keeping the canvas-local point `anchor` fixed on
    /// screen.
    pub fn zoom_about(&mut self, factor: f32, anchor: Vec2) {
        let old = self.zoom;
        let new = (old * factor).clamp(MIN_ZOOM, MAX_ZOOM);
        let at = (anchor - self.pan) / old;
        self.pan = anchor - at * new;
        self.zoom = new;
    }

    /// Pixel-space bounding box of every component and wire, `None` when the
    /// schematic is empty.
    pub fn content_bounds(&self) -> Option<egui::Rect> {
        let mut pts = self
            .components
            .iter()
            .flat_map(|c| c.pins())
            .chain(self.components.iter().flat_map(|c| [c.p1, c.p2]))
            .chain(self.wires.iter().flat_map(|w| [w.p1, w.p2]))
            .chain(self.labels.iter().map(|l| l.at))
            .chain(self.directives.iter().map(|d| d.at))
            .map(|g| g.to_pos());
        let first = pts.next()?;
        let mut bounds = egui::Rect::from_min_max(first, first);
        for p in pts {
            bounds.extend_with(p);
        }
        Some(bounds)
    }

    /// Zoom to fit: scale and center the content in a canvas of `canvas`
    /// pixels (View → Zoom to Fit). An empty schematic resets the view.
    pub fn fit_view(&mut self, canvas: Vec2) {
        let Some(bounds) = self.content_bounds() else {
            self.zoom = 1.0;
            self.pan = Vec2::ZERO;
            return;
        };
        let avail = (canvas - Vec2::splat(2.0 * FIT_MARGIN)).max(Vec2::splat(20.0));
        let zx = if bounds.width() > 0.0 { avail.x / bounds.width() } else { f32::MAX };
        let zy = if bounds.height() > 0.0 { avail.y / bounds.height() } else { f32::MAX };
        let zoom = zx.min(zy).clamp(MIN_ZOOM, FIT_MAX_ZOOM);
        self.zoom = zoom;
        self.pan = canvas / 2.0 - bounds.center().to_vec2() * zoom;
    }

    /// Build the typed netlist IR from the schematic. The editor and the
    /// simulator then speak one language, and the emitted text comes from
    /// the canonical [`Netlist`] writer instead of hand-built format
    /// strings.
    ///
    /// Element values are user text with optional SPICE suffixes, so they
    /// parse through the shared value grammar — a bad literal is reported
    /// here instead of silently becoming 0.
    pub fn build_netlist(&self) -> Result<Netlist, String> {
        let assignment = NodeAssignment::of(&self.components, &self.wires, &self.labels);
        if let Some(conflict) = assignment.conflicts().into_iter().next() {
            return Err(conflict);
        }

        let mut circuit = CsCircuit::new("schematic");
        let mut r_idx = 1; let mut c_idx = 1; let mut l_idx = 1; let mut v_idx = 1;

        for c in &self.components {
            // Grounds are pins, and library devices are written as text cards
            // by `deck_text` — the typed netlist is the R/C/L/V core.
            if c.kind == CompKind::Ground || c.kind.is_device() { continue; }

            let n1 = NodeId(assignment.node_of(c.p1));
            let n2 = NodeId(assignment.node_of(c.p2));
            circuit.ensure_node(n1.0);
            circuit.ensure_node(n2.0);

            match c.kind {
                CompKind::Resistor => {
                    let resistance = parse_value(&c.val)
                        .map_err(|e| format!("R{r_idx} value '{}': {e}", c.val))?;
                    circuit.resistors.push(CsResistor {
                        name: format!("R{r_idx}"),
                        n1, n2, resistance,
                        model: None,
                    });
                    r_idx += 1;
                }
                CompKind::Capacitor => {
                    let capacitance = parse_value(&c.val)
                        .map_err(|e| format!("C{c_idx} value '{}': {e}", c.val))?;
                    circuit.capacitors.push(CsCapacitor {
                        name: format!("C{c_idx}"), n1, n2, capacitance,
                        model: None,
                    });
                    c_idx += 1;
                }
                CompKind::Inductor => {
                    let inductance = parse_value(&c.val)
                        .map_err(|e| format!("L{l_idx} value '{}': {e}", c.val))?;
                    circuit.inductors.push(CsInductor {
                        name: format!("L{l_idx}"), n1, n2, inductance,
                        model: None,
                    });
                    l_idx += 1;
                }
                CompKind::Voltage => {
                    // A source's text is the card's value grammar (`5`,
                    // `DC 5`, `AC 1 0`, `PULSE(…)`): the popup writes `DC 5`
                    // and a fresh part drops with it, which the plain number
                    // parser rejected — no placed source could build.
                    let (voltage, waveform, ac) = match parse_source_value(&c.val, &ParseOptions::default())
                        .map_err(|e| format!("V{v_idx} value '{}': {e}", c.val))?
                    {
                        SourceValue::Dc { value } => (value, None, None),
                        SourceValue::Ac { magnitude, phase } => {
                            (0.0, None, Some(CsAc { mag: magnitude, phase }))
                        }
                        SourceValue::Waveform(w) => (0.0, Some(w), None),
                    };
                    circuit.voltage_sources.push(CsVoltageSource {
                        name: format!("V{v_idx}"),
                        n1, n2, voltage,
                        waveform, ac,
                    });
                    v_idx += 1;
                }
                _ => unreachable!(),
            }
        }

        // Labeled nets carry their label as the node's name, so the emitted
        // deck, the run's `V(name)` traces and the canvas all agree. A net no
        // element touches has no card to show a name on — and no node.
        for node in 1..circuit.nodes {
            if let Some(name) = assignment.name_of(node) {
                circuit.set_node_name(node, name);
            }
        }

        // A `.tran` / `.ac` / `.op` directive on the canvas *is* the analysis:
        // it replaces the field-driven command of its kind, so the deck never
        // carries two of them and a bad value in an unused field cannot block
        // the build.
        let placed: HashSet<String> = self
            .directives
            .iter()
            .flat_map(|d| directive_words(&d.text))
            .collect();

        let mut analysis = AnalysisCommands::default();
        analysis.dc_op = !placed.contains(".op");
        match self.analysis_type {
            AnalysisType::Tran if placed.contains(".tran") => {}
            AnalysisType::Ac if placed.contains(".ac") => {}
            AnalysisType::Tran => {
                let step = parse_value(&self.tran_step)
                    .map_err(|e| format!(".tran step '{}': {e}", self.tran_step))?;
                let stop = parse_value(&self.tran_stop)
                    .map_err(|e| format!(".tran stop '{}': {e}", self.tran_stop))?;
                analysis.tran_step = Some(step);
                analysis.tran_stop = Some(stop);
                analysis.tran_start = 0.0;
            }
            AnalysisType::Ac => {
                let scale = match self.ac_variation.to_ascii_lowercase().as_str() {
                    "dec" => AcScale::Dec,
                    "oct" => AcScale::Oct,
                    "lin" => AcScale::Lin,
                    other => {
                        return Err(format!(
                            ".ac sweep type '{other}' must be dec, oct, or lin"
                        ));
                    }
                };
                let points = self.ac_points.trim().parse::<usize>()
                    .map_err(|_| {
                        format!(".ac points '{}' must be a whole number", self.ac_points)
                    })?;
                let f_start = parse_value(&self.ac_fstart)
                    .map_err(|e| format!(".ac start '{}': {e}", self.ac_fstart))?;
                let f_stop = parse_value(&self.ac_fstop)
                    .map_err(|e| format!(".ac stop '{}': {e}", self.ac_fstop))?;
                analysis.ac = Some(AcCommand { scale, points, f_start, f_stop, stimulus_source: None });
            }
        }

        Ok(Netlist {
            circuit,
            analysis,
            // Schematic-built decks carry no source text and no parameters:
            // the parameter language only exists in the text editor.
            source: String::new(),
            params: HashMap::new(),
            funcs: HashMap::new(),
            mc_seed: 0,
            // Nothing was parsed here — no import diagnostics exist yet.
            warnings: Vec::new(),
            options: ParseOptions::default(),
            overrides: HashMap::new(),
        })
    }

    /// The deck text the schematic stands for: the canonical writer's output
    /// for the elements and the analysis fields, with every directive's text
    /// spliced in verbatim ahead of the element cards (where `.param`,
    /// `.model` and `.subckt` conventionally sit; the deck grammar does not
    /// care about order, a reader does). This — not [`Self::build_netlist`]
    /// alone — is what the Generate Netlist command writes and what a run
    /// of the schematic simulates.
    pub fn deck_text(&self) -> Result<String, String> {
        let mut text = netlist_emit(&self.build_netlist()?);
        let (cards, default_models) = self.device_cards()?;
        if !cards.is_empty() {
            // Device cards ride just ahead of the terminator; the grammar
            // does not care about card order.
            let block: String = cards.iter().map(|c| format!("{c}\n")).collect();
            let end = text.rfind("\n.end").map_or(text.len(), |i| i + 1);
            text.insert_str(end, &block);
        }
        let mut block: String = default_models.iter().map(|c| format!("{c}\n")).collect();
        block.extend(self.directives.iter().map(|d| format!("{}\n", d.text.trim())));
        if block.is_empty() {
            return Ok(text);
        }
        // The writer opens with a `* Canonical netlist` comment line.
        let after_title = text.find('\n').map_or(text.len(), |i| i + 1);
        text.insert_str(after_title, &block);
        Ok(text)
    }

    /// The text cards of every library device, in component order, plus the
    /// stock `.model` cards the parts lean on. A default model (`NMOS`, `D`,
    /// …) is only added when no directive on the canvas defines that name, so
    /// a user's own `.model NMOS …` always wins and nothing is defined twice.
    fn device_cards(&self) -> Result<(Vec<String>, Vec<String>), String> {
        if !self.components.iter().any(|c| c.kind.is_device()) {
            return Ok((Vec::new(), Vec::new()));
        }
        let assignment = NodeAssignment::of(&self.components, &self.wires, &self.labels);
        let names = instance_names(&self.components);
        let defined: HashSet<String> = self
            .directives
            .iter()
            .flat_map(|d| d.text.lines())
            .filter_map(|line| {
                let mut w = line.split_whitespace();
                (w.next()?.eq_ignore_ascii_case(".model")).then(|| w.next().map(str::to_ascii_lowercase))?
            })
            .collect();

        let mut cards = Vec::new();
        let mut models: Vec<String> = Vec::new();
        for (c, name) in self.components.iter().zip(&names) {
            if !c.kind.is_device() {
                continue;
            }
            let spec = symbols::spec_of(c.kind);
            let pin_nodes: Vec<String> = c.pins().iter().map(|&p| assignment.spelling(assignment.node_of(p))).collect();
            let val = c.val.trim();
            if c.kind == CompKind::Transformer {
                let (lp, ls, k) = transformer_params(name, val)?;
                cards.push(format!("L{name}p {} {} {lp}", pin_nodes[0], pin_nodes[1]));
                cards.push(format!("L{name}s {} {} {ls}", pin_nodes[2], pin_nodes[3]));
                cards.push(format!("{name} L{name}p L{name}s {k}"));
                continue;
            }
            let slots: Vec<&str> = spec.card_order.iter().map(|&i| pin_nodes[i].as_str()).collect();
            let mut card = format!("{name} {}", slots.join(" "));
            if !val.is_empty() {
                card.push(' ');
                card.push_str(val);
            }
            cards.push(card);
            if let Some(model) = symbols::model_token(c.kind, val) {
                let key = model.to_ascii_lowercase();
                if !defined.contains(&key) {
                    if let Some(card) = symbols::default_model_card(model) {
                        if !models.iter().any(|m| m == card) {
                            models.push(card.to_string());
                        }
                    }
                }
            }
        }
        Ok((cards, models))
    }

    /// Parse a netlist text and lay a schematic out from it: `Ok` carries
    /// the new state plus a success status line; `Err` carries the status
    /// message (parse complaint or "nothing drawable") and leaves the
    /// current schematic untouched.
    ///
    /// Layout: see `autolayout` — signal flow left to right, shunts hang to
    /// ground symbols, nets are routed orthogonally around the parts.
    pub fn from_netlist(text: &str, previous: &SchematicState) -> Result<(SchematicState, String), String> {
        Self::from_netlist_with_options(text, previous, &ParseOptions::default())
    }

    /// [`Self::from_netlist`] with library search options: a deck that
    /// `.include`s a model library imports only when the library resolves
    /// the way the simulation run will resolve it.
    pub fn from_netlist_with_options(
        text: &str,
        previous: &SchematicState,
        options: &ParseOptions,
    ) -> Result<(SchematicState, String), String> {
        // ── 1. Parse the netlist through the one shared grammar ──────────────
        // (The schematic previously re-tokenised the text by hand, silently
        // dropping every diode/MOSFET card and whatever else it misread.)
        #[derive(Clone)]
        struct ParsedComp {
            kind:   CompKind,
            node_a: usize,
            node_b: usize,
            val:    String,
        }

        let netlist = parse_with_options(text, options).map_err(|e| format!("Netlist error: {e}"))?;
        let circuit = &netlist.circuit;
        // Tolerant import reports what it carried over but could not apply
        // (unknown directives, unsupported device families).
        let import_warnings = netlist.warnings.len();

        // Values show the way a schematic writes them (`1k`, `100n`), and
        // only when that spelling re-parses to the very same number — the
        // round trip never loses the number.
        let fmt_val = fmt_si;

        // A device whose model carries series resistance (`RS`, `RD`, `RE`, …)
        // was split at parse time into the bare device plus a `<name>_R?`
        // resistor and an internal node. The `.model` card rides along as
        // text and will split the device again on the way out, so the pair is
        // folded back into the one device here — drawing the resistor too
        // would double the resistance on the first round trip.
        let series_arms: HashMap<&str, (usize, usize)> =
            circuit.resistors.iter().map(|r| (r.name.as_str(), (r.n1.0, r.n2.0))).collect();
        let mut folded: HashSet<String> = HashSet::new();
        let mut outer = |device: &str, arm: &str, node: usize| -> usize {
            let name = format!("{device}_R{arm}");
            match series_arms.get(name.as_str()) {
                Some(&(external, internal)) if internal == node => {
                    folded.insert(name);
                    external
                }
                _ => node,
            }
        };

        // Devices first: folding has to see every device before the passive
        // loop decides which resistors are real.
        struct ParsedDevice {
            kind:  CompKind,
            /// Node per pin, in the symbol's pin order.
            nodes: Vec<usize>,
            val:   String,
        }
        let mut devices: Vec<ParsedDevice> = vec![];
        let mut two_pin: Vec<ParsedComp> = vec![];
        // Elements the schematic has no symbol (or no faithful symbol) for;
        // counted, never dropped silently.
        let mut not_drawn: Vec<String> = vec![];

        // The sensing source an `F`/`H`/`W` card names will be renumbered
        // like every drawn source (`V1`, `V2`, … in deck order); rename the
        // reference with it so the regenerated deck still resolves.
        let source_names: HashMap<String, String> = circuit
            .voltage_sources
            .iter()
            .enumerate()
            .map(|(i, v)| (v.name.to_ascii_lowercase(), format!("V{}", i + 1)))
            .collect();
        let sensed = |name: &str| -> String {
            source_names.get(&name.to_ascii_lowercase()).cloned().unwrap_or_else(|| name.to_string())
        };
        let with_area = |model: &str, area: f64| -> String {
            if area == 1.0 { model.to_string() } else { format!("{model} AREA={}", fmt_si(area)) }
        };

        for d in &circuit.diodes {
            let anode = outer(&d.name, "S", d.anode.0);
            two_pin.push(ParsedComp {
                kind: CompKind::Diode, node_a: anode, node_b: d.cathode.0,
                val: with_area(&d.model, d.area),
            });
        }
        for m in &circuit.mosfets {
            let kind = match circuit.mosfet_type(&m.model) {
                Some(MosfetType::Nmos) => CompKind::Nmos,
                Some(MosfetType::Pmos) => CompKind::Pmos,
                None => { not_drawn.push(m.name.clone()); continue; }
            };
            if m.nb != m.ns {
                // The 3-pin symbol ties the bulk to the source.
                not_drawn.push(m.name.clone());
                continue;
            }
            let mut val = format!("{} W={} L={}", m.model, fmt_si(m.w), fmt_si(m.l));
            for (key, v) in [("AD", m.ad), ("AS", m.asr), ("PD", m.pd), ("PS", m.ps)] {
                if v != 0.0 {
                    val.push_str(&format!(" {key}={}", fmt_si(v)));
                }
            }
            let (d, g, s) = (m.nd.0, m.ng.0, m.ns.0);
            let nodes = if kind == CompKind::Nmos { vec![d, s, g] } else { vec![s, d, g] };
            devices.push(ParsedDevice { kind, nodes, val });
        }
        for q in &circuit.bjts {
            let kind = match circuit.bjt_type(&q.model) {
                Some(BjtType::Npn) => CompKind::Npn,
                Some(BjtType::Pnp) => CompKind::Pnp,
                None => { not_drawn.push(q.name.clone()); continue; }
            };
            if q.ns.is_some() {
                not_drawn.push(q.name.clone());
                continue;
            }
            let c = outer(&q.name, "C", q.nc.0);
            let b = outer(&q.name, "B", q.nb.0);
            let e = outer(&q.name, "E", q.ne.0);
            let nodes = if kind == CompKind::Npn { vec![c, e, b] } else { vec![e, c, b] };
            devices.push(ParsedDevice { kind, nodes, val: with_area(&q.model, q.area) });
        }
        for j in &circuit.jfets {
            let kind = match circuit.jfet_type(&j.model) {
                Some(JfetType::Njf) => CompKind::Njf,
                Some(JfetType::Pjf) => CompKind::Pjf,
                None => { not_drawn.push(j.name.clone()); continue; }
            };
            let d = outer(&j.name, "D", j.nd.0);
            let sr = outer(&j.name, "S", j.ns.0);
            devices.push(ParsedDevice {
                kind, nodes: vec![d, sr, j.ng.0], val: with_area(&j.model, j.area),
            });
        }
        for i in &circuit.current_sources {
            let val = match (&i.waveform, &i.ac) {
                (Some(w), _) => emit_source_value(&SourceValue::Waveform(w.clone())),
                (None, Some(ac)) => format!("DC {} AC {} {}", i.current, ac.mag, ac.phase),
                (None, None) => emit_source_value(&SourceValue::Dc { value: i.current }),
            };
            two_pin.push(ParsedComp { kind: CompKind::Current, node_a: i.n1.0, node_b: i.n2.0, val });
        }
        // Controlled sources draw only in their plain gain form; POLY and
        // multi-term cards stay in the netlist text editor.
        let gain = |src: &circuitsim::circuit::ControlledSource| -> Option<f64> {
            (src.controls.len() == 1 && src.coeffs.len() == 2 && src.coeffs[0] == 0.0)
                .then(|| src.coeffs[1])
        };
        for (family, list) in [(CompKind::Vcvs, circuit.vcvs()), (CompKind::Vccs, circuit.vccs())] {
            for src in list {
                match (gain(src), src.controls.first()) {
                    (Some(g), Some(ControlVar::Voltage(cp, cn))) => devices.push(ParsedDevice {
                        kind: family,
                        nodes: vec![src.out_p.0, src.out_n.0, cp.0, cn.0],
                        val: fmt_si(g),
                    }),
                    _ => not_drawn.push(src.name.clone()),
                }
            }
        }
        for (family, list) in [(CompKind::Ccvs, circuit.ccvs()), (CompKind::Cccs, circuit.cccs())] {
            for src in list {
                match (gain(src), src.controls.first()) {
                    (Some(g), Some(ControlVar::Current(by))) => two_pin.push(ParsedComp {
                        kind: family, node_a: src.out_p.0, node_b: src.out_n.0,
                        val: format!("{} {}", sensed(by), fmt_si(g)),
                    }),
                    _ => not_drawn.push(src.name.clone()),
                }
            }
        }
        for b in &circuit.behavioral {
            let drawable = b.abm.is_none() && b.name.to_ascii_uppercase().starts_with('B');
            if !drawable {
                not_drawn.push(b.name.clone());
                continue;
            }
            let lead = if b.kind == BehavioralKind::Voltage { "V" } else { "I" };
            two_pin.push(ParsedComp {
                kind: CompKind::Behavioral, node_a: b.out_p.0, node_b: b.out_n.0,
                val: format!("{lead}={}", b.expr),
            });
        }
        for sw in &circuit.switches {
            let initial = match sw.initial {
                Some(true) => " ON",
                Some(false) => " OFF",
                None => "",
            };
            match &sw.control {
                SwitchControl::Voltage(cp, cn) => devices.push(ParsedDevice {
                    kind: CompKind::Switch,
                    nodes: vec![sw.n1.0, sw.n2.0, cp.0, cn.0],
                    val: format!("{}{initial}", sw.model),
                }),
                SwitchControl::Current(by) => two_pin.push(ParsedComp {
                    kind: CompKind::CSwitch, node_a: sw.n1.0, node_b: sw.n2.0,
                    val: format!("{} {}{initial}", sensed(by), sw.model),
                }),
            }
        }
        for t in &circuit.tlines {
            devices.push(ParsedDevice {
                kind: CompKind::Tline,
                nodes: vec![t.n1.0, t.n2.0, t.n3.0, t.n4.0],
                val: format!("Z0={} TD={}", fmt_si(t.z0), fmt_si(t.delay)),
            });
        }
        for u in &circuit.opamps {
            devices.push(ParsedDevice {
                kind: CompKind::Opamp,
                nodes: vec![u.in_p.0, u.in_n.0, u.out_p.0, u.out_n.0],
                val: u.model.clone(),
            });
        }
        // Coupled inductors (`K`) draw as two inductors with no coupling:
        // the pair is never recognised as a transformer, so say so.
        for k in circuit.mutual_inductances() {
            not_drawn.push(k.name.clone());
        }

        let mut parsed: Vec<ParsedComp> = vec![];
        for r in circuit.resistors.iter().filter(|r| !folded.contains(&r.name)) {
            parsed.push(ParsedComp {
                kind:   CompKind::Resistor,
                node_a: r.n1.0,
                node_b: r.n2.0,
                val:    fmt_val(r.resistance),
            });
        }
        for c in &circuit.capacitors {
            parsed.push(ParsedComp {
                kind:   CompKind::Capacitor,
                node_a: c.n1.0,
                node_b: c.n2.0,
                val:    fmt_val(c.capacitance),
            });
        }
        for l in &circuit.inductors {
            parsed.push(ParsedComp {
                kind:   CompKind::Inductor,
                node_a: l.n1.0,
                node_b: l.n2.0,
                val:    fmt_val(l.inductance),
            });
        }
        for v in &circuit.voltage_sources {
            // Only the DC level is drawn; PULSE/SIN parameters stay in the
            // netlist (the schematic has no waveform editing yet).
            parsed.push(ParsedComp {
                kind:   CompKind::Voltage,
                node_a: v.n1.0,
                node_b: v.n2.0,
                val:    fmt_val(v.voltage),
            });
        }
        parsed.extend(two_pin);

        if parsed.is_empty() && devices.is_empty() {
            let msg = if not_drawn.is_empty() {
                "No drawable components found in netlist".to_string()
            } else {
                format!(
                    "No drawable components found in netlist — {} element(s) have no faithful schematic symbol ({})",
                    not_drawn.len(),
                    not_drawn.join(", ")
                )
            };
            return Err(msg);
        }

        // ── 2. Place the parts and route the nets ────────────────────────────
        // (`autolayout` owns the geometry; see its module docs.)
        let parts: Vec<crate::autolayout::Part> = parsed
            .iter()
            .map(|c| crate::autolayout::Part {
                kind: c.kind,
                a: c.node_a,
                b: c.node_b,
                val: c.val.clone(),
            })
            .collect();
        let layout = crate::autolayout::layout(&parts);
        let mut new_components = layout.components.clone();
        let mut new_wires = layout.wires.clone();

        // ── 3. Multi-pin devices: a band below the routed circuit ────────────
        // The layout draws two-pin parts only. Each multi-pin device drops
        // facing down in its own band under everything else, and each of its
        // pins joins its net by *name*: a label sitting on a pin names that
        // pin's net, and labels of one name are one net with or without
        // wires. A ground pin gets a short stub and a ground symbol instead.
        // Electrically exact by construction; a named net keeps its name, and
        // an unnamed (numeric) net a device touches is named `_n<N>` on both
        // sides, as the layout's own unrouted ties are.
        let mut device_pins: Vec<(usize, GridPt)> = Vec::new();
        let mut band_y = new_components
            .iter()
            .flat_map(|c| [c.p1.1, c.p2.1])
            .chain(new_wires.iter().flat_map(|w| [w.p1.1, w.p2.1]))
            .max()
            .map_or(ORIGIN_Y, |bottom| bottom + 5);
        for dev in &devices {
            let spec = symbols::spec_of(dev.kind);
            let p1 = GridPt(ORIGIN_X + 8, band_y);
            let p2 = GridPt(p1.0, p1.1 + spec.axis_len);
            let comp = Component { kind: dev.kind, p1, p2, val: dev.val.clone() };
            let (lo, hi) = symbols::local_bounds(dev.kind, spec.axis_len as f32);
            let (u, v) = symbols::frame(p1, p2);
            let rows = [lo.1, hi.1].map(|b| b * v.1 as f32).into_iter().chain([lo.0, hi.0].map(|a| a * u.1 as f32));
            let (top, bottom) = rows.fold((f32::MAX, f32::MIN), |(t, b), y| (t.min(y), b.max(y)));
            let pins = comp.pins();
            let mid_x = pins.iter().map(|p| p.0).sum::<i32>() as f32 / pins.len() as f32;
            for (&pin, &node) in pins.iter().zip(&dev.nodes) {
                if node == 0 {
                    // Stub away from the symbol's middle, then ground.
                    let dx = if pin.0 as f32 >= mid_x { 2 } else { -2 };
                    let stub = GridPt(pin.0 + dx, pin.1);
                    new_wires.push(Wire { p1: pin, p2: stub });
                    new_components.push(Component { kind: CompKind::Ground, p1: stub, p2: stub, val: "".into() });
                } else {
                    device_pins.push((node, pin));
                }
            }
            new_components.push(comp);
            band_y += (bottom - top).ceil() as i32 + 5;
        }

        // ── 4b. Net labels: every named net keeps its name ──────────────────
        // The label sits on a pin of the net, which the layout guarantees is
        // a wire end.
        let node_set: Vec<usize> = layout.anchors.keys().copied().collect();
        let is_named = |node: usize| {
            let name = circuit.node_name(node);
            !name.is_empty() && name != node.to_string()
        };
        let mut new_labels: Vec<NetLabel> = Vec::new();
        for &node in &node_set {
            if is_named(node) {
                new_labels.push(NetLabel {
                    at: layout.anchors[&node],
                    name: circuit.node_name(node).to_string(),
                });
            }
        }
        // The name a net is joined by when wires don't do it: its own name,
        // else a fresh `_n<N>` (never one a real net already uses).
        let tie_name = |node: usize| -> String {
            if is_named(node) {
                circuit.node_name(node).to_string()
            } else {
                let mut cand = format!("_n{node}");
                while (0..circuit.nodes).any(|m| circuit.node_name(m).eq_ignore_ascii_case(&cand)) {
                    cand.push('_');
                }
                cand
            }
        };
        // Connections the router could not draw are joined by name instead.
        for &(node, at) in &layout.ties {
            new_labels.push(NetLabel { at, name: tie_name(node) });
        }
        // Device pins join their nets by name. An unnamed net also needs the
        // name on its wired side (a pin of the net, hence a wire end), once.
        let mut named_at_anchor: HashSet<usize> = HashSet::new();
        for &(node, pin) in &device_pins {
            if !is_named(node) && named_at_anchor.insert(node) {
                if let Some(&at) = layout.anchors.get(&node) {
                    new_labels.push(NetLabel { at, name: tie_name(node) });
                }
            }
            new_labels.push(NetLabel { at: pin, name: tie_name(node) });
        }

        // ── 4c. SPICE directives: the cards the schematic cannot draw ───────
        // `.tran`/`.ac`/`.op` are the analysis fields' job (step 5); every
        // other dot card rides along as text below the circuit, so a
        // generate-netlist round trip keeps `.model`, `.meas`, `.step`, …
        let mut new_directives: Vec<Directive> = Vec::new();
        {
            let bottom = new_components
                .iter()
                .flat_map(|c| [c.p1.1, c.p2.1])
                .chain(new_wires.iter().flat_map(|w| [w.p1.1, w.p2.1]))
                .max()
                .unwrap_or(ORIGIN_Y);
            let mut y = bottom + 3;
            for card in deck_directives(text).map_err(|e| format!("Netlist error: {e}"))? {
                let word = card.split_whitespace().next().unwrap_or("").to_ascii_lowercase();
                if matches!(word.as_str(), ".tran" | ".ac" | ".op") {
                    continue;
                }
                let rows = card.lines().count() as i32;
                new_directives.push(Directive { at: GridPt(ORIGIN_X, y), text: card });
                y += rows + 1;
            }
        }

        // ── 5. Analysis directives from the typed parse ──────────────────────
        // (The editor radio is single-choice; when a netlist carries both
        // sweeps, AC wins the UI — the simulation itself still runs both.)
        let mut state = SchematicState {
            tool: previous.tool,
            current_dir: previous.current_dir,
            active_wire_start: previous.active_wire_start,
            drag_last_grid: previous.drag_last_grid,
            analysis_type: previous.analysis_type,
            tran_step: previous.tran_step.clone(),
            tran_stop: previous.tran_stop.clone(),
            ac_variation: previous.ac_variation.clone(),
            ac_points: previous.ac_points.clone(),
            ac_fstart: previous.ac_fstart.clone(),
            ac_fstop: previous.ac_fstop.clone(),
            ..SchematicState::default()
        };
        let a = &netlist.analysis;
        if let Some(ac) = &a.ac {
            state.analysis_type = AnalysisType::Ac;
            state.ac_variation = match ac.scale {
                AcScale::Dec => "dec",
                AcScale::Oct => "oct",
                AcScale::Lin => "lin",
            }.to_string();
            state.ac_points = ac.points.to_string();
            state.ac_fstart = format!("{}", ac.f_start);
            state.ac_fstop  = format!("{}", ac.f_stop);
        } else if a.tran_step.is_some() {
            state.analysis_type = AnalysisType::Tran;
            // SI spellings (`1u`, `5m`) rather than `0.000001`, which the
            // narrow analysis fields would clip to `0.00000`.
            state.tran_step = fmt_si(a.tran_step.unwrap());
            state.tran_stop = fmt_si(a.tran_stop.unwrap_or(0.0));
        }

        // ── 6. Commit (a fresh canvas: nothing selected, view reset) ─────────
        state.components = new_components;
        state.wires      = new_wires;
        state.labels     = new_labels;
        state.directives = new_directives;

        let n = parsed.len() + devices.len();
        let mut status = format!("Imported {} component(s) from netlist", n);
        if !not_drawn.is_empty() {
            status.push_str(&format!(
                " ({} element(s) not drawn: {})",
                not_drawn.len(),
                not_drawn.join(", ")
            ));
        }
        if !state.directives.is_empty() {
            status.push_str(&format!(" · {} SPICE directive(s) kept as text", state.directives.len()));
        }
        if import_warnings > 0 {
            status.push_str(&format!(" · {} import warning(s)", import_warnings));
        }
        Ok((state, status))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use circuitsim::netlist::parse;

    fn comp(kind: CompKind, x: i32, y: i32, val: &str) -> Component {
        Component { kind, p1: GridPt(x, y), p2: GridPt(x + 3, y), val: val.to_string() }
    }

    #[test]
    fn fmt_si_uses_suffixes_only_when_the_round_trip_is_exact() {
        // Values as the importer meets them: parsed from suffix spellings.
        for spelling in ["1k", "4.7k", "100n", "2.2u", "5", "10Meg", "1m", "47p", "0"] {
            let v = parse_value(spelling).unwrap();
            assert_eq!(fmt_si(v), spelling, "{spelling}");
        }
        // Whatever the spelling, the number survives the round trip exactly —
        // including values whose suffix form would round (1e-7 is not 100n).
        for v in [
            0.123456789012345, 1e-7, 1.0000000000000001e-7, 9.999999999999999e-8, 3.3e-12,
            4.7e6, -2.5e-3, 123456789.123, 1e15, 7.0e-18, -1e3,
        ] {
            assert_eq!(parse_value(&fmt_si(v)).unwrap(), v, "{v} -> {}", fmt_si(v));
        }
    }

    /// An imported `.tran` fills the step/stop fields with SI spellings; plain
    /// decimals (`0.000001`) overflow the narrow fields and read as `0.00000`.
    #[test]
    fn import_fills_tran_fields_with_si_spellings() {
        let deck = "* rc\nV1 1 0 DC 5\nR1 1 2 1k\nC1 2 0 1u\n.tran 1u 5m\n.end\n";
        let (state, _) = SchematicState::from_netlist(deck, &SchematicState::default())
            .expect("deck imports");
        assert_eq!(state.tran_step, "1u");
        assert_eq!(state.tran_stop, "5m");
    }

    #[test]
    fn instance_names_count_per_kind_and_skip_ground() {
        let comps = vec![
            comp(CompKind::Resistor, 0, 0, "1k"),
            comp(CompKind::Voltage, 0, 4, "5"),
            comp(CompKind::Resistor, 0, 8, "2k"),
            comp(CompKind::Ground, 0, 12, ""),
            comp(CompKind::Capacitor, 0, 16, "1u"),
            comp(CompKind::Inductor, 0, 20, "1m"),
        ];
        assert_eq!(instance_names(&comps), ["R1", "V1", "R2", "", "C1", "L1"]);
    }

    /// The canvas labels must be the names the netlist writer assigns.
    #[test]
    fn instance_names_match_the_netlist_writer() {
        let mut st = SchematicState::default();
        st.components = vec![
            comp(CompKind::Resistor, 0, 0, "1k"),
            comp(CompKind::Capacitor, 0, 4, "1u"),
            comp(CompKind::Resistor, 0, 8, "2k"),
            comp(CompKind::Voltage, 0, 12, "5"),
            comp(CompKind::Ground, 0, 16, ""),
        ];
        let net = st.build_netlist().expect("netlist builds");
        let names = instance_names(&st.components);
        assert_eq!(net.circuit.resistors[0].name, names[0]);
        assert_eq!(net.circuit.capacitors[0].name, names[1]);
        assert_eq!(net.circuit.resistors[1].name, names[2]);
        assert_eq!(net.circuit.voltage_sources[0].name, names[3]);
    }

    #[test]
    fn delete_selection_prefers_the_wire_then_the_component() {
        let mut st = SchematicState::default();
        st.components = vec![comp(CompKind::Resistor, 0, 0, "1k")];
        st.wires = vec![Wire { p1: GridPt(0, 0), p2: GridPt(3, 0) }];
        st.select(Some(0), Some(0));
        assert!(st.delete_selection());
        assert!(st.wires.is_empty() && st.components.len() == 1);
        st.select(Some(0), None);
        assert!(st.delete_selection());
        assert!(st.components.is_empty());
        assert!(!st.delete_selection(), "nothing selected, nothing deleted");
    }

    #[test]
    fn zoom_about_keeps_the_anchor_fixed() {
        let mut st = SchematicState::default();
        st.pan = Vec2::new(30.0, -10.0);
        let anchor = Vec2::new(200.0, 150.0);
        let before = (anchor - st.pan) / st.zoom;
        st.zoom_about(1.5, anchor);
        let after = (anchor - st.pan) / st.zoom;
        assert!((before - after).length() < 1e-3, "{before:?} vs {after:?}");
        assert!((st.zoom - 1.5).abs() < 1e-6);
        st.zoom_about(1000.0, anchor);
        assert_eq!(st.zoom, MAX_ZOOM);
    }

    #[test]
    fn fit_view_centers_content_inside_the_canvas() {
        let mut st = SchematicState::default();
        assert!(st.content_bounds().is_none());
        st.components = vec![comp(CompKind::Resistor, 2, 2, "1k")];
        st.wires = vec![Wire { p1: GridPt(5, 2), p2: GridPt(5, 12) }];
        let canvas = Vec2::new(800.0, 500.0);
        st.fit_view(canvas);
        let b = st.content_bounds().unwrap();
        let top_left = b.min.to_vec2() * st.zoom + st.pan;
        let bottom_right = b.max.to_vec2() * st.zoom + st.pan;
        assert!(top_left.x >= 0.0 && top_left.y >= 0.0, "{top_left:?}");
        assert!(bottom_right.x <= canvas.x && bottom_right.y <= canvas.y, "{bottom_right:?}");
        let center = (top_left + bottom_right) / 2.0;
        assert!((center - canvas / 2.0).length() < 1.0, "{center:?}");

        // Contained and centred holds at any scale, so also pin the scale: this
        // 60 x 200 px circuit is limited by height, 380 px of a 500 px canvas.
        assert_zoom_is(&st, expected_fit_zoom(b, canvas));
        assert!(st.zoom > MIN_ZOOM && st.zoom < FIT_MAX_ZOOM, "zoom {}", st.zoom);

        st.components.clear();
        st.wires.clear();
        st.fit_view(canvas);
        assert_eq!((st.zoom, st.pan), (1.0, Vec2::ZERO));
    }

    /// The scale `fit_view` should choose: the largest that keeps `bounds`
    /// inside the margins, within the fit limits.
    fn expected_fit_zoom(bounds: egui::Rect, canvas: Vec2) -> f32 {
        let avail = canvas - Vec2::splat(2.0 * FIT_MARGIN);
        (avail.x / bounds.width())
            .min(avail.y / bounds.height())
            .clamp(MIN_ZOOM, FIT_MAX_ZOOM)
    }

    fn assert_zoom_is(st: &SchematicState, expected: f32) {
        assert!((st.zoom - expected).abs() < 1e-4, "zoom {} but expected {expected}", st.zoom);
    }

    /// Zoom-to-fit on a lone vertical wire of `len` grid units.
    fn fit_wire(len: i32, canvas: Vec2) -> (SchematicState, egui::Rect) {
        let mut st = SchematicState::default();
        st.wires = vec![Wire { p1: GridPt(0, 0), p2: GridPt(0, len) }];
        st.fit_view(canvas);
        let bounds = st.content_bounds().unwrap();
        (st, bounds)
    }

    #[test]
    fn fit_view_scales_large_content_down_to_the_canvas() {
        // 1200 px tall in a 380 px band: between the limits, so exactly 380/1200.
        let canvas = Vec2::new(800.0, 500.0);
        let (st, bounds) = fit_wire(60, canvas);
        assert_zoom_is(&st, 380.0 / 1200.0);
        assert_zoom_is(&st, expected_fit_zoom(bounds, canvas));
    }

    #[test]
    fn fit_view_caps_small_content_and_floors_huge_content() {
        let canvas = Vec2::new(800.0, 500.0);
        // A lone resistor would fit at 11x; the cap stops it at FIT_MAX_ZOOM.
        let mut st = SchematicState::default();
        st.components = vec![comp(CompKind::Resistor, 2, 2, "1k")];
        st.fit_view(canvas);
        assert_zoom_is(&st, FIT_MAX_ZOOM);
        // 8000 px of wire would fit at 0.05x; the floor keeps it at MIN_ZOOM.
        let (st, _) = fit_wire(400, canvas);
        assert_zoom_is(&st, MIN_ZOOM);
    }

    // ─── Net labels and SPICE directives ────────────────────────────────

    fn label(x: i32, y: i32, name: &str) -> NetLabel {
        NetLabel { at: GridPt(x, y), name: name.to_string() }
    }

    /// V1 across 0..3 on x=0 with R1 from the top pin to ground via a wire.
    fn divider() -> SchematicState {
        let mut st = SchematicState::default();
        st.components = vec![
            Component { kind: CompKind::Voltage, p1: GridPt(0, 0), p2: GridPt(0, 3), val: "5".into() },
            Component { kind: CompKind::Resistor, p1: GridPt(4, 0), p2: GridPt(4, 3), val: "1k".into() },
            Component { kind: CompKind::Ground, p1: GridPt(0, 3), p2: GridPt(0, 3), val: "".into() },
            Component { kind: CompKind::Ground, p1: GridPt(4, 3), p2: GridPt(4, 3), val: "".into() },
        ];
        st.wires = vec![Wire { p1: GridPt(0, 0), p2: GridPt(4, 0) }];
        st
    }

    #[test]
    fn net_names_are_validated() {
        assert_eq!(check_net_name("  out ").unwrap(), "out");
        assert_eq!(check_net_name("0").unwrap(), "0");
        for bad in ["", "  ", "a b", "x{y}", "v(out)", "12", ".tran", "+a", "a;b"] {
            assert!(check_net_name(bad).is_err(), "{bad:?} must be refused");
        }
    }

    #[test]
    fn directives_refuse_blank_text_and_end() {
        assert!(check_directive("  \n ").is_err());
        assert!(check_directive(".meas tran m MAX V(1)\n.END").is_err());
        assert_eq!(check_directive(" .step param r 1 3 1 ").unwrap(), ".step param r 1 3 1");
    }

    #[test]
    fn unlabeled_schematics_number_as_before() {
        let st = divider();
        let a = NodeAssignment::of(&st.components, &st.wires, &[]);
        assert_eq!(a.node_of(GridPt(0, 0)), 1);
        assert_eq!(a.node_of(GridPt(4, 0)), 1);
        assert_eq!(a.node_of(GridPt(0, 3)), 0);
        assert!(a.name_of(1).is_none());
        assert!(a.conflicts().is_empty());
    }

    #[test]
    fn a_label_names_its_net_in_the_deck() {
        let mut st = divider();
        st.labels.push(label(0, 0, "Vin"));
        let text = st.deck_text().expect("deck");
        assert!(text.contains("V1 Vin 0"), "{text}");
        assert!(text.contains("R1 Vin 0"), "{text}");
        let net = parse(&text).expect("re-parse");
        assert_eq!(net.circuit.node_name(1).to_string().to_lowercase(), "vin");
    }

    #[test]
    fn same_name_labels_join_unwired_nets() {
        // Two parts with no wire between them become one net by label alone.
        let mut st = SchematicState::default();
        st.components = vec![
            Component { kind: CompKind::Resistor, p1: GridPt(0, 0), p2: GridPt(0, 3), val: "1k".into() },
            Component { kind: CompKind::Resistor, p1: GridPt(8, 0), p2: GridPt(8, 3), val: "2k".into() },
            Component { kind: CompKind::Ground, p1: GridPt(0, 3), p2: GridPt(0, 3), val: "".into() },
            Component { kind: CompKind::Ground, p1: GridPt(8, 3), p2: GridPt(8, 3), val: "".into() },
        ];
        let apart = NodeAssignment::of(&st.components, &st.wires, &[]);
        assert_ne!(apart.node_of(GridPt(0, 0)), apart.node_of(GridPt(8, 0)));
        st.labels = vec![label(0, 0, "bus"), label(8, 0, "BUS")];
        let joined = NodeAssignment::of(&st.components, &st.wires, &st.labels);
        assert_eq!(joined.node_of(GridPt(0, 0)), joined.node_of(GridPt(8, 0)));
        assert!(joined.conflicts().is_empty(), "case-insensitive: one name");
        assert_eq!(joined.name_of(joined.node_of(GridPt(0, 0))), Some("bus"), "first spelling wins");
    }

    #[test]
    fn zero_and_gnd_labels_ground_a_net() {
        let mut st = divider();
        st.components.retain(|c| c.kind != CompKind::Ground);
        st.labels = vec![label(4, 3, "GND"), label(0, 3, "0")];
        let a = NodeAssignment::of(&st.components, &st.wires, &st.labels);
        assert_eq!(a.node_of(GridPt(4, 3)), 0);
        assert_eq!(a.node_of(GridPt(0, 3)), 0);
        assert!(a.conflicts().is_empty(), "aliases are not names");
        assert!(st.deck_text().is_ok());
    }

    #[test]
    fn two_labels_on_one_net_are_refused_not_guessed() {
        let mut st = divider();
        st.labels = vec![label(0, 0, "a"), label(4, 0, "b")];
        let err = st.deck_text().unwrap_err();
        assert!(err.contains("'a'") && err.contains("'b'"), "{err}");
    }

    #[test]
    fn a_named_ground_is_refused() {
        let mut st = divider();
        st.labels = vec![label(0, 3, "out")];
        let err = st.build_netlist().map(|_| ()).unwrap_err();
        assert!(err.contains("ground"), "{err}");
    }

    #[test]
    fn placing_a_label_arms_then_drops_once() {
        let mut st = divider();
        assert!(st.arm_label("bad name").is_err());
        assert_eq!(st.tool, Tool::Select, "a refused name arms nothing");
        st.arm_label("out").unwrap();
        assert_eq!(st.tool, Tool::Label);
        assert!(st.place_note(GridPt(4, 0)));
        assert_eq!(st.labels, vec![label(4, 0, "out")]);
        assert_eq!(st.tool, Tool::Select, "one placement per arming");
        assert!(!st.place_note(GridPt(1, 1)), "nothing armed, nothing placed");
        // Re-labeling the same point renames instead of stacking.
        st.arm_label("vout").unwrap();
        st.place_note(GridPt(4, 0));
        assert_eq!(st.labels, vec![label(4, 0, "vout")]);
    }

    #[test]
    fn labels_travel_with_a_dragged_or_rotated_pin() {
        let mut st = divider();
        st.labels.push(label(4, 3, "x"));
        st.drag_component(1, 2, 0); // R1 right by two
        assert_eq!(st.labels[0].at, GridPt(6, 3));
        st.selected_component = Some(1);
        st.rotate_selected();
        let r1 = &st.components[1];
        assert_eq!(st.labels[0].at, r1.p2, "label stays on the rotated pin");
    }

    #[test]
    fn notes_edit_delete_and_select() {
        let mut st = divider();
        st.labels.push(label(0, 0, "a"));
        st.directives.push(Directive { at: GridPt(0, 8), text: ".param r=1k".into() });
        st.select_note(Some(Note::Directive(0)));
        st.edit_note(Note::Directive(0), ".param r=2k").unwrap();
        assert_eq!(st.directives[0].text, ".param r=2k");
        assert!(st.edit_note(Note::Label(0), "no good").is_err());
        assert_eq!(st.labels[0].name, "a", "refused edit changes nothing");
        assert!(st.delete_selection());
        assert!(st.directives.is_empty());
        assert!(st.selected_note.is_none());
        st.select_note(Some(Note::Label(0)));
        assert!(st.delete_selection());
        assert!(st.labels.is_empty());
        assert!(!st.delete_selection());
    }

    #[test]
    fn directives_are_spliced_into_the_deck_verbatim() {
        let mut st = divider();
        st.directives.push(Directive { at: GridPt(0, 8), text: ".param w=2u\n.meas tran vmax MAX V(1)".into() });
        let text = st.deck_text().expect("deck");
        assert!(text.contains(".param w=2u\n.meas tran vmax MAX V(1)\n"), "{text}");
        assert!(text.trim_end().ends_with(".end"));
        let net = parse(&text).expect("parses");
        assert_eq!(net.params.len(), 1);
    }

    #[test]
    fn a_tran_directive_replaces_the_field_command() {
        let mut st = divider();
        st.tran_stop = "garbage".into(); // would fail the field parse
        assert!(st.deck_text().is_err());
        st.directives.push(Directive { at: GridPt(0, 8), text: ".tran 2u 1m".into() });
        let text = st.deck_text().expect("the directive owns the analysis");
        assert_eq!(text.matches(".tran").count(), 1, "{text}");
        assert!(text.contains(".tran"), "{text}");
        let net = parse(&text).unwrap();
        assert_eq!(net.analysis.tran_stop, Some(1e-3));
    }

    #[test]
    fn import_keeps_net_names_and_directive_cards() {
        let deck = "* d\n\
                    .param w=2u\n\
                    .model dx D(IS=1e-14)\n\
                    V1 in 0 DC 5\n\
                    R1 in out 1k\n\
                    R2 out 0 2k\n\
                    .meas tran vmax MAX V(out)\n\
                    .op\n\
                    .tran 1u 5m\n\
                    .end\n";
        let (st, status) = SchematicState::from_netlist(deck, &SchematicState::default()).expect("import");
        let names: Vec<_> = st.labels.iter().map(|l| l.name.as_str()).collect();
        assert!(names.contains(&"in") && names.contains(&"out"), "{names:?}");
        let cards: Vec<_> = st.directives.iter().map(|d| d.text.as_str()).collect();
        assert_eq!(cards, [".param w=2u", ".model dx D(IS=1e-14)", ".meas tran vmax MAX V(out)"]);
        assert!(status.contains("3 SPICE directive"), "{status}");
        // Directives are stacked below the circuit, not on top of it.
        let bottom = st.components.iter().flat_map(|c| [c.p1.1, c.p2.1]).max().unwrap();
        assert!(st.directives.iter().all(|d| d.at.1 > bottom));
        // Round trip: the regenerated deck keeps names, cards and one analysis.
        let again = st.deck_text().expect("regenerates");
        assert!(again.contains("R1 in out") || again.contains("R1 out in"), "{again}");
        for card in [".param w=2u", ".model dx D(IS=1e-14)", ".meas tran vmax MAX V(out)"] {
            assert!(again.contains(card), "{card} lost:\n{again}");
        }
        assert_eq!(again.matches(".tran").count(), 1, "{again}");
        let net = parse(&again).expect("re-parses");
        assert_eq!(net.analysis.tran_stop, Some(5e-3));
    }

    /// A node with several pins must come back as ONE net. (The old layout
    /// only joined a node's first and last pin, so every pin in between fell
    /// off the net and the regenerated deck had extra nodes.)
    #[test]
    fn import_keeps_nodes_with_many_pins_connected() {
        let deck = "V1 1 0 DC 5\nR1 1 2 1k\nR2 1 2 2k\nR3 1 3 3k\nR4 2 3 4k\nR5 3 0 5k\nR6 2 0 6k\nC1 1 0 1u\n.end\n";
        let orig = parse(deck).expect("deck parses");
        let (st, _) = SchematicState::from_netlist(deck, &SchematicState::default()).expect("import");
        let back = st.build_netlist().expect("netlist builds");
        let (o, b) = (&orig.circuit, &back.circuit);
        assert_eq!(o.resistors.len(), b.resistors.len());
        let mut map = HashMap::new();
        let mut rev = HashMap::new();
        map.insert(0usize, 0usize);
        rev.insert(0usize, 0usize);
        for (x, y) in o.resistors.iter().zip(&b.resistors) {
            for (from, to) in [(x.n1.0, y.n1.0), (x.n2.0, y.n2.0)] {
                assert_eq!(*map.entry(from).or_insert(to), to, "node {from} split in two");
                assert_eq!(*rev.entry(to).or_insert(from), from, "nodes merged into {to}");
            }
        }
    }

    #[test]
    fn imported_labels_sit_on_their_nets() {
        let deck = "V1 a 0 DC 1\nR1 a b 1k\nR2 b 0 1k\nR3 a b 2k\n.end\n";
        let (st, _) = SchematicState::from_netlist(deck, &SchematicState::default()).expect("import");
        let assignment = NodeAssignment::of(&st.components, &st.wires, &st.labels);
        for l in &st.labels {
            let node = assignment.node_of(l.at);
            assert_eq!(assignment.name_of(node), Some(l.name.as_str()));
            assert_ne!(node, 0);
        }
        assert!(assignment.conflicts().is_empty());
        assert_eq!(st.labels.len(), 2);
    }

    // ── Symbol library: placement, cards, motion, import ────────────────────

    /// Solve a deck's operating point; node voltage by node name.
    fn op_by_name(deck: &str) -> HashMap<String, f64> {
        let net = parse(deck).unwrap_or_else(|e| panic!("deck does not parse: {e}\n{deck}"));
        let dc = circuitsim::run(&net)
            .unwrap_or_else(|e| panic!("deck does not solve: {e}\n{deck}"))
            .dc
            .expect("operating point");
        (1..net.circuit.nodes)
            .map(|i| (net.circuit.node_name(i).to_string(), dc.node_voltages[i]))
            .collect()
    }

    /// A schematic builder for tests: parts drop facing down; nets are tied
    /// by label (a label on a pin names its net, no wire needed).
    struct Bench {
        st: SchematicState,
    }

    impl Bench {
        fn new() -> Self {
            Self { st: SchematicState::default() }
        }

        /// Place a part (tool, value) at `at`; returns its pins.
        fn part(&mut self, tool: Tool, at: (i32, i32), val: Option<&str>) -> Vec<GridPt> {
            self.st.tool = tool;
            self.st.current_dir = Direction::Down;
            self.st.place_at(GridPt(at.0, at.1));
            let c = self.st.components.last_mut().unwrap();
            if let Some(v) = val {
                c.val = v.to_string();
            }
            c.pins()
        }

        fn tie(&mut self, pin: GridPt, net: &str) {
            self.st.labels.push(NetLabel { at: pin, name: net.to_string() });
        }

        fn deck(&self) -> String {
            self.st.deck_text().expect("deck")
        }
    }

    #[test]
    fn a_fresh_part_of_every_family_writes_a_card_the_parser_accepts() {
        for &kind in symbols::PICKER {
            let mut b = Bench::new();
            let v = b.part(Tool::V, (0, 0), Some("1"));
            b.tie(v[0], "a");
            b.tie(v[1], "0");
            let pins = b.part(Tool::Part(kind), (10, 10), None);
            for (i, &pin) in pins.iter().enumerate() {
                b.tie(pin, if i == 0 { "a" } else { "0" });
            }
            let deck = b.deck();
            parse(&deck).unwrap_or_else(|e| panic!("{kind:?}: {e}\n{deck}"));
        }
    }

    #[test]
    fn instance_names_count_per_spice_letter() {
        let mut b = Bench::new();
        b.part(Tool::Part(CompKind::Nmos), (0, 0), None);
        b.part(Tool::Part(CompKind::Pmos), (10, 0), None);
        b.part(Tool::Part(CompKind::Transformer), (20, 0), None);
        b.part(Tool::R, (30, 0), None);
        b.part(Tool::Part(CompKind::Tline), (40, 0), None);
        assert_eq!(instance_names(&b.st.components), ["M1", "M2", "K1", "R1", "T1"]);
    }

    #[test]
    fn a_diode_clamps_a_resistor_fed_node_near_a_diode_drop() {
        let mut b = Bench::new();
        let v = b.part(Tool::V, (0, 0), Some("5"));
        let r = b.part(Tool::R, (10, 0), Some("1k"));
        let d = b.part(Tool::Part(CompKind::Diode), (20, 0), None);
        b.tie(v[0], "in");
        b.tie(v[1], "0");
        b.tie(r[0], "in");
        b.tie(r[1], "d");
        b.tie(d[0], "d");
        b.tie(d[1], "0");
        let deck = b.deck();
        assert!(deck.contains("D1 d 0 D"), "{deck}");
        assert!(deck.contains(".model D D"), "default model supplied:\n{deck}");
        let vd = op_by_name(&deck)["d"];
        assert!((0.5..0.9).contains(&vd), "diode drop {vd}");
    }

    #[test]
    fn mosfet_pins_land_on_the_cards_drain_gate_and_source_slots() {
        for (kind, model) in [(CompKind::Nmos, "NMOS"), (CompKind::Pmos, "PMOS")] {
            let mut b = Bench::new();
            let m = b.part(Tool::Part(kind), (0, 0), None);
            // Pin order differs (P puts the source on top); name each by role.
            let roles = symbols::spec_of(kind).pin_names;
            for (pin, role) in m.iter().zip(roles) {
                b.tie(*pin, role);
            }
            let deck = b.deck();
            let card = deck.lines().find(|l| l.starts_with("M1")).expect("M1 card");
            assert!(card.starts_with("M1 drain gate source source"), "{kind:?} wired as {card}");
            assert!(card.contains(model), "{card}");
        }
    }

    #[test]
    fn an_nmos_common_source_stage_pulls_its_drain_down() {
        let drain = |vg_val: &str| {
            let mut b = Bench::new();
            let vdd = b.part(Tool::V, (0, 0), Some("5"));
            let vg = b.part(Tool::V, (6, 0), Some(vg_val));
            let rl = b.part(Tool::R, (12, 0), Some("1k"));
            let m = b.part(Tool::Part(CompKind::Nmos), (20, 0), None);
            b.tie(vdd[0], "vdd");
            b.tie(vdd[1], "0");
            b.tie(vg[0], "g");
            b.tie(vg[1], "0");
            b.tie(rl[0], "vdd");
            b.tie(rl[1], "d");
            b.tie(m[0], "d"); // drain
            b.tie(m[1], "0"); // source
            b.tie(m[2], "g"); // gate
            op_by_name(&b.deck())["d"]
        };
        let (off, on) = (drain("0"), drain("3"));
        assert!((off - 5.0).abs() < 1e-3, "gate at 0 V leaves the drain at the rail: {off}");
        assert!(on < off - 0.1, "gate at 3 V conducts: {on} vs {off}");
    }

    #[test]
    fn an_opamp_with_feedback_amplifies_by_one_plus_rf_over_rg() {
        let mut b = Bench::new();
        let vin = b.part(Tool::V, (0, 0), Some("0.1"));
        let rf = b.part(Tool::R, (6, 0), Some("9k"));
        let rg = b.part(Tool::R, (12, 0), Some("1k"));
        let u = b.part(Tool::Part(CompKind::Opamp), (20, 10), None);
        b.tie(vin[0], "in");
        b.tie(vin[1], "0");
        b.tie(rf[0], "out");
        b.tie(rf[1], "fb");
        b.tie(rg[0], "fb");
        b.tie(rg[1], "0");
        b.tie(u[0], "in"); // in+
        b.tie(u[1], "fb"); // in-
        b.tie(u[2], "out"); // out+
        b.tie(u[3], "0"); // out-
        let v = op_by_name(&b.deck());
        assert!((v["out"] - 1.0).abs() < 1e-3, "gain stage output {}", v["out"]);
    }

    #[test]
    fn the_transformer_writes_two_inductors_and_a_coupling_card() {
        let mut b = Bench::new();
        let k = b.part(Tool::Part(CompKind::Transformer), (0, 0), Some("Lp=1m Ls=4m k=0.9"));
        for (pin, net) in k.iter().zip(["p", "0", "s", "0"]) {
            b.tie(*pin, net);
        }
        let deck = b.deck();
        assert!(deck.contains("LK1p p 0 1m"), "{deck}");
        assert!(deck.contains("LK1s s 0 4m"), "{deck}");
        assert!(deck.contains("K1 LK1p LK1s 0.9"), "{deck}");
        parse(&deck).expect("parses");

        b.st.components[0].val = "Lp=oops".into();
        let err = b.st.deck_text().unwrap_err();
        assert!(err.contains("K1 Lp"), "{err}");
        b.st.components[0].val = "Q=1".into();
        assert!(b.st.deck_text().unwrap_err().contains("unknown parameter"));
    }

    #[test]
    fn a_users_own_model_card_replaces_the_default_one() {
        let mut b = Bench::new();
        let m = b.part(Tool::Part(CompKind::Nmos), (0, 0), None);
        for (pin, net) in m.iter().zip(["d", "0", "g"]) {
            b.tie(*pin, net);
        }
        b.st.directives.push(Directive { at: GridPt(0, 20), text: ".model NMOS NMOS(VTO=0.5)".into() });
        let deck = b.deck();
        assert_eq!(deck.matches(".model NMOS").count(), 1, "{deck}");
        assert!(deck.contains("VTO=0.5"));
    }

    #[test]
    fn rotating_a_multi_pin_part_drags_every_wire_with_its_pin() {
        let mut b = Bench::new();
        let m = b.part(Tool::Part(CompKind::Nmos), (10, 10), None);
        // A stub wire on each of the three pins.
        for &pin in &m {
            b.st.wires.push(Wire { p1: pin, p2: GridPt(pin.0 - 1, pin.1 - 1) });
        }
        b.st.select(Some(0), None);
        for _ in 0..4 {
            assert!(b.st.rotate_selected());
            let pins = b.st.components[0].pins();
            for (w, pin) in b.st.wires.iter().zip(&pins) {
                assert_eq!(w.p1, *pin, "wire left behind its pin");
            }
        }
        // Four quarter turns come home.
        assert_eq!(b.st.components[0].pins(), m);
    }

    #[test]
    fn dragging_a_multi_pin_part_stretches_wires_on_all_its_pins() {
        let mut b = Bench::new();
        let m = b.part(Tool::Part(CompKind::Vcvs), (10, 10), None);
        for &pin in &m {
            b.st.wires.push(Wire { p1: pin, p2: GridPt(0, pin.1) });
        }
        assert!(b.st.drag_component(0, 2, 3));
        for ((w, pin), old) in b.st.wires.iter().zip(b.st.components[0].pins()).zip(&m) {
            assert_eq!(w.p1, pin);
            assert_eq!(w.p2, GridPt(0, old.1), "the far end stays put");
        }
    }

    /// Import a deck, regenerate its text, and require the same operating
    /// point on every named node — the round trip may renumber and relayout,
    /// never change the circuit.
    fn assert_round_trip(deck: &str) -> String {
        let (st, status) = SchematicState::from_netlist(deck, &SchematicState::default()).expect("imports");
        let again = st.deck_text().expect("regenerates");
        let (before, after) = (op_by_name(deck), op_by_name(&again));
        assert_eq!(
            before.keys().collect::<std::collections::BTreeSet<_>>(),
            after.keys().collect::<std::collections::BTreeSet<_>>(),
            "node sets differ\n--- original\n{deck}\n--- regenerated\n{again}"
        );
        for (node, v) in &before {
            assert!(
                (v - after[node]).abs() <= 1e-6 * v.abs().max(1.0),
                "node {node}: {v} before, {} after\n{again}\n{status}",
                after[node]
            );
        }
        status
    }

    #[test]
    fn a_node_with_three_pins_survives_import() {
        // The importer used to draw one spine wire per node, end to end, and a
        // wire joins a net only at its endpoints: the middle pin floated and
        // the regenerated deck was a different circuit.
        assert_round_trip("V1 a 0 DC 1\nR1 a b 1k\nR2 b 0 1k\nR3 a b 2k\n.end\n");
    }

    const EVERY_FAMILY: &str = "\
.model dm D(RS=10)
.model nm NMOS
.model pm PMOS
.model qn NPN
.model qp PNP
.model jn NJF
.model jp PJF
.model sw1 SW
V1 vdd 0 DC 5
V2 g 0 DC 3
R1 vdd d 1k
M1 d g 0 0 nm W=10u L=1u
M2 pd g vdd vdd pm W=10u L=1u
R2 pd 0 1k
R3 vdd bb 100k
Q1 vdd bb e qn
R4 e 0 1k
Q2 0 g e2 qp
R5 e2 vdd 1k
D1 vdd dk dm
R6 dk 0 1k
J1 vdd gj sj jn
V3 gj 0 DC -1
R7 sj 0 1k
I1 0 x DC 1m
R8 x 0 1k
E1 eo 0 vdd 0 2
R9 eo 0 1k
G1 0 go vdd 0 1m
R10 go 0 1k
H1 ho 0 V1 2k
R11 ho 0 1k
F1 fo 0 V1 2
R12 fo 0 1k
B1 bo 0 V=V(vdd)*2
R13 bo 0 1k
S1 vdd sa g 0 sw1
R14 sa 0 1k
W1 vdd wa V1 sw1
R15 wa 0 1k
T1 t1 0 t3 0 Z0=50 TD=1n
V4 t1 0 DC 1
R16 t3 0 50
U1 uo 0 up un
V5 up 0 DC 1m
R17 uo un 9k
R18 un 0 1k
.op
.end
";

    #[test]
    fn a_deck_with_every_device_family_round_trips_through_the_schematic() {
        let status = assert_round_trip(EVERY_FAMILY);
        assert!(!status.contains("not drawn"), "{status}");
        let (st, _) = SchematicState::from_netlist(EVERY_FAMILY, &SchematicState::default()).unwrap();
        let kinds: HashSet<CompKind> = st.components.iter().map(|c| c.kind).collect();
        for kind in [
            CompKind::Diode, CompKind::Current, CompKind::Nmos, CompKind::Pmos, CompKind::Npn,
            CompKind::Pnp, CompKind::Njf, CompKind::Vcvs, CompKind::Vccs, CompKind::Ccvs,
            CompKind::Cccs, CompKind::Behavioral, CompKind::Switch, CompKind::CSwitch,
            CompKind::Tline, CompKind::Opamp,
        ] {
            assert!(kinds.contains(&kind), "{kind:?} was not drawn");
        }
        // Imported parts wire only where pins meet: no two distinct nets
        // share a grid point, so the node count matches the deck's.
        let a = NodeAssignment::of(&st.components, &st.wires, &st.labels);
        let nodes: HashSet<usize> = a.pt_list.iter().map(|&p| a.node_of(p)).collect();
        // One fewer than the parsed deck: D1's series-resistance node exists
        // only after parse splits the diode, never on the canvas.
        let want = parse(EVERY_FAMILY).unwrap().circuit.nodes - 1;
        assert_eq!(nodes.len(), want, "regenerated nets");
    }

    #[test]
    fn sensing_source_names_follow_the_renumbering() {
        // The deck lists V2 before V1; the schematic renumbers sources in
        // deck order, so `F1 … V1` must now name the second source it drew.
        assert_round_trip(
            "V2 a 0 DC 2\nV1 b 0 DC 1\nR1 a 0 1k\nR2 b 0 1k\nF1 f 0 V1 2\nR3 f 0 1k\n.end\n",
        );
    }

    #[test]
    fn series_resistance_is_not_doubled_by_the_round_trip() {
        // `RS` splits the diode into a resistor and an internal node at parse
        // time; the model card rides along as text and splits it again.
        let deck = ".model dm D(RS=100)\nV1 a 0 DC 5\nR1 a b 1k\nD1 b 0 dm\n.end\n";
        assert_round_trip(deck);
        let (st, _) = SchematicState::from_netlist(deck, &SchematicState::default()).unwrap();
        let resistors = st.components.iter().filter(|c| c.kind == CompKind::Resistor).count();
        assert_eq!(resistors, 1, "the folded RS arm must not be drawn");
    }

    #[test]
    fn what_has_no_faithful_symbol_is_named_not_dropped() {
        let deck = "\
.model nm NMOS
V1 a 0 DC 1
R1 a b 1k
L1 b c 1m
L2 c 0 1m
K1 L1 L2 0.9
M1 a b 0 c nm
.end
";
        let (st, status) = SchematicState::from_netlist(deck, &SchematicState::default()).unwrap();
        assert!(status.contains("not drawn") && status.contains("K1") && status.contains("M1"), "{status}");
        assert!(!st.components.iter().any(|c| c.kind.is_device()));
    }

    #[test]
    fn an_only_device_deck_imports_instead_of_reporting_nothing_drawable() {
        let (st, _) = SchematicState::from_netlist(
            ".model d D\nI1 0 a DC 1m\nD1 a 0 d\n.end\n",
            &SchematicState::default(),
        )
        .unwrap();
        assert_eq!(st.components.iter().filter(|c| c.kind.is_device()).count(), 2);
    }

    #[test]
    fn a_freshly_placed_source_builds_and_keeps_its_waveform() {
        let mut st = SchematicState::default();
        st.tool = Tool::V;
        st.place_at(GridPt(0, 0));
        assert_eq!(st.components[0].val, "DC 5");
        let net = st.build_netlist().expect("the default value builds");
        assert_eq!(net.circuit.voltage_sources[0].voltage, 5.0);

        st.components[0].val = "PULSE(0 5 1m 1u 1u 5m 10m)".into();
        let deck = st.deck_text().expect("a waveform builds");
        assert!(deck.contains("PULSE"), "{deck}");
        st.components[0].val = "AC 1 0".into();
        assert!(st.deck_text().unwrap().contains("AC"));
        st.components[0].val = "5".into();
        assert!(st.build_netlist().is_ok(), "a bare number still reads as DC");
    }

    #[test]
    fn a_numeric_net_a_device_touches_is_named_on_both_sides() {
        // Device pins join their nets by label, and a label cannot be a bare
        // number: net 3 comes back as `_n3`, on the wired side too, with the
        // same voltage.
        let deck = ".model nm NMOS\nV1 1 0 DC 5\nV2 2 0 DC 3\nR1 1 3 1k\nM1 3 2 0 0 nm W=10u L=1u\n.end\n";
        let (st, _) = SchematicState::from_netlist(deck, &SchematicState::default()).unwrap();
        let again = st.deck_text().unwrap();
        let (before, after) = (op_by_name(deck), op_by_name(&again));
        assert!((before["3"] - after["_n3"]).abs() < 1e-9, "{again}");
        assert!(st.labels.iter().filter(|l| l.name == "_n3").count() >= 2, "both sides labelled");
    }
}
