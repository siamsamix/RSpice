//! The schematic's data layer: the element IR the editor manipulates, the
//! union-find node assignment shared by the netlist writer and the on-canvas
//! node labels, and the editor state machine — placement carpet, tool
//! gestures, model mutations, and both deck conversions. A schematic and a
//! netlist are one round trip apart, and both directions live in this module.

use circuitsim::circuit::{
    Capacitor as CsCapacitor, Circuit as CsCircuit, Inductor as CsInductor, NodeId,
    Resistor as CsResistor, VoltageSource as CsVoltageSource,
};
use circuitsim::netlist::{
    deck_directives, emit as netlist_emit, parse, AcCommand, AcScale, AnalysisCommands, Netlist,
    ParseOptions,
};
use circuitsim::symbols::{Symbol, SymbolLibrary};
use circuitsim::units::parse_value;
use egui::{Pos2, Vec2};
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

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum CompKind {
    Resistor,
    Capacitor,
    Inductor,
    Voltage,
    Ground,
    /// A library symbol (diode, MOSFET, subcircuit …): pins, drawing and
    /// attached SPICE data come from its [`PartData`].
    Part,
}

#[derive(Clone)]
pub struct Component {
    pub kind: CompKind,
    pub p1: GridPt,
    pub p2: GridPt,
    /// The value text. For a [`CompKind::Part`] this is the attached model or
    /// `.subckt` name (the reference the card carries after its nodes).
    pub val: String,
    /// Present exactly when `kind` is [`CompKind::Part`].
    pub part: Option<PartData>,
}

/// What a placed library symbol carries beyond the two-terminal fields.
/// `Component::p1` / `p2` hold the symbol's first two pins (pin order is the
/// SPICE card's node order); the rest sit in `extra`, so every pin has one
/// home and moving a part cannot leave a pin behind.
#[derive(Clone, PartialEq, Debug)]
pub struct PartData {
    /// The library symbol's name (looked up in [`SchematicState::symbols`]).
    pub symbol: String,
    /// The card's first letter, kept here so instance numbering needs no
    /// library lookup.
    pub prefix: char,
    /// Quarter turns of the symbol's authored frame, for the drawing.
    pub rot: u8,
    /// Pins 3…n, absolute.
    pub extra: Vec<GridPt>,
    /// Total pin count (a one-pin symbol keeps `p2 == p1`).
    pub pin_count: usize,
    /// Explicit instance name (`MM1`); empty numbers it automatically.
    pub name: String,
    /// Extra card text after the reference (`W=10u L=1u`).
    pub spiceline: String,
}

impl Component {
    /// A two-terminal / ground component without part data.
    pub fn basic(kind: CompKind, p1: GridPt, p2: GridPt, val: impl Into<String>) -> Self {
        Self { kind, p1, p2, val: val.into(), part: None }
    }

    /// Place library `symbol` with its origin pin at `at`, turned `rot`
    /// quarter turns, carrying the symbol's default reference and spiceline.
    pub fn from_symbol(symbol: &Symbol, at: GridPt, rot: u8) -> Self {
        let pins: Vec<GridPt> = symbol
            .pins
            .iter()
            .map(|pin| {
                let (x, y) = turn((pin.x, pin.y), rot);
                GridPt(at.0 + x, at.1 + y)
            })
            .collect();
        Self {
            kind: CompKind::Part,
            p1: pins[0],
            p2: pins.get(1).copied().unwrap_or(pins[0]),
            val: symbol.reference.clone(),
            part: Some(PartData {
                symbol: symbol.name.clone(),
                prefix: symbol.prefix,
                rot: rot % 4,
                extra: pins.iter().skip(2).copied().collect(),
                pin_count: pins.len(),
                name: String::new(),
                spiceline: symbol.spiceline.clone(),
            }),
        }
    }

    /// The connection points that join the node assignment, in pin order. A
    /// two-terminal component contributes both ends (a ground's coincide).
    pub fn pin_points(&self) -> Vec<GridPt> {
        match &self.part {
            None => vec![self.p1, self.p2],
            Some(part) => {
                let mut pts = vec![self.p1];
                if part.pin_count >= 2 {
                    pts.push(self.p2);
                }
                pts.extend(part.extra.iter().copied());
                pts
            }
        }
    }
}

/// One quarter turn of the editor's rotation, `(x, y) → (−y, x)` — the law
/// `rotate_selected` has always applied (it takes Right to Down).
pub fn turn(pt: (i32, i32), quarter_turns: u8) -> (i32, i32) {
    let (mut x, mut y) = pt;
    for _ in 0..(quarter_turns % 4) {
        (x, y) = (-y, x);
    }
    (x, y)
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
    // Library parts count per prefix letter (`D1`, `M1`, `M2`, `X1`); a part
    // the user named keeps its name and its number is never handed out again.
    let taken: HashSet<String> = components
        .iter()
        .filter_map(|comp| comp.part.as_ref())
        .filter(|part| !part.name.is_empty())
        .map(|part| part.name.to_ascii_lowercase())
        .collect();
    let mut per_prefix: HashMap<char, usize> = HashMap::new();
    components
        .iter()
        .map(|comp| match (comp.kind, &comp.part) {
            (CompKind::Resistor, _) => { r += 1; format!("R{r}") }
            (CompKind::Capacitor, _) => { c += 1; format!("C{c}") }
            (CompKind::Inductor, _) => { l += 1; format!("L{l}") }
            (CompKind::Voltage, _) => { v += 1; format!("V{v}") }
            (CompKind::Part, Some(part)) if !part.name.is_empty() => part.name.clone(),
            (CompKind::Part, Some(part)) => loop {
                let n = per_prefix.entry(part.prefix).or_insert(0);
                *n += 1;
                let candidate = format!("{}{}", part.prefix, n);
                if !taken.contains(&candidate.to_ascii_lowercase()) {
                    break candidate;
                }
            },
            (CompKind::Part, None) | (CompKind::Ground, _) => String::new(),
        })
        .collect()
}

#[derive(Clone, PartialEq, Debug)]
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
            points.extend(c.pin_points());
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
pub enum Tool { Select, Wire, R, C, L, V, Gnd, Label, Directive, Part }

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
    /// The symbol library the component picker offers and parts resolve
    /// through (the shipped `standard.sym`, plus any library the user loaded).
    pub symbols: SymbolLibrary,
    /// The library symbol the Part tool places on each click.
    pub armed_part: Option<String>,
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
            symbols: SymbolLibrary::builtin(),
            armed_part: None,
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
    /// the wires that touch its rotated pins. True if something rotated.
    pub fn rotate_selected(&mut self) -> bool {
        let Some(idx) = self.selected_component.filter(|&i| i < self.components.len()) else {
            return false;
        };
        let origin = self.components[idx].p1;
        self.retarget_pins(idx, |pt| {
            // 90-degree rotation formula about p1: (x, y) -> (-y, x)
            let (x, y) = turn((pt.0 - origin.0, pt.1 - origin.1), 1);
            GridPt(origin.0 + x, origin.1 + y)
        });
        if let Some(part) = self.components[idx].part.as_mut() {
            part.rot = (part.rot + 1) % 4;
        }
        true
    }

    /// Drag a selected component by a grid delta; wires attached at any of
    /// its pins stretch by the same delta. True if the component moved.
    pub fn drag_component(&mut self, idx: usize, dx: i32, dy: i32) -> bool {
        if idx >= self.components.len() {
            return false;
        }
        self.retarget_pins(idx, |pt| GridPt(pt.0 + dx, pt.1 + dy));
        true
    }

    /// Move every pin of component `idx` through `to_new`, then carry the
    /// wire ends and net labels that sat on an old pin to its new place. The
    /// old→new pairs are collected first and applied in one lookup, so a pin
    /// that lands on another pin's old point never drags that pin's wires
    /// along twice.
    fn retarget_pins(&mut self, idx: usize, to_new: impl Fn(GridPt) -> GridPt) {
        let c = &mut self.components[idx];
        let moves: HashMap<GridPt, GridPt> = c
            .pin_points()
            .into_iter()
            .map(|old| (old, to_new(old)))
            .filter(|(old, new)| old != new)
            .collect();
        c.p1 = to_new(c.p1);
        c.p2 = to_new(c.p2);
        if let Some(part) = c.part.as_mut() {
            for pt in part.extra.iter_mut() {
                *pt = to_new(*pt);
            }
        }
        if moves.is_empty() {
            return;
        }
        for w in self.wires.iter_mut() {
            if let Some(&new) = moves.get(&w.p1) {
                w.p1 = new;
            }
            if let Some(&new) = moves.get(&w.p2) {
                w.p2 = new;
            }
        }
        // A label sitting on a pin names that pin's net: it travels with it.
        for l in self.labels.iter_mut() {
            if let Some(&new) = moves.get(&l.at) {
                l.at = new;
            }
        }
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
    /// are pins, not directed elements, so both points coincide). The Part
    /// tool places its armed library symbol, carrying the symbol's default
    /// model/subckt reference and spiceline.
    pub fn place_at(&mut self, at: GridPt) {
        match self.tool {
            Tool::Gnd => {
                self.components.push(Component::basic(CompKind::Ground, at, at, ""));
                return;
            }
            Tool::Part => {
                // A symbol that vanished from the library (or was never
                // armed) places nothing; `arm_part` is where it is refused.
                let Some(symbol) = self.armed_part.as_deref().and_then(|n| self.symbols.get(n)) else {
                    return;
                };
                let rot = self.current_dir_turns();
                self.components.push(Component::from_symbol(symbol, at, rot));
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
        self.components.push(Component::basic(kind, at, GridPt(at.0 + dx, at.1 + dy), val));
    }

    /// The placement facing as quarter turns of a symbol's authored frame
    /// (authored facing right, so Right is no turn).
    pub fn current_dir_turns(&self) -> u8 {
        match self.current_dir {
            Direction::Right => 0,
            Direction::Down => 1,
            Direction::Left => 2,
            Direction::Up => 3,
        }
    }

    /// Arm the Part tool with library symbol `name`: the picker's OK. An
    /// unknown name is refused here, not at the click.
    pub fn arm_part(&mut self, name: &str) -> Result<(), String> {
        let symbol = self
            .symbols
            .get(name)
            .ok_or_else(|| format!("no symbol named '{name}' in the library"))?;
        self.armed_part = Some(symbol.name.clone());
        self.tool = Tool::Part;
        Ok(())
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
            .flat_map(|c| c.pin_points())
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
            if c.kind == CompKind::Ground { continue; }

            // A library part is a free-text card (see `part_cards`), not a
            // typed element — but its pins still own nets: registering them
            // here keeps their numbers and label names in this circuit.
            if c.kind == CompKind::Part {
                for pin in c.pin_points() {
                    circuit.ensure_node(assignment.node_of(pin));
                }
                continue;
            }

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
                CompKind::Ground | CompKind::Part => unreachable!(),
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

    /// The SPICE cards of the placed library parts, in component order, plus
    /// the `.include` lines their symbols' libraries need (each file once,
    /// and not when a directive on the canvas already includes it).
    ///
    /// A part is one card — `<name> <pin nets…> <model|subckt> <spiceline>` —
    /// written from the symbol, with the nets spelled exactly as the typed
    /// elements spell them (`netlist`'s node names: labels, else numbers).
    pub fn part_cards(&self, netlist: &Netlist) -> Result<(Vec<String>, Vec<String>), String> {
        let assignment = NodeAssignment::of(&self.components, &self.wires, &self.labels);
        let names = instance_names(&self.components);
        let mut seen: HashSet<String> = HashSet::new();
        for part in self.components.iter().filter_map(|c| c.part.as_ref()) {
            if !part.name.is_empty() && !seen.insert(part.name.to_ascii_lowercase()) {
                return Err(format!("instance name '{}' is used by two parts", part.name));
            }
        }

        let (mut includes, mut cards): (Vec<String>, Vec<String>) = (Vec::new(), Vec::new());
        for (c, name) in self.components.iter().zip(&names) {
            let Some(part) = c.part.as_ref() else { continue };
            let symbol = self.symbols.get(&part.symbol).ok_or_else(|| {
                format!("{name}: symbol '{}' is not in the library", part.symbol)
            })?;
            let nets: Vec<String> = c
                .pin_points()
                .into_iter()
                .map(|pin| match assignment.node_of(pin) {
                    0 => "0".to_string(),
                    node => netlist.circuit.node_name(node).to_string(),
                })
                .collect();
            cards.push(symbol.card(name, &nets, &c.val, &part.spiceline).map_err(|e| match e {
                circuitsim::SimError::Circuit(msg) | circuitsim::SimError::Parse(msg) => msg,
                other => other.to_string(),
            })?);
            if let Some(lib) = &symbol.lib {
                let already = |l: &String| l.eq_ignore_ascii_case(lib);
                let on_canvas = self.directives.iter().any(|d| {
                    d.text.lines().any(|line| {
                        let mut words = line.split_whitespace();
                        matches!(words.next(), Some(w) if w.eq_ignore_ascii_case(".include") || w.eq_ignore_ascii_case(".lib"))
                            && words.next().is_some_and(|f| f.trim_matches(['"', '\'']).eq_ignore_ascii_case(lib))
                    })
                });
                if !on_canvas && !includes.iter().any(already) {
                    includes.push(lib.clone());
                }
            }
        }
        Ok((includes.into_iter().map(|l| format!(".include {l}")).collect(), cards))
    }

    /// The deck text the schematic stands for: the canonical writer's output
    /// for the elements and the analysis fields, with the library parts'
    /// `.include` lines, every directive's text and the part cards spliced in
    /// verbatim ahead of the element cards (where `.param`, `.model` and
    /// `.subckt` conventionally sit; the deck grammar does not care about
    /// order, a reader does). This — not [`Self::build_netlist`] alone — is
    /// what the Generate Netlist command writes and what a run of the
    /// schematic simulates.
    pub fn deck_text(&self) -> Result<String, String> {
        let netlist = self.build_netlist()?;
        let mut text = netlist_emit(&netlist);
        let (includes, cards) = self.part_cards(&netlist)?;
        if self.directives.is_empty() && includes.is_empty() && cards.is_empty() {
            return Ok(text);
        }
        let mut block: String = includes.iter().map(|l| format!("{l}\n")).collect();
        for d in &self.directives {
            block.push_str(&format!("{}\n", d.text.trim()));
        }
        for card in &cards {
            block.push_str(&format!("{card}\n"));
        }
        // The writer opens with a `* Canonical netlist` comment line.
        let after_title = text.find('\n').map_or(text.len(), |i| i + 1);
        text.insert_str(after_title, &block);
        Ok(text)
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

            new_components.push(Component::basic(comp.kind, p1, p2, comp.val.clone()));

            // Horizontal wire: p1 -> spine of pin1_node
            if comp.node_a != 0 {
                let spine_x = ORIGIN_X + col_a * COL_W;
                if spine_x != comp_x {
                    new_wires.push(Wire { p1: GridPt(comp_x, y_top), p2: GridPt(spine_x, y_top) });
                }
                spine_pins.entry(comp.node_a).or_default().push(y_top);
            } else {
                // Ground pin for node A
                new_components.push(Component::basic(
                    CompKind::Ground, GridPt(comp_x, y_top), GridPt(comp_x, y_top), "",
                ));
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
                new_components.push(Component::basic(
                    CompKind::Ground,
                    GridPt(comp_x, y_top + COMP_LEN),
                    GridPt(comp_x, y_top + COMP_LEN),
                    "",
                ));
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

        // ── 4b. Net labels: every named net keeps its name ──────────────────
        // The label sits on the net's first spine point, a point the layout
        // above guarantees is on the net (a wire end or the pin itself).
        let mut new_labels: Vec<NetLabel> = Vec::new();
        for &node in &node_set {
            let name = circuit.node_name(node);
            if name.is_empty() || name == node.to_string() {
                continue;
            }
            let y = spine_pins[&node].iter().copied().min().unwrap_or(ORIGIN_Y);
            new_labels.push(NetLabel {
                at: GridPt(ORIGIN_X + node_to_col[&node] * COL_W, y),
                name: name.to_string(),
            });
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
            symbols: previous.symbols.clone(),
            armed_part: previous.armed_part.clone(),
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

        let n = parsed.len();
        let mut status = format!("Imported {} component(s) from netlist", n);
        if skipped > 0 {
            status.push_str(&format!(" ({} D/M/I element(s) not drawn)", skipped));
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

    fn comp(kind: CompKind, x: i32, y: i32, val: &str) -> Component {
        Component { kind, p1: GridPt(x, y), p2: GridPt(x + 3, y), val: val.to_string(), part: None }
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
            Component { kind: CompKind::Voltage, p1: GridPt(0, 0), p2: GridPt(0, 3), val: "5".into(), part: None },
            Component { kind: CompKind::Resistor, p1: GridPt(4, 0), p2: GridPt(4, 3), val: "1k".into(), part: None },
            Component { kind: CompKind::Ground, p1: GridPt(0, 3), p2: GridPt(0, 3), val: "".into(), part: None },
            Component { kind: CompKind::Ground, p1: GridPt(4, 3), p2: GridPt(4, 3), val: "".into(), part: None },
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
            Component { kind: CompKind::Resistor, p1: GridPt(0, 0), p2: GridPt(0, 3), val: "1k".into(), part: None },
            Component { kind: CompKind::Resistor, p1: GridPt(8, 0), p2: GridPt(8, 3), val: "2k".into(), part: None },
            Component { kind: CompKind::Ground, p1: GridPt(0, 3), p2: GridPt(0, 3), val: "".into(), part: None },
            Component { kind: CompKind::Ground, p1: GridPt(8, 3), p2: GridPt(8, 3), val: "".into(), part: None },
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

    // ── Library parts (symbol library + component picker) ───────────────────

    fn libs_options() -> circuitsim::netlist::ParseOptions {
        circuitsim::netlist::ParseOptions {
            deck_dir: None,
            search_paths: vec![std::path::PathBuf::from(concat!(env!("CARGO_MANIFEST_DIR"), "/../libs"))],
        }
    }

    /// An nmos placed at (10,10): D(10,10) G(8,12) S(10,14) B(12,12), source
    /// grounded and drain labeled `out`.
    fn schematic_with_nmos() -> SchematicState {
        let mut s = SchematicState::default();
        s.current_dir = Direction::Right; // the symbols' authored facing
        s.arm_part("nmos").unwrap();
        s.place_at(GridPt(10, 10));
        s.components.push(Component::basic(CompKind::Ground, GridPt(10, 14), GridPt(10, 14), ""));
        s.labels.push(NetLabel { at: GridPt(10, 10), name: "out".into() });
        s
    }

    #[test]
    fn placing_a_part_attaches_the_symbols_model_and_spiceline() {
        let s = schematic_with_nmos();
        let part = &s.components[0];
        assert!(part.kind == CompKind::Part);
        assert_eq!(part.val, "NMOS1", "the model reference starts from the symbol default");
        assert_eq!(part.part.as_ref().unwrap().spiceline, "W=10u L=1u");
        assert_eq!(part.pin_points(), [GridPt(10, 10), GridPt(8, 12), GridPt(10, 14), GridPt(12, 12)]);
    }

    #[test]
    fn a_placed_part_writes_its_card_with_label_and_ground_nets_and_the_include() {
        let mut s = schematic_with_nmos();
        s.components[0].part.as_mut().unwrap().name = "MM1".into();
        s.components[0].val = "NMOS1".into();
        s.components[0].part.as_mut().unwrap().spiceline = "W=4u L=0.5u".into();
        let deck = s.deck_text().unwrap();
        assert!(deck.contains(".include standard.lib\n"), "{deck}");
        let card = deck.lines().find(|l| l.starts_with("MM1 ")).unwrap_or_else(|| panic!("{deck}"));
        let t: Vec<&str> = card.split_whitespace().collect();
        assert_eq!((t[1], t[3]), ("out", "0"), "{card}");
        assert_eq!(&t[5..], ["NMOS1", "W=4u", "L=0.5u"], "{card}");
        // The deck the editor would run reads, and the model resolves.
        let parsed = circuitsim::netlist::parse_with_options(&deck, &libs_options()).unwrap_or_else(|e| panic!("{e}\n{deck}"));
        assert_eq!(parsed.circuit.mosfets.len(), 1);
    }

    #[test]
    fn the_library_include_is_written_once_and_not_when_a_directive_has_it() {
        let mut s = SchematicState::default();
        s.arm_part("diode").unwrap();
        s.place_at(GridPt(0, 0));
        s.place_at(GridPt(0, 4));
        let deck = s.deck_text().unwrap();
        assert_eq!(deck.matches(".include standard.lib").count(), 1, "{deck}");

        s.directives.push(Directive { at: GridPt(0, 8), text: ".include standard.lib".into() });
        let deck = s.deck_text().unwrap();
        assert_eq!(deck.matches(".include standard.lib").count(), 1, "the user's own include is enough: {deck}");
    }

    #[test]
    fn parts_number_per_prefix_and_a_chosen_name_is_never_reissued() {
        let mut s = SchematicState::default();
        s.arm_part("nmos").unwrap();
        s.place_at(GridPt(0, 0));
        s.place_at(GridPt(0, 8));
        s.place_at(GridPt(0, 16));
        s.arm_part("diode").unwrap();
        s.place_at(GridPt(20, 0));
        s.components[1].part.as_mut().unwrap().name = "M1".into();
        assert_eq!(instance_names(&s.components), ["M2", "M1", "M3", "D1"]);
    }

    #[test]
    fn two_parts_with_one_name_are_refused() {
        let mut s = SchematicState::default();
        s.arm_part("diode").unwrap();
        s.place_at(GridPt(0, 0));
        s.place_at(GridPt(0, 4));
        for c in &mut s.components {
            c.part.as_mut().unwrap().name = "D7".into();
        }
        assert!(s.deck_text().unwrap_err().contains("used by two parts"));
    }

    #[test]
    fn a_device_without_its_model_name_is_refused_not_written() {
        let mut s = SchematicState::default();
        s.arm_part("subckt2").unwrap();
        s.place_at(GridPt(0, 0));
        assert!(s.deck_text().unwrap_err().contains("needs a subckt name"));
        s.components[0].val = "lowpass".into();
        assert!(s.deck_text().unwrap().contains("X1 "));
    }

    #[test]
    fn attached_text_cannot_inject_a_second_card_through_the_editor() {
        let mut s = schematic_with_nmos();
        s.components[0].part.as_mut().unwrap().spiceline = "W=1u\nV9 1 0 DC 5".into();
        assert!(s.deck_text().is_err());
    }

    #[test]
    fn an_unknown_symbol_is_refused_when_armed_and_when_the_library_loses_it() {
        let mut s = SchematicState::default();
        assert!(s.arm_part("flux-capacitor").is_err());
        assert!(s.tool == Tool::Select);
        s.arm_part("diode").unwrap();
        s.place_at(GridPt(0, 0));
        s.symbols = SymbolLibrary::default();
        assert!(s.deck_text().unwrap_err().contains("not in the library"));
    }

    #[test]
    fn placement_facing_turns_the_symbol() {
        let mut s = SchematicState::default();
        s.arm_part("diode").unwrap();
        s.current_dir = Direction::Down;
        s.place_at(GridPt(5, 5));
        assert_eq!(s.components[0].pin_points(), [GridPt(5, 5), GridPt(5, 8)]);
        assert_eq!(s.components[0].part.as_ref().unwrap().rot, 1);
    }

    #[test]
    fn rotating_a_part_turns_every_pin_and_keeps_wires_on_them() {
        let mut s = schematic_with_nmos();
        s.components.pop(); // no ground: only the part and wires here
        s.labels.clear();
        // A wire on the gate pin and one on the bulk pin.
        s.wires.push(Wire { p1: GridPt(8, 12), p2: GridPt(4, 12) });
        s.wires.push(Wire { p1: GridPt(12, 12), p2: GridPt(16, 12) });
        s.selected_component = Some(0);
        assert!(s.rotate_selected());
        // (dx, dy) -> (-dy, dx) about the drain at (10,10).
        assert_eq!(s.components[0].pin_points(), [GridPt(10, 10), GridPt(8, 8), GridPt(6, 10), GridPt(8, 12)]);
        assert_eq!(s.components[0].part.as_ref().unwrap().rot, 1);
        assert_eq!(s.wires[0].p1, GridPt(8, 8), "the gate wire followed its pin");
        assert_eq!(s.wires[1].p1, GridPt(8, 12), "the bulk wire followed its pin");
        // Four turns come home.
        for _ in 0..3 {
            s.rotate_selected();
        }
        assert_eq!(s.components[0].pin_points(), [GridPt(10, 10), GridPt(8, 12), GridPt(10, 14), GridPt(12, 12)]);
        assert_eq!((s.wires[0].p1, s.wires[1].p1), (GridPt(8, 12), GridPt(12, 12)));
    }

    #[test]
    fn dragging_a_part_moves_every_pin_and_stretches_attached_wires() {
        let mut s = schematic_with_nmos();
        s.wires.push(Wire { p1: GridPt(10, 14), p2: GridPt(10, 20) });
        assert!(s.drag_component(0, 2, 1));
        assert_eq!(s.components[0].pin_points(), [GridPt(12, 11), GridPt(10, 13), GridPt(12, 15), GridPt(14, 13)]);
        assert_eq!(s.wires[0], Wire { p1: GridPt(12, 15), p2: GridPt(10, 20) });
    }

    #[test]
    fn a_part_only_net_still_gets_a_node_and_pins_join_wired_nets() {
        let mut s = SchematicState::default();
        s.current_dir = Direction::Right;
        s.arm_part("diode").unwrap();
        s.place_at(GridPt(0, 0)); // A(0,0) K(3,0)
        s.components.push(Component::basic(CompKind::Resistor, GridPt(3, 0), GridPt(6, 0), "1k"));
        let deck = s.deck_text().unwrap();
        // The diode's cathode and the resistor's first end are one net.
        let d: Vec<&str> = deck.lines().find(|l| l.starts_with("D1 ")).unwrap().split_whitespace().collect();
        let r: Vec<&str> = deck.lines().find(|l| l.starts_with("R1 ")).unwrap().split_whitespace().collect();
        assert_eq!(d[2], r[1], "{deck}");
    }
}
