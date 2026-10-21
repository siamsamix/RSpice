//! Element cards: one parser per element family beside the canonical

//! emission of that family's cards.

use super::*;
use crate::circuit::{AbmForm, AbmMeasure};
use super::dotcards::ignored_tokens;
use super::waveforms::waveform_text;
use super::nodeplan::{element_letter, parse_node};
use super::subckts::{poly_count, q_is_four_terminal};
use super::waveforms::parse_source_value_specs;

// ─── Element cards ───────────────────────────────────────────────────────────

/// SPICE syntax:
///   Dname  Anode  Cathode  ModelName  [AREA=<a>] [<a>] [OFF] [M=<m>]
///
/// The junction area multiplier scales the saturation current in the stamp
/// (TODO §2); the bare-numeric form (area as the 5th token) and the
/// `AREA=` keyword are both accepted.
pub(super) fn parse_diode(
    tokens: &[&str],
    circuit: &mut Circuit,
    plan: &HashMap<String, usize>,
    warnings: &mut Vec<String>,
    line_no: usize,
) -> Result<()> {
    if tokens.len() < 4 {
        return Err(SimError::Parse(
            "Diode requires: Dname Anode Cathode Model [AREA=<a>]".into(),
        ));
    }
    let anode   = parse_node(tokens[1], circuit, plan)?;
    let cathode = parse_node(tokens[2], circuit, plan)?;
    let model = tokens[3].to_ascii_lowercase();

    let mut area = 1.0;
    for tok in &tokens[4..] {
        let lower = tok.to_ascii_lowercase();
        if let Some(rest) = lower.strip_prefix("area=") {
            area = parse_value(rest).map_err(SimError::Parse)?;
        } else if lower.strip_prefix("m=").is_some() {
            warnings.push(format!(
                "line {line_no}: diode {}: the M= multiplier is parsed but not applied",
                tokens[0]
            ));
        } else if lower == "off" {
            warnings.push(format!(
                "line {line_no}: diode {}: OFF is parsed but not applied",
                tokens[0]
            ));
        } else if let Ok(a) = parse_value(tok) {
            area = a;
        } else {
            warnings.push(format!(
                "line {line_no}: diode {}: token '{tok}' is not supported — ignored",
                tokens[0]
            ));
        }
    }

    circuit.diodes.push(Diode { name: tokens[0].to_string(), anode, cathode, model, area });
    Ok(())
}

/// Parse a MOSFET element line.
///
/// SPICE syntax:
///   Mname  Nd  Ng  Ns  Nb  ModelName  [W=<w>]  [L=<l>]
///
/// W and L default to 1µm if not supplied.
pub(super) fn parse_mosfet(
    tokens: &[&str],
    circuit: &mut Circuit,
    plan: &HashMap<String, usize>,
    warnings: &mut Vec<String>,
    line_no: usize,
) -> Result<()> {
    if tokens.len() < 6 {
        return Err(SimError::Parse(
            "MOSFET requires: Mname Nd Ng Ns Nb ModelName [W=<w>] [L=<l>] \
             [AD=<ad>] [AS=<as>] [PS=<ps>] [PD=<pd>]"
                .into(),
        ));
    }
    let nd = parse_node(tokens[1], circuit, plan)?;
    let ng = parse_node(tokens[2], circuit, plan)?;
    let ns = parse_node(tokens[3], circuit, plan)?;
    let nb = parse_node(tokens[4], circuit, plan)?;
    let model = tokens[5].to_ascii_lowercase();

    let mut w = 1e-6; // 1 µm default
    let mut l = 1e-6;
    // Per-instance junction sizing (TODO §2): parsed and stored for the
    // capacitance work (TODO §5); not stamped yet.
    let mut ad = 0.0;
    let mut asr = 0.0;
    let mut ps = 0.0;
    let mut pd = 0.0;

    for tok in &tokens[6..] {
        let lower = tok.to_ascii_lowercase();
        if let Some(rest) = lower.strip_prefix("w=") {
            w = parse_value(rest).map_err(SimError::Parse)?;
        } else if let Some(rest) = lower.strip_prefix("l=") {
            l = parse_value(rest).map_err(SimError::Parse)?;
        } else if let Some(rest) = lower.strip_prefix("ad=") {
            ad = parse_value(rest).map_err(SimError::Parse)?;
        } else if let Some(rest) = lower.strip_prefix("as=") {
            asr = parse_value(rest).map_err(SimError::Parse)?;
        } else if let Some(rest) = lower.strip_prefix("ps=") {
            ps = parse_value(rest).map_err(SimError::Parse)?;
        } else if let Some(rest) = lower.strip_prefix("pd=") {
            pd = parse_value(rest).map_err(SimError::Parse)?;
        } else if lower.strip_prefix("m=").is_some() {
            warnings.push(format!(
                "line {line_no}: MOSFET {}: the M= multiplier is parsed but not applied",
                tokens[0]
            ));
        } else if lower == "off" {
            warnings.push(format!(
                "line {line_no}: MOSFET {}: OFF is parsed but not applied",
                tokens[0]
            ));
        } else {
            warnings.push(format!(
                "line {line_no}: MOSFET {}: token '{tok}' is not supported — ignored",
                tokens[0]
            ));
        }
    }

    circuit.mosfets.push(Mosfet {
        name: tokens[0].to_string(),
        nd, ng, ns, nb, model, w, l,
        ad, asr, ps, pd,
    });
    Ok(())
}

/// Parse one `E`/`F`/`G`/`H` controlled source, either the plain gain form
/// (`E out+ out- ctrl+ ctrl- gain`) or `POLY(n)` with one control per
/// variable and `n+1` coefficients (constant first). An ABM card
/// (`E out+ out- VALUE={expr} …`, TODO §5) dispatches to
/// [`parse_abm_source`] instead.
pub(super) fn parse_controlled_source(
    tokens: &[&str],
    circuit: &mut Circuit,
    plan: &HashMap<String, usize>,
    line_no: usize,
    warnings: &mut Vec<String>,
    params: &HashMap<String, ParamValue>,
) -> Result<()> {
    let kind = element_letter(tokens[0]).to_ascii_uppercase();
    // ABM cards claim the keyword-then-brace shape, which the classic
    // grammar cannot produce (control names are bare tokens, never `=` with
    // a following group). A control node named `freq`/`table`/`value` still
    // takes the classic path because no brace group follows it.
    if tokens.len() >= 4 && abm_card(tokens) {
        return parse_abm_source(tokens, circuit, plan, line_no, warnings, params);
    }
    if tokens.len() < 5 {
        return Err(SimError::Parse(format!(
            "line {line_no}: {kind} source requires: {kind}name out+ out- <controls> <gain|POLY(n) …>"
        )));
    }
    let out_p = parse_node(tokens[1], circuit, plan)?;
    let out_n = parse_node(tokens[2], circuit, plan)?;
    let voltage_controlled = matches!(kind, 'E' | 'G');

    let is_poly = tokens[3].to_ascii_lowercase().starts_with("poly");
    let (controls, coeffs, consumed): (Vec<ControlVar>, Vec<f64>, usize) = if is_poly {
        let n = poly_count(tokens[3]).ok_or_else(|| {
            SimError::Parse(format!(
                "line {line_no}: {kind} source '{}': malformed POLY(n) token '{}'",
                tokens[0], tokens[3]
            ))
        })?;
        if n == 0 {
            return Err(SimError::Parse(format!(
                "line {line_no}: {kind} source '{}': POLY needs at least one control",
                tokens[0]
            )));
        }
        if voltage_controlled {
            let need = 4 + 2 * n + (n + 1);
            if tokens.len() < need {
                return Err(SimError::Parse(format!(
                    "line {line_no}: {kind} source '{}': POLY({n}) needs {n} node pairs and {} coefficients",
                    tokens[0],
                    n + 1
                )));
            }
            let mut controls = Vec::with_capacity(n);
            for k in 0..n {
                let a = parse_node(tokens[4 + 2 * k], circuit, plan)?;
                let b = parse_node(tokens[5 + 2 * k], circuit, plan)?;
                controls.push(ControlVar::Voltage(a, b));
            }
            let mut coeffs = Vec::with_capacity(n + 1);
            for tok in &tokens[4 + 2 * n..need] {
                coeffs.push(parse_value(tok).map_err(SimError::Parse)?);
            }
            (controls, coeffs, need)
        } else {
            let need = 4 + n + (n + 1);
            if tokens.len() < need {
                return Err(SimError::Parse(format!(
                    "line {line_no}: {kind} source '{}': POLY({n}) needs {n} source name(s) and {} coefficients",
                    tokens[0],
                    n + 1
                )));
            }
            let controls = tokens[4..4 + n]
                .iter()
                .map(|s| ControlVar::Current((*s).to_string()))
                .collect();
            let mut coeffs = Vec::with_capacity(n + 1);
            for tok in &tokens[4 + n..need] {
                coeffs.push(parse_value(tok).map_err(SimError::Parse)?);
            }
            (controls, coeffs, need)
        }
    } else if voltage_controlled {
        if tokens.len() < 6 {
            return Err(SimError::Parse(format!(
                "line {line_no}: {kind} source '{}' requires: {kind}{{name}} out+ out- ctrl+ ctrl- gain",
                tokens[0]
            )));
        }
        let cp = parse_node(tokens[3], circuit, plan)?;
        let cn = parse_node(tokens[4], circuit, plan)?;
        let gain = parse_value(tokens[5]).map_err(SimError::Parse)?;
        (vec![ControlVar::Voltage(cp, cn)], vec![0.0, gain], 6)
    } else {
        if tokens.len() < 5 {
            return Err(SimError::Parse(format!(
                "line {line_no}: {kind} source '{}' requires: {kind}{{name}} out+ out- Vname gain",
                tokens[0]
            )));
        }
        let gain = parse_value(tokens[4]).map_err(SimError::Parse)?;
        (vec![ControlVar::Current(tokens[3].to_string())], vec![0.0, gain], 5)
    };

    if tokens.len() > consumed {
        warnings.push(ignored_tokens(line_no, &format!("{kind} source"), &tokens[consumed..]));
    }

    let source = ControlledSource {
        name: tokens[0].to_string(),
        out_p,
        out_n,
        controls,
        coeffs,
    };
    match kind {
        'E' => circuit.vcvs.push(source),
        'G' => circuit.vccs.push(source),
        'H' => circuit.ccvs.push(source),
        'F' => circuit.cccs.push(source),
        _ => unreachable!(),
    }
    Ok(())
}

// ─── PSpice ABM extensions on E/F/G/H (TODO §5) ──────────────────────────────

/// The keywords an E/F/G/H card carries after its output nodes on an ABM
/// card (`VALUE=`, `TABLE`, `LAPLACE`, `FREQ`, `CHEBYSHEV`, or their
/// `EQUATION` alias for the expression form) — the canonical list lives in
/// [`crate::params`], where the substitution pass needs it too.
use crate::params::ABM_KEYWORDS;

/// One ABM declaration, split by its own shape: keyword, brace-wrapped
/// control expression, and the trailing detail text (table pairs, polynomial
/// groups, frequency rows, gain coefficients).
#[derive(Debug)]
pub(super) struct AbmSplit {
    /// Lowercased keyword as written.
    pub keyword: String,
    /// The expression inside the brace group, braces stripped.
    pub expr: String,
    /// Everything after the closing brace of the group (may be empty).
    pub tail: String,
}

/// Read an ABM shape from the text that follows an E/F/G/H card's output
/// nodes: `KEYWORD {expr} …` (with an optional `=` between them). `None`
/// means "not an ABM card" — the caller falls through to the classic
/// gain/`POLY(n)` grammar, so a control node named `freq` or `table` still
/// parses as a node; only the keyword-followed-by-brace shape is unambiguous.
pub(super) fn split_abm(text: &str) -> Option<AbmSplit> {
    let chars: Vec<char> = text.chars().collect();
    let mut i = 0usize;
    let skip_ws = |i: &mut usize| {
        while *i < chars.len() && chars[*i].is_whitespace() {
            *i += 1;
        }
    };
    skip_ws(&mut i);
    // Keyword: one letter-only run (an `=` glued to it is possible space-free).
    let keyword_start = i;
    while i < chars.len() && chars[i].is_ascii_alphabetic() {
        i += 1;
    }
    if i == keyword_start {
        return None;
    }
    let keyword_raw: String = chars[keyword_start..i].iter().collect();
    let keyword = keyword_raw.to_ascii_lowercase();
    if !ABM_KEYWORDS.contains(&keyword.as_str()) {
        return None;
    }
    // Allow space or a glued `=` (optionally with spaces) before the group.
    skip_ws(&mut i);
    if chars.get(i) == Some(&'=') {
        i += 1;
        skip_ws(&mut i);
    }
    if chars.get(i) != Some(&'{') {
        return None;
    }
    // Brace group with nesting tolerance (parameter-level braces have been
    // substituted already; a nested pair still belongs to the expression).
    let mut depth = 1usize;
    i += 1;
    let expr_start = i;
    while i < chars.len() {
        match chars[i] {
            '{' => depth += 1,
            '}' => {
                depth -= 1;
                if depth == 0 {
                    break;
                }
            }
            _ => {}
        }
        i += 1;
    }
    if depth != 0 {
        return None;
    }
    let expr: String = chars[expr_start..i].iter().collect();
    let tail: String = chars[i + 1..].iter().collect();
    Some(AbmSplit { keyword, expr: expr.trim().to_string(), tail: tail.trim().to_string() })
}

/// True when the card text after the output nodes opens with an ABM shape.
pub(super) fn abm_card(tokens: &[&str]) -> bool {
    tokens.get(3).is_some_and(|_| split_abm(&join_tokens(&tokens[3..])).is_some())
}

fn join_tokens(tokens: &[&str]) -> String {
    tokens.join(" ")
}

/// Skip whitespace, an optional `=`, and whitespace; the matchers between
/// pairs/groups are consistently spelled `… = …` with or without the space.
struct TailCursor {
    chars: Vec<char>,
    pos: usize,
}

impl TailCursor {
    fn new(text: &str) -> Self {
        TailCursor { chars: text.chars().collect(), pos: 0 }
    }

    fn skip_ws(&mut self) {
        while self.pos < self.chars.len() && self.chars[self.pos].is_whitespace() {
            self.pos += 1;
        }
    }

    fn eat(&mut self, c: char) -> bool {
        self.skip_ws();
        if self.chars.get(self.pos) == Some(&c) {
            self.pos += 1;
            true
        } else {
            false
        }
    }

    fn eat_eq(&mut self) {
        self.eat('=');
    }

    fn at_end(&mut self) -> bool {
        self.skip_ws();
        self.pos >= self.chars.len()
    }

    /// Remaining text from the current position (for diagnostics).
    fn rest(&self) -> String {
        self.chars[self.pos.min(self.chars.len())..].iter().collect()
    }

    /// Read one number (SPICE value syntax) as a whitespace/comma-delimited
    /// run, stopping at `stop` characters.
    fn number(&mut self, stop: &[char]) -> std::result::Result<f64, String> {
        self.skip_ws();
        let start = self.pos;
        while self.pos < self.chars.len() {
            let c = self.chars[self.pos];
            if c.is_whitespace() || stop.contains(&c) {
                break;
            }
            self.pos += 1;
        }
        if self.pos == start {
            return Err(format!("expected a number, found '{}'", self.rest_char()));
        }
        let text: String = self.chars[start..self.pos].iter().collect();
        crate::units::parse_value(&text).map_err(|e| format!("bad number '{text}': {e}"))
    }

    /// Peek the next non-space character (code point) for diagnostics.
    fn rest_char(&self) -> char {
        let mut p = self.pos;
        while p < self.chars.len() && self.chars[p].is_whitespace() {
            p += 1;
        }
        self.chars.get(p).copied().unwrap_or('\u{0}')
    }
}

/// `(input, output)` pair list of a `TABLE` card: `(x,y) (x,y) …` with an
/// optional `=` before the first pair and commas inside each. Pairs are
/// sorted ascending by input (the reference grammar requires ascending).
fn parse_table_pairs(text: &str) -> std::result::Result<Vec<(f64, f64)>, String> {
    let mut cur = TailCursor::new(text);
    cur.eat_eq();
    let mut points = Vec::new();
    while !cur.at_end() {
        if !cur.eat('(') {
            return Err(format!(
                "TABLE expects (input, output) pairs — found '{}'",
                cur.rest()
            ));
        }
        let x = cur.number(&[',', ')', '('])?;
        cur.eat(',');
        let y = cur.number(&[')', ','])?;
        if !cur.eat(')') {
            return Err(format!("TABLE pair is not closed — found '{}'", cur.rest()));
        }
        points.push((x, y));
    }
    if points.is_empty() {
        return Err("TABLE needs at least one (input, output) pair".into());
    }
    points.sort_by(|a, b| a.0.partial_cmp(&b.0).unwrap_or(std::cmp::Ordering::Equal));
    Ok(points)
}

/// One `LAPLACE` polynomial group: comma/space-separated coefficients in
/// *descending* power order (`{2s+1}` style expressions fall back to a
/// mini-polynomial parser), PSpice's `{1, 0.1, 10e-12}` spelling first.
fn parse_laplace_group(text: &str) -> std::result::Result<Vec<f64>, String> {
    // Coefficient list: every comma/space word parses as a plain number.
    let words: Vec<&str> = text
        .split(|c: char| c == ',' || c.is_whitespace())
        .map(str::trim)
        .filter(|w| !w.is_empty())
        .collect();
    if !words.is_empty()
        && words.iter().all(|w| crate::units::parse_value(w).is_ok())
    {
        return Ok(words
            .iter()
            .map(|w| crate::units::parse_value(w).expect("checked above"))
            .collect());
    }
    // s-expression fallback: `1+s`, `2*s*s - 1`, `(1+s)*(1+s)`.
    poly_from_s_expression(text)
}

/// Mini polynomial algebra in `s`: real coefficients, `+ - *`, `^`+integer on
/// primary terms, parentheses for products (`(1+s)*(1+s)`). Output is
/// *descending*: position 0 is the highest-order power of s.
fn poly_from_s_expression(text: &str) -> std::result::Result<Vec<f64>, String> {
    struct PolyParser {
        chars: Vec<char>,
        pos: usize,
        depth: u32,
    }
        // Vectors are ascending (index = power); the interface returns descending.
        impl PolyParser {
            const MAX_DEPTH: u32 = 32;
            fn skip(&mut self) {
                while self.pos < self.chars.len() && self.chars[self.pos].is_whitespace() {
                    self.pos += 1;
                }
            }
            fn peek(&mut self) -> char {
                self.skip();
                self.chars.get(self.pos).copied().unwrap_or('\u{0}')
            }
            fn eat(&mut self, c: char) -> bool {
                if self.peek() == c {
                    self.pos += 1;
                    true
                } else {
                    false
                }
            }
            fn expr(&mut self) -> std::result::Result<Vec<f64>, String> {
                if self.depth > Self::MAX_DEPTH {
                    return Err("polynomial in s is nested too deeply".into());
                }
                self.depth += 1;
                let mut left = self.term()?;
                loop {
                    if self.eat('+') {
                        let right = self.term()?;
                        left = poly_add(left, right);
                    } else if self.peek() == '-' {
                        // A '-' here separates terms; unary minus lives in
                        // `unary` below.
                        self.pos += 1;
                        let right = self.term()?;
                        left = poly_add(left, poly_neg(right));
                    } else {
                        break;
                    }
                }
                self.depth -= 1;
                Ok(left)
            }
            fn term(&mut self) -> std::result::Result<Vec<f64>, String> {
                let mut left = self.unary()?;
                while self.eat('*') {
                    let right = self.unary()?;
                    left = poly_mul(left, right);
                }
                Ok(left)
            }
            fn unary(&mut self) -> std::result::Result<Vec<f64>, String> {
                if self.eat('-') {
                    return Ok(poly_neg(self.unary()?));
                }
                if self.eat('+') {
                    return self.unary();
                }
                let base = self.primary()?;
                if self.eat('^') {
                    self.skip();
                    let start = self.pos;
                    while self.pos < self.chars.len() && self.chars[self.pos].is_ascii_digit() {
                        self.pos += 1;
                    }
                    if self.pos == start {
                        return Err("non-integer exponent in the polynomial".into());
                    }
                    let exp: String = self.chars[start..self.pos].iter().collect();
                    let n: usize = exp.parse().map_err(|_| format!("bad exponent '{exp}'"))?;
                    if n > 12 {
                        return Err(format!("exponent {n} is out of the supported range"));
                    }
                    let mut acc = vec![1.0];
                    for _ in 0..n {
                        acc = poly_mul(acc, base.clone());
                    }
                    return Ok(acc);
                }
                Ok(base)
            }
            fn primary(&mut self) -> std::result::Result<Vec<f64>, String> {
                if self.eat('(') {
                    let inner = self.expr()?;
                    if !self.eat(')') {
                        return Err("polynomial group is not closed".into());
                    }
                    return Ok(inner);
                }
                if matches!(self.peek(), 's' | 'S') {
                    self.pos += 1;
                    return Ok(vec![0.0, 1.0]); // ascending: s¹
                }
                // Number run: digits/dot/units via parse_value.
                let start = self.pos;
                while self.pos < self.chars.len()
                    && !self.chars[self.pos].is_whitespace()
                    && !matches!(self.chars[self.pos], '+' | '-' | '*' | '^' | '(' | ')' | 's' | 'S')
                {
                    self.pos += 1;
                }
                if self.pos == start {
                    return Err(format!(
                        "polynomial: unexpected '{}'",
                        self.chars.get(self.pos).copied().unwrap_or(' ')
                    ));
                }
                let token: String = self.chars[start..self.pos].iter().collect();
                let v = crate::units::parse_value(&token)
                    .map_err(|e| format!("polynomial term '{token}': {e}"))?;
                Ok(vec![v])
            }
        }
        let mut p = PolyParser {
            chars: text.chars().collect(),
            pos: 0,
            depth: 0,
        };
        let asc = p.expr()?;
        if p.pos < p.chars.len() {
            return Err(format!(
                "unexpected '{}' after the polynomial",
                p.chars[p.pos..].iter().collect::<String>().trim_start()
            ));
        }
        // The ascending vector reverses into the descending spelling the
        // PSpice ABM groups use (first entry = highest power of s).
        Ok(asc.into_iter().rev().collect())
    }

/// Ascending-coefficient addition (commutative; swap to extend `a`).
fn poly_add(mut a: Vec<f64>, mut b: Vec<f64>) -> Vec<f64> {
    if b.len() > a.len() {
        std::mem::swap(&mut a, &mut b);
    }
    for (i, v) in b.into_iter().enumerate() {
        a[i] += v;
    }
    a
}

/// Ascending-coefficient negation.
fn poly_neg(a: Vec<f64>) -> Vec<f64> {
    a.into_iter().map(|v| -v).collect()
}

/// Ascending-coefficient product.
fn poly_mul(a: Vec<f64>, b: Vec<f64>) -> Vec<f64> {
    if a.is_empty() || b.is_empty() {
        return Vec::new();
    }
    let mut out = vec![0.0; a.len() + b.len() - 1];
    for (i, av) in a.iter().enumerate() {
        for (j, bv) in b.iter().enumerate() {
            out[i + j] += av * bv;
        }
    }
    out
}

/// `FREQ` rows: `<f, gain, [phase]>` (parens also honored), followed by an
/// optional `MEASURE=<DB|RADS|DEGREES>`. Pairs of numbers become zero-phase
/// rows.
fn parse_freq_tail(text: &str, warnings: &mut Vec<String>, tag: &str) -> std::result::Result<(Vec<(f64, f64, f64)>, Option<crate::circuit::AbmMeasure>), String> {
    use crate::circuit::AbmMeasure;
    let mut cur = TailCursor::new(text);
    cur.eat_eq();
    let mut rows = Vec::new();
    while !cur.at_end() {
        // `MEASURE=DB` closes the row list when it appears.
        cur.skip_ws();
        let lower_rest = cur.rest().to_ascii_lowercase();
        if lower_rest.starts_with("measure=") {
            let value = lower_rest["measure=".len()..]
                .chars()
                .take_while(|c| c.is_ascii_alphabetic())
                .collect::<String>();
            cur.pos += "measure=".len() + value.len();
            let key = value.as_str();
            let m = match key {
                "db" => Some(AbmMeasure::Db),
                "rads" | "rad" => Some(AbmMeasure::Rads),
                "degrees" | "degree" | "degs" | "deg" => Some(AbmMeasure::Degrees),
                other => {
                    warnings.push(format!(
                        "ABM `{tag}`: MEASURE={other} is not a recognized unit — gains stay plain, phases degrees"
                    ));
                    None
                }
            };
            let trailing = cur.rest().trim().to_string();
            if !trailing.is_empty() {
                warnings.push(format!(
                    "{tag}: token(s) after the FREQ rows are not supported — ignored: '{trailing}'"
                ));
            }
            return Ok((sort_freq_rows(rows), m));
        }
        let open = cur.rest_char();
        let paren = open == '(' || open == '<';
        if !paren {
            return Err(format!(
                "FREQ expects <frequency, gain [, phase]> rows — found '{}'",
                cur.rest()
            ));
        }
        cur.eat('(');
        cur.eat('<'); // the PSpice angle spelling, checked above
        let f = cur.number(&[',', ')', '>', '<'])?;
        cur.eat(',');
        let g = cur.number(&[',', ')', '>', '<'])?;
        cur.eat(',');
        let phase = cur.number(&[',', ')', '>', '<']).unwrap_or(0.0);
        if !(cur.eat(')') || cur.eat('>')) {
            return Err(format!("FREQ row is not closed — found '{}'", cur.rest()));
        }
        rows.push((f, g, phase));
    }
    if rows.is_empty() {
        return Err("FREQ needs at least one <frequency, gain [, phase]> row".into());
    }
    Ok((sort_freq_rows(rows), None))
}

/// Rows must travel ascending by frequency (the SPICE table rule): sort them.
fn sort_freq_rows(rows: Vec<(f64, f64, f64)>) -> Vec<(f64, f64, f64)> {
    let mut rows = rows;
    rows.sort_by(|a, b| a.0.partial_cmp(&b.0).unwrap_or(std::cmp::Ordering::Equal));
    rows
}

/// `CHEBYSHEV` coefficients: plain numbers after the expression (the second
/// order coefficient first, then third, fourth, …).
fn parse_chebyshev_text(text: &str) -> std::result::Result<Vec<f64>, String> {
    let mut cur = TailCursor::new(text);
    cur.eat_eq();
    let mut coeffs = Vec::new();
    while !cur.at_end() {
        let c = cur
            .number(&[','])
            .map_err(|e| format!("CHEBYSHEV coefficients {e}"))?;
        coeffs.push(c);
    }
    if coeffs.is_empty() {
        return Err("CHEBYSHEV needs at least one gain coefficient".into());
    }
    coeffs.shrink_to_fit();
    Ok(coeffs)
}

/// Resolve the netlist parameter table into the map the B-language parser
/// folds from (numeric entries only; strings cannot appear in arithmetic).
fn abm_params(params: &HashMap<String, ParamValue>) -> HashMap<String, f64> {
    params
        .iter()
        .filter_map(|(k, v)| match v {
            ParamValue::Num(v) => Some((k.clone(), *v)),
            ParamValue::Str(_) => None,
        })
        .collect()
}

/// Parse one ABM E/F/G/H card into a behavioral source (TODO §5).
///
/// Letters carry the output flavour (PSpice grammar): `E`/`H` produce the
/// node voltage `out_p − out_n` (one MNA branch row), `G`/`F` produce the
/// current flowing `out_p → out_n` (an independent-style injection). The
/// control expression is the B-language: `V(a[,b])`, `I(src)`, and any of
/// the supported math/history functions, with deck `.param` names folded
/// in. The dynamic forms (LAPLACE/FREQ/CHEBYSHEV) stamp their DC gain in
/// DC/transient and their full frequency response in AC — that limit is
/// warned once per card at parse.
pub(super) fn parse_abm_source(
    tokens: &[&str],
    circuit: &mut Circuit,
    plan: &HashMap<String, usize>,
    line_no: usize,
    warnings: &mut Vec<String>,
    params: &HashMap<String, ParamValue>,
) -> Result<()> {
    let kind = element_letter(tokens[0]).to_ascii_uppercase();
    if tokens.len() < 4 {
        return Err(SimError::Parse(format!(
            "line {line_no}: {kind} ABM requires: {kind}name out+ out- KEYWORD {{expr}} [detail]"
        )));
    }
    let split = split_abm(&join_tokens(&tokens[3..])).ok_or_else(|| {
        SimError::Parse(format!(
            "line {line_no}: {kind} source '{}': not an ABM card",
            tokens[0]
        ))
    })?;
    let expr = split.expr.clone();
    if expr.is_empty() {
        return Err(SimError::Parse(format!(
            "line {line_no}: ABM source '{}' has an empty expression",
            tokens[0]
        )));
    }
    let numeric = abm_params(params);
    let mut behavior = crate::behavior::parse_with_params(&expr, &numeric)
        .map_err(|e| SimError::Parse(format!("line {line_no}: {e}")))?;
    behavior.noise_tag = crate::params::tag_word(tokens[0]);
    let out_p = parse_node(tokens[1], circuit, plan)?;
    let out_n = parse_node(tokens[2], circuit, plan)?;

    let tag = tokens[0].to_string();
    let abm = match split.keyword.as_str() {
        "value" | "equation" => {
            if !split.tail.is_empty() {
                warnings.push(format!(
                    "ABM source '{tag}': token(s) after the expression are not supported — ignored: '{}'",
                    split.tail
                ));
            }
            crate::circuit::AbmForm::Value
        }
        "table" => {
            let points = parse_table_pairs(&split.tail)
                .map_err(|e| SimError::Parse(format!("line {line_no}: {} {e}", tokens[0])))?;
            behavior.ast = crate::behavior::BExpr::Table {
                input: Box::new(behavior.ast),
                points: points.clone(),
            };
            crate::circuit::AbmForm::Table
        }
        "laplace" => {
            let (num, den) = parse_laplace_groups(&split.tail).map_err(
                |e| SimError::Parse(format!("line {line_no}: {} laplace {e}", tokens[0])),
            )?;
            warnings.push(abm_dynamic_note(&tag));
            crate::circuit::AbmForm::Laplace { num, den }
        }
        "freq" => {
            let (rows, measure) =
                parse_freq_tail(&split.tail, warnings, &tag).map_err(|e| {
                    SimError::Parse(format!("line {line_no}: {} freq {e}", tokens[0]))
                })?;
            warnings.push(abm_dynamic_note(&tag));
            crate::circuit::AbmForm::Freq { rows, measure }
        }
        "chebyshev" => {
            let coeffs = parse_chebyshev_text(&split.tail).map_err(
                |e| SimError::Parse(format!("line {line_no}: {} chebyshev {e}", tokens[0])),
            )?;
            warnings.push(abm_dynamic_note(&tag));
            crate::circuit::AbmForm::Chebyshev { coeffs }
        }
        other => {
            return Err(SimError::Parse(format!(
                "line {line_no}: ABM keyword '{other}' is not one of VALUE/EQUATION/TABLE/LAPLACE/FREQ/CHEBYSHEV"
            )))
        }
    };

    let kind = if matches!(kind, 'E' | 'H') {
        crate::circuit::BehavioralKind::Voltage
    } else {
        crate::circuit::BehavioralKind::Current
    };
    circuit.behavioral.push(crate::circuit::BehavioralSource {
        name: tokens[0].to_string(),
        out_p,
        out_n,
        kind,
        expr,
        behavior,
        abm: Some(abm),
    });
    Ok(())
}

/// One documented-interpretation note for the dynamic ABM forms: this
/// engine's s-domain responses live in AC — the DC/transient stamps carry
/// only the DC gain H(0) (a strictly s-domain source like `1/s` therefore
/// outputs nothing outside AC).
fn abm_dynamic_note(tag: &str) -> String {
    format!(
        "ABM source '{tag}': LAPLACE/FREQ/CHEBYSHEV respond in AC; DC and transient stamp the DC gain H(0) \
         — a source with den(0) = 0 or no 0 Hz row is silent outside AC"
    )
}

/// `LAPLACE` tail: `{num} {den}` (an optional `=` between groups, like the
/// reference spelling `= {…} = {…}`).
fn parse_laplace_groups(text: &str) -> std::result::Result<(Vec<f64>, Vec<f64>), String> {
    let mut cur = TailCursor::new(text);
    cur.eat_eq();
    cur.skip_ws();
    if cur.rest_char() != '{' {
        return Err(format!(
            "LAPLACE expects = {{numerator}} {{denominator}} — found '{}'",
            cur.rest()
        ));
    }
    let num = read_brace_group(&mut cur)?;
    cur.eat_eq();
    cur.skip_ws();
    if cur.rest_char() != '{' {
        return Err(format!(
            "LAPLACE expects the denominator after the numerator — found '{remainder}'",
            remainder = cur.rest()
        ));
    }
    let den = read_brace_group(&mut cur)?;
    if !cur.at_end() {
        return Err(format!(
            "LAPLACE takes exactly two groups — trailing text '{}'",
            cur.rest()
        ));
    }
    Ok((num, den))
}

/// One `{ … }` group (content handed to the group parser).
fn read_brace_group(cur: &mut TailCursor) -> std::result::Result<Vec<f64>, String> {
    cur.skip_ws();
    if cur.chars.get(cur.pos) != Some(&'{') {
        return Err("expected '{'".into());
    }
    cur.pos += 1;
    // The opening brace is consumed, so depth starts at 1 (as in
    // `split_abm`): the group's own closing brace brings it back to 0, and
    // a nested group adds one level.
    let mut depth = 1usize;
    let start = cur.pos;
    let end;
    {
        let chars = &cur.chars;
        let mut i = cur.pos;
        while i < chars.len() {
            match chars[i] {
                '{' => depth += 1,
                '}' => {
                    depth -= 1;
                    if depth == 0 {
                        break;
                    }
                }
                _ => {}
            }
            i += 1;
        }
        if depth != 0 {
            return Err("brace group is not closed".into());
        }
        end = i;
    }
    let text: String = cur.chars[start..end].iter().collect();
    cur.pos = end + 1;
    parse_laplace_group(&text)
}


/// Emit one ABM behavioral entry: the source's element name keeps its own
/// family letter (the name *is* `E1`/`G1`/…), so the re-spelled card
/// re-parses through the same ABM dispatch. Numbers round-trip through
/// `parse_value` (Rust's shortest-round-trip `f64` display).
fn emit_abm_source(
    circuit: &Circuit,
    b: &BehavioralSource,
    abm: &AbmForm,
    out: &mut String,
) {
    let name_of = |n: NodeId| circuit.node_name(n.0).to_string();
    let head = format!("{} {} {}", b.name, name_of(b.out_p), name_of(b.out_n));
    let card = match abm {
        // Every shape re-spells the brace group `split_abm` needs — without
        // it the cards would re-parse through the classic gain grammar.
        AbmForm::Value => format!("{} VALUE={{{}}}", head, b.expr),
        AbmForm::Table => format!("{} TABLE {{{}}} = {}", head, b.expr, table_points_text(b)),
        AbmForm::Laplace { num, den } => format!(
            "{} LAPLACE {{{}}} = {{{}}} {{{}}}",
            head,
            b.expr,
            poly_text(num),
            poly_text(den)
        ),
        AbmForm::Freq { rows, measure } => {
            let measure = match measure {
                Some(AbmMeasure::Db) => " MEASURE=DB",
                Some(AbmMeasure::Rads) => " MEASURE=RADS",
                Some(AbmMeasure::Degrees) => " MEASURE=DEGREES",
                None => "",
            };
            let rows_text = rows
                .iter()
                .map(|(f, g, p)| if *p == 0.0 {
                    format!("<{}, {}>", f, g)
                } else {
                    format!("<{}, {}, {}>", f, g, p)
                })
                .collect::<Vec<_>>()
                .join(" ");
            format!("{} FREQ {{{}}} = {}{}", head, b.expr, rows_text, measure)
        }
        AbmForm::Chebyshev { coeffs } => {
            let coeffs_text = coeffs
                .iter()
                .map(|c| c.to_string())
                .collect::<Vec<_>>()
                .join(" ");
            format!("{} CHEBYSHEV {{{}}} {}", head, b.expr, coeffs_text)
        }
    };
    out.push_str(&card);
    out.push('\n');
}

/// The `(input, output)` pairs of a `TABLE` entry — the AST holds the same
/// sorted list the simulator evaluates against.
fn table_points_text(b: &BehavioralSource) -> String {
    let points = match &b.behavior.ast {
        crate::behavior::BExpr::Table { points, .. } => points.clone(),
        _ => Vec::new(),
    };
    points
        .iter()
        .map(|(x, y)| format!("({}, {})", x, y))
        .collect::<Vec<_>>()
        .join(" ")
}

/// One descending polynomial in s as coefficient text (`1, 2` for `2s + 1`).
fn poly_text(coeffs: &[f64]) -> String {
    coeffs
        .iter()
        .map(|c| c.to_string())
        .collect::<Vec<_>>()
        .join(", ")
}

/// Parse a bipolar transistor:
/// `Qname c b e [substrate] model [AREA=…] [OFF]`.
pub(super) fn parse_bjt(
    tokens: &[&str],
    circuit: &mut Circuit,
    plan: &HashMap<String, usize>,
    warnings: &mut Vec<String>,
    line_no: usize,
) -> Result<()> {
    if tokens.len() < 5 {
        return Err(SimError::Parse(format!(
            "line {line_no}: Q requires: Qname c b e [substrate] model [AREA=…]"
        )));
    }
    let nc = parse_node(tokens[1], circuit, plan)?;
    let nb = parse_node(tokens[2], circuit, plan)?;
    let ne = parse_node(tokens[3], circuit, plan)?;
    let four = q_is_four_terminal(tokens);
    let (ns, model_idx) = if four {
        (Some(parse_node(tokens[4], circuit, plan)?), 5)
    } else {
        (None, 4)
    };
    let model = tokens
        .get(model_idx)
        .ok_or_else(|| {
            SimError::Parse(format!("line {line_no}: Q '{}' is missing its model", tokens[0]))
        })?
        .to_ascii_lowercase();
    let mut area = 1.0;
    for tok in &tokens[model_idx + 1..] {
        let lower = tok.to_ascii_lowercase();
        if let Some(rest) = lower.strip_prefix("area=") {
            area = parse_value(rest).map_err(SimError::Parse)?;
        } else if lower == "off" {
            warnings.push(format!(
                "line {line_no}: BJT '{}': OFF is parsed but not applied",
                tokens[0]
            ));
        } else if let Some(rest) = lower.strip_prefix("m=") {
            area = parse_value(rest).map_err(SimError::Parse)?;
        } else if let Ok(a) = parse_value(tok) {
            area = a;
        } else {
            warnings.push(format!(
                "line {line_no}: BJT '{}': token '{tok}' is not supported — ignored",
                tokens[0]
            ));
        }
    }
    circuit.bjts.push(Bjt {
        name: tokens[0].to_string(),
        nc,
        nb,
        ne,
        ns,
        model,
        area,
    });
    Ok(())
}

/// Parse a `J` card: `Jname nd ng ns model [AREA=…]`.
///
/// Trailing tokens mirror the BJT grammar: `AREA=` (also the `M=` multiplier
/// spelling), a bare numeric area, and an ignored `OFF`.
pub(super) fn parse_jfet(
    tokens: &[&str],
    circuit: &mut Circuit,
    plan: &HashMap<String, usize>,
    warnings: &mut Vec<String>,
    line_no: usize,
) -> Result<()> {
    if tokens.len() < 5 {
        return Err(SimError::Parse(format!(
            "line {line_no}: J requires: Jname drain gate source model [AREA=…]"
        )));
    }
    let nd = parse_node(tokens[1], circuit, plan)?;
    let ng = parse_node(tokens[2], circuit, plan)?;
    let ns = parse_node(tokens[3], circuit, plan)?;
    let model = tokens
        .get(4)
        .ok_or_else(|| {
            SimError::Parse(format!("line {line_no}: J '{}' is missing its model", tokens[0]))
        })?
        .to_ascii_lowercase();
    let mut area = 1.0;
    for tok in &tokens[5..] {
        let lower = tok.to_ascii_lowercase();
        if let Some(rest) = lower.strip_prefix("area=") {
            area = parse_value(rest).map_err(SimError::Parse)?;
        } else if lower == "off" {
            warnings.push(format!(
                "line {line_no}: JFET '{}': OFF is parsed but not applied",
                tokens[0]
            ));
        } else if let Some(rest) = lower.strip_prefix("m=") {
            area = parse_value(rest).map_err(SimError::Parse)?;
        } else if let Ok(a) = parse_value(tok) {
            area = a;
        } else {
            warnings.push(format!(
                "line {line_no}: JFET '{}': token '{tok}' is not supported — ignored",
                tokens[0]
            ));
        }
    }
    circuit.jfets.push(Jfet {
        name: tokens[0].to_string(),
        nd,
        ng,
        ns,
        model,
        area,
    });
    Ok(())
}

/// Parse a voltage-controlled (`S`) or current-controlled (`W`) switch.
///
/// `Sname n+ n- ctrl+ ctrl- model [ON|OFF]`
/// `Wname n+ n- Vname model [ON|OFF]`
pub(super) fn parse_switch(
    tokens: &[&str],
    circuit: &mut Circuit,
    plan: &HashMap<String, usize>,
    line_no: usize,
) -> Result<()> {
    let kind = element_letter(tokens[0]).to_ascii_uppercase();
    let min = if kind == 'S' { 6 } else { 5 };
    if tokens.len() < min {
        return Err(SimError::Parse(if kind == 'S' {
            format!("line {line_no}: S switch requires: Sname n+ n- ctrl+ ctrl- model [ON|OFF]")
        } else {
            format!("line {line_no}: W switch requires: Wname n+ n- Vname model [ON|OFF]")
        }));
    }
    let n1 = parse_node(tokens[1], circuit, plan)?;
    let n2 = parse_node(tokens[2], circuit, plan)?;
    let (control, model_idx) = if kind == 'S' {
        let cp = parse_node(tokens[3], circuit, plan)?;
        let cn = parse_node(tokens[4], circuit, plan)?;
        (SwitchControl::Voltage(cp, cn), 5)
    } else {
        (SwitchControl::Current(tokens[3].to_string()), 4)
    };
    let model = tokens[model_idx].to_ascii_lowercase();
    let mut initial = None;
    for tok in &tokens[model_idx + 1..] {
        if tok.eq_ignore_ascii_case("on") {
            initial = Some(true);
        } else if tok.eq_ignore_ascii_case("off") {
            initial = Some(false);
        } else {
            return Err(SimError::Parse(format!(
                "line {line_no}: switch '{}': unexpected token '{tok}'",
                tokens[0]
            )));
        }
    }
    circuit.switches.push(Switch {
        name: tokens[0].to_string(),
        n1,
        n2,
        control,
        model,
        initial,
    });
    Ok(())
}

/// An ideal op-amp card: `Uname out+ out- in+ in- [model]` — TODO §5,
/// documented interpretation (the reference tools reserve `U` for nothing).
/// With a model name it binds the `.model <name> U(…)` card; a bare `U` keeps
/// the default parameters (GAIN 1e6, RIN ∞, ROUT 0, VOS 0).
pub(super) fn parse_opamp(
    tokens: &[&str],
    circuit: &mut Circuit,
    plan: &HashMap<String, usize>,
    line_no: usize,
    warnings: &mut Vec<String>,
) -> Result<()> {
    let kind = element_letter(tokens[0]).to_ascii_uppercase();
    if tokens.len() < 5 {
        return Err(SimError::Parse(format!(
            "line {line_no}: {kind} op-amp requires: {kind}name out+ out- in+ in- [model]"
        )));
    }
    let out_p = parse_node(tokens[1], circuit, plan)?;
    let out_n = parse_node(tokens[2], circuit, plan)?;
    let in_p = parse_node(tokens[3], circuit, plan)?;
    let in_n = parse_node(tokens[4], circuit, plan)?;
    let model = tokens
        .get(5)
        .map(|m| m.to_ascii_lowercase())
        .unwrap_or_default();
    if tokens.len() > 6 {
        warnings.push(ignored_tokens(line_no, "op-amp card", &tokens[6..]));
    }
    circuit.opamps.push(OpAmp {
        name: tokens[0].to_string(),
        out_p,
        out_n,
        in_p,
        in_n,
        model,
    });
    Ok(())
}

/// Parse a mutual-inductance card: `Kname L1 L2 <k>`.
pub(super) fn parse_mutual_inductance(
    tokens: &[&str],
    circuit: &mut Circuit,
    line_no: usize,
) -> Result<()> {
    if tokens.len() < 4 {
        return Err(SimError::Parse(format!(
            "line {line_no}: K requires: Kname L1 L2 <coupling>"
        )));
    }
    let coupling = parse_value(tokens[3]).map_err(SimError::Parse)?;
    circuit.mutual_inductances.push(crate::circuit::MutualInductance {
        name: tokens[0].to_string(),
        inductor1: tokens[1].to_string(),
        inductor2: tokens[2].to_string(),
        coupling,
    });
    Ok(())
}

/// Shared R/C/L card grammar: `<name> n1 n2 [value] [model] [TC1=… TC2=…]`.
/// The value may be omitted when a model card supplies `R=`/`C=`/`L=`.
pub(super) fn parse_passive_card<'a>(
    tokens: &'a [&'a str],
    kind: &str,
    warnings: &mut Vec<String>,
    line_no: usize,
) -> Result<(f64, Option<String>)> {
    if tokens.len() < 4 {
        return Err(SimError::Parse(format!(
            "line {line_no}: {kind} requires: {kind}name n1 n2 <value> [model]"
        )));
    }
    let mut value: Option<f64> = None;
    let mut model: Option<String> = None;
    for tok in &tokens[3..] {
        if tok.contains('=') {
            // Per-instance TC overrides are not wired into the model lookup;
            // the model card's TC1/TC2 is the supported spelling.
            warnings.push(format!(
                "line {line_no}: {kind} '{}': parameter '{tok}' is not applied — \
                 put TC1/TC2 on the .model card",
                tokens[0]
            ));
        } else if let Ok(v) = parse_value(tok) {
            if value.is_some() {
                return Err(SimError::Parse(format!(
                    "line {line_no}: {kind} '{}' has more than one value",
                    tokens[0]
                )));
            }
            value = Some(v);
        } else if model.is_none() {
            model = Some(tok.to_ascii_lowercase());
        } else {
            return Err(SimError::Parse(format!(
                "line {line_no}: {kind} '{}': unexpected token '{tok}'",
                tokens[0]
            )));
        }
    }
    Ok((value.unwrap_or(0.0), model))
}

pub(super) fn parse_resistor(
    tokens: &[&str],
    circuit: &mut Circuit,
    plan: &HashMap<String, usize>,
    warnings: &mut Vec<String>,
    line_no: usize,
) -> Result<()> {
    let n1 = parse_node(tokens[1], circuit, plan)?;
    let n2 = parse_node(tokens[2], circuit, plan)?;
    let (resistance, model) = parse_passive_card(tokens, "resistor", warnings, line_no)?;
    circuit.resistors.push(Resistor {
        name: tokens[0].to_string(),
        n1,
        n2,
        resistance,
        model,
    });
    Ok(())
}

pub(super) fn parse_capacitor(
    tokens: &[&str],
    circuit: &mut Circuit,
    plan: &HashMap<String, usize>,
    warnings: &mut Vec<String>,
    line_no: usize,
) -> Result<()> {
    let n1 = parse_node(tokens[1], circuit, plan)?;
    let n2 = parse_node(tokens[2], circuit, plan)?;
    let (capacitance, model) = parse_passive_card(tokens, "capacitor", warnings, line_no)?;
    circuit.capacitors.push(Capacitor {
        name: tokens[0].to_string(),
        n1,
        n2,
        capacitance,
        model,
    });
    Ok(())
}

pub(super) fn parse_inductor(
    tokens: &[&str],
    circuit: &mut Circuit,
    plan: &HashMap<String, usize>,
    warnings: &mut Vec<String>,
    line_no: usize,
) -> Result<()> {
    let n1 = parse_node(tokens[1], circuit, plan)?;
    let n2 = parse_node(tokens[2], circuit, plan)?;
    let (inductance, model) = parse_passive_card(tokens, "inductor", warnings, line_no)?;
    circuit.inductors.push(Inductor {
        name: tokens[0].to_string(),
        n1,
        n2,
        inductance,
        model,
    });
    Ok(())
}

pub(super) fn parse_voltage_source(
    tokens: &[&str],
    circuit: &mut Circuit,
    plan: &HashMap<String, usize>,
    options: &ParseOptions,
) -> Result<()> {
    let n1 = parse_node(tokens[1], circuit, plan)?;
    let n2 = parse_node(tokens[2], circuit, plan)?;
    let spec = parse_source_value_specs(&tokens[3..], options)?;

    let final_voltage = match (spec.dc, &spec.waveform) {
        (Some(v), _) => v,
        (None, Some(w)) => w.start_value(),
        (None, None) => 0.0,
    };

    circuit.voltage_sources.push(VoltageSource {
        name: tokens[0].to_string(),
        n1, n2,
        voltage: final_voltage,
        waveform: spec.waveform,
        ac: spec.ac,
    });
    Ok(())
}

pub(super) fn parse_current_source(
    tokens: &[&str],
    circuit: &mut Circuit,
    plan: &HashMap<String, usize>,
    options: &ParseOptions,
) -> Result<()> {
    let n1 = parse_node(tokens[1], circuit, plan)?;
    let n2 = parse_node(tokens[2], circuit, plan)?;
    let spec = parse_source_value_specs(&tokens[3..], options)?;

    let final_current = match (spec.dc, &spec.waveform) {
        (Some(v), _) => v,
        (None, Some(w)) => w.start_value(),
        (None, None) => 0.0,
    };

    circuit.current_sources.push(CurrentSource {
        name: tokens[0].to_string(),
        n1, n2,
        current: final_current,
        waveform: spec.waveform,
        ac: spec.ac,
    });
    Ok(())
}

// ─── Behavioral sources (`B`) ───────────────────────────────────────────────

/// One `B` card split into its output flavour and its raw expression text.
pub(super) struct BCardSplit {
    pub(super) kind: BehavioralKind,
    /// Expression as written after the `V=`/`I=` keyword (the keyword's own
    /// letter is stripped: the compiled grammar treats `V(…)` as a probe).
    pub(super) expr: String,
}

/// Find the `V=`/`I=` keyword in a `B` card and rejoin the expression.
///
/// Both keyword shapes are legal: glued (`V=V(in)*2`, possibly the '=' with
/// the expression in the same token) and split (`V = V(in) * V(out)`), and
/// the expression itself may carry spaces — so everything after the keyword
/// is rejoined with single spaces. Scanning starts at token 3 (name, and two
/// nodes, come first); an expression can never contain '=' otherwise.
pub(super) fn split_b_card(tokens: &[&str]) -> Result<BCardSplit> {
    for k in 3..tokens.len() {
        let lower = tokens[k].to_ascii_lowercase();
        let glued = if lower.starts_with("v=") {
            Some(BehavioralKind::Voltage)
        } else if lower.starts_with("i=") {
            Some(BehavioralKind::Current)
        } else {
            None
        };
        if let Some(kind) = glued {
            let mut expr_parts: Vec<&str> = Vec::with_capacity(tokens.len() - k);
            let tail = &tokens[k][2..];
            if !tail.is_empty() {
                expr_parts.push(tail);
            }
            expr_parts.extend(tokens[k + 1..].iter().copied());
            return Ok(BCardSplit { kind, expr: expr_parts.join(" ") });
        }
        // Split: bare `V` / `I`, then either a bare `=` or one starting with `=`.
        if (lower == "v" || lower == "i")
            && tokens.get(k + 1).is_some_and(|n| n.starts_with('='))
        {
            let kind = if lower == "v" { BehavioralKind::Voltage } else { BehavioralKind::Current };
            let mut expr_parts: Vec<&str> = Vec::with_capacity(tokens.len() - k - 1);
            let after_eq = &tokens[k + 1][1..];
            if !after_eq.is_empty() {
                expr_parts.push(after_eq);
            }
            expr_parts.extend(tokens[k + 2..].iter().copied());
            return Ok(BCardSplit { kind, expr: expr_parts.join(" ") });
        }
    }
    Err(SimError::Parse(format!(
        "behavioral source '{}' has no V=<expr> or I=<expr> output",
        tokens[0]
    )))
}

pub(super) fn parse_behavioral_source(
    tokens: &[&str],
    circuit: &mut Circuit,
    plan: &HashMap<String, usize>,
    line_no: usize,
    params: &HashMap<String, ParamValue>,
) -> Result<()> {
    let split = split_b_card(tokens)
        .map_err(|e| SimError::Parse(format!("line {line_no}: {e}")))?;
    // An LTspice-style braced group (`V={V(in)*gain}`) reaches the card
    // parser verbatim — `{}`-expansion leaves probe groups alone. Peel the
    // outer pair; the expression language owns the rest.
    let expr = split.expr.trim();
    let expr = expr
        .strip_prefix('{').and_then(|s| s.strip_suffix('}'))
        .unwrap_or(expr)
        .trim();
    if expr.is_empty() {
        return Err(SimError::Parse(format!(
            "line {line_no}: behavioral source '{}' has an empty expression",
            tokens[0]
        )));
    }
    // Deck parameters ride B expressions too (`V={V(in)*gain}`): the parser
    // folds every numeric `.param` entry into the AST, so the compiled form
    // is a plain constant-product circuit as far as a solve is concerned.
    let mut behavior = crate::behavior::parse_with_params(expr, &abm_params(params))
        .map_err(|e| SimError::Parse(format!("line {line_no}: {e}")))?;
    // Keyed noise draws (`white()`/`flicker()`) tag their keys with the
    // source's element name, so two B cards never roll the same numbers.
    behavior.noise_tag = crate::params::tag_word(tokens[0]);
    let out_p = parse_node(tokens[1], circuit, plan)?;
    let out_n = parse_node(tokens[2], circuit, plan)?;
    circuit.behavioral.push(BehavioralSource {
        name: tokens[0].to_string(),
        out_p,
        out_n,
        kind: split.kind,
        expr: expr.to_string(),
        behavior,
        abm: None,
    });
    Ok(())
}

/// Emit every element family, in SPICE card order (the inverse of the
/// element parsers above); sources print their waveform via
/// [`waveform_text`].
pub(super) fn emit_elements(circuit: &Circuit, out: &mut String) {
    // ── Element cards ────────────────────────────────────────────────────
    let name_of = |n: NodeId| circuit.node_name(n.0).to_string();
    for r in &circuit.resistors {
        let mut card = format!(
            "{} {} {} {}",
            r.name, name_of(r.n1), name_of(r.n2), r.resistance
        );
        if let Some(model) = &r.model {
            card.push_str(&format!(" {model}"));
        }
        out.push_str(&card);
        out.push('\n');
    }
    for c in &circuit.capacitors {
        let mut card = format!(
            "{} {} {} {}",
            c.name, name_of(c.n1), name_of(c.n2), c.capacitance
        );
        if let Some(model) = &c.model {
            card.push_str(&format!(" {model}"));
        }
        out.push_str(&card);
        out.push('\n');
    }
    for l in &circuit.inductors {
        let mut card = format!(
            "{} {} {} {}",
            l.name, name_of(l.n1), name_of(l.n2), l.inductance
        );
        if let Some(model) = &l.model {
            card.push_str(&format!(" {model}"));
        }
        out.push_str(&card);
        out.push('\n');
    }
    for d in &circuit.diodes {
        if d.area != 1.0 {
            out.push_str(&format!(
                "{} {} {} {} AREA={}\n",
                d.name, name_of(d.anode), name_of(d.cathode), d.model, d.area
            ));
        } else {
            out.push_str(&format!(
                "{} {} {} {}\n",
                d.name, name_of(d.anode), name_of(d.cathode), d.model
            ));
        }
    }
    for m in &circuit.mosfets {
        let mut line = format!(
            "{} {} {} {} {} {} W={} L={}",
            m.name, name_of(m.nd), name_of(m.ng), name_of(m.ns), name_of(m.nb), m.model, m.w, m.l
        );
        // Junction sizing survives the round trip only when written back.
        if m.ad != 0.0 { line.push_str(&format!(" AD={}", m.ad)); }
        if m.asr != 0.0 { line.push_str(&format!(" AS={}", m.asr)); }
        if m.ps != 0.0 { line.push_str(&format!(" PS={}", m.ps)); }
        if m.pd != 0.0 { line.push_str(&format!(" PD={}", m.pd)); }
        line.push('\n');
        out.push_str(&line);
    }
    for q in &circuit.bjts {
        let substrate = q.ns.map(|n| format!(" {}", name_of(n))).unwrap_or_default();
        let area = if q.area != 1.0 {
            format!(" AREA={}", q.area)
        } else {
            String::new()
        };
        out.push_str(&format!(
            "{} {} {} {}{} {}{}\n",
            q.name,
            name_of(q.nc),
            name_of(q.nb),
            name_of(q.ne),
            substrate,
            q.model,
            area
        ));
    }
    for j in &circuit.jfets {
        let area = if j.area != 1.0 {
            format!(" AREA={}", j.area)
        } else {
            String::new()
        };
        out.push_str(&format!(
            "{} {} {} {} {}{}\n",
            j.name,
            name_of(j.nd),
            name_of(j.ng),
            name_of(j.ns),
            j.model,
            area
        ));
    }
    for v in &circuit.voltage_sources {
        // `DC <value>` is redundant for waveform sources (the value equals
        // their starting level) but keeps every source's operating point.
        out.push_str(&format!(
            "{} {} {} DC {}",
            v.name, name_of(v.n1), name_of(v.n2), v.voltage
        ));
        if let Some(ac) = &v.ac {
            out.push_str(&format!(" AC {}", ac.mag));
            if ac.phase != 0.0 {
                out.push_str(&format!(" {}", ac.phase));
            }
        }
        if let Some(w) = &v.waveform {
            out.push(' ');
            out.push_str(&waveform_text(w));
        }
        out.push('\n');
    }
    for i in &circuit.current_sources {
        out.push_str(&format!(
            "{} {} {} DC {}",
            i.name, name_of(i.n1), name_of(i.n2), i.current
        ));
        if let Some(ac) = &i.ac {
            out.push_str(&format!(" AC {}", ac.mag));
            if ac.phase != 0.0 {
                out.push_str(&format!(" {}", ac.phase));
            }
        }
        if let Some(w) = &i.waveform {
            out.push(' ');
            out.push_str(&waveform_text(w));
        }
        out.push('\n');
    }
    // Behavioral sources: verbatim expressions (the compiled AST recompiles
    // byte-for-byte; emitting any canonicalized form would risk double-scale).
    // ABM entries (TODO §5) re-spell the PSpice card shape they were parsed
    // from — VALUE/TABLE/LAPLACE/FREQ/CHEBYSHEV on E/F/G/H.
    for b in &circuit.behavioral {
        match &b.abm {
            Some(abm) => emit_abm_source(circuit, b, abm, out),
            None => {
                let letter = match b.kind {
                    BehavioralKind::Voltage => 'V',
                    BehavioralKind::Current => 'I',
                };
                out.push_str(&format!(
                    "{} {} {} {}={}\n",
                    b.name,
                    name_of(b.out_p),
                    name_of(b.out_n),
                    letter,
                    b.expr
                ));
            }
        }
    }

    // Op-amps: the bare `U` keeps its empty model slot; a model name
    // re-attaches the `.model … U(…)` card emitted by `emit_models`.
    for u in &circuit.opamps {
        let model = if u.model.is_empty() { String::new() } else { format!(" {}", u.model) };
        out.push_str(&format!(
            "{} {} {} {} {}{}\n",
            u.name,
            name_of(u.out_p),
            name_of(u.out_n),
            name_of(u.in_p),
            name_of(u.in_n),
            model
        ));
    }

    // ── Controlled sources, switches, coupled inductors ──────────────────
    for (letter, family) in [
        ('E', &circuit.vcvs),
        ('G', &circuit.vccs),
        ('H', &circuit.ccvs),
        ('F', &circuit.cccs),
    ] {
        for c in family {
            out.push_str(&format!(
                "{} {} {}",
                c.name,
                name_of(c.out_p),
                name_of(c.out_n)
            ));
            if c.is_linear() {
                match &c.controls[0] {
                    ControlVar::Voltage(a, b) => {
                        out.push_str(&format!(" {} {} {}", name_of(*a), name_of(*b), c.coeff(0)));
                    }
                    ControlVar::Current(src) => {
                        out.push_str(&format!(" {src} {}", c.coeff(0)));
                    }
                }
            } else {
                out.push_str(&format!(" POLY({})", c.controls.len()));
                for cv in &c.controls {
                    match cv {
                        ControlVar::Voltage(a, b) => {
                            out.push_str(&format!(" {} {}", name_of(*a), name_of(*b)));
                        }
                        ControlVar::Current(src) => out.push_str(&format!(" {src}")),
                    }
                }
                for coeff in &c.coeffs {
                    out.push_str(&format!(" {coeff}"));
                }
            }
            let _ = letter;
            out.push('\n');
        }
    }
    for s in &circuit.switches {
        let control = match &s.control {
            SwitchControl::Voltage(a, b) => format!("{} {}", name_of(*a), name_of(*b)),
            SwitchControl::Current(src) => src.clone(),
        };
        let state = match s.initial {
            Some(true) => " ON",
            Some(false) => " OFF",
            None => "",
        };
        out.push_str(&format!(
            "{} {} {} {} {}{}\n",
            s.name,
            name_of(s.n1),
            name_of(s.n2),
            control,
            s.model,
            state
        ));
    }
    for k in &circuit.mutual_inductances {
        out.push_str(&format!(
            "{} {} {} {}\n",
            k.name, k.inductor1, k.inductor2, k.coupling
        ));
    }
}
