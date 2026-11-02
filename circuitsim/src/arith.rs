//! Waveform arithmetic (TODO §6): LTspice's "Add Trace" — derived plot
//! columns computed from a plot's existing ones, named by the expression
//! that made them (`V(out)*I(V1)` for power, `2*V(out)+1`, `V(a,b)`).
//!
//! An expression is parsed once ([`TraceExpr::parse`]) and evaluated per
//! plot ([`TraceExpr::evaluate`]) over whole columns, so the derivative
//! `d()` sees the axis. [`add_expression_traces`] appends the results to a
//! [`RawFile`] as ordinary columns; everything downstream that selects traces
//! by name — the CSV and SVG exporters, the FFT, the `.raw` writer — then
//! treats them like any simulated trace, with no second code path.
//!
//! Grammar: `+ - * /`, `^` (right-associative, binds tighter than unary
//! minus: `-2^2 = -4`), parentheses, numbers with SPICE suffixes (`1k`,
//! `10u`), and these operands:
//!
//! - **probes** — `V(node)`, `I(device)`, and any `Name(text)` spelled like a
//!   column of the plot (`Id(M1)`); `V(a,b)` is `V(a) - V(b)`; `V(0)` and
//!   `V(gnd)` are zero. Matching is case-insensitive.
//! - **bare names** — the axis (`time`, `frequency`) or any other column by
//!   name (a step column such as `r`), else a numeric deck parameter
//!   (the column wins, so a stepped `r` is the run's value), else `pi`.
//! - **functions** — `abs mag ph arg re real im imag db sgn sqrt exp ln log
//!   log10 sin cos tan asin acos atan sinh cosh tanh floor ceil int round d
//!   pow pwr pwrs atan2 hypot min max`. `log` is base 10 like the deck
//!   grammar; `ph` is degrees, `arg` radians; `pwr(x,y)` is `|x|^y`,
//!   `pwrs(x,y)` its signed form `sgn(x)·|x|^y`; `d(x)` differentiates
//!   against the plot's axis (central differences inside, one-sided at the
//!   ends, coincident axis samples read 0). The function set follows
//!   LTspice's waveform-arithmetic list as remembered — it was not checked
//!   against the reference install's help.
//!
//! Complex plots (`.ac`) evaluate in complex arithmetic, so `V(out)/V(in)`
//! is the transfer phasor; real plots evaluate in `f64` (`sqrt(-1)` is NaN,
//! never an imaginary value). A plot that lacks a column an expression
//! names is skipped silently — like `--traces`, requests no plot carries
//! are quiet — but an expression no plot can evaluate is an error, and a
//! malformed expression is an error before any plot is touched.

use crate::error::{Result, SimError};
use crate::params::ParamEnv;
use crate::rawwave::{RawDataset, RawFile, RawVariable};
use num_complex::Complex64;

/// The `Variables:` kind word of an expression column: ngspice's
/// "no particular type" spelling, since the result mixes quantities.
const EXPRESSION_KIND: &str = "notype";

/// Function names and their argument counts.
const FUNCTIONS: &[(&str, usize)] = &[
    ("abs", 1),
    ("mag", 1),
    ("ph", 1),
    ("arg", 1),
    ("re", 1),
    ("real", 1),
    ("im", 1),
    ("imag", 1),
    ("db", 1),
    ("sgn", 1),
    ("sqrt", 1),
    ("exp", 1),
    ("ln", 1),
    ("log", 1),
    ("log10", 1),
    ("sin", 1),
    ("cos", 1),
    ("tan", 1),
    ("asin", 1),
    ("acos", 1),
    ("atan", 1),
    ("sinh", 1),
    ("cosh", 1),
    ("tanh", 1),
    ("floor", 1),
    ("ceil", 1),
    ("int", 1),
    ("round", 1),
    ("d", 1),
    ("pow", 2),
    ("pwr", 2),
    ("pwrs", 2),
    ("atan2", 2),
    ("hypot", 2),
    ("min", 2),
    ("max", 2),
];

const MAX_DEPTH: u32 = 64;

fn arity(name: &str) -> Option<usize> {
    FUNCTIONS.iter().find(|(n, _)| *n == name).map(|(_, a)| *a)
}

/// One parsed expression tree.
#[derive(Debug, Clone, PartialEq)]
enum Node {
    Num(f64),
    /// A bare name: a column of the plot first, the fallback value (a deck
    /// parameter or `pi`) when the plot has no such column.
    Name(String, Option<f64>),
    /// A spelled probe, `V(out)` / `I(V1)` / `Id(M1)`, as the column name.
    Probe(String),
    /// `V(a,b)`: the two node names.
    Difference(String, String),
    Neg(Box<Node>),
    Binary(char, Box<Node>, Box<Node>),
    Call(String, Vec<Node>),
}

/// A parsed waveform expression: the text it was typed as (which also names
/// the column it produces) and its tree.
#[derive(Debug, Clone)]
pub struct TraceExpr {
    text: String,
    root: Node,
}

impl TraceExpr {
    /// Parse one expression. `env` supplies the deck's numeric parameters
    /// for bare names; a syntax error names the offending spot.
    pub fn parse(text: &str, env: &ParamEnv) -> Result<TraceExpr> {
        let text = text.trim();
        let fail = |why: String| SimError::Parse(format!("trace expression '{text}': {why}"));
        if text.is_empty() {
            return Err(fail("empty expression".into()));
        }
        let mut parser = ExprParser {
            chars: text.chars().collect(),
            pos: 0,
            env,
            depth: 0,
        };
        let root = parser.expression().map_err(&fail)?;
        if let Some(extra) = parser.peek() {
            return Err(fail(format!("unexpected '{extra}' after the expression")));
        }
        Ok(TraceExpr {
            text: text.to_string(),
            root,
        })
    }

    /// The expression as typed (trimmed) — the new column's name.
    pub fn text(&self) -> &str {
        &self.text
    }

    /// The expression's column over one plot, one value per row; `None`
    /// when the plot lacks a column the expression names.
    pub fn evaluate(&self, ds: &RawDataset) -> Option<Vec<Complex64>> {
        let frame = Frame {
            ds,
            axis: ds.axis(),
            real: !ds.flags.iter().any(|f| f.eq_ignore_ascii_case("complex")),
        };
        eval(&self.root, &frame)
    }
}

/// Append every expression as a new column of each plot that can evaluate
/// it. An expression is skipped on a plot that already carries a column of
/// that name (case-insensitive) or lacks a column it names; one that no
/// plot takes is an error, as is a syntax error — in both cases `raw` is
/// left untouched. Blank expressions are ignored and repeats added once.
pub fn add_expression_traces(
    raw: &mut RawFile,
    expressions: &[String],
    env: &ParamEnv,
) -> Result<()> {
    let mut parsed: Vec<TraceExpr> = Vec::new();
    for text in expressions {
        if text.trim().is_empty() {
            continue;
        }
        let expr = TraceExpr::parse(text, env)?;
        if !parsed.iter().any(|p| p.text.eq_ignore_ascii_case(&expr.text)) {
            parsed.push(expr);
        }
    }

    let mut matched = vec![false; parsed.len()];
    let mut additions: Vec<Vec<(usize, Vec<Complex64>)>> = Vec::with_capacity(raw.datasets.len());
    for ds in &raw.datasets {
        let mut adds = Vec::new();
        for (k, expr) in parsed.iter().enumerate() {
            let present = ds
                .variables
                .iter()
                .any(|v| v.name.eq_ignore_ascii_case(&expr.text));
            if present {
                matched[k] = true;
            } else if let Some(series) = expr.evaluate(ds) {
                matched[k] = true;
                adds.push((k, series));
            }
        }
        additions.push(adds);
    }
    if let Some(k) = matched.iter().position(|m| !m) {
        return Err(SimError::Analysis(format!(
            "trace expression '{}' matches no plot (no analysis carries every column it names)",
            parsed[k].text
        )));
    }

    for (ds, adds) in raw.datasets.iter_mut().zip(additions) {
        for (k, series) in adds {
            ds.variables.push(RawVariable {
                name: parsed[k].text.clone(),
                kind: EXPRESSION_KIND.to_string(),
            });
            for (row, value) in ds.values.iter_mut().zip(series) {
                row.push(value);
            }
        }
    }
    Ok(())
}

// ─── Parser ──────────────────────────────────────────────────────────────────

struct ExprParser<'a> {
    chars: Vec<char>,
    pos: usize,
    env: &'a ParamEnv,
    depth: u32,
}

type ParseResult = std::result::Result<Node, String>;

impl ExprParser<'_> {
    fn peek(&mut self) -> Option<char> {
        while self.pos < self.chars.len() && self.chars[self.pos].is_whitespace() {
            self.pos += 1;
        }
        self.chars.get(self.pos).copied()
    }

    fn eat(&mut self, c: char) -> bool {
        if self.peek() == Some(c) {
            self.pos += 1;
            true
        } else {
            false
        }
    }

    /// A sub-expression one nesting level down (parentheses, arguments,
    /// chained unary signs), bounded so hostile input cannot overflow the
    /// stack.
    fn nested(&mut self, parse: fn(&mut Self) -> ParseResult) -> ParseResult {
        self.depth += 1;
        if self.depth > MAX_DEPTH {
            return Err("expression nests too deeply".into());
        }
        let node = parse(self);
        self.depth -= 1;
        node
    }

    fn expression(&mut self) -> ParseResult {
        let mut left = self.term()?;
        while let Some(op @ ('+' | '-')) = self.peek() {
            self.pos += 1;
            let right = self.term()?;
            left = Node::Binary(op, Box::new(left), Box::new(right));
        }
        Ok(left)
    }

    fn term(&mut self) -> ParseResult {
        let mut left = self.unary()?;
        while let Some(op @ ('*' | '/')) = self.peek() {
            self.pos += 1;
            let right = self.unary()?;
            left = Node::Binary(op, Box::new(left), Box::new(right));
        }
        Ok(left)
    }

    fn unary(&mut self) -> ParseResult {
        match self.peek() {
            Some('-') => {
                self.pos += 1;
                Ok(Node::Neg(Box::new(self.nested(Self::unary)?)))
            }
            Some('+') => {
                self.pos += 1;
                self.nested(Self::unary)
            }
            _ => self.power(),
        }
    }

    fn power(&mut self) -> ParseResult {
        let base = self.primary()?;
        if self.eat('^') {
            let exponent = self.nested(Self::unary)?;
            Ok(Node::Binary('^', Box::new(base), Box::new(exponent)))
        } else {
            Ok(base)
        }
    }

    fn primary(&mut self) -> ParseResult {
        match self.peek() {
            None => Err("the expression ends where an operand was expected".into()),
            Some('(') => {
                self.pos += 1;
                let inner = self.nested(Self::expression)?;
                if self.eat(')') {
                    Ok(inner)
                } else {
                    Err("missing ')'".into())
                }
            }
            Some(c) if c.is_ascii_digit() || c == '.' => self.number(),
            Some(c) if c.is_ascii_alphabetic() || c == '_' => self.name(),
            Some(other) => Err(format!("unexpected '{other}'")),
        }
    }

    /// A number with its SPICE suffix, read the way the parameter language
    /// reads one (`1k`, `2.2u`, `1e-3`).
    fn number(&mut self) -> ParseResult {
        let n = self.chars.len();
        let start = self.pos;
        while self.pos < n && (self.chars[self.pos].is_ascii_digit() || self.chars[self.pos] == '.')
        {
            self.pos += 1;
        }
        if self.pos < n && matches!(self.chars[self.pos], 'e' | 'E') {
            let mut j = self.pos + 1;
            if j < n && matches!(self.chars[j], '+' | '-') {
                j += 1;
            }
            if j < n && self.chars[j].is_ascii_digit() {
                while j < n && self.chars[j].is_ascii_digit() {
                    j += 1;
                }
                self.pos = j;
            }
        }
        while self.pos < n && self.chars[self.pos].is_ascii_alphabetic() {
            self.pos += 1;
        }
        let text: String = self.chars[start..self.pos].iter().collect();
        crate::units::parse_value(&text)
            .map(Node::Num)
            .map_err(|e| format!("bad number '{text}': {e}"))
    }

    /// A name: a function call, a probe, or a bare column/parameter name.
    fn name(&mut self) -> ParseResult {
        let n = self.chars.len();
        let start = self.pos;
        while self.pos < n && (self.chars[self.pos].is_ascii_alphanumeric() || self.chars[self.pos] == '_')
        {
            self.pos += 1;
        }
        let ident: String = self.chars[start..self.pos].iter().collect();
        let lower = ident.to_ascii_lowercase();

        if !self.eat('(') {
            let fallback = self
                .env
                .lookup_num(&lower)
                .or((lower == "pi").then_some(std::f64::consts::PI));
            return Ok(Node::Name(ident, fallback));
        }

        if let Some(wanted) = arity(&lower) {
            let mut args = Vec::new();
            if self.peek() != Some(')') {
                loop {
                    args.push(self.nested(Self::expression)?);
                    if !self.eat(',') {
                        break;
                    }
                }
            }
            if !self.eat(')') {
                return Err(format!("missing ')' after {lower}("));
            }
            if args.len() != wanted {
                return Err(format!(
                    "{lower}() takes {wanted} argument(s), got {}",
                    args.len()
                ));
            }
            return Ok(Node::Call(lower, args));
        }

        // Anything else followed by `(` is a probe: node and device names
        // carry `.`, `:`, `#`, `/` and the like, so the inner text is kept
        // raw up to the matching parenthesis.
        let inner_start = self.pos;
        let mut open = 1u32;
        while self.pos < n {
            match self.chars[self.pos] {
                '(' => open += 1,
                ')' => {
                    open -= 1;
                    if open == 0 {
                        break;
                    }
                }
                _ => {}
            }
            self.pos += 1;
        }
        if open != 0 {
            return Err(format!("missing ')' after {ident}("));
        }
        let inner: String = self.chars[inner_start..self.pos].iter().collect();
        self.pos += 1;
        let inner = inner.trim();
        if inner.is_empty() {
            return Err(format!("{ident}() names nothing"));
        }
        if lower == "v" {
            if let Some((a, b)) = inner.split_once(',') {
                let (a, b) = (a.trim(), b.trim());
                if a.is_empty() || b.is_empty() {
                    return Err(format!("{ident}({inner}) is missing a node"));
                }
                return Ok(Node::Difference(a.to_string(), b.to_string()));
            }
        }
        Ok(Node::Probe(format!("{ident}({inner})")))
    }
}

// ─── Evaluator ───────────────────────────────────────────────────────────────

struct Frame<'a> {
    ds: &'a RawDataset,
    axis: Vec<f64>,
    /// Real plots compute in `f64`; complex plots in full complex arithmetic.
    real: bool,
}

fn real_value(x: f64) -> Complex64 {
    Complex64::new(x, 0.0)
}

/// Real plots never carry an imaginary part, whatever a product of
/// infinities left behind.
fn settle(z: Complex64, real: bool) -> Complex64 {
    if real {
        real_value(z.re)
    } else {
        z
    }
}

fn sgn(x: f64) -> f64 {
    if x > 0.0 {
        1.0
    } else if x < 0.0 {
        -1.0
    } else {
        0.0
    }
}

fn is_ground(node: &str) -> bool {
    node == "0" || node.eq_ignore_ascii_case("gnd")
}

/// One side of `V(a,b)`: ground is zero, any other node its `V(node)` column.
fn voltage(node: &str, f: &Frame) -> Option<Vec<Complex64>> {
    if is_ground(node) {
        Some(vec![real_value(0.0); f.axis.len()])
    } else {
        f.ds.trace(&format!("V({node})"))
    }
}

fn eval(node: &Node, f: &Frame) -> Option<Vec<Complex64>> {
    let rows = f.axis.len();
    match node {
        Node::Num(v) => Some(vec![real_value(*v); rows]),
        Node::Name(name, fallback) => f
            .ds
            .trace(name)
            .or_else(|| fallback.map(|v| vec![real_value(v); rows])),
        Node::Probe(name) => {
            let inner = name
                .split_once('(')
                .map(|(_, rest)| rest.trim_end_matches(')'))
                .unwrap_or_default();
            if name[..1].eq_ignore_ascii_case("v") && is_ground(inner) {
                Some(vec![real_value(0.0); rows])
            } else {
                f.ds.trace(name)
            }
        }
        Node::Difference(a, b) => {
            let (a, b) = (voltage(a, f)?, voltage(b, f)?);
            Some(a.into_iter().zip(b).map(|(a, b)| settle(a - b, f.real)).collect())
        }
        Node::Neg(inner) => {
            Some(eval(inner, f)?.into_iter().map(|z| settle(-z, f.real)).collect())
        }
        Node::Binary(op, left, right) => {
            let (l, r) = (eval(left, f)?, eval(right, f)?);
            Some(
                l.into_iter()
                    .zip(r)
                    .map(|(a, b)| settle(binary(*op, a, b, f.real), f.real))
                    .collect(),
            )
        }
        Node::Call(name, args) => {
            let columns: Option<Vec<Vec<Complex64>>> = args.iter().map(|a| eval(a, f)).collect();
            Some(call(name, columns?, f))
        }
    }
}

fn binary(op: char, a: Complex64, b: Complex64, real: bool) -> Complex64 {
    match op {
        '+' => a + b,
        '-' => a - b,
        '*' => a * b,
        '/' if real => real_value(a.re / b.re),
        '/' => a / b,
        _ => power(a, b, real),
    }
}

fn power(a: Complex64, b: Complex64, real: bool) -> Complex64 {
    if real {
        real_value(a.re.powf(b.re))
    } else if a == Complex64::new(0.0, 0.0) {
        real_value(if b == Complex64::new(0.0, 0.0) { 1.0 } else { 0.0 })
    } else {
        a.powc(b)
    }
}

fn call(name: &str, args: Vec<Vec<Complex64>>, f: &Frame) -> Vec<Complex64> {
    if name == "d" {
        return derivative(&args[0], &f.axis);
    }
    if args.len() == 1 {
        args[0]
            .iter()
            .map(|&z| settle(apply1(name, z, f.real), f.real))
            .collect()
    } else {
        args[0]
            .iter()
            .zip(&args[1])
            .map(|(&a, &b)| settle(apply2(name, a, b, f.real), f.real))
            .collect()
    }
}

fn apply1(name: &str, z: Complex64, real: bool) -> Complex64 {
    // A function with a real and a complex reading: `f64` on real plots
    // (domain errors are NaN), the analytic continuation on complex ones.
    let analytic = |on_real: fn(f64) -> f64, on_complex: fn(Complex64) -> Complex64| {
        if real {
            real_value(on_real(z.re))
        } else {
            on_complex(z)
        }
    };
    match name {
        "abs" | "mag" => real_value(z.norm()),
        "ph" => real_value(z.arg().to_degrees()),
        "arg" => real_value(z.arg()),
        "re" | "real" => real_value(z.re),
        "im" | "imag" => real_value(z.im),
        "db" => real_value(20.0 * z.norm().log10()),
        "sgn" => real_value(sgn(z.re)),
        "floor" => real_value(z.re.floor()),
        "ceil" => real_value(z.re.ceil()),
        "int" => real_value(z.re.trunc()),
        "round" => real_value(z.re.round()),
        "sqrt" => analytic(f64::sqrt, Complex64::sqrt),
        "exp" => analytic(f64::exp, Complex64::exp),
        "ln" => analytic(f64::ln, Complex64::ln),
        "log" | "log10" => analytic(f64::log10, |w| w.ln() / std::f64::consts::LN_10),
        "sin" => analytic(f64::sin, Complex64::sin),
        "cos" => analytic(f64::cos, Complex64::cos),
        "tan" => analytic(f64::tan, Complex64::tan),
        "asin" => analytic(f64::asin, Complex64::asin),
        "acos" => analytic(f64::acos, Complex64::acos),
        "atan" => analytic(f64::atan, Complex64::atan),
        "sinh" => analytic(f64::sinh, Complex64::sinh),
        "cosh" => analytic(f64::cosh, Complex64::cosh),
        "tanh" => analytic(f64::tanh, Complex64::tanh),
        _ => real_value(f64::NAN),
    }
}

fn apply2(name: &str, a: Complex64, b: Complex64, real: bool) -> Complex64 {
    match name {
        "pow" => power(a, b, real),
        "pwr" => real_value(a.norm().powf(b.re)),
        "pwrs" => real_value(sgn(a.re) * a.norm().powf(b.re)),
        "atan2" => real_value(a.re.atan2(b.re)),
        "hypot" => real_value(a.norm().hypot(b.norm())),
        "min" => real_value(a.re.min(b.re)),
        "max" => real_value(a.re.max(b.re)),
        _ => real_value(f64::NAN),
    }
}

/// `d(x)`: the derivative against the plot's axis — central differences
/// inside the span, one-sided at both ends. Where the two samples it
/// differences share an axis value (a doubled breakpoint) the slope reads 0.
fn derivative(y: &[Complex64], axis: &[f64]) -> Vec<Complex64> {
    let n = y.len();
    (0..n)
        .map(|i| {
            let (lo, hi) = (i.saturating_sub(1), (i + 1).min(n - 1));
            let dx = axis[hi] - axis[lo];
            if dx != 0.0 && dx.is_finite() {
                (y[hi] - y[lo]) / dx
            } else {
                real_value(0.0)
            }
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    use crate::export::{csv_document, svg_document};
    use crate::fft::{spectra, Window};
    use crate::netlist::parse;
    use crate::params::ParamValue;
    use crate::rawwave::build;
    use crate::runs::run;
    use approx::assert_relative_eq;

    const TRAN_DECK: &str = "\
V1 in 0 dc 5
R1 in out 1k
R2 out 0 3k
.tran 10u 100u
.end
";

    const AC_DECK: &str = "V1 in 0 ac 1\nR1 in out 1k\nC1 out 0 1u\n\
         .ac dec 2 1k 10k\n.end\n";

    fn raw_of(deck: &str) -> RawFile {
        let netlist = parse(deck).expect("deck parses");
        let result = run(&netlist).expect("deck runs");
        build("test", &netlist, &result)
    }

    fn env() -> ParamEnv {
        ParamEnv::default()
    }

    fn texts(items: &[&str]) -> Vec<String> {
        items.iter().map(|s| s.to_string()).collect()
    }

    /// A hand-built real plot: `time` plus the named columns.
    fn plot(time: &[f64], columns: &[(&str, &[f64])]) -> RawDataset {
        let mut variables = vec![RawVariable {
            name: "time".into(),
            kind: "time".into(),
        }];
        variables.extend(columns.iter().map(|(name, _)| RawVariable {
            name: name.to_string(),
            kind: "voltage".into(),
        }));
        let values = (0..time.len())
            .map(|r| {
                let mut row = vec![real_value(time[r])];
                row.extend(columns.iter().map(|(_, c)| real_value(c[r])));
                row
            })
            .collect();
        RawDataset {
            title: "t".into(),
            plotname: "Transient Analysis".into(),
            flags: vec!["real".into(), "forward".into()],
            variables,
            values,
            steps: Vec::new(),
        }
    }

    fn eval_text(text: &str, ds: &RawDataset) -> Vec<f64> {
        TraceExpr::parse(text, &env())
            .expect("parses")
            .evaluate(ds)
            .expect("evaluates")
            .iter()
            .map(|z| z.re)
            .collect()
    }

    fn constant(text: &str) -> f64 {
        eval_text(text, &plot(&[0.0], &[]))[0]
    }

    /// Power: the row-wise product of two raw columns, and the 4.6875 mW the
    /// divider dissipates in the source (3.75 V across R2 at 1.25 mA).
    #[test]
    fn power_trace_is_the_columnwise_product() {
        let mut raw = raw_of(TRAN_DECK);
        add_expression_traces(&mut raw, &texts(&["V(out)*I(V1)"]), &env()).unwrap();
        let ds = &raw.datasets[0];
        let power = ds.trace("V(out)*I(V1)").expect("column added");
        let v = ds.trace("V(out)").unwrap();
        let i = ds.trace("I(V1)").unwrap();
        assert_eq!(power.len(), v.len());
        for ((p, v), i) in power.iter().zip(&v).zip(&i) {
            assert_relative_eq!(p.re, v.re * i.re, max_relative = 1e-12);
        }
        assert_relative_eq!(power[3].re.abs(), 4.6875e-3, max_relative = 1e-6);
        assert_eq!(ds.variables.last().unwrap().kind, EXPRESSION_KIND);
    }

    #[test]
    fn scaled_offset_and_combined_traces() {
        let raw = raw_of(TRAN_DECK);
        let ds = &raw.datasets[0];
        assert_relative_eq!(eval_text("2*V(out)+1", ds)[2], 8.5, max_relative = 1e-9);
        assert_relative_eq!(eval_text("V(in) - V(out)", ds)[2], 1.25, max_relative = 1e-9);
        assert_relative_eq!(eval_text("(V(in)-V(out))/1k", ds)[2], 1.25e-3, max_relative = 1e-9);
        assert_relative_eq!(eval_text("V(out)*1e3", ds)[2], 3750.0, max_relative = 1e-9);
    }

    #[test]
    fn differential_probe_and_ground() {
        let raw = raw_of(TRAN_DECK);
        let ds = &raw.datasets[0];
        assert_relative_eq!(eval_text("V(in,out)", ds)[1], 1.25, max_relative = 1e-9);
        assert_relative_eq!(eval_text("V(out,0)", ds)[1], 3.75, max_relative = 1e-9);
        assert_relative_eq!(eval_text("V(0,out)", ds)[1], -3.75, max_relative = 1e-9);
        assert_relative_eq!(eval_text("V(out,gnd)", ds)[1], 3.75, max_relative = 1e-9);
        assert_eq!(eval_text("V(gnd)", ds)[1], 0.0);
    }

    #[test]
    fn probes_match_case_insensitively() {
        let raw = raw_of(TRAN_DECK);
        let ds = &raw.datasets[0];
        let lower = eval_text("v(OUT)*i(v1)", ds);
        let upper = eval_text("V(out)*I(V1)", ds);
        assert_eq!(lower, upper);
    }

    #[test]
    fn precedence_and_associativity() {
        assert_eq!(constant("1+2*3"), 7.0);
        assert_eq!(constant("(1+2)*3"), 9.0);
        assert_eq!(constant("-2^2"), -4.0);
        assert_eq!(constant("2^3^2"), 512.0);
        assert_eq!(constant("2^-1"), 0.5);
        assert_eq!(constant("8/4/2"), 1.0);
        assert_eq!(constant("10-4-3"), 3.0);
        assert_eq!(constant("--3"), 3.0);
        assert_eq!(constant("+3"), 3.0);
    }

    #[test]
    fn numbers_take_spice_suffixes() {
        assert_relative_eq!(constant("1k"), 1e3);
        assert_relative_eq!(constant("2.2u"), 2.2e-6);
        assert_relative_eq!(constant("1meg*2"), 2e6);
        assert_relative_eq!(constant("1e-3"), 1e-3);
    }

    #[test]
    fn functions_on_real_plots() {
        assert_relative_eq!(constant("sqrt(16)"), 4.0);
        assert_relative_eq!(constant("abs(-3)"), 3.0);
        assert_relative_eq!(constant("log(1000)"), 3.0, max_relative = 1e-12);
        assert_relative_eq!(constant("ln(exp(2))"), 2.0, max_relative = 1e-12);
        assert_relative_eq!(constant("db(10)"), 20.0, max_relative = 1e-12);
        assert_relative_eq!(constant("pow(2,10)"), 1024.0);
        assert_relative_eq!(constant("pwr(-2,2)"), 4.0);
        assert_relative_eq!(constant("pwrs(-2,2)"), -4.0);
        assert_relative_eq!(constant("hypot(3,4)"), 5.0);
        assert_relative_eq!(constant("atan2(1,1)"), std::f64::consts::FRAC_PI_4);
        assert_eq!(constant("min(3,2)"), 2.0);
        assert_eq!(constant("max(3,2)"), 3.0);
        assert_eq!(constant("sgn(-7)"), -1.0);
        assert_eq!(constant("sgn(0)"), 0.0);
        assert_eq!(constant("floor(2.7)"), 2.0);
        assert_eq!(constant("ceil(2.1)"), 3.0);
        assert_eq!(constant("int(-2.7)"), -2.0);
        assert_relative_eq!(constant("pi"), std::f64::consts::PI);
        assert_relative_eq!(constant("sin(pi/2)"), 1.0, max_relative = 1e-12);
        assert_relative_eq!(constant("ph(-1)"), 180.0);
    }

    #[test]
    fn domain_errors_on_real_plots_are_nan_not_imaginary() {
        let ds = plot(&[0.0, 1.0], &[]);
        let series = TraceExpr::parse("sqrt(-1)", &env())
            .unwrap()
            .evaluate(&ds)
            .unwrap();
        assert!(series[0].re.is_nan());
        assert_eq!(series[0].im, 0.0);
    }

    /// `d()` differentiates against the axis: a ramp reads its slope, the
    /// ends included; a doubled axis sample reads 0.
    #[test]
    fn derivative_follows_the_axis() {
        let ds = plot(&[0.0, 1.0, 2.0, 4.0], &[("V(a)", &[0.0, 3.0, 6.0, 12.0])]);
        for slope in eval_text("d(V(a))", &ds) {
            assert_relative_eq!(slope, 3.0, max_relative = 1e-12);
        }
        let squared = plot(&[0.0, 1.0, 2.0], &[("V(a)", &[0.0, 1.0, 4.0])]);
        let slopes = eval_text("d(V(a))", &squared);
        assert_relative_eq!(slopes[0], 1.0);
        assert_relative_eq!(slopes[1], 2.0);
        assert_relative_eq!(slopes[2], 3.0);
        let doubled = plot(&[1.0, 1.0], &[("V(a)", &[0.0, 5.0])]);
        assert_eq!(eval_text("d(V(a))", &doubled), vec![0.0, 0.0]);
        let single = plot(&[0.0], &[("V(a)", &[7.0])]);
        assert_eq!(eval_text("d(V(a))", &single), vec![0.0]);
    }

    #[test]
    fn the_axis_and_step_columns_are_bare_names() {
        let ds = plot(&[0.0, 1e-3, 2e-3], &[("V(a)", &[1.0, 2.0, 3.0])]);
        assert_eq!(eval_text("time*1000", &ds), vec![0.0, 1.0, 2.0]);
        assert_eq!(eval_text("TIME", &ds), vec![0.0, 1e-3, 2e-3]);
    }

    #[test]
    fn deck_parameters_resolve_and_columns_win() {
        let mut env = ParamEnv::default();
        env.params.insert("gain".into(), ParamValue::Num(10.0));
        env.params.insert("r".into(), ParamValue::Num(1000.0));
        let ds = plot(&[0.0, 1.0], &[("V(a)", &[1.0, 2.0]), ("r", &[2200.0, 2200.0])]);
        let gained = TraceExpr::parse("gain*V(a)", &env).unwrap().evaluate(&ds).unwrap();
        assert_eq!(gained.iter().map(|z| z.re).collect::<Vec<_>>(), vec![10.0, 20.0]);
        // The step column `r` carries the run's value, not the deck's base.
        let stepped = TraceExpr::parse("r/1k", &env).unwrap().evaluate(&ds).unwrap();
        assert_relative_eq!(stepped[0].re, 2.2);
        // With no column, the parameter answers.
        let bare = plot(&[0.0], &[]);
        let from_param = TraceExpr::parse("r/1k", &env).unwrap().evaluate(&bare).unwrap();
        assert_relative_eq!(from_param[0].re, 1.0);
    }

    /// AC plots compute in complex arithmetic: the RC's transfer phasor at
    /// 1 kHz is 1/(1 + j·2π·1000·1k·1u) — |H| = 0.15717, ∠ = −80.96°.
    #[test]
    fn complex_plots_use_complex_arithmetic() {
        let mut raw = raw_of(AC_DECK);
        add_expression_traces(
            &mut raw,
            &texts(&["V(out)/V(in)", "db(V(out))", "ph(V(out))", "re(V(out))", "im(V(out))"]),
            &env(),
        )
        .unwrap();
        let ds = &raw.datasets[0];
        assert_relative_eq!(ds.values[0][0].re, 1000.0);
        let w_rc = 2.0 * std::f64::consts::PI * 1000.0 * 1e3 * 1e-6;
        let h = Complex64::new(1.0, 0.0) / Complex64::new(1.0, w_rc);
        let at = |name: &str| ds.trace(name).unwrap()[0];
        let transfer = at("V(out)/V(in)");
        assert_relative_eq!(transfer.re, h.re, max_relative = 1e-6);
        assert_relative_eq!(transfer.im, h.im, max_relative = 1e-6);
        assert_relative_eq!(at("db(V(out))").re, 20.0 * h.norm().log10(), max_relative = 1e-6);
        assert_relative_eq!(at("ph(V(out))").re, h.arg().to_degrees(), max_relative = 1e-6);
        assert_relative_eq!(at("re(V(out))").re, h.re, max_relative = 1e-6);
        assert_relative_eq!(at("im(V(out))").re, h.im, max_relative = 1e-6);
    }

    #[test]
    fn duplicate_and_existing_columns_are_not_added_twice() {
        let mut raw = raw_of(TRAN_DECK);
        let before = raw.datasets[0].variables.len();
        add_expression_traces(&mut raw, &texts(&["V(out)", "v(OUT)"]), &env()).unwrap();
        assert_eq!(raw.datasets[0].variables.len(), before);
        add_expression_traces(&mut raw, &texts(&["V(out)*2", " V(out)*2 "]), &env()).unwrap();
        assert_eq!(raw.datasets[0].variables.len(), before + 1);
        for row in &raw.datasets[0].values {
            assert_eq!(row.len(), before + 1);
        }
    }

    #[test]
    fn a_plot_missing_a_probe_is_skipped_but_an_orphan_expression_errors() {
        // Two analyses: the transient carries V(out); the operating point's
        // columns are node voltages too, but `.tran`-only `time` is missing
        // from a frequency plot.
        let mut raw = raw_of(&format!("{AC_DECK}").replace(".end\n", ".tran 10u 100u\n.end\n"));
        assert!(raw.datasets.len() >= 2);
        add_expression_traces(&mut raw, &texts(&["time*2"]), &env()).unwrap();
        let with_time = raw
            .datasets
            .iter()
            .filter(|ds| ds.trace("time*2").is_some())
            .count();
        assert_eq!(with_time, 1, "only the transient has a time axis");

        let before = raw.datasets.clone();
        let err = add_expression_traces(&mut raw, &texts(&["V(nope)*2"]), &env()).unwrap_err();
        assert!(err.to_string().contains("V(nope)*2"), "{err}");
        assert_eq!(
            raw.datasets.iter().map(|d| d.variables.len()).collect::<Vec<_>>(),
            before.iter().map(|d| d.variables.len()).collect::<Vec<_>>(),
            "a failed call leaves the file untouched"
        );
    }

    #[test]
    fn syntax_errors_are_loud_before_anything_changes() {
        let mut raw = raw_of(TRAN_DECK);
        let before = raw.datasets[0].variables.len();
        for bad in [
            "V(out",
            "2*",
            "(1+2",
            "1+2)",
            "sin(1,2)",
            "sin()",
            "V()",
            "V(a,)",
            "3 $ 4",
            "1.2.3",
            "2 3",
        ] {
            let err = add_expression_traces(&mut raw, &texts(&["V(out)*2", bad]), &env())
                .expect_err(bad);
            assert!(matches!(err, SimError::Parse(_)), "{bad}: {err}");
            assert!(err.to_string().contains(bad.trim()), "{bad}: {err}");
        }
        assert_eq!(raw.datasets[0].variables.len(), before);
    }

    #[test]
    fn blank_expressions_are_ignored_and_deep_nesting_is_bounded() {
        let mut raw = raw_of(TRAN_DECK);
        let before = raw.datasets[0].variables.len();
        add_expression_traces(&mut raw, &texts(&["", "   "]), &env()).unwrap();
        assert_eq!(raw.datasets[0].variables.len(), before);
        let deep = format!("{}1{}", "(".repeat(5000), ")".repeat(5000));
        let err = TraceExpr::parse(&deep, &env()).unwrap_err();
        assert!(err.to_string().contains("deeply"), "{err}");
        let signs = format!("{}1", "-".repeat(5000));
        assert!(TraceExpr::parse(&signs, &env()).is_err());
    }

    /// One column feeds every trace-selecting consumer: the CSV keeps it by
    /// its expression name, the SVG draws it, and the FFT spectrum takes it
    /// like any simulated trace.
    #[test]
    fn expression_columns_flow_through_the_exporters_and_the_fft() {
        let netlist = parse(
            "V1 in 0 sin(0 1 1k)\nR1 in out 1k\nR2 out 0 1k\n.tran 5u 4m\n.end\n",
        )
        .unwrap();
        let result = run(&netlist).unwrap();
        let mut raw = build("test", &netlist, &result);
        add_expression_traces(&mut raw, &texts(&["V(out)*I(V1)", "2*V(out)"]), &env()).unwrap();

        let csv = csv_document(&raw, &[], &texts(&["V(out)*I(V1)"]));
        let header = csv.lines().nth(1).unwrap();
        assert_eq!(header, "time,V(out)*I(V1)", "{header}");

        let svg = svg_document(&raw, &texts(&["V(out)*I(V1)"]));
        assert!(svg.contains("V(out)*I(V1)"));
        assert!(svg.contains("<polyline"));

        let scaled = spectra(&raw, &texts(&["2*V(out)"]), Window::Hann);
        assert_eq!(scaled.datasets.len(), 1);
        assert_eq!(scaled.datasets[0].plotname, "2*V(out) (FFT)");
        // V(out) is a 0.5 V sine at 1 kHz, so twice it reads 1 V there.
        let peak = scaled.datasets[0]
            .values
            .iter()
            .map(|row| (row[0].re, row[1].re))
            .min_by(|a, b| (a.0 - 1000.0).abs().total_cmp(&(b.0 - 1000.0).abs()))
            .unwrap();
        assert_relative_eq!(peak.1, 1.0, max_relative = 0.05);
    }

    #[test]
    fn the_expression_columns_survive_a_raw_round_trip() {
        let mut raw = raw_of(TRAN_DECK);
        add_expression_traces(&mut raw, &texts(&["V(out)*I(V1)"]), &env()).unwrap();
        let bytes = crate::rawwave::to_bytes(&raw, crate::rawwave::RawFlavor::Ascii);
        let back = crate::rawwave::read(&bytes).unwrap();
        assert!(back.datasets[0].trace("V(out)*I(V1)").is_some());
    }
}
