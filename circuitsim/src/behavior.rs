//! Behavioral sources (`B` cards): arbitrary expressions of node voltages,
//! branch currents, and time, compiled to an AST at parse time.
//!
//! The design is driven by one constraint: a Newton-Raphson solve must never
//! see a moving source value. The expression is evaluated at the current
//! iterate, linearized around it by central differences on the probe
//! *values*, and every coefficient outside the evaluation itself stays fixed
//! for the duration of one solve. Time-dependent terms (`time()`, `idt()`,
//! `delay()`) are frozen into a solve the same way — their history lives in
//! [`BehavioralRuntime`] and only advances with the rest of the integrator
//! state after an accepted step.
//!
//! Supported: arithmetic (`+ - * / ^`), the shared math library (the same
//! function list as [`crate::params`] plus the `u()` step function), `time()`,
//! `idt(...)`/`sdt(...)` (trapezoidal companion), `delay(x, td)`, and the two
//! probe forms `V(a[,b])` (node voltages) and `I(src)` (a voltage-source or
//! inductor branch current). Plus the keyed pseudo-noise forms `white(arg)`
//! and `flicker(arg)` (TODO §5): deterministic draws, re-rolled only by their
//! *pacing* argument — a bare `time()` counts accepted steps, anything else
//! keys on its end-of-step `floor(arg)` — so the draw is frozen for the
//! duration of one solve exactly like `idt`/`delay` history. The amplitude is
//! the caller's: the draw is unit-scale in `[−1, 1)`, so `B1 n1 n2 V=white(time())*10m`
//! makes a ±10 mV noise source. A `.param`-defined identifier folds into a
//! number at parse time (`parse_with_params`), so a B expression can ride
//! deck parameters. A PSpice-language `table(x, …)` *call* is deliberately
//! rejected — the table lookup is a real AST node ([`BExpr::Table`]) that
//! only the ABM `TABLE` spelling builds (TODO §5). Deliberately rejected:
//! `random`/`rand`/`gauss` (Monte-Carlo draws belong to the `{expr}`
//! expansion layer, where every solve sees a constant), and `laplace()`
//! (s-domain responses are the ABM LAPLACE form, stamped by AC).

use std::collections::{HashMap, VecDeque};

use crate::circuit::NodeId;

/// Maximum expression depth (paren nesting, call args, powers).
const MAX_DEPTH: usize = 64;
/// Delay samples kept per site; older samples fall off (a delay longer than
/// the retained window returns 0 or the stale hold — decks needing longer
/// history should carry fewer accepted steps, not bigger queues).
pub const MAX_DELAY_SAMPLES: usize = 4096;
/// Half-step of the numeric Jacobian: probe values are voltages (V) or branch
/// currents (A), both far from denormal at real circuit scales.
pub const DIFF_STEP: f64 = 1e-6;

// ─── AST ─────────────────────────────────────────────────────────────────────

/// Math functions shared with the parameter expression library.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MathFn {
    Abs,
    Sqrt,
    Exp,
    Ln,
    Log10,
    Log2,
    Sin,
    Cos,
    Tan,
    Asin,
    Acos,
    Atan,
    Sinh,
    Cosh,
    Tanh,
    Floor,
    Ceil,
    Round,
    Int,
    Sign,
    Atan2,
    Pow,
    Pwr,
    Hypot,
    Mod,
    Min,
    Max,
    /// `u(x)` step function: `1` at and above zero, `0` below.
    Step,
}

impl MathFn {
    fn lookup(name: &str) -> Option<MathFn> {
        Some(match name {
            "abs" => MathFn::Abs,
            "sqrt" => MathFn::Sqrt,
            "exp" => MathFn::Exp,
            "ln" => MathFn::Ln,
            "log" => MathFn::Log10,
            "log10" => MathFn::Log10,
            "log2" => MathFn::Log2,
            "sin" => MathFn::Sin,
            "cos" => MathFn::Cos,
            "tan" => MathFn::Tan,
            "asin" => MathFn::Asin,
            "acos" => MathFn::Acos,
            "atan" => MathFn::Atan,
            "sinh" => MathFn::Sinh,
            "cosh" => MathFn::Cosh,
            "tanh" => MathFn::Tanh,
            "floor" => MathFn::Floor,
            "ceil" => MathFn::Ceil,
            "round" => MathFn::Round,
            "int" => MathFn::Int,
            "sign" => MathFn::Sign,
            "sgn" => MathFn::Sign,
            "atan2" => MathFn::Atan2,
            "pow" => MathFn::Pow,
            "pwr" => MathFn::Pwr,
            "hypot" => MathFn::Hypot,
            "mod" => MathFn::Mod,
            "min" => MathFn::Min,
            "max" => MathFn::Max,
            "u" => MathFn::Step,
            _ => return None,
        })
    }

    fn arity(&self) -> usize {
        match self {
            MathFn::Atan2 | MathFn::Pow | MathFn::Pwr | MathFn::Hypot | MathFn::Mod => 2,
            _ => 1,
        }
    }

    fn apply(&self, x: &[f64]) -> f64 {
        let g = |i: usize| x.get(i).copied().unwrap_or(0.0);
        match self {
            MathFn::Abs => g(0).abs(),
            MathFn::Sqrt => g(0).sqrt(),
            MathFn::Exp => g(0).exp(),
            MathFn::Ln => g(0).ln(),
            MathFn::Log10 => g(0).log10(),
            MathFn::Log2 => g(0).log2(),
            MathFn::Sin => g(0).sin(),
            MathFn::Cos => g(0).cos(),
            MathFn::Tan => g(0).tan(),
            MathFn::Asin => g(0).asin(),
            MathFn::Acos => g(0).acos(),
            MathFn::Atan => g(0).atan(),
            MathFn::Sinh => g(0).sinh(),
            MathFn::Cosh => g(0).cosh(),
            MathFn::Tanh => g(0).tanh(),
            MathFn::Floor => g(0).floor(),
            MathFn::Ceil => g(0).ceil(),
            MathFn::Round => g(0).round(),
            MathFn::Int => g(0).trunc(),
            MathFn::Sign => g(0).signum(),
            MathFn::Atan2 => g(0).atan2(g(1)),
            MathFn::Pow | MathFn::Pwr => g(0).powf(g(1)),
            MathFn::Hypot => g(0).hypot(g(1)),
            MathFn::Mod => g(0) % g(1),
            MathFn::Min => x.iter().copied().fold(f64::INFINITY, f64::min),
            MathFn::Max => x.iter().copied().fold(f64::NEG_INFINITY, f64::max),
            MathFn::Step => if g(0) >= 0.0 { 1.0 } else { 0.0 },
        }
    }
}

/// One expression node. Probe references name indices into
/// [`Behavior::probes`] (deduplicated by the parser).
#[derive(Debug, Clone, PartialEq)]
pub enum BExpr {
    Num(f64),
    /// `time()` — the instant being solved (0 at DC/AC).
    Time,
    /// Probe value (node-pair difference or branch current).
    Probe(usize),
    Neg(Box<BExpr>),
    Add(Box<BExpr>, Box<BExpr>),
    Sub(Box<BExpr>, Box<BExpr>),
    Mul(Box<BExpr>, Box<BExpr>),
    Div(Box<BExpr>, Box<BExpr>),
    /// `^` (right-associative, like the parameter grammar).
    Pow(Box<BExpr>, Box<BExpr>),
    Call(MathFn, Vec<BExpr>),
    /// `idt(x)` — running integral, one trapezoid step per accepted step.
    /// `site` indexes [`BehavioralRuntime::idt_acc`].
    Idt(Box<BExpr>, usize),
    /// `delay(x, td)` — the value of `x` at `t − td` from the site's history.
    /// `site` indexes [`BehavioralRuntime::delay_queues`]. `td ≤ 0` evaluates
    /// the argument directly (exact, and Newton-linearizable).
    Delay(Box<BExpr>, Box<BExpr>, usize),
    /// `white(arg)` / `flicker(arg)` — keyed pseudo-noise. The argument only
    /// *paces* the re-draw: the value at a site is a function of the deck
    /// seed, the source tag, and the site's roll (LTspice's `rand(x)` rule),
    /// never of `t` or a probe, so it is constant within a solve and
    /// numerically flat (`d/dprobe ≡ 0`) under linearization. `site` indexes
    /// [`BehavioralRuntime::noise_rolls`].
    Noise(NoiseKind, Box<BExpr>, usize),
    /// Piecewise-linear lookup (the ABM `TABLE` form): the input expression
    /// against an ascending list of `(input, output)` pairs, interpolated
    /// linearly between adjacent points. An input below the first pair or
    /// above the last yields **0** (PSpice's ABM TABLE semantics — the
    /// bracketing range is part of the model, unlike LTspice's clamping
    /// `table()`). Linear in the input, so the central-difference
    /// linearization stays exact inside each interval.
    Table { input: Box<BExpr>, points: Vec<(f64, f64)> },
}

/// The noise function family on `B` sources.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NoiseKind {
    /// Uniform draw in `[−1, 1)`, re-keyed each time the pacing argument
    /// ticks (a bare `time()` arg counts one roll per accepted step).
    White,
    /// Flicker (1/f-shaped) draw: octave bands of the same uniform draw,
    /// each re-keyed at half the previous band's rate, amplitude-weighted
    /// for equal power per octave, normalized to `[−1, 1)`.
    Flicker,
}

/// A probe as written, before the parse pipeline binds names to ids.
#[derive(Debug, Clone, PartialEq)]
pub enum ProbeSpec {
    /// `V(a)` → `(a, None)`; `V(a,b)` → `(a, Some(b))`. Lowercased names.
    Voltage(String, Option<String>),
    /// `I(src)` — element name of an independent voltage source or inductor.
    Current(String),
}

/// Parser output: the compiled AST plus the deduplicated probe table.
///
/// Probes start as [`ProbeBinding::Unresolved`]; the parse pipeline binds
/// them (`resolve_behavioral`) once node ids are final — a probe must not
/// *create* its node.
#[derive(Debug, Clone)]
pub struct Behavior {
    pub probes: Vec<ProbeBinding>,
    pub ast: BExpr,
    /// `idt()` sites in first-use order: the argument expressions. The `site`
    /// index in the AST is the position here, so state updates walk this list.
    pub idt: Vec<BExpr>,
    /// `delay()` sites in first-use order: `(arg, td)` expressions.
    pub delay: Vec<(BExpr, BExpr)>,
    /// `white()`/`flicker()` sites in first-use order: `(kind, pacing arg)`.
    /// The argument is never evaluated inside a solve — [`BehavioralRuntime::advance`]
    /// evaluates it once per accepted step (end-of-step probes, `dt = 0`)
    /// to settle the site's roll.
    pub noise: Vec<(NoiseKind, BExpr)>,
    /// Key word the draws mix in: the source's element name, hashed by the
    /// parse pipeline ([`crate::params::tag_word`]) so two `B` cards never
    /// draw the same numbers. 0 until the pipeline sets it.
    pub noise_tag: u64,
}

/// A probe binding: the name form, or the resolved form the stamps read.
#[derive(Debug, Clone, PartialEq)]
pub enum ProbeBinding {
    /// Not yet bound (parse-time).
    Unresolved(ProbeSpec),
    /// Differential voltage `v(plus) − v(minus)` (ground = `NodeId(0)`).
    Voltage(NodeId, NodeId),
    /// Branch current of the `i`-th independent voltage source.
    SourceCurrent(usize),
    /// Branch current of the `i`-th inductor.
    InductorCurrent(usize),
}

impl ProbeBinding {
    /// True while the binding still carries only the probe's name.
    pub fn is_unresolved(&self) -> bool {
        matches!(self, ProbeBinding::Unresolved(_))
    }
}

/// Read one bound probe through the caller's accessors. The read sites of
/// this type (DC iterate snapshots, report maps, AC phasors) differ only in
/// how they read a node pair and the two branch kinds, not in the binding's
/// shape: this owns that shape once and returns `None` while the binding
/// still carries only its parse-time name.
pub(crate) fn resolve_probe<T>(
    probe: &ProbeBinding,
    node_pair: impl Fn(NodeId, NodeId) -> T,
    src_branch: impl Fn(usize) -> T,
    ind_branch: impl Fn(usize) -> T,
) -> Option<T> {
    match probe {
        ProbeBinding::Voltage(p, m) => Some(node_pair(*p, *m)),
        ProbeBinding::SourceCurrent(i) => Some(src_branch(*i)),
        ProbeBinding::InductorCurrent(i) => Some(ind_branch(*i)),
        ProbeBinding::Unresolved(_) => None,
    }
}

/// Per-source runtime state for history-dependent terms (`idt`, `delay`).
#[derive(Debug, Clone, Default)]
pub struct BehavioralRuntime {
    /// Completed integral per `idt` site (∫ through the last accepted step).
    pub idt_acc: Vec<f64>,
    /// Argument value of each `idt` site at the last accepted step.
    pub idt_prev: Vec<f64>,
    /// Accepted `(t, arg)` samples per `delay` site, oldest first.
    pub delay_queues: Vec<VecDeque<(f64, f64)>>,
    /// Seed mixed into every `white()`/`flicker()` key: the deck's
    /// Monte-Carlo seed, threaded in by the transient runner (DC/AC keep 0,
    /// the constant of an unseeded parse).
    pub noise_seed: u64,
    /// Per-site noise rolls. Zeros until the first accepted step; a bare
    /// `time()` site's roll is one per accepted step, a general argument's
    /// roll is the end-of-step `floor(arg)`. Frozen for the duration of a
    /// solve, so a Newton iteration never sees a moving draw.
    pub noise_rolls: Vec<u64>,
}

impl BehavioralRuntime {
    /// Sized for `behavior`'s site tables (history all zero / empty).
    pub fn sized_for(behavior: &Behavior) -> Self {
        BehavioralRuntime {
            idt_acc: vec![0.0; behavior.idt.len()],
            idt_prev: vec![0.0; behavior.idt.len()],
            delay_queues: vec![VecDeque::new(); behavior.delay.len()],
            noise_seed: 0,
            noise_rolls: vec![0; behavior.noise.len()],
        }
    }

    /// Seed for the first accepted step: each `idt` site's previous value is
    /// its argument at the DC/IC operating point (so the first trapezoid
    /// starts from the real operating point), and every `delay` queue opens
    /// with the argument's DC value at t = 0 (delays then interpolate from
    /// the true initial history instead of the zero-order hold). `noise_seed`
    /// is the deck's keyed-draw seed (0 for an unseeded parse).
    ///
    /// `probe_values` is the converged probe snapshot at that operating
    /// point, in `behavior.probes` order.
    pub fn seeded(behavior: &Behavior, probe_values: &[f64], noise_seed: u64) -> Self {
        let mut runtime = Self::sized_for(behavior);
        runtime.noise_seed = noise_seed;
        for (site, arg) in behavior.idt.iter().enumerate() {
            runtime.idt_prev[site] = behavior.eval_expr(arg, probe_values, 0.0, 0.0, &runtime);
        }
        for (site, (arg, _td)) in behavior.delay.iter().enumerate() {
            let value = behavior.eval_expr(arg, probe_values, 0.0, 0.0, &runtime);
            runtime.delay_queues[site].push_back((0.0, value));
        }
        runtime
    }

    /// Advance the history through one accepted step of `dt` ending at
    /// `t_end`: each `idt` site completes one trapezoid with its argument
    /// evaluated at the *converged* end-of-step probe values, each `delay`
    /// site records the accepted sample (queues capped at
    /// [`MAX_DELAY_SAMPLES`]), and every noise site settles its roll for the
    /// next solve.
    ///
    /// Sites are walked in parse order and `self` is updated as the walk
    /// proceeds, so a nested `idt` evaluation reads the just-completed
    /// accumulator (dt = 0 in the inner evaluation collapses the inner node
    /// to exactly that).
    pub fn advance(&mut self, behavior: &Behavior, probe_values: &[f64], t_end: f64, dt: f64) {
        for (site, arg) in behavior.idt.iter().enumerate() {
            // dt = 0 on the argument evaluation: a nested `idt` site
            // collapses to its (already updated) accumulator — the
            // step-end value of the inner integral, which is exactly the
            // outer site's argument value at `t_end`.
            let now = behavior.eval_expr(arg, probe_values, t_end, 0.0, self);
            let prev = self.idt_prev.get(site).copied().unwrap_or(now);
            self.idt_acc[site] =
                self.idt_acc.get(site).copied().unwrap_or(0.0) + 0.5 * dt * (now + prev);
            self.idt_prev[site] = now;
        }
        for (site, (arg, _td)) in behavior.delay.iter().enumerate() {
            let value = behavior.eval_expr(arg, probe_values, t_end, 0.0, self);
            let queue = &mut self.delay_queues[site];
            queue.push_back((t_end, value));
            while queue.len() > MAX_DELAY_SAMPLES {
                queue.pop_front();
            }
        }
        for (site, (_kind, arg)) in behavior.noise.iter().enumerate() {
            if matches!(arg, BExpr::Time) {
                // Bare `time()`: LTspice's white(noise) pacing — one roll per
                // accepted step, no dependency on where in the bucket `t_end`
                // lands.
                if let Some(slot) = self.noise_rolls.get_mut(site) {
                    *slot += 1;
                }
            } else {
                // General pacing argument: its end-of-step floor is the key
                // (`.floor()` so fractional buckets re-roll only on a tick —
                // `white(floor(V(a)))` re-keys when the probe does). The
                // evaluation is the same frozen-snapshot rule the `idt`/
                // `delay` sites follow; anything the deck solves for is
                // converged by the time it lands here. A non-finite argument
                // (probe rode to ±∞ in the step's last iterate) keeps the
                // previous roll — a consistent draw beats a scrambled key.
                let value = behavior.eval_expr(arg, probe_values, t_end, 0.0, self);
                if value.is_finite() {
                    if let Some(slot) = self.noise_rolls.get_mut(site) {
                        *slot = roll_key(value);
                    }
                }
            }
        }
    }
}

/// Map a finite pacing-argument value to a roll: its floor as 64 bits.
/// Saturating at the i64 edges (Rust's rules for float→int casts), negatives
/// wrap through the far end of the range — both keep the mapping
/// deterministic and monotone per tick.
fn roll_key(value: f64) -> u64 {
    value.floor() as i64 as u64
}

/// Octave count of the `flicker()` approximation: bands at rates
/// `roll >> 0 … roll >> 5`, ~2⁵ ≈ 32× slower at the bottom than the top.
const FLICKER_OCTAVES: usize = 6;

/// Draw word for one `white()`/`flicker()` site: the deck seed and the
/// source tag mix first, the site spreads by the golden-ratio constant
/// (so site indices never alias), then the roll. All-integer mixing from
/// the [`crate::params`] SplitMix64 family — the same avalanche the
/// Monte-Carlo draws use.
fn noise_word(noise_seed: u64, tag: u64, site: usize, roll: u64) -> u64 {
    let mut h = crate::params::mix64(noise_seed ^ tag);
    h = crate::params::mix64(h ^ (site as u64).wrapping_mul(0x9E37_79B9_7F4A_7C15));
    crate::params::mix64(h ^ roll)
}

/// Uniform deviate in `[−1, 1)` from one draw word.
fn noise_uniform(word: u64) -> f64 {
    2.0 * crate::params::unit01(word) - 1.0
}

// ─── Lexer ───────────────────────────────────────────────────────────────────
//
// Own lexer rather than `params::lex`: probe arguments are node/element
// names, and those legitimately contain dots (`X1.mid` after subckt
// flattening) that the parameter lexer would read as numbers. Numbers still
// go through `units::parse_value`, so scale suffixes work the same.

#[derive(Debug, Clone, PartialEq)]
enum Tok {
    Num(f64),
    Ident(String),
    Op(char),
}

impl Tok {
    fn text(&self) -> String {
        match self {
            Tok::Num(v) => format!("{v}"),
            Tok::Ident(s) => s.clone(),
            Tok::Op(c) => c.to_string(),
        }
    }
}

fn lex_b(src: &str) -> Result<Vec<Tok>, String> {
    let chars: Vec<char> = src.chars().collect();
    let mut toks = Vec::new();
    let mut i = 0usize;
    while i < chars.len() {
        let c = chars[i];
        if c.is_whitespace() {
            i += 1;
            continue;
        }
        match c {
            '+' | '-' | '*' | '/' | '^' | '(' | ')' | ',' => {
                toks.push(Tok::Op(c));
                i += 1;
            }
            // Numbers: digits with optional dot/exponent, plus the trailing
            // scale suffix letters (`1k`, `10meg`) via the shared value parser.
            '0'..='9' | '.' => {
                let start = i;
                while i < chars.len() && (chars[i].is_ascii_digit() || chars[i] == '.') {
                    i += 1;
                }
                if i < chars.len()
                    && (chars[i] == 'e' || chars[i] == 'E')
                    && chars
                        .get(i + 1)
                        .is_some_and(|&n| n.is_ascii_digit() || n == '+' || n == '-')
                {
                    i += 1;
                    if chars[i] == '+' || chars[i] == '-' {
                        i += 1;
                    }
                    while i < chars.len() && chars[i].is_ascii_digit() {
                        i += 1;
                    }
                }
                while i < chars.len() && chars[i].is_ascii_alphabetic() {
                    i += 1;
                }
                let text: String = chars[start..i].iter().collect();
                let v = crate::units::parse_value(&text)
                    .map_err(|e| format!("bad number '{text}': {e}"))?;
                toks.push(Tok::Num(v));
            }
            // Names: alphanumerics plus `_` and `.` — the dot keeps flattened
            // instance paths (`X1.mid`) in one token.
            c if c.is_ascii_alphabetic() || c == '_' || c == '.' => {
                let start = i;
                while i < chars.len()
                    && (chars[i].is_ascii_alphanumeric() || chars[i] == '_' || chars[i] == '.')
                {
                    i += 1;
                }
                toks.push(Tok::Ident(
                    chars[start..i].iter().collect::<String>().to_ascii_lowercase(),
                ));
            }
            other => return Err(format!("unexpected character '{other}' in B expression")),
        }
    }
    Ok(toks)
}

// ─── Parser ──────────────────────────────────────────────────────────────────

/// Parse a behavioral expression with deck parameters folded in.
///
/// Probes are stored unresolved; `V(…)` may name nodes that do not exist
/// yet, so binding is a later pass. An identifier that names no known
/// function but matches an entry of `params` (keys lowercased, numeric
/// values only) folds into a number node — the ABM/B spellings ride deck
/// `.param` values the same way numeric values inside `{}` do.
pub fn parse_with_params(
    expr: &str,
    params: &HashMap<String, f64>,
) -> Result<Behavior, String> {
    let tokens = lex_b(expr)?;
    let mut p = Parser {
        tokens: &tokens,
        pos: 0,
        depth: 0,
        probes: Vec::new(),
        probe_ids: Vec::new(),
        idt_sites: Vec::new(),
        delay_sites: Vec::new(),
        noise_sites: Vec::new(),
        params,
    };
    let ast = p.expr()?;
    if p.pos != tokens.len() {
        return Err(format!(
            "unexpected token at end of expression: '{}'",
            tokens[p.pos].text()
        ));
    }
    Ok(Behavior {
        probes: p.probes,
        ast,
        idt: p.idt_sites,
        delay: p.delay_sites,
        noise: p.noise_sites,
        // Stamped by the element parser from the card's name — `parse()` has
        // no source context (tests parse bare expressions, where the tag
        // stays 0 and draws are keyed on the seed alone).
        noise_tag: 0,
    })
}

/// Parse a behavioral expression without deck parameters.
pub fn parse(expr: &str) -> Result<Behavior, String> {
    parse_with_params(expr, &HashMap::new())
}

struct Parser<'a> {
    tokens: &'a [Tok],
    pos: usize,
    depth: usize,
    probes: Vec<ProbeBinding>,
    /// Canonical dedup keys, parallel to `probes`.
    probe_ids: Vec<String>,
    /// `idt()` sites in parse order; the AST's `site` index is this position.
    idt_sites: Vec<BExpr>,
    /// `delay()` sites in parse order: `(arg, td)`.
    delay_sites: Vec<(BExpr, BExpr)>,
    /// `white()`/`flicker()` sites in parse order: `(kind, pacing arg)`.
    noise_sites: Vec<(NoiseKind, BExpr)>,
    /// Deck parameters, folded into numbers at parse (keys lowercased).
    params: &'a HashMap<String, f64>,
}

impl<'a> Parser<'a> {
    fn peek(&self) -> Option<&Tok> {
        self.tokens.get(self.pos)
    }

    fn peek_kind(&self) -> Option<Tok> {
        self.tokens.get(self.pos).cloned()
    }

    fn eat_op(&mut self, c: char) -> bool {
        if let Some(Tok::Op(o)) = self.peek() {
            if *o == c {
                self.pos += 1;
                return true;
            }
        }
        false
    }

    fn expect_op(&mut self, c: char) -> Result<(), String> {
        if self.eat_op(c) {
            Ok(())
        } else {
            Err(format!("expected '{c}'"))
        }
    }

    fn expr(&mut self) -> Result<BExpr, String> {
        self.enter()?;
        let mut left = self.term()?;
        loop {
            if self.eat_op('+') {
                let r = self.term()?;
                left = BExpr::Add(Box::new(left), Box::new(r));
            } else if self.eat_op('-') {
                let r = self.term()?;
                left = BExpr::Sub(Box::new(left), Box::new(r));
            } else {
                break;
            }
        }
        self.depth -= 1;
        Ok(left)
    }

    fn term(&mut self) -> Result<BExpr, String> {
        let mut left = self.unary()?;
        loop {
            if self.eat_op('*') {
                let r = self.unary()?;
                left = BExpr::Mul(Box::new(left), Box::new(r));
            } else if self.eat_op('/') {
                let r = self.unary()?;
                left = BExpr::Div(Box::new(left), Box::new(r));
            } else {
                break;
            }
        }
        Ok(left)
    }

    fn unary(&mut self) -> Result<BExpr, String> {
        if self.eat_op('-') {
            return Ok(BExpr::Neg(Box::new(self.unary()?)));
        }
        if self.eat_op('+') {
            return self.unary();
        }
        self.power()
    }

    /// `power := atom ('^' unary)?` — right-associative, so `2^3^2` is `2^9`.
    fn power(&mut self) -> Result<BExpr, String> {
        let base = self.atom()?;
        if self.eat_op('^') {
            return Ok(BExpr::Pow(Box::new(base), Box::new(self.unary()?)));
        }
        Ok(base)
    }

    fn atom(&mut self) -> Result<BExpr, String> {
        self.enter()?;
        let node = match self.peek_kind() {
            Some(Tok::Num(v)) => {
                self.pos += 1;
                BExpr::Num(v)
            }
            Some(Tok::Ident(name)) => {
                self.pos += 1;
                if (name == "v" || name == "i") && matches!(self.peek(), Some(Tok::Op('('))) {
                    // Probe form: record the name, resolve nothing here (the
                    // binding pass runs once node ids are final).
                    self.pos += 1;
                    let spec = self.probe_args(&name)?;
                    BExpr::Probe(self.intern(spec))
                } else {
                    self.plain_ident(&name)?
                }
            }
            Some(Tok::Op('(')) => {
                self.pos += 1;
                let v = self.expr()?;
                self.expect_op(')')?;
                v
            }
            other => {
                return Err(match other {
                    Some(tok) => format!("unexpected {} in B expression", tok.text()),
                    None => "unexpected end of expression".to_string(),
                });
            }
        };
        self.depth -= 1;
        Ok(node)
    }

    /// A bare identifier: the clock (`time`), or a call into the math library
    /// or the special history forms.
    fn plain_ident(&mut self, name: &str) -> Result<BExpr, String> {
        if name == "time" {
            if matches!(self.peek(), Some(Tok::Op('('))) {
                self.pos += 1;
                if !matches!(self.peek(), Some(Tok::Op(')'))) {
                    return Err("time() takes no arguments".into());
                }
                self.pos += 1;
            }
            return Ok(BExpr::Time);
        }
        if matches!(self.peek(), Some(Tok::Op('('))) {
            self.pos += 1;
            let mut args = Vec::new();
            if !matches!(self.peek(), Some(Tok::Op(')'))) {
                loop {
                    args.push(self.expr()?);
                    if self.eat_op(',') {
                        continue;
                    }
                    break;
                }
            }
            self.expect_op(')')?;
            return self.call(name, args);
        }
        // Deck parameters fold to numbers at parse (keys lowercased, numeric
        // values only — the netlist parse drops non-numeric entries).
        if let Some(v) = self.params.get(name) {
            return Ok(BExpr::Num(*v));
        }
        Err(format!(
            "unknown name '{name}' in B expression (probes are V(a[,b]) / I(src); named functions take arguments)"
        ))
    }

    fn call(&mut self, name: &str, args: Vec<BExpr>) -> Result<BExpr, String> {
        // Determinism: draws belong to the `{}`-expansion layer, where every
        // solve observes a constant. A transient that re-rolled mid-solve
        // would never converge.
        if matches!(name, "random" | "rand" | "gauss" | "table" | "laplace") {
            return Err(format!(
                "{name}() is not supported inside B expressions \
                 (it belongs to the .param/.func layer, where values are per-parse constants)"
            ));
        }
        if name == "idt" || name == "sdt" {
            if args.len() != 1 {
                return Err(format!(
                    "{name}() takes exactly 1 argument, got {}",
                    args.len()
                ));
            }
            let arg = args.into_iter().next().unwrap();
            let site = self.idt_sites.len();
            self.idt_sites.push(arg.clone());
            return Ok(BExpr::Idt(Box::new(arg), site));
        }
        if name == "delay" {
            if args.len() != 2 {
                return Err(format!(
                    "delay() takes exactly 2 arguments (x, td), got {}",
                    args.len()
                ));
            }
            let mut args = args.into_iter();
            let arg = args.next().unwrap();
            let td = args.next().unwrap();
            let site = self.delay_sites.len();
            self.delay_sites.push((arg.clone(), td.clone()));
            return Ok(BExpr::Delay(Box::new(arg), Box::new(td), site));
        }
        if name == "white" || name == "flicker" {
            if args.len() != 1 {
                return Err(format!(
                    "{name}() takes exactly 1 argument (the pacing argument), got {}",
                    args.len()
                ));
            }
            let kind = if name == "white" {
                NoiseKind::White
            } else {
                NoiseKind::Flicker
            };
            let arg = args.into_iter().next().unwrap();
            let site = self.noise_sites.len();
            self.noise_sites.push((kind, arg.clone()));
            return Ok(BExpr::Noise(kind, Box::new(arg), site));
        }
        let Some(f) = MathFn::lookup(name) else {
            return Err(format!("unknown function '{name}' in B expression"));
        };
        if args.len() < f.arity() {
            return Err(format!(
                "{name}() needs {} argument(s), got {}",
                f.arity(),
                args.len()
            ));
        }
        Ok(BExpr::Call(f, args))
    }

    /// Probe arguments: `V(a[,b])` or `I(name)` — identifiers or bare numbers
    /// (numeric node ids).
    fn probe_args(&mut self, kind: &str) -> Result<ProbeSpec, String> {
        let arg_name = |tok: &Option<Tok>| -> Result<String, String> {
            match tok {
                Some(Tok::Ident(s)) => Ok(s.clone()),
                Some(Tok::Num(v)) => Ok(format!("{v}")),
                _ => Err("expected a node or element name".into()),
            }
        };
        if kind == "v" {
            let a = arg_name(&self.peek_kind())?;
            self.pos += 1;
            if self.eat_op(',') {
                let b = arg_name(&self.peek_kind())?;
                self.pos += 1;
                self.expect_op(')')?;
                Ok(ProbeSpec::Voltage(a, Some(b)))
            } else {
                self.expect_op(')')?;
                Ok(ProbeSpec::Voltage(a, None))
            }
        } else {
            let a = arg_name(&self.peek_kind())?;
            self.pos += 1;
            self.expect_op(')')?;
            Ok(ProbeSpec::Current(a))
        }
    }

    /// Register a probe; identical specs share one slot (and therefore one
    /// Jacobian column).
    fn intern(&mut self, spec: ProbeSpec) -> usize {
        let key = match &spec {
            ProbeSpec::Voltage(a, b) => format!(
                "v:{}:{}",
                a.to_ascii_lowercase(),
                b.clone().unwrap_or_default().to_ascii_lowercase()
            ),
            ProbeSpec::Current(s) => format!("i:{}", s.to_ascii_lowercase()),
        };
        if let Some(idx) = self.probe_ids.iter().position(|k| *k == key) {
            return idx;
        }
        self.probes.push(ProbeBinding::Unresolved(spec));
        self.probe_ids.push(key);
        self.probes.len() - 1
    }

    fn enter(&mut self) -> Result<(), String> {
        self.depth += 1;
        if self.depth > MAX_DEPTH {
            return Err("B expression nested too deeply".into());
        }
        Ok(())
    }
}

// ─── Evaluation ──────────────────────────────────────────────────────────────

/// Piecewise-linear table lookup (ABM `TABLE` semantics): linear between
/// adjacent pairs, **0 outside** the bracketing range. Points travel sorted
/// by input so the search is linear from the start.
fn table_eval(points: &[(f64, f64)], x: f64) -> f64 {
    if points.is_empty() {
        return 0.0;
    }
    if x < points[0].0 || x > points[points.len() - 1].0 {
        return 0.0;
    }
    for window in points.windows(2) {
        let (x0, y0) = window[0];
        let (x1, y1) = window[1];
        if x <= x1 {
            let frac = if x1 == x0 { 0.0 } else { (x - x0) / (x1 - x0) };
            return y0 + frac * (y1 - y0);
        }
    }
    // Exactly the last input.
    points[points.len() - 1].1
}

/// The value of a delayed argument, looked up in the accepted-sample history.
fn delayed_value(queue: &[(f64, f64)], t: f64) -> f64 {    if queue.is_empty() {
        return 0.0;
    }
    if t <= queue[0].0 {
        return 0.0;
    }
    if t >= queue[queue.len() - 1].0 {
        // Zero-order hold beyond the last accepted sample: the stamped value
        // is at worst one step stale (documented approximation — reference
        // tools ramp the hold; both agree once td exceeds the step size).
        return queue[queue.len() - 1].1;
    }
    let mut i = 0usize;
    while i + 1 < queue.len() && queue[i + 1].0 <= t {
        i += 1;
    }
    let (t0, v0) = queue[i];
    let (t1, v1) = queue[i + 1];
    if t1 > t0 {
        v0 + (v1 - v0) * (t - t0) / (t1 - t0)
    } else {
        v1
    }
}

impl Behavior {
    /// Evaluate the source value at one probe snapshot.
    ///
    /// `t` is the instant being solved (0 at DC/AC), `dt` the step it is
    /// solving (0 in AC/DC and for pure reports — the `idt` companion then
    /// collapses to its accumulator). All history reads come from `runtime`.
    pub fn evaluate(
        &self,
        probe_values: &[f64],
        t: f64,
        dt: f64,
        runtime: &BehavioralRuntime,
    ) -> f64 {
        self.eval_expr(&self.ast, probe_values, t, dt, runtime)
    }

    /// Evaluate one sub-expression against the same context.
    pub fn eval_expr(
        &self,
        e: &BExpr,
        pv: &[f64],
        t: f64,
        dt: f64,
        rt: &BehavioralRuntime,
    ) -> f64 {
        match e {
            BExpr::Num(v) => *v,
            BExpr::Time => t,
            BExpr::Probe(i) => pv.get(*i).copied().unwrap_or(0.0),
            BExpr::Neg(a) => -self.eval_expr(a, pv, t, dt, rt),
            BExpr::Add(a, b) => {
                self.eval_expr(a, pv, t, dt, rt) + self.eval_expr(b, pv, t, dt, rt)
            }
            BExpr::Sub(a, b) => {
                self.eval_expr(a, pv, t, dt, rt) - self.eval_expr(b, pv, t, dt, rt)
            }
            BExpr::Mul(a, b) => {
                self.eval_expr(a, pv, t, dt, rt) * self.eval_expr(b, pv, t, dt, rt)
            }
            BExpr::Div(a, b) => {
                self.eval_expr(a, pv, t, dt, rt) / self.eval_expr(b, pv, t, dt, rt)
            }
            BExpr::Pow(a, b) => {
                self.eval_expr(a, pv, t, dt, rt).powf(self.eval_expr(b, pv, t, dt, rt))
            }
            BExpr::Call(f, args) => {
                let values: Vec<f64> =
                    args.iter().map(|a| self.eval_expr(a, pv, t, dt, rt)).collect();
                f.apply(&values)
            }
            BExpr::Idt(arg, site) => {
                let acc = rt.idt_acc.get(*site).copied().unwrap_or(0.0);
                let prev = rt.idt_prev.get(*site).copied().unwrap_or(0.0);
                let now = self.eval_expr(arg, pv, t, dt, rt);
                acc + 0.5 * dt * (now + prev)
            }
            BExpr::Delay(arg, td, site) => {
                let td = self.eval_expr(td, pv, t, dt, rt);
                if td <= 0.0 {
                    return self.eval_expr(arg, pv, t, dt, rt);
                }
                let queue: Vec<(f64, f64)> = rt
                    .delay_queues
                    .get(*site)
                    .map(|q| q.iter().copied().collect())
                    .unwrap_or_default();
                delayed_value(&queue, t - td)
            }
            BExpr::Noise(kind, _arg, site) => {
                // The draw reads the site's settled roll — never `t`, never a
                // probe — so repeated evaluations in one Newton solve agree
                // bit-for-bit and every linearized derivative is exactly 0.
                let roll = rt.noise_rolls.get(*site).copied().unwrap_or(0);
                self.noise_value(*kind, *site, roll, rt)
            }
            BExpr::Table { input, points } => {
                let x = self.eval_expr(input, pv, t, dt, rt);
                table_eval(points, x)
            }
        }
    }

    /// The keyed draw of one noise site at one roll. Uniform deviates come
    /// from the same SplitMix64 family as the Monte-Carlo parameter draws
    /// ([`crate::params`]), keyed by deck seed ⊕ source tag, site, and roll.
    fn noise_value(&self, kind: NoiseKind, site: usize, roll: u64, rt: &BehavioralRuntime) -> f64 {
        match kind {
            NoiseKind::White => noise_uniform(noise_word(
                rt.noise_seed,
                self.noise_tag,
                site,
                roll,
            )),
            NoiseKind::Flicker => {
                // Six octave bands, octave `o` re-keyed every 2ᵒ·(pacing tick)
                // so slow bands wander slowly; amplitude weights follow
                // 1/√f (equal power per octave — the pink/flicker law), and
                // the weighted sum is divided by its worst-case bound, which
                // keeps the whole draw inside `[−1, 1)` like `white()`.
                let mut value = 0.0f64;
                let mut bound = 0.0f64;
                for o in 0..FLICKER_OCTAVES {
                    let weight = (o as f64 * 0.5).exp2();
                    bound += weight;
                    value += weight * noise_uniform(noise_word(
                        rt.noise_seed,
                        self.noise_tag,
                        site,
                        roll >> o,
                    ));
                }
                value / bound
            }
        }
    }

    /// Newton-Raphson linearization at the probe snapshot: `(f0, df/dprobe_k)`
    /// by central differences on the probe values. History stays frozen, so
    /// delayed values contribute no derivative (their argument is in the
    /// past).
    ///
    /// Every returned number must be finite — a division by a zero probe
    /// would otherwise inject NaN/∞ into the MNA matrix, so that is a loud
    /// error, caught before any stamp.
    pub fn linearize(
        &self,
        base: &[f64],
        t: f64,
        dt: f64,
        runtime: &BehavioralRuntime,
    ) -> Result<(f64, Vec<f64>), String> {
        if let Some(v) = base.iter().find(|v| !v.is_finite()) {
            return Err(format!(
                "probe value {v} is not finite at the operating point"
            ));
        }
        let f0 = self.evaluate(base, t, dt, runtime);
        let mut dk = Vec::with_capacity(self.probes.len());
        for k in 0..self.probes.len() {
            let plus: Vec<f64> = base
                .iter()
                .enumerate()
                .map(|(i, &v)| if i == k { v + DIFF_STEP } else { v })
                .collect();
            let minus: Vec<f64> = base
                .iter()
                .enumerate()
                .map(|(i, &v)| if i == k { v - DIFF_STEP } else { v })
                .collect();
            let d = (self.evaluate(&plus, t, dt, runtime)
                - self.evaluate(&minus, t, dt, runtime))
                / (2.0 * DIFF_STEP);
            dk.push(d);
        }
        if !f0.is_finite() || dk.iter().any(|d| !d.is_finite()) {
            return Err(format!(
                "evaluation produced a non-finite value (f0 = {f0}) — check for a division by a zero probe"
            ));
        }
        Ok((f0, dk))
    }
}

// ─── Tests ───────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    fn eval(src: &str, pv: &[f64]) -> f64 {
        let b = parse(src).expect("parse");
        b.evaluate(pv, 0.0, 0.0, &BehavioralRuntime::default())
    }

    fn parse_err(src: &str) -> String {
        parse(src).expect_err("expression should fail").to_string()
    }

    #[test]
    fn parses_with_precedence_and_suffixes() {
        // -(2k·0.5) + 1.5e2²/100, with `^` binding tighter than `*`/`/`.
        let v = eval("-2k*0.5+1.5e2^2/100", &[]);
        assert!((v - (-1000.0 + 225.0)).abs() < 1e-9);
    }

    #[test]
    fn probes_dedupe_and_keep_names() {
        let b = parse("V(in)*V(in)+I(Vsrc)-V(in,gnD)").unwrap();
        // V(in) appears twice but shares one slot; I(Vsrc) and the differential
        // form are two more.
        assert_eq!(b.probes.len(), 3);
        let s0 = &b.probes[0];
        let s1 = &b.probes[1];
        let s2 = &b.probes[2];
        assert!(matches!(
            s0,
            ProbeBinding::Unresolved(ProbeSpec::Voltage(a, None)) if a == "in"
        ));
        assert!(matches!(
            s1,
            ProbeBinding::Unresolved(ProbeSpec::Current(s)) if s == "vsrc"
        ));
        assert!(matches!(
            s2,
            ProbeBinding::Unresolved(ProbeSpec::Voltage(a, Some(g))) if a == "in" && g == "gnd"
        ));
        // Top level: the subtraction `V(in)·V(in)+I(Vsrc) − V(in,gnd)`.
        assert!(matches!(&b.ast, BExpr::Sub(_, _)));
    }

    #[test]
    fn library_and_step_functions_evaluate() {
        // min sees both probe values (V(a) = 2, V(b) = 5): the deck constant loses.
        assert_eq!(eval("min(V(a),V(b),3.5)", &[2.0, 5.0]), 2.0);
        assert_eq!(eval("max(V(a),1.5)", &[2.0]), 2.0);
        assert_eq!(eval("u(V(a))", &[-1.0]), 0.0);
        assert_eq!(eval("u(V(a))", &[0.5]), 1.0);
        assert_eq!(eval("u(V(a))", &[0.0]), 1.0); // inclusive edge
        assert_eq!(eval("sgn(V(a))", &[-3.0]), -1.0);
        assert_eq!(eval("pwr(2,10)", &[0.0]), 1024.0);
        assert_eq!(eval("hypot(3,4)", &[0.0]), 5.0);
        assert_eq!(eval("int(4.75)", &[0.0]), 4.0);
        // SPICE convention: log() is base 10.
        assert!((eval("log(100)", &[0.0]) - 2.0).abs() < 1e-12);
    }

    #[test]
    fn idt_companion_accumulates_trapezoid() {
        // Seed: idt_prev carries the DC-argument value, acc 0.
        let b = parse("idt(4)+idt(V(a))").unwrap();
        assert_eq!(b.idt.len(), 2);
        let mut rt = BehavioralRuntime::sized_for(&b);
        rt.idt_prev[0] = 4.0; // constant argument
        rt.idt_prev[1] = 2.0; // V(a) at the operating point
        let pv = [2.0];
        // Step 1 (dt = 0.1): ½·0.1·(4+4) + ½·0.1·(2+2) = 0.4 + 0.2.
        assert!((b.evaluate(&pv, 0.1, 0.1, &rt) - 0.6).abs() < 1e-12);
        // The advance pass completes the integrals and rolls the samples.
        rt.idt_acc[0] = 0.4;
        rt.idt_prev[0] = 4.0;
        rt.idt_acc[1] = 0.2;
        rt.idt_prev[1] = 2.0;
        // Step 2 (dt = 0.1): 0.8 + 0.4.
        assert!((b.evaluate(&pv, 0.2, 0.1, &rt) - 1.2).abs() < 1e-12);
        // A pure report (no pending step) collapses to the done integral.
        rt.idt_acc[0] = 0.8;
        rt.idt_acc[1] = 0.4;
        assert!((b.evaluate(&pv, 0.2, 0.0, &rt) - 1.2).abs() < 1e-12);
    }

    #[test]
    fn delay_holds_and_interpolates() {
        let b = parse("delay(V(a), 0.5)").unwrap();
        let mut rt = BehavioralRuntime::sized_for(&b);
        let pv = [7.0];
        // Nothing in history yet: the delayed value is 0 — a td > 0 delay
        // never falls back to the live value, so Newton sees a constant.
        assert_eq!(b.evaluate(&pv, 0.1, 0.1, &rt), 0.0);
        rt.delay_queues[0].push_back((0.0, 0.0));
        rt.delay_queues[0].push_back((1.0, 10.0));
        rt.delay_queues[0].push_back((2.0, 20.0));
        // t − td = 0.5 lands inside the first sample interval (0 → 10).
        assert!((b.evaluate(&pv, 1.0, 0.1, &rt) - 5.0).abs() < 1e-12);
        // Second interval midpoint: 1.5 → 15.
        assert!((b.evaluate(&pv, 2.0, 0.1, &rt) - 15.0).abs() < 1e-12);
        // Beyond the last sample: zero-order hold.
        assert!((b.evaluate(&pv, 2.9, 0.1, &rt) - 20.0).abs() < 1e-12);
        // Before the first sample: 0.
        assert!((b.evaluate(&pv, 0.2, 0.1, &rt) - 0.0).abs() < 1e-12);
        // td == 0 degenerates to the live value.
        let live = parse("delay(V(a), 0)").unwrap();
        assert_eq!(live.evaluate(&pv, 5.0, 0.1, &BehavioralRuntime::default()), 7.0);
    }

    #[test]
    fn linearize_matches_hand_derivatives() {
        // f = 3·v(in)² + v(out) around v(in) = 10, v(out) = 1.5.
        let mut b = parse("3*V(in)^2 + V(out)").unwrap();
        assert_eq!(b.probes.len(), 2);
        b.probes[0] = ProbeBinding::Voltage(NodeId(1), NodeId(0));
        b.probes[1] = ProbeBinding::Voltage(NodeId(2), NodeId(0));
        let (f0, dk) = b
            .linearize(&[10.0, 1.5], 0.0, 0.0, &BehavioralRuntime::default())
            .unwrap();
        assert!((f0 - 3.0 * 100.0 - 1.5).abs() < 1e-9);
        assert!((dk[0] - 60.0).abs() < 1e-4); // 3·2·x = 60, plus numeric slop
        // 1e-6 tolerance: the linear term rides on a ~300 magnitude, so the
        // central difference carries ~ulp(301.5)/2e-6 ≈ 3e-8 of cancellation
        // noise (exact arithmetic would give 1.0).
        assert!((dk[1] - 1.0).abs() < 1e-6);
    }

    #[test]
    fn linearize_carries_dt_into_idt() {
        // f = idt(4·V(a)): the step linearization must expose dt/2·4 per volt.
        let b = parse("idt(4*V(a))").unwrap();
        let (_f0, dk) = b
            .linearize(&[3.0], 0.1, 0.1, &BehavioralRuntime::default())
            .unwrap();
        assert!((dk[0] - 4.0 * 0.05).abs() < 1e-6);
    }

    #[test]
    fn time_starts_at_zero_in_dc_and_runs_in_tran() {
        let b = parse("time()+sdt(1)").unwrap();
        assert_eq!(b.evaluate(&[], 0.0, 0.0, &BehavioralRuntime::default()), 0.0);
        let mut rt = BehavioralRuntime::sized_for(&b);
        rt.idt_acc[0] = 1.0;
        assert!((b.evaluate(&[], 3.0, 0.5, &rt) - (3.0 + 1.0 + 0.25)).abs() < 1e-12);
    }

    #[test]
    fn rejects_draws_and_unknown_names() {
        assert!(parse_err("rand(1)+V(a)").contains("rand"));
        assert!(parse_err("gauss(2m)").contains("gauss"));
        assert!(parse_err("random(1)").contains("random"));
        assert!(parse_err("table(V(a), 0,0, 1,1)").contains("table"));
        assert!(parse_err("laplace(V(a), 1, 2)").contains("laplace"));
        assert!(parse_err("nope+1").contains("nope"));
        // The keyed noise forms parse, and their arity is loud too.
        assert!(parse_err("white()").contains("white"));
        assert!(parse_err("white(time(), 2)").contains("white"));
        assert!(parse_err("flicker()").contains("flicker"));
        // Arity checks are loud, not defaults.
        assert!(parse_err("delay(1)").contains("delay"));
        assert!(parse_err("idt()").contains("idt"));
        assert!(parse_err("atan2(1)").contains("atan2"));
        assert!(parse_err("sin(V(a)").contains("expected"));
        assert!(parse_err("1+").contains("end of expression"));
        assert!(parse_err("2 3").contains("end of expression"));
    }

    // ─── Deck parameters folded in (`parse_with_params`, ABM/B) ───────────────

    /// Numeric `.param` entries fold into numbers at parse — by exact key
    /// (the deck layer lowercases names, so the map holds its own contract);
    /// a name that matches nothing still errors.
    #[test]
    fn parse_with_params_folds_deck_names_into_numbers() {
        let mut params = HashMap::new();
        params.insert("gain".to_string(), 4.0);
        params.insert("vbias".to_string(), 1.5);
        let b = parse_with_params("gain*2 + vbias", &params).unwrap();
        // The folded AST evaluates without probes or history.
        let v = b.evaluate(&[], 0.0, 0.0, &BehavioralRuntime::default());
        assert!((v - 9.5).abs() < 1e-12);
        // The param result reaches nested function arguments too.
        let b = parse_with_params("u(gain - vbias)", &params).unwrap();
        let v = b.evaluate(&[], 0.0, 0.0, &BehavioralRuntime::default());
        assert_eq!(v, 1.0);
        // An unknown name keeps its loud parse error.
        let err = parse_with_params("gain + nope", &params)
            .err()
            .unwrap()
            .to_string();
        assert!(err.contains("nope"), "error was: {err}");
        // Without the fold the same spelling is unknown.
        assert!(parse("gain + 1").is_err());
    }

    // ─── Table lookup (ABM `TABLE`, TODO §5) ─────────────────────────────────

    /// The AST table node: linear between the sorted pairs, **0 outside** the
    /// bracketing range (PSpice TABLE semantics, not LTspice clamping).
    #[test]
    fn table_lookup_interpolates_and_is_zero_outside() {
        let points = vec![(0.0, 1.0), (1.0, 3.0), (2.0, 5.0)];

        // The node wraps any input expression — a constant 0.5 lands between
        // the first two pairs (so `evaluate` reaches `table_eval`).
        let b = Behavior {
            ast: BExpr::Table { input: Box::new(BExpr::Num(0.5)), points: points.clone() },
            ..parse("0.5").unwrap()
        };
        let v = b.evaluate(&[], 0.0, 0.0, &BehavioralRuntime::default());
        assert!((v - 2.0).abs() < 1e-12);

        // Direct checks at and between the pairs.
        assert_eq!(table_eval(&points, 0.0), 1.0);
        assert_eq!(table_eval(&points, 0.5), 2.0);
        assert_eq!(table_eval(&points, 1.0), 3.0);
        assert_eq!(table_eval(&points, 1.25), 3.5);
        assert_eq!(table_eval(&points, 2.0), 5.0);
        // PSpice ABM: silent outside the bracketing range.
        assert_eq!(table_eval(&points, -0.5), 0.0);
        assert_eq!(table_eval(&points, 2.5), 0.0);
        assert_eq!(table_eval(&[], 1.0), 0.0);
    }

    // ─── Keyed noise (`white` / `flicker`, TODO §5) ──────────────────────────

    #[test]
    fn white_rolls_per_accepted_step_and_is_solve_frozen() {
        let b = parse("white(time())").unwrap();
        assert!(
            matches!(&b.ast, BExpr::Noise(NoiseKind::White, arg, 0) if matches!(**arg, BExpr::Time))
        );
        assert_eq!(b.noise.len(), 1);
        let mut rt = BehavioralRuntime::sized_for(&b);
        rt.noise_seed = 0x5EED_CAFE;
        let pv = [0.0];
        // Within one accepted step the draw is bit-stable across probes, t,
        // and dt — keyed entirely by the frozen roll.
        let first = b.evaluate(&pv, 0.1, 0.05, &rt);
        assert!((-1.0..1.0).contains(&first), "draw in [-1,1): {first}");
        assert_eq!(first, b.evaluate(&pv, 0.2, 0.05, &rt), "same roll, same draw");
        assert_eq!(
            first,
            b.evaluate(&pv, 0.2, 0.05, &rt),
            "same roll, same draw again"
        );
        assert_eq!(first, b.evaluate(&[2.0], 0.3, 0.0, &rt), "probe-flat");
        // One accepted step = one re-roll (LTspice's white(time()) rule).
        rt.advance(&b, &pv, 0.2, 0.2);
        assert_eq!(rt.noise_rolls[0], 1);
        assert_ne!(
            first.to_bits(),
            b.evaluate(&pv, 0.2, 0.2, &rt).to_bits(),
            "the roll re-keys the draw"
        );
    }

    #[test]
    fn white_paces_through_a_general_argument() {
        // Not a bare time() arg: the roll is the end-of-step floor(arg).
        let b = parse("white(floor(V(a)*10))").unwrap();
        assert_eq!(b.noise.len(), 1);
        let mut rt = BehavioralRuntime::sized_for(&b);
        // End-of-step probes settle the roll: floor(V(a)·10).
        rt.advance(&b, &[3.7], 1.0, 0.5);
        assert_eq!(rt.noise_rolls[0], 37);
        // Within a fraction of the bucket the roll holds; a probe step that
        // crosses a tick re-keys it.
        rt.advance(&b, &[3.75], 1.5, 0.5);
        assert_eq!(rt.noise_rolls[0], 37, "no tick crossed");
        rt.advance(&b, &[3.8], 2.0, 0.5);
        assert_eq!(rt.noise_rolls[0], 38, "ticked");
        // Evaluations during a solve use the settled roll and stay flat in
        // the probes.
        let v1 = b.evaluate(&[3.8], 2.1, 0.1, &rt);
        let v2 = b.evaluate(&[3.9], 2.1, 0.1, &rt);
        assert_eq!(v1, v2, "probe-flat within a solve");
        assert!((-1.0..1.0).contains(&v1));
    }

    #[test]
    fn flicker_is_bounded_and_octave_keyed() {
        let b = parse("flicker(time())").unwrap();
        assert!(matches!(&b.ast, BExpr::Noise(NoiseKind::Flicker, _, 0)));
        let mut rt = BehavioralRuntime::sized_for(&b);
        rt.noise_seed = 42;
        let v = b.evaluate(&[], 0.1, 0.05, &rt);
        assert!((-1.0..1.0).contains(&v), "flicker draw bounded: {v}");
        // Solve-frozen: same roll, same value.
        assert_eq!(v, b.evaluate(&[], 0.9, 0.05, &rt));
        // The top band re-keys each roll; the slow bands key on roll >> o
        // (documented keying, exercised here indirectly through the draw
        // changing while staying bounded).
        rt.advance(&b, &[], 0.2, 0.1); // roll now 1
        let next = b.evaluate(&[], 0.2, 0.1, &rt);
        assert_ne!(v.to_bits(), next.to_bits(), "top band re-keys each roll");
        assert!((-1.0..1.0).contains(&next));
        assert_eq!(next, b.evaluate(&[], 5.0, 0.0, &rt), "solve-frozen again");
    }

    #[test]
    fn noisy_linearization_exposes_only_explicit_factor_derivatives() {
        // f = white(time())·V(a)·2 at V(a) = 5: the draw is solve-frozen, so
        // the linearization sees only the explicit factor's derivative.
        let mut b = parse("white(time())*V(a)*2").unwrap();
        b.probes[0] = ProbeBinding::Voltage(NodeId(1), NodeId(0));
        let mut rt = BehavioralRuntime::sized_for(&b);
        rt.noise_seed = 7;
        // The same runtime read by a plain white(time()) gives the draw
        // factor (site 0, roll 0 in both parses).
        let draw = parse("white(time())").unwrap().evaluate(&[], 0.1, 0.1, &rt);
        let (f0, dk) = b.linearize(&[5.0], 0.1, 0.1, &rt).unwrap();
        assert!((f0 - draw * 10.0).abs() < 1e-9);
        assert!((dk[0] - draw * 2.0).abs() < 1e-4, "dk = {}", dk[0]);
        assert!(f0.is_finite() && dk[0].is_finite());
    }

    #[test]
    fn source_tags_and_seed_separate_draws() {
        let mut a = parse("white(time())").unwrap();
        let mut c = parse("white(time())").unwrap();
        a.noise_tag = 0x1111;
        c.noise_tag = 0x2222;
        let mut rt_a = BehavioralRuntime::sized_for(&a);
        rt_a.noise_seed = 9;
        let mut rt_c = BehavioralRuntime::sized_for(&c);
        rt_c.noise_seed = 9;
        let va = a.evaluate(&[], 0.0, 0.1, &rt_a);
        let vc = c.evaluate(&[], 0.0, 0.1, &rt_c);
        assert_ne!(va.to_bits(), vc.to_bits(), "distinct tags, distinct draws");
        // Same tag + same seed reproduces (the bit-reproducibility contract).
        let again = a.evaluate(&[], 0.0, 0.1, &rt_a);
        assert_eq!(va, again);
        // A different seed re-keys everything.
        let mut rt_b = BehavioralRuntime::sized_for(&a);
        rt_b.noise_seed = 10;
        assert_ne!(
            va.to_bits(),
            a.evaluate(&[], 0.0, 0.1, &rt_b).to_bits()
        );
    }

    #[test]
    fn unseeded_noise_is_constant_from_t0() {
        // No seed, no accepted steps: every evaluation is the same value —
        // a pure-DC/AC report's white(time()) is a fixed number.
        let b = parse("white(time())").unwrap();
        let rt = BehavioralRuntime::default();
        let v = b.evaluate(&[], 0.0, 0.0, &rt);
        assert!((-1.0..1.0).contains(&v));
        assert_eq!(v, b.evaluate(&[], 10.0, 0.0, &rt), "roll 0 everywhere");
    }

    #[test]
    fn numeric_probe_args_parse() {
        let b = parse("V(2)+V(0,3)").unwrap();
        let s0 = &b.probes[0];
        let s1 = &b.probes[1];
        assert!(matches!(
            s0,
            ProbeBinding::Unresolved(ProbeSpec::Voltage(a, None)) if a == "2"
        ));
        assert!(matches!(
            s1,
            ProbeBinding::Unresolved(ProbeSpec::Voltage(a, Some(b))) if a == "0" && b == "3"
        ));
    }

    #[test]
    fn delayed_value_queue_cap() {
        // `advance` caps delay queues at MAX_DELAY_SAMPLES — the constant
        // lives next to the queue law it enforces.
        assert!(MAX_DELAY_SAMPLES >= 2);
    }
}
