//! The schematic's data layer: the element IR the editor manipulates, the
//! union-find node assignment shared by the netlist writer and the on-canvas
//! node labels, and the editor state machine — placement carpet, tool
//! gestures, model mutations, and both deck conversions. A schematic and a
//! netlist are one round trip apart, and both directions live in this module.

use circuitsim::circuit::{
    Capacitor as CsCapacitor, Circuit as CsCircuit, Inductor as CsInductor, NodeId,
    Resistor as CsResistor, VoltageSource as CsVoltageSource,
};
use circuitsim::netlist::{parse, AcCommand, AcScale, AnalysisCommands, Netlist, ParseOptions};
use circuitsim::units::parse_value;
use egui::{Pos2, Vec2};
use std::collections::{HashMap, HashSet};

/// Canvas grid pitch in screen pixels.
pub const GRID_SIZE: f32 = 20.0;

/// Canvas zoom limits (shared by the wheel, the zoom buttons and zoom-to-fit).
pub const MIN_ZOOM: f32 = 0.2;
pub const MAX_ZOOM: f32 = 5.0;

#[derive(Clone, Copy, PartialEq, Eq, Hash)]
pub struct GridPt(pub i32, pub i32);

impl GridPt {
    pub fn from_pos(pos: Pos2) -> Self {
        Self((pos.x / GRID_SIZE).round() as i32, (pos.y / GRID_SIZE).round() as i32)
    }

    pub fn to_pos(self) -> Pos2 {
        Pos2::new(self.0 as f32 * GRID_SIZE, self.1 as f32 * GRID_SIZE)
    }
}

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum CompKind {
    Resistor,
    Capacitor,
    Inductor,
    Voltage,
    Ground,
}

#[derive(Clone)]
pub struct Component {
    pub kind: CompKind,
    pub p1: GridPt,
    pub p2: GridPt,
    pub val: String,
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
/// numbering [`SchematicState::build_netlist`] assigns (counted per kind), so
/// the canvas can label parts with the names the netlist will carry. Ground
/// symbols are not elements and get an empty name.
pub fn instance_names(components: &[Component]) -> Vec<String> {
    let (mut r, mut c, mut l, mut v) = (0, 0, 0, 0);
    components
        .iter()
        .map(|comp| match comp.kind {
            CompKind::Resistor => { r += 1; format!("R{r}") }
            CompKind::Capacitor => { c += 1; format!("C{c}") }
            CompKind::Inductor => { l += 1; format!("L{l}") }
            CompKind::Voltage => { v += 1; format!("V{v}") }
            CompKind::Ground => String::new(),
        })
        .collect()
}

#[derive(Clone)]
pub struct Wire {
    pub p1: GridPt,
    pub p2: GridPt,
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
}

impl NodeAssignment {
    /// Assign SPICE node numbers to the schematic's points.
    pub fn of(components: &[Component], wires: &[Wire]) -> Self {
        // 1. Collect all unique grid points touched by components or wires
        //    (a ground's p2 equals its p1 everywhere the editor creates them;
        //    including it here matches the historical writer exactly).
        let mut points = HashSet::new();
        for c in components {
            points.insert(c.p1);
            points.insert(c.p2);
        }
        for w in wires {
            points.insert(w.p1);
            points.insert(w.p2);
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

        // 3. Ground roots are forced to node 0.
        let mut ground_roots = HashSet::new();
        for c in components {
            if c.kind == CompKind::Ground {
                if let Some(&idx) = pt_to_idx.get(&c.p1) {
                    ground_roots.insert(dsu.find(idx));
                }
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

        let pt_to_node = pt_list
            .iter()
            .enumerate()
            .map(|(i, &pt)| {
                let root = root_to_node[&dsu.find(i)];
                (pt, root)
            })
            .collect();
        Self { pt_list, pt_to_node }
    }

    /// SPICE node id at a grid point.
    pub fn node_of(&self, pt: GridPt) -> usize {
        self.pt_to_node[&pt]
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
pub enum Tool { Select, Wire, R, C, L, V, Gnd }

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
        let Some((old_p2, new_p2)) = self
            .selected_component
            .and_then(|idx| self.components.get_mut(idx))
            .map(|c| {
                let old_p2 = c.p2;
                let dx = c.p2.0 - c.p1.0;
                let dy = c.p2.1 - c.p1.1;
                // 90-degree rotation formula: (x, y) -> (-y, x)
                c.p2 = GridPt(c.p1.0 - dy, c.p1.1 + dx);
                (old_p2, c.p2)
            })
        else {
            return false;
        };
        // Keep wires attached to the rotated pin
        for w in self.wires.iter_mut() {
            if w.p1 == old_p2 {
                w.p1 = new_p2;
            }
            if w.p2 == old_p2 {
                w.p2 = new_p2;
            }
        }
        true
    }

    /// Drag a selected component by a grid delta; wires attached at either
    /// of its pins stretch by the same delta. True if the component moved.
    pub fn drag_component(&mut self, idx: usize, dx: i32, dy: i32) -> bool {
        let Some(pins) = self.components.get_mut(idx).map(|c| {
            let pins = (c.p1, c.p2);
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
            if w.p1 == pins.0 || w.p1 == pins.1 {
                w.p1.0 += dx;
                w.p1.1 += dy;
            }
            if w.p2 == pins.0 || w.p2 == pins.1 {
                w.p2.0 += dx;
                w.p2.1 += dy;
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
            Tool::R | Tool::C | Tool::L | Tool::V => {}
            _ => return,
        }
        let kind = match self.tool {
            Tool::R => CompKind::Resistor, Tool::C => CompKind::Capacitor,
            Tool::L => CompKind::Inductor, _ => CompKind::Voltage,
        };
        let val = match kind {
            CompKind::Resistor => "1k", CompKind::Capacitor => "1u",
            CompKind::Inductor => "1m", _ => "DC 5",
        }.to_string();

        // Calculate P2 based on current rotation direction
        let (dx, dy) = self.current_dir.offset(3);
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
    }

    /// Deselect everything.
    pub fn clear_selection(&mut self) {
        self.selected_component = None;
        self.selected_wire = None;
    }

    /// Right click: cancel the in-flight wire and drop any selection.
    pub fn cancel_gesture(&mut self) {
        self.active_wire_start = None;
        self.selected_component = None;
        self.selected_wire = None;
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
            .flat_map(|c| [c.p1, c.p2])
            .chain(self.wires.iter().flat_map(|w| [w.p1, w.p2]))
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
        let margin = 60.0;
        let avail = (canvas - Vec2::splat(2.0 * margin)).max(Vec2::splat(20.0));
        let zx = if bounds.width() > 0.0 { avail.x / bounds.width() } else { f32::MAX };
        let zy = if bounds.height() > 0.0 { avail.y / bounds.height() } else { f32::MAX };
        // A lone point would "fit" at the zoom limit; cap the fit at 2x.
        let zoom = zx.min(zy).clamp(MIN_ZOOM, 2.0);
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
        let assignment = NodeAssignment::of(&self.components, &self.wires);

        let mut circuit = CsCircuit::new("schematic");
        let mut r_idx = 1; let mut c_idx = 1; let mut l_idx = 1; let mut v_idx = 1;

        for c in &self.components {
            if c.kind == CompKind::Ground { continue; }

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
                    let voltage = parse_value(&c.val)
                        .map_err(|e| format!("V{v_idx} value '{}': {e}", c.val))?;
                    circuit.voltage_sources.push(CsVoltageSource {
                        name: format!("V{v_idx}"),
                        n1, n2, voltage,
                        waveform: None, ac: None,
                    });
                    v_idx += 1;
                }
                CompKind::Ground => unreachable!(),
            }
        }

        let mut analysis = AnalysisCommands::default();
        analysis.dc_op = true;
        match self.analysis_type {
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

    /// Parse a netlist text and lay a schematic out from it: `Ok` carries
    /// the new state plus a success status line; `Err` carries the status
    /// message (parse complaint or "nothing drawable") and leaves the
    /// current schematic untouched.
    ///
    /// Layout strategy:
    ///   • Each unique non-ground node gets a vertical column (x = col * COL_W).
    ///   • Components are placed vertically between their two node columns at
    ///     increasing y positions (one slot per column pair).
    ///   • Horizontal wires connect component pins to the node column spine.
    ///   • A vertical wire runs along each node spine connecting all the pins.
    ///   • Node 0 (ground) gets a Ground symbol instead of a spine wire.
    pub fn from_netlist(text: &str, previous: &SchematicState) -> Result<(SchematicState, String), String> {
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

        let netlist = parse(text).map_err(|e| format!("Netlist error: {e}"))?;
        let circuit = &netlist.circuit;
        // Tolerant import reports what it carried over but could not apply
        // (unknown directives, unsupported device families).
        let import_warnings = netlist.warnings.len();

        // Values show the way a schematic writes them (`1k`, `100n`), and
        // only when that spelling re-parses to the very same number — the
        // round trip never loses the number.
        let fmt_val = fmt_si;

        let mut parsed: Vec<ParsedComp> = vec![];
        for r in &circuit.resistors {
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
        // Diodes, MOSFETs and current sources have no schematic symbol yet —
        // count them instead of silently dropping them like the old
        // hand-parser did.
        let skipped =
            circuit.diodes.len() + circuit.mosfets.len() + circuit.current_sources.len();

        if parsed.is_empty() {
            let msg = if skipped > 0 {
                format!(
                    "No supported components (R/C/L/V) found in netlist — {} diode/MOSFET/current-source element(s) have no schematic symbol",
                    skipped
                )
            } else {
                "No supported components (R/C/L/V) found in netlist".into()
            };
            return Err(msg);
        }

        // ── 2. Collect all non-ground node numbers and assign columns ─────────
        let mut node_set: Vec<usize> = vec![];
        for c in &parsed {
            for &n in &[c.node_a, c.node_b] {
                if n != 0 && !node_set.contains(&n) {
                    node_set.push(n);
                }
            }
        }
        node_set.sort_unstable();

        let node_to_col: HashMap<usize, i32> = node_set
        .iter()
        .enumerate()
        .map(|(i, &n)| (n, i as i32))
        .collect();

        // ── 3. Lay out components ─────────────────────────────────────────────
        const COL_W:    i32 = 6;
        const COMP_LEN: i32 = 4;
        const ROW_GAP:  i32 = 6;
        const ORIGIN_X: i32 = 3;
        const ORIGIN_Y: i32 = 3;

        let mut new_components: Vec<Component> = vec![];
        let mut new_wires:      Vec<Wire>      = vec![];
        let mut spine_pins: HashMap<usize, Vec<i32>> = HashMap::new();

        // Tracker for packing parallel components efficiently into rows
        struct Interval { start: i32, end: i32 }
        let mut row_intervals: Vec<Vec<Interval>> = vec![];

        for comp in &parsed {
            let col_a = node_to_col.get(&comp.node_a).copied().unwrap_or(-1);
            let col_b = node_to_col.get(&comp.node_b).copied().unwrap_or(-1);

            // Ignore malformed ground-to-ground components
            if col_a < 0 && col_b < 0 { continue; }

            let start_half: i32;
            let end_half: i32;
            let mut mid_half: i32;

            // Calculate "half-column" intervals to map out the X-axis territory
            if col_a >= 0 && col_b >= 0 {
                start_half = col_a.min(col_b) * 2;
                end_half = col_a.max(col_b) * 2;
                mid_half = start_half + (end_half - start_half) / 2;

                // If midpoint lands exactly on a spine (even), push it to the right channel
                if mid_half % 2 == 0 { mid_half += 1; }
            } else {
                let c = col_a.max(col_b); // The non-ground column
                start_half = c * 2;
                end_half = c * 2 + 1;
                mid_half = end_half; // Push ground components into the right-side channel
            }

            // Interval Packer: Find the lowest Y-row where this X-interval doesn't collide
            let mut row = 0;
            loop {
                if row >= row_intervals.len() {
                    row_intervals.push(vec![]);
                }
                // Overlap check (they collide if they share any X-territory)
                let overlaps = row_intervals[row].iter().any(|i| {
                    !(end_half < i.start || start_half > i.end)
                });

                if !overlaps {
                    row_intervals[row].push(Interval { start: start_half, end: end_half });
                    break;
                }
                row += 1;
            }

            let y_top = ORIGIN_Y + (row as i32) * (COMP_LEN + ROW_GAP);
            let comp_x = ORIGIN_X + mid_half * (COL_W / 2);

            let p1 = GridPt(comp_x, y_top);
            let p2 = GridPt(comp_x, y_top + COMP_LEN);

            new_components.push(Component {
                kind: comp.kind,
                p1,
                p2,
                val: comp.val.clone(),
            });

            // Horizontal wire: p1 -> spine of pin1_node
            if comp.node_a != 0 {
                let spine_x = ORIGIN_X + col_a * COL_W;
                if spine_x != comp_x {
                    new_wires.push(Wire { p1: GridPt(comp_x, y_top), p2: GridPt(spine_x, y_top) });
                }
                spine_pins.entry(comp.node_a).or_default().push(y_top);
            } else {
                // Ground pin for node A
                new_components.push(Component {
                    kind: CompKind::Ground,
                    p1: GridPt(comp_x, y_top),
                                    p2: GridPt(comp_x, y_top),
                                    val: "".into(),
                });
            }

            // Horizontal wire: p2 -> spine of pin2_node
            if comp.node_b != 0 {
                let spine_x = ORIGIN_X + col_b * COL_W;
                if spine_x != comp_x {
                    new_wires.push(Wire { p1: GridPt(comp_x, y_top + COMP_LEN), p2: GridPt(spine_x, y_top + COMP_LEN) });
                }
                spine_pins.entry(comp.node_b).or_default().push(y_top + COMP_LEN);
            } else {
                // Ground pin for node B
                new_components.push(Component {
                    kind: CompKind::Ground,
                    p1: GridPt(comp_x, y_top + COMP_LEN),
                                    p2: GridPt(comp_x, y_top + COMP_LEN),
                                    val: "".into(),
                });
            }
        }

        // ── 4. Vertical spine wires for each non-ground node ─────────────────
        for (&node, pins) in &spine_pins {
            let y_min = *pins.iter().min().unwrap();
            let y_max = *pins.iter().max().unwrap();
            // Only draw the vertical spine if the node connects across multiple Y levels
            if y_min != y_max {
                let spine_x = ORIGIN_X + node_to_col[&node] * COL_W;
                new_wires.push(Wire { p1: GridPt(spine_x, y_min), p2: GridPt(spine_x, y_max) });
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

        let n = parsed.len();
        let mut status = format!("Imported {} component(s) from netlist", n);
        if skipped > 0 {
            status.push_str(&format!(" ({} D/M/I element(s) not drawn)", skipped));
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

        st.components.clear();
        st.wires.clear();
        st.fit_view(canvas);
        assert_eq!((st.zoom, st.pan), (1.0, Vec2::ZERO));
    }
}
