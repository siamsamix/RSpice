//! The schematic's symbol library: one [`Spec`] per device family (pins,
//! card layout, default value) and the drawing of every symbol as plain data.
//!
//! **Frame.** A component is two stored points, `p1` and `p2`, and everything
//! else is derived from them. `u` is the unit grid step from `p1` toward `p2`,
//! `v` is `u` turned 90° (the same `(x, y) → (-y, x)` the Rotate command
//! applies), and a symbol-local point `(a, b)` sits at `p1 + a·u + b·v`.
//! Pins beyond `p1`/`p2` and every drawn line are given in `(a, b)` cells, so
//! rotating `p2` about `p1` carries the whole symbol rigidly — no per-kind
//! rotation code, and no mirroring.
//!
//! Symbols are [`Prim`] lists rather than painter calls so the same data
//! drives the canvas, the hit-test box and the unit tests that prove every
//! pin touches a lead.

use crate::schematic::{CompKind, Component, GridPt};
use egui::{Align2, Color32, FontId, Painter, Pos2, Rect, Shape, Stroke, Vec2};

/// A point in symbol-local cells: `a` along the `p1 → p2` axis, `b` across it.
pub type Local = (f32, f32);

/// One drawing primitive in symbol-local cells.
#[derive(Clone, Debug, PartialEq)]
pub enum Prim {
    Line(Local, Local),
    Poly { pts: Vec<Local>, closed: bool, fill: bool },
    Circle { c: Local, r: f32, fill: bool },
    /// Upright text centred on the point (polarity marks, a part letter).
    Text { at: Local, s: &'static str },
}

/// Everything the editor knows about one device family.
pub struct Spec {
    /// Picker / popup name.
    pub label: &'static str,
    /// SPICE element letter (`M`, `Q`, …) — the instance-name prefix.
    pub letter: char,
    /// Grid cells from `p1` to `p2` when freshly placed.
    pub axis_len: i32,
    /// Pins beyond `p1`/`p2`, in `(a, b)` cells.
    pub extra: &'static [(i32, i32)],
    /// Every pin's role, in pin order (`p1`, `p2`, then `extra`).
    pub pin_names: &'static [&'static str],
    /// Index into the pin list for each node slot of the element card.
    pub card_order: &'static [usize],
    /// Value a fresh part drops with.
    pub default_val: &'static str,
    /// What the value text means — shown in the part's edit popup.
    pub val_hint: &'static str,
}

const fn spec(
    label: &'static str,
    letter: char,
    axis_len: i32,
    extra: &'static [(i32, i32)],
    pin_names: &'static [&'static str],
    card_order: &'static [usize],
    default_val: &'static str,
    val_hint: &'static str,
) -> Spec {
    Spec { label, letter, axis_len, extra, pin_names, card_order, default_val, val_hint }
}

// Pin order is `[p1, p2, extra…]`.
static RESISTOR: Spec = spec("Resistor", 'R', 3, &[], &["1", "2"], &[0, 1], "1k", "resistance, e.g. 4.7k");
static CAPACITOR: Spec = spec("Capacitor", 'C', 3, &[], &["1", "2"], &[0, 1], "1u", "capacitance, e.g. 100n");
static INDUCTOR: Spec = spec("Inductor", 'L', 3, &[], &["1", "2"], &[0, 1], "1m", "inductance, e.g. 10u");
static VOLTAGE: Spec = spec("Voltage source", 'V', 3, &[], &["+", "-"], &[0, 1], "DC 5", "DC 5, AC 1, PULSE(…), SIN(…)");
static GROUND: Spec = spec("Ground", ' ', 0, &[], &["gnd"], &[0], "", "");

static DIODE: Spec = spec("Diode", 'D', 3, &[], &["anode", "cathode"], &[0, 1], "D", "model [AREA=x]");
static CURRENT: Spec = spec("Current source", 'I', 3, &[], &["+", "-"], &[0, 1], "DC 1m", "DC 1m, AC 1, PULSE(…), SIN(…)");
static NMOS: Spec = spec("N-channel MOSFET", 'M', 4, &[(2, 2)], &["drain", "source", "gate"], &[0, 2, 1, 1], "NMOS W=10u L=1u", "model [W=… L=…] — bulk is tied to the source");
static PMOS: Spec = spec("P-channel MOSFET", 'M', 4, &[(2, 2)], &["source", "drain", "gate"], &[1, 2, 0, 0], "PMOS W=10u L=1u", "model [W=… L=…] — bulk is tied to the source");
static NPN: Spec = spec("NPN transistor", 'Q', 4, &[(2, 2)], &["collector", "emitter", "base"], &[0, 2, 1], "NPN", "model [AREA=x]");
static PNP: Spec = spec("PNP transistor", 'Q', 4, &[(2, 2)], &["emitter", "collector", "base"], &[1, 2, 0], "PNP", "model [AREA=x]");
static NJF: Spec = spec("N-channel JFET", 'J', 4, &[(2, 2)], &["drain", "source", "gate"], &[0, 2, 1], "NJF", "model [AREA=x]");
static PJF: Spec = spec("P-channel JFET", 'J', 4, &[(2, 2)], &["drain", "source", "gate"], &[0, 2, 1], "PJF", "model [AREA=x]");
static VCVS: Spec = spec("Voltage-controlled voltage source (E)", 'E', 4, &[(1, 3), (3, 3)], &["out+", "out-", "ctrl+", "ctrl-"], &[0, 1, 2, 3], "1", "gain, e.g. 10");
static VCCS: Spec = spec("Voltage-controlled current source (G)", 'G', 4, &[(1, 3), (3, 3)], &["out+", "out-", "ctrl+", "ctrl-"], &[0, 1, 2, 3], "1m", "transconductance, e.g. 1m");
static CCVS: Spec = spec("Current-controlled voltage source (H)", 'H', 3, &[], &["out+", "out-"], &[0, 1], "V1 1k", "sensing source and transresistance: V1 1k");
static CCCS: Spec = spec("Current-controlled current source (F)", 'F', 3, &[], &["out+", "out-"], &[0, 1], "V1 1", "sensing source and gain: V1 2");
static BEHAVIORAL: Spec = spec("Behavioral source (B)", 'B', 3, &[], &["+", "-"], &[0, 1], "V=1", "V=<expr> or I=<expr>, e.g. V=V(a)*2");
static SWITCH: Spec = spec("Voltage-controlled switch (S)", 'S', 4, &[(1, 3), (3, 3)], &["n1", "n2", "ctrl+", "ctrl-"], &[0, 1, 2, 3], "SW", "model [ON|OFF]");
static CSWITCH: Spec = spec("Current-controlled switch (W)", 'W', 3, &[], &["n1", "n2"], &[0, 1], "V1 SW", "sensing source and model: V1 SW");
static TRANSFORMER: Spec = spec("Transformer (coupled inductors)", 'K', 6, &[(1, 4), (5, 4)], &["P+", "P-", "S+", "S-"], &[0, 1, 2, 3], "Lp=10m Ls=10m k=0.99", "Lp=… Ls=… k=…");
static TLINE: Spec = spec("Transmission line (T)", 'T', 4, &[(1, 4), (3, 4)], &["port1+", "port1-", "port2+", "port2-"], &[0, 1, 2, 3], "Z0=50 TD=1n", "Z0=… TD=… (delay) or F=… NL=…");
static OPAMP: Spec = spec("Op-amp (U)", 'U', 4, &[(2, -4), (3, -3)], &["in+", "in-", "out+", "out-"], &[2, 3, 0, 1], "", "optional model name");

/// The part picker's order — every device the editor can place, by family.
pub const PICKER: &[CompKind] = &[
    CompKind::Diode,
    CompKind::Nmos,
    CompKind::Pmos,
    CompKind::Npn,
    CompKind::Pnp,
    CompKind::Njf,
    CompKind::Pjf,
    CompKind::Current,
    CompKind::Vcvs,
    CompKind::Vccs,
    CompKind::Ccvs,
    CompKind::Cccs,
    CompKind::Behavioral,
    CompKind::Switch,
    CompKind::CSwitch,
    CompKind::Transformer,
    CompKind::Tline,
    CompKind::Opamp,
];

pub fn spec_of(kind: CompKind) -> &'static Spec {
    match kind {
        CompKind::Resistor => &RESISTOR,
        CompKind::Capacitor => &CAPACITOR,
        CompKind::Inductor => &INDUCTOR,
        CompKind::Voltage => &VOLTAGE,
        CompKind::Ground => &GROUND,
        CompKind::Diode => &DIODE,
        CompKind::Current => &CURRENT,
        CompKind::Nmos => &NMOS,
        CompKind::Pmos => &PMOS,
        CompKind::Npn => &NPN,
        CompKind::Pnp => &PNP,
        CompKind::Njf => &NJF,
        CompKind::Pjf => &PJF,
        CompKind::Vcvs => &VCVS,
        CompKind::Vccs => &VCCS,
        CompKind::Ccvs => &CCVS,
        CompKind::Cccs => &CCCS,
        CompKind::Behavioral => &BEHAVIORAL,
        CompKind::Switch => &SWITCH,
        CompKind::CSwitch => &CSWITCH,
        CompKind::Transformer => &TRANSFORMER,
        CompKind::Tline => &TLINE,
        CompKind::Opamp => &OPAMP,
    }
}

// ─── Frame and pins ─────────────────────────────────────────────────────────

/// The unit steps `(u, v)` of a component's frame. A degenerate component
/// (`p1 == p2`) faces down, like the placement default.
pub fn frame(p1: GridPt, p2: GridPt) -> ((i32, i32), (i32, i32)) {
    let u = ((p2.0 - p1.0).signum(), (p2.1 - p1.1).signum());
    let u = if u == (0, 0) { (0, 1) } else { u };
    (u, (-u.1, u.0))
}

/// Where a symbol-local cell lands on the grid.
pub fn at(p1: GridPt, p2: GridPt, a: i32, b: i32) -> GridPt {
    let (u, v) = frame(p1, p2);
    GridPt(p1.0 + a * u.0 + b * v.0, p1.1 + a * u.1 + b * v.1)
}

/// Every pin of a component, in the spec's pin order. A ground is one pin.
pub fn pins(kind: CompKind, p1: GridPt, p2: GridPt) -> Vec<GridPt> {
    if kind == CompKind::Ground {
        return vec![p1];
    }
    let mut out = vec![p1, p2];
    out.extend(spec_of(kind).extra.iter().map(|&(a, b)| at(p1, p2, a, b)));
    out
}

// ─── Value grammar helpers ──────────────────────────────────────────────────

/// The model-card name a part's value text refers to, if its family has one.
pub fn model_token(kind: CompKind, val: &str) -> Option<&str> {
    let mut words = val.split_whitespace();
    match kind {
        CompKind::Diode
        | CompKind::Nmos
        | CompKind::Pmos
        | CompKind::Npn
        | CompKind::Pnp
        | CompKind::Njf
        | CompKind::Pjf
        | CompKind::Switch => words.next(),
        CompKind::CSwitch => words.nth(1),
        _ => None,
    }
    .filter(|w| !w.contains('='))
}

/// The stock `.model` card for a model name the editor hands out by default
/// (`D`, `NMOS`, `NPN`, …), so a freshly placed part simulates without a
/// hand-written card — the same convenience the reference tools give. Only
/// emitted when no directive already defines the name.
pub fn default_model_card(name: &str) -> Option<&'static str> {
    Some(match name.to_ascii_lowercase().as_str() {
        "d" => ".model D D",
        "nmos" => ".model NMOS NMOS",
        "pmos" => ".model PMOS PMOS",
        "npn" => ".model NPN NPN",
        "pnp" => ".model PNP PNP",
        "njf" => ".model NJF NJF",
        "pjf" => ".model PJF PJF",
        "sw" => ".model SW SW",
        _ => return None,
    })
}

// ─── Symbol drawings ────────────────────────────────────────────────────────

fn line(a: Local, b: Local) -> Prim {
    Prim::Line(a, b)
}

fn poly(pts: &[Local]) -> Prim {
    Prim::Poly { pts: pts.to_vec(), closed: false, fill: false }
}

fn closed(pts: &[Local]) -> Prim {
    Prim::Poly { pts: pts.to_vec(), closed: true, fill: false }
}

fn circle(c: Local, r: f32) -> Prim {
    Prim::Circle { c, r, fill: false }
}

fn dot(c: Local) -> Prim {
    Prim::Circle { c, r: 0.09, fill: true }
}

/// A filled arrowhead whose tip is `tip`, pointing along `dir`.
fn arrow(tip: Local, dir: Local) -> Prim {
    let len = (dir.0 * dir.0 + dir.1 * dir.1).sqrt();
    let d = (dir.0 / len, dir.1 / len);
    let n = (-d.1, d.0);
    let base = (tip.0 - d.0 * 0.32, tip.1 - d.1 * 0.32);
    Prim::Poly {
        pts: vec![tip, (base.0 + n.0 * 0.15, base.1 + n.1 * 0.15), (base.0 - n.0 * 0.15, base.1 - n.1 * 0.15)],
        closed: true,
        fill: true,
    }
}

/// Mirror a symbol along its axis (`a → len − a`), used to turn the N-type
/// drawings into their P-type twins with the source/emitter on top.
fn flip(prims: Vec<Prim>, len: f32) -> Vec<Prim> {
    let m = |p: Local| (len - p.0, p.1);
    prims
        .into_iter()
        .map(|p| match p {
            Prim::Line(a, b) => Prim::Line(m(a), m(b)),
            Prim::Poly { pts, closed, fill } => Prim::Poly { pts: pts.into_iter().map(m).collect(), closed, fill },
            Prim::Circle { c, r, fill } => Prim::Circle { c: m(c), r, fill },
            Prim::Text { at, s } => Prim::Text { at: m(at), s },
        })
        .collect()
}

fn mosfet(p_channel: bool) -> Vec<Prim> {
    let mut v = vec![
        poly(&[(0.0, 0.0), (0.8, 0.0), (0.8, 0.5)]),
        line((0.8, 0.5), (1.4, 0.5)),
        line((1.6, 0.5), (2.4, 0.5)),
        line((2.6, 0.5), (3.2, 0.5)),
        poly(&[(3.2, 0.5), (3.2, 0.0), (4.0, 0.0)]),
        line((0.8, 1.0), (3.2, 1.0)),
        line((2.0, 1.0), (2.0, 2.0)),
        // Body, tied to the source: the 3-pin symbol has no bulk pin.
        line((2.0, 0.5), (2.0, 0.0)),
        line((2.0, 0.0), (3.2, 0.0)),
    ];
    // The body arrow points into the channel for N, out of it for P.
    v.push(if p_channel { arrow((2.0, 0.0), (0.0, -1.0)) } else { arrow((2.0, 0.5), (0.0, 1.0)) });
    if p_channel {
        flip(v, 4.0)
    } else {
        v
    }
}

fn bjt(pnp: bool) -> Vec<Prim> {
    let mut v = vec![
        line((0.6, 1.0), (3.4, 1.0)),
        line((2.0, 1.0), (2.0, 2.0)),
        poly(&[(0.0, 0.0), (0.6, 0.0), (1.4, 1.0)]),
        poly(&[(4.0, 0.0), (3.4, 0.0), (2.6, 1.0)]),
    ];
    // Emitter arrow: out of the base for NPN, into it for PNP.
    v.push(if pnp { arrow((2.84, 0.7), (-0.8, 1.0)) } else { arrow((3.28, 0.15), (0.8, -1.0)) });
    if pnp {
        flip(v, 4.0)
    } else {
        v
    }
}

fn jfet(p_channel: bool) -> Vec<Prim> {
    vec![
        line((0.5, 0.8), (3.5, 0.8)),
        poly(&[(0.0, 0.0), (0.5, 0.0), (0.5, 0.8)]),
        poly(&[(4.0, 0.0), (3.5, 0.0), (3.5, 0.8)]),
        line((2.0, 2.0), (2.0, 0.8)),
        if p_channel { arrow((2.0, 1.5), (0.0, 1.0)) } else { arrow((2.0, 0.8), (0.0, -1.0)) },
    ]
}

/// The diamond every dependent source wears, centred at `mid` on the axis.
fn diamond(mid: f32, half_a: f32, half_b: f32) -> Prim {
    closed(&[(mid - half_a, 0.0), (mid, half_b), (mid + half_a, 0.0), (mid, -half_b)])
}

fn controlled_voltage_4() -> Vec<Prim> {
    vec![
        line((0.0, 0.0), (0.4, 0.0)),
        line((3.6, 0.0), (4.0, 0.0)),
        diamond(2.0, 1.6, 1.2),
        Prim::Text { at: (1.1, 0.0), s: "+" },
        Prim::Text { at: (2.9, 0.0), s: "-" },
        line((1.0, 3.0), (1.0, 2.0)),
        line((3.0, 3.0), (3.0, 2.0)),
        Prim::Text { at: (0.45, 2.4), s: "+" },
        Prim::Text { at: (3.55, 2.4), s: "-" },
    ]
}

fn controlled_current_4() -> Vec<Prim> {
    let mut v = controlled_voltage_4();
    v.retain(|p| !matches!(p, Prim::Text { at, .. } if at.1 == 0.0));
    v.push(line((1.2, 0.0), (2.8, 0.0)));
    v.push(arrow((2.8, 0.0), (1.0, 0.0)));
    v
}

fn switch_blade(x0: f32, x1: f32) -> Vec<Prim> {
    vec![
        circle((x0, 0.0), 0.1),
        circle((x1, 0.0), 0.1),
        line((x0, 0.0), (x1 - 0.2, 0.85)),
    ]
}

/// The symbol of `kind` for an axis of `len` cells. Two-pin bodies are
/// centred on the axis so an imported part (axis 4) and a placed one (axis 3)
/// share one drawing; the multi-pin symbols have a fixed axis.
pub fn prims(kind: CompKind, len: f32) -> Vec<Prim> {
    let mid = len / 2.0;
    match kind {
        CompKind::Diode => vec![
            line((0.0, 0.0), (mid - 0.5, 0.0)),
            line((mid + 0.5, 0.0), (len, 0.0)),
            Prim::Poly {
                pts: vec![(mid - 0.5, -0.5), (mid - 0.5, 0.5), (mid + 0.5, 0.0)],
                closed: true,
                fill: false,
            },
            line((mid + 0.5, -0.5), (mid + 0.5, 0.5)),
        ],
        CompKind::Current => vec![
            line((0.0, 0.0), (mid - 0.55, 0.0)),
            line((mid + 0.55, 0.0), (len, 0.0)),
            circle((mid, 0.0), 0.55),
            line((mid - 0.35, 0.0), (mid + 0.3, 0.0)),
            arrow((mid + 0.38, 0.0), (1.0, 0.0)),
        ],
        CompKind::Ccvs => vec![
            line((0.0, 0.0), (mid - 0.8, 0.0)),
            line((mid + 0.8, 0.0), (len, 0.0)),
            diamond(mid, 0.8, 0.8),
            Prim::Text { at: (mid - 0.38, 0.0), s: "+" },
            Prim::Text { at: (mid + 0.38, 0.0), s: "-" },
        ],
        CompKind::Cccs => vec![
            line((0.0, 0.0), (mid - 0.8, 0.0)),
            line((mid + 0.8, 0.0), (len, 0.0)),
            diamond(mid, 0.8, 0.8),
            line((mid - 0.4, 0.0), (mid + 0.3, 0.0)),
            arrow((mid + 0.4, 0.0), (1.0, 0.0)),
        ],
        CompKind::Behavioral => vec![
            line((0.0, 0.0), (mid - 0.8, 0.0)),
            line((mid + 0.8, 0.0), (len, 0.0)),
            diamond(mid, 0.8, 0.8),
            Prim::Text { at: (mid, 0.0), s: "B" },
        ],
        CompKind::CSwitch => {
            let mut v = vec![line((0.0, 0.0), (mid - 0.9, 0.0)), line((mid + 0.9, 0.0), (len, 0.0))];
            v.extend(switch_blade(mid - 0.9, mid + 0.9));
            v
        }
        CompKind::Nmos => mosfet(false),
        CompKind::Pmos => mosfet(true),
        CompKind::Npn => bjt(false),
        CompKind::Pnp => bjt(true),
        CompKind::Njf => jfet(false),
        CompKind::Pjf => jfet(true),
        CompKind::Vcvs => controlled_voltage_4(),
        CompKind::Vccs => controlled_current_4(),
        CompKind::Switch => {
            let mut v = vec![line((0.0, 0.0), (1.1, 0.0)), line((2.9, 0.0), (4.0, 0.0))];
            v.extend(switch_blade(1.1, 2.9));
            v.extend([
                line((1.0, 3.0), (1.0, 2.2)),
                line((3.0, 3.0), (3.0, 2.2)),
                Prim::Text { at: (0.45, 2.6), s: "+" },
                Prim::Text { at: (3.55, 2.6), s: "-" },
            ]);
            v
        }
        CompKind::Transformer => {
            let coil = |b0: f32, sign: f32| -> Prim {
                let mut pts = Vec::new();
                for k in 0..4 {
                    for t in 0..=8 {
                        let th = std::f32::consts::PI * t as f32 / 8.0;
                        pts.push((1.0 + k as f32 + 0.5 - 0.5 * th.cos(), b0 + sign * 0.5 * th.sin()));
                    }
                }
                Prim::Poly { pts, closed: false, fill: false }
            };
            vec![
                line((0.0, 0.0), (1.0, 0.0)),
                line((5.0, 0.0), (6.0, 0.0)),
                coil(0.0, 1.0),
                coil(4.0, -1.0),
                line((1.0, 1.85), (5.0, 1.85)),
                line((1.0, 2.15), (5.0, 2.15)),
                dot((0.55, 0.45)),
                dot((0.55, 3.55)),
            ]
        }
        CompKind::Tline => vec![
            poly(&[(0.0, 0.0), (0.5, 0.0), (0.5, 1.0)]),
            poly(&[(4.0, 0.0), (3.5, 0.0), (3.5, 1.0)]),
            line((1.0, 4.0), (1.0, 3.0)),
            line((3.0, 4.0), (3.0, 3.0)),
            closed(&[(0.5, 1.0), (3.5, 1.0), (3.5, 3.0), (0.5, 3.0)]),
            Prim::Text { at: (2.0, 2.0), s: "T" },
        ],
        CompKind::Opamp => vec![
            closed(&[(-1.0, -1.0), (5.0, -1.0), (2.0, -4.0)]),
            line((0.0, 0.0), (0.0, -1.0)),
            line((4.0, 0.0), (4.0, -1.0)),
            Prim::Text { at: (0.0, -1.75), s: "+" },
            Prim::Text { at: (4.0, -1.75), s: "-" },
        ],
        // Drawn by the canvas's own legacy painter.
        CompKind::Resistor | CompKind::Capacitor | CompKind::Inductor | CompKind::Voltage | CompKind::Ground => {
            Vec::new()
        }
    }
}

/// Whether the canvas paints this kind with the symbol library (everything
/// but the original R / C / L / V / ground, which keep their hand painters).
pub fn is_library_kind(kind: CompKind) -> bool {
    !matches!(
        kind,
        CompKind::Resistor | CompKind::Capacitor | CompKind::Inductor | CompKind::Voltage | CompKind::Ground
    )
}

/// The drawing's line segments in local cells (polygons opened out, circles
/// boxed) — what the tests check pins against.
#[cfg(test)]
fn segments(prims: &[Prim]) -> Vec<(Local, Local)> {
    let mut out = Vec::new();
    for p in prims {
        match p {
            Prim::Line(a, b) => out.push((*a, *b)),
            Prim::Poly { pts, closed, .. } => {
                out.extend(pts.windows(2).map(|w| (w[0], w[1])));
                if *closed && pts.len() > 2 {
                    out.push((pts[pts.len() - 1], pts[0]));
                }
            }
            Prim::Circle { c, r, .. } => {
                let (x0, y0, x1, y1) = (c.0 - r, c.1 - r, c.0 + r, c.1 + r);
                out.extend([((x0, y0), (x1, y0)), ((x1, y0), (x1, y1)), ((x1, y1), (x0, y1)), ((x0, y1), (x0, y0))]);
            }
            Prim::Text { .. } => {}
        }
    }
    out
}

/// Local bounding box `(min, max)` of a drawing and its pins.
pub fn local_bounds(kind: CompKind, len: f32) -> (Local, Local) {
    let mut lo = (0.0f32, 0.0f32);
    let mut hi = (len, 0.0f32);
    let mut take = |p: Local| {
        lo = (lo.0.min(p.0), lo.1.min(p.1));
        hi = (hi.0.max(p.0), hi.1.max(p.1));
    };
    for p in prims(kind, len) {
        match p {
            Prim::Line(a, b) => {
                take(a);
                take(b);
            }
            Prim::Poly { pts, .. } => pts.into_iter().for_each(&mut take),
            Prim::Circle { c, r, .. } => {
                take((c.0 - r, c.1 - r));
                take((c.0 + r, c.1 + r));
            }
            Prim::Text { at, .. } => take(at),
        }
    }
    for &(a, b) in spec_of(kind).extra {
        take((a as f32, b as f32));
    }
    (lo, hi)
}

// ─── Painting ───────────────────────────────────────────────────────────────

/// Draw a library-kind component and return its screen bounding box (used to
/// park the name / value labels beside the symbol).
pub fn paint(
    painter: &Painter,
    c: &Component,
    to_screen: &dyn Fn(GridPt) -> Pos2,
    stroke: Stroke,
    ink: Color32,
) -> Rect {
    let (u, v) = frame(c.p1, c.p2);
    let len = (((c.p2.0 - c.p1.0).abs() + (c.p2.1 - c.p1.1).abs()) as f32).max(1.0);
    let origin = to_screen(c.p1);
    let su = to_screen(GridPt(c.p1.0 + u.0, c.p1.1 + u.1)) - origin;
    let sv = to_screen(GridPt(c.p1.0 + v.0, c.p1.1 + v.1)) - origin;
    let map = |l: Local| -> Pos2 { origin + su * l.0 + sv * l.1 };
    let cell = su.length();
    let font = FontId::monospace((cell * 0.6).clamp(7.0, 16.0));

    let mut bounds = Rect::NOTHING;
    for p in prims(c.kind, len) {
        match p {
            Prim::Line(a, b) => {
                let (a, b) = (map(a), map(b));
                painter.line_segment([a, b], stroke);
                bounds.extend_with(a);
                bounds.extend_with(b);
            }
            Prim::Poly { pts, closed, fill } => {
                let pts: Vec<Pos2> = pts.into_iter().map(map).collect();
                pts.iter().for_each(|&p| bounds.extend_with(p));
                if fill {
                    painter.add(Shape::convex_polygon(pts, ink, Stroke::NONE));
                } else if closed {
                    painter.add(Shape::closed_line(pts, stroke));
                } else {
                    painter.add(Shape::line(pts, stroke));
                }
            }
            Prim::Circle { c: centre, r, fill } => {
                let ctr = map(centre);
                let radius = r * cell;
                bounds.extend_with(ctr + Vec2::splat(radius));
                bounds.extend_with(ctr - Vec2::splat(radius));
                painter.circle(ctr, radius, if fill { ink } else { Color32::TRANSPARENT }, stroke);
            }
            Prim::Text { at, s } => {
                painter.text(map(at), Align2::CENTER_CENTER, s, font.clone(), ink);
            }
        }
    }
    for pin in pins(c.kind, c.p1, c.p2) {
        bounds.extend_with(to_screen(pin));
    }
    bounds
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::schematic::Direction;
    use std::collections::HashSet;

    const LIBRARY: &[CompKind] = &[
        CompKind::Diode, CompKind::Current, CompKind::Nmos, CompKind::Pmos, CompKind::Npn,
        CompKind::Pnp, CompKind::Njf, CompKind::Pjf, CompKind::Vcvs, CompKind::Vccs,
        CompKind::Ccvs, CompKind::Cccs, CompKind::Behavioral, CompKind::Switch,
        CompKind::CSwitch, CompKind::Transformer, CompKind::Tline, CompKind::Opamp,
    ];

    fn dist_to_segment(p: Local, a: Local, b: Local) -> f32 {
        let (abx, aby) = (b.0 - a.0, b.1 - a.1);
        let len2 = abx * abx + aby * aby;
        let t = if len2 == 0.0 { 0.0 } else { (((p.0 - a.0) * abx + (p.1 - a.1) * aby) / len2).clamp(0.0, 1.0) };
        ((p.0 - (a.0 + t * abx)).powi(2) + (p.1 - (a.1 + t * aby)).powi(2)).sqrt()
    }

    #[test]
    fn every_pin_sits_on_its_symbol() {
        for &kind in LIBRARY {
            let s = spec_of(kind);
            let len = s.axis_len as f32;
            let drawing = segments(&prims(kind, len));
            let mut local_pins: Vec<Local> = vec![(0.0, 0.0), (len, 0.0)];
            local_pins.extend(s.extra.iter().map(|&(a, b)| (a as f32, b as f32)));
            assert_eq!(local_pins.len(), s.pin_names.len(), "{}: pin names", s.label);
            for pin in local_pins {
                assert!(
                    drawing.iter().any(|&(a, b)| dist_to_segment(pin, a, b) < 1e-3),
                    "{}: pin {pin:?} touches no lead",
                    s.label
                );
            }
        }
    }

    #[test]
    fn two_pin_symbols_reach_both_pins_at_any_axis_length() {
        for kind in [CompKind::Diode, CompKind::Current, CompKind::Ccvs, CompKind::Cccs, CompKind::Behavioral, CompKind::CSwitch] {
            for len in [3.0f32, 4.0] {
                let drawing = segments(&prims(kind, len));
                for pin in [(0.0, 0.0), (len, 0.0)] {
                    assert!(drawing.iter().any(|&(a, b)| dist_to_segment(pin, a, b) < 1e-3), "{kind:?} len {len}");
                }
            }
        }
    }

    #[test]
    fn card_slots_index_real_pins_and_cover_every_pin() {
        for &kind in LIBRARY {
            let s = spec_of(kind);
            assert!(s.card_order.iter().all(|&i| i < s.pin_names.len()), "{}", s.label);
            for i in 0..s.pin_names.len() {
                assert!(s.card_order.contains(&i), "{}: pin {i} never reaches the card", s.label);
            }
        }
    }

    #[test]
    fn pins_stay_distinct_through_every_rotation() {
        for &kind in LIBRARY {
            let len = spec_of(kind).axis_len;
            for dir in [Direction::Down, Direction::Left, Direction::Up, Direction::Right] {
                let (dx, dy) = dir.offset(len);
                let ps = pins(kind, GridPt(5, 5), GridPt(5 + dx, 5 + dy));
                let unique: HashSet<_> = ps.iter().copied().collect();
                assert_eq!(unique.len(), ps.len(), "{kind:?} {dir:?}");
            }
        }
    }

    #[test]
    fn rotating_p2_about_p1_carries_every_pin_rigidly() {
        // The Rotate command only moves p2; extra pins must follow as the
        // same quarter turn about p1.
        for &kind in LIBRARY {
            let len = spec_of(kind).axis_len;
            let p1 = GridPt(10, 10);
            let p2 = GridPt(10, 10 + len);
            let before = pins(kind, p1, p2);
            let (dx, dy) = (p2.0 - p1.0, p2.1 - p1.1);
            let turned = GridPt(p1.0 - dy, p1.1 + dx);
            let after = pins(kind, p1, turned);
            for (old, new) in before.iter().zip(&after) {
                let (rx, ry) = (old.0 - p1.0, old.1 - p1.1);
                assert_eq!(*new, GridPt(p1.0 - ry, p1.1 + rx), "{kind:?}");
            }
        }
    }

    #[test]
    fn model_tokens_follow_each_cards_grammar() {
        assert_eq!(model_token(CompKind::Nmos, "NMOS W=1u L=1u"), Some("NMOS"));
        assert_eq!(model_token(CompKind::CSwitch, "V1 SW"), Some("SW"));
        assert_eq!(model_token(CompKind::Opamp, ""), None);
        assert_eq!(model_token(CompKind::Diode, "AREA=2"), None);
        assert_eq!(model_token(CompKind::Vcvs, "10"), None);
        assert!(default_model_card("nmos").is_some());
        assert!(default_model_card("D1N4148").is_none());
    }
}
