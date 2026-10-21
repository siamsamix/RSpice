//! Parameter language — `.param`, `.func`, `{expr}` substitution, and the
//! math function library.
//!
//! Element values and directive arguments are written against a grammar, so
//! the expression grammar lives beside the value grammar ([`crate::units`]).
//! Every consumer (element lines, analysis commands, `.dc` sweeps of a
//! parameter) goes through the same environment; string parameters substitute
//! into element cards verbatim (e.g. model names), numbers back into text
//! through a round-trip-safe format.

use std::collections::HashMap;

/// One `.param` entry: a number or a string (model/file names, quoted text).
#[derive(Debug, Clone, PartialEq)]
pub enum ParamValue {
    Num(f64),
    Str(String),
}

impl ParamValue {
    /// Canonical re-parseable text. Rust's `Display` for `f64` prints the
    /// shortest round-tripping decimal, which [`crate::units::parse_value`]
    /// always reads back exactly.
    pub fn to_text(&self) -> String {
        match self {
            ParamValue::Num(v) => format!("{v}"),
            ParamValue::Str(s) => s.clone(),
        }
    }
}

/// A user-defined `.func` body.
#[derive(Debug, Clone)]
pub struct FuncDef {
    pub args: Vec<String>,
    pub body: String,
}

/// The parameter environment built while the deck is read: parameter table,
/// function table, and every element line expands against the current state
/// (in-order evaluation, as in the reference tools).
///
/// `mc_seed` feeds the deterministic `random()`/`rand()`/`gauss()` draws
/// (TODO §8, LTspice-style Monte Carlo faked through `.step`): one parse is
/// stable — every expansion of the same draw lands on the same number, so a
/// Newton solve never sees a moving parameter — and a stepped run re-rolls
/// because the sweep machinery re-parses per step with that step's overrides
/// mixed into the seed. A plain `parse()` keeps seed 0, so an unstepped deck
/// is bit-reproducible.
#[derive(Debug, Clone, Default)]
pub struct ParamEnv {
    pub params: HashMap<String, ParamValue>,
    pub funcs: HashMap<String, FuncDef>,
    /// Seed mixed into every pseudo-random draw; derived from the
    /// step/sweep overrides by the caller.
    pub mc_seed: u64,
}

/// Leaf failure of expression evaluation; callers wrap it with line context.
pub type EvalError = String;

impl ParamEnv {
    /// Evaluate one expression to a parameter value.
    pub fn eval(&self, expr: &str) -> Result<ParamValue, EvalError> {
        let tokens = lex(expr)?;
        let mut p = Parser { tokens: &tokens, pos: 0, env: self, locals: &HashMap::new(), depth: 0 };
        let v = p.expr()?;
        if p.pos != tokens.len() {
            return Err(format!(
                "unexpected token after expression: '{}'",
                token_text(&tokens[p.pos])
            ));
        }
        // Functions such as sqrt(-1), 1e300^2 or ln(0) produce non-finite
        // results without raising anywhere else — reject them loudly here.
        match v {
            ParamValue::Num(n) if !n.is_finite() => {
                Err("expression evaluated to a non-finite value".into())
            }
            _ => Ok(v),
        }
    }

    /// Evaluate one expression to a number (the common case for values).
    pub fn eval_num(&self, expr: &str) -> Result<f64, EvalError> {
        match self.eval(expr)? {
            ParamValue::Num(v) => Ok(v),
            ParamValue::Str(s) => Err(format!("string parameter '{s}' used where a number is required")),
        }
    }

    /// Numeric lookup used by the parameter-sweep resolution.
    pub fn lookup_num(&self, name: &str) -> Option<f64> {
        match self.params.get(&name.to_ascii_lowercase()) {
            Some(ParamValue::Num(v)) => Some(*v),
            _ => None,
        }
    }
}

/// Format a numeric parameter back into netlist text.
pub fn num_to_text(v: f64) -> String {
    format!("{v}")
}

// ─── Lexer ───────────────────────────────────────────────────────────────────

#[derive(Debug, Clone, PartialEq)]
enum Tok {
    Num(f64),
    Ident(String),
    Op(char),
}

fn token_text(t: &Tok) -> String {
    match t {
        Tok::Num(v) => format!("{v}"),
        Tok::Ident(s) => s.clone(),
        Tok::Op(c) => c.to_string(),
    }
}

fn lex(src: &str) -> Result<Vec<Tok>, EvalError> {
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
            // Braces are grouping noise at this level: `expand_line` peels the
            // outer pair, so nested `{ {a} + 2 }` reintroduces them and `.func`
            // bodies may embed `{param}` references. Skip them.
            '{' | '}' => {
                i += 1;
            }
            '0'..='9' | '.' => {
                let start = i;
                while i < chars.len() && (chars[i].is_ascii_digit() || chars[i] == '.') {
                    i += 1;
                }
                // Exponent, only when actually followed by digits.
                if i < chars.len() && (chars[i] == 'e' || chars[i] == 'E') {
                    let mut j = i + 1;
                    if j < chars.len() && (chars[j] == '+' || chars[j] == '-') {
                        j += 1;
                    }
                    if j < chars.len() && chars[j].is_ascii_digit() {
                        while j < chars.len() && chars[j].is_ascii_digit() {
                            j += 1;
                        }
                        i = j;
                    }
                }
                // Trailing scale suffix / unit letters belong to the number
                // (`1k`, `1meg`); the shared value parser handles them.
                while i < chars.len() && chars[i].is_ascii_alphabetic() {
                    i += 1;
                }
                let text: String = chars[start..i].iter().collect();
                let v = crate::units::parse_value(&text)
                    .map_err(|e| format!("bad number '{text}': {e}"))?;
                toks.push(Tok::Num(v));
            }
            c if c.is_ascii_alphabetic() || c == '_' => {
                let start = i;
                while i < chars.len() && (chars[i].is_ascii_alphanumeric() || chars[i] == '_') {
                    i += 1;
                }
                toks.push(Tok::Ident(chars[start..i].iter().collect::<String>().to_ascii_lowercase()));
            }
            other => return Err(format!("unexpected character '{other}' in expression")),
        }
    }
    Ok(toks)
}

// ─── Parser / evaluator ──────────────────────────────────────────────────────

struct Parser<'a> {
    tokens: &'a [Tok],
    pos: usize,
    env: &'a ParamEnv,
    locals: &'a HashMap<String, ParamValue>,
    depth: u32,
}

const MAX_DEPTH: u32 = 64;

impl<'a> Parser<'a> {
    fn peek(&self) -> Option<&Tok> {
        self.tokens.get(self.pos)
    }

    fn peek_clone(&self) -> Option<Tok> {
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

    fn expect_op(&mut self, c: char) -> Result<(), EvalError> {
        if self.eat_op(c) {
            Ok(())
        } else {
            Err(format!("expected '{}'", c))
        }
    }

    /// expr := term (('+' | '-') term)*
    fn expr(&mut self) -> Result<ParamValue, EvalError> {
        let mut left = self.term()?;
        loop {
            if self.eat_op('+') {
                let r = self.term()?;
                left = arithmetic(left, r, '+')?;
            } else if self.eat_op('-') {
                let r = self.term()?;
                left = arithmetic(left, r, '-')?;
            } else {
                return Ok(left);
            }
        }
    }

    /// term := unary (('*' | '/') unary)*
    fn term(&mut self) -> Result<ParamValue, EvalError> {
        let mut left = self.unary()?;
        loop {
            if self.eat_op('*') {
                let r = self.unary()?;
                left = arithmetic(left, r, '*')?;
            } else if self.eat_op('/') {
                let r = self.unary()?;
                left = arithmetic(left, r, '/')?;
            } else {
                return Ok(left);
            }
        }
    }

    /// unary := ('-' | '+') unary | power
    fn unary(&mut self) -> Result<ParamValue, EvalError> {
        if self.eat_op('-') {
            let v = self.unary()?;
            return match v {
                ParamValue::Num(n) => Ok(ParamValue::Num(-n)),
                ParamValue::Str(s) => Err(format!("cannot negate string '{s}'")),
            };
        }
        if self.eat_op('+') {
            return self.unary();
        }
        self.power()
    }

    /// power := atom ('^' unary)? — right-associative, so `2^3^2` is 2^9.
    fn power(&mut self) -> Result<ParamValue, EvalError> {
        let base = self.atom()?;
        if self.eat_op('^') {
            let exp = self.unary()?;
            return arithmetic(base, exp, '^');
        }
        Ok(base)
    }

    /// atom := number | ident | ident '(' args ')' | '(' expr ')'
    fn atom(&mut self) -> Result<ParamValue, EvalError> {
        if self.depth > MAX_DEPTH {
            return Err("expression nested too deeply".into());
        }
        let next = match self.peek_clone() {
            Some(t) => t,
            None => return Err("unexpected end of expression".into()),
        };
        match next {
            Tok::Num(v) => {
                self.pos += 1;
                Ok(ParamValue::Num(v))
            }
            Tok::Ident(name) => {
                self.pos += 1;
                if matches!(self.peek_clone(), Some(Tok::Op('('))) {
                    self.pos += 1;
                    let mut args = Vec::new();
                    // Zero-argument calls still need the `()`.
                    if !matches!(self.peek_clone(), Some(Tok::Op(')'))) {
                        loop {
                            self.depth += 1;
                            let a = self.expr();
                            self.depth -= 1;
                            args.push(a?);
                            if self.eat_op(',') {
                                continue;
                            }
                            break;
                        }
                    }
                    self.expect_op(')')?;
                    self.call_function(&name, args)
                } else {
                    // Variable reference: function-arg local or parameter,
                    // both case-insensitive.
                    if let Some(v) = self.locals.get(&name) {
                        Ok(v.clone())
                    } else if let Some(v) = self.env.params.get(&name) {
                        Ok(v.clone())
                    } else {
                        Err(format!("unknown parameter or function '{name}'"))
                    }
                }
            }
            Tok::Op('(') => {
                self.pos += 1;
                self.depth += 1;
                let v = self.expr();
                self.depth -= 1;
                let v = v?;
                self.expect_op(')')?;
                Ok(v)
            }
            other => Err(format!("unexpected {} in expression", token_text(&other))),
        }
    }

    fn call_function(&self, name: &str, args: Vec<ParamValue>) -> Result<ParamValue, EvalError> {
        // User-defined functions shadow the library, as in the reference tools.
        if let Some(def) = self.env.funcs.get(name) {
            if args.len() != def.args.len() {
                return Err(format!(
                    "function {name} expects {} argument(s), got {}",
                    def.args.len(),
                    args.len()
                ));
            }
            let mut locals = HashMap::new();
            for (arg, val) in def.args.iter().zip(args) {
                locals.insert(arg.clone(), val);
            }
            let mut nested = Parser {
                tokens: &lex(&def.body)?,
                pos: 0,
                env: self.env,
                locals: &locals,
                depth: self.depth + 1,
            };
            return nested.expr().map_err(|e| format!("in {name}(): {e}"));
        }

        let nums: Result<Vec<f64>, _> = args
            .into_iter()
            .map(|a| match a {
                ParamValue::Num(v) => Ok(v),
                ParamValue::Str(s) => Err(format!("string parameter '{s}' used where a number is required")),
            })
            .collect();
        let nums = nums?;
        let need = |n: usize| -> Result<(), EvalError> {
            if nums.len() != n {
                Err(format!("{name}() expects {n} argument(s), got {}", nums.len()))
            } else {
                Ok(())
            }
        };
        let num0 = nums.first().copied().unwrap_or(0.0);
        let num1 = nums.get(1).copied().unwrap_or(0.0);
        let v = match name {
            "abs" => { need(1)?; num0.abs() }
            "sqrt" => { need(1)?; num0.sqrt() }
            "exp" => { need(1)?; num0.exp() }
            "ln" => { need(1)?; num0.ln() }
            "log" => { need(1)?; num0.log10() } // SPICE convention: log() is base 10
            "log10" => { need(1)?; num0.log10() }
            "log2" => { need(1)?; num0.log2() }
            "sin" => { need(1)?; num0.sin() }
            "cos" => { need(1)?; num0.cos() }
            "tan" => { need(1)?; num0.tan() }
            "asin" => { need(1)?; num0.asin() }
            "acos" => { need(1)?; num0.acos() }
            "atan" => { need(1)?; num0.atan() }
            "sinh" => { need(1)?; num0.sinh() }
            "cosh" => { need(1)?; num0.cosh() }
            "tanh" => { need(1)?; num0.tanh() }
            "floor" => { need(1)?; num0.floor() }
            "ceil" => { need(1)?; num0.ceil() }
            "round" => { need(1)?; num0.round() }
            "int" => { need(1)?; num0.trunc() }
            "sign" => { need(1)?; num0.signum() }
            "atan2" => { need(2)?; num0.atan2(num1) }
            "pow" | "pwr" => { need(2)?; num0.powf(num1) }
            "hypot" => { need(2)?; num0.hypot(num1) }
            "mod" => {
                need(2)?;
                if num1 == 0.0 {
                    return Err("mod(): division by zero".into());
                }
                num0 % num1
            }
            "min" => {
                if nums.is_empty() {
                    return Err("min() expects at least 1 argument".into());
                }
                nums.iter().cloned().fold(f64::INFINITY, f64::min)
            }
            "max" => {
                if nums.is_empty() {
                    return Err("max() expects at least 1 argument".into());
                }
                nums.iter().cloned().fold(f64::NEG_INFINITY, f64::max)
            }
            // Monte-Carlo draws (TODO §8). Each is fully determined by the
            // parse seed (mixed from the step/sweep overrides) and the call's
            // amplitude, so one netlist expansion is stable and a `.step`
            // re-parse re-rolls the draw.
            "random" => { need(1)?; unit01(draw_word(self.env.mc_seed, "random", num0)) * num0 }
            "rand" => { need(1)?; signed_uniform(draw_word(self.env.mc_seed, "rand", num0), num0) }
            "gauss" => {
                need(1)?;
                num0 * standard_normal(draw_word(self.env.mc_seed, "gauss", num0))
            }
            _ => return Err(format!("unknown function '{name}'")),
        };
        Ok(ParamValue::Num(v))
    }
}

fn arithmetic(l: ParamValue, r: ParamValue, op: char) -> Result<ParamValue, EvalError> {
    let (a, b) = match (l, r) {
        (ParamValue::Num(a), ParamValue::Num(b)) => (a, b),
        (ParamValue::Str(s), _) | (_, ParamValue::Str(s)) => {
            return Err(format!("string parameter '{s}' used in arithmetic"));
        }
    };
    let v = match op {
        '+' => a + b,
        '-' => a - b,
        '*' => a * b,
        '/' => {
            if b == 0.0 {
                return Err("division by zero in expression".into());
            }
            a / b
        }
        '^' => a.powf(b),
        _ => unreachable!(),
    };
    if v.is_nan() || v.is_infinite() {
        return Err(format!("arithmetic overflow ({a} {op} {b})"));
    }
    Ok(ParamValue::Num(v))
}

// ─── Deterministic pseudo-random draws (TODO §8) ─────────────────────────────

/// SplitMix64 finalizer: one avalanche step gives well-mixed 64-bit words
/// from small inputs, without carrying generator state between calls.
/// Also the keyed-draw primitive behind the B sources' `white()`/`flicker()`
/// rolls ([`crate::behavior`]).
pub(crate) fn mix64(mut x: u64) -> u64 {
    x = x.wrapping_add(0x9E37_79B9_7F4A_7C15);
    x = (x ^ (x >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
    x = (x ^ (x >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
    x ^ (x >> 31)
}

/// Mix a string tag into the draw key (function-stand-in discrimination).
fn mix_str(x: u64, tag: &str) -> u64 {
    let mut h = x ^ 0x27D4_EB2F_1656_67C5;
    for b in tag.bytes() {
        h = mix64(h ^ (b as u64));
    }
    h
}

/// The draw word for one call site: function tag, amplitude, and the parse's
/// random seed. Every equal call inside one netlist expansion gets the same
/// word; a stepped re-parse (different seed) re-rolls it.
fn draw_word(seed: u64, tag: &str, amplitude: f64) -> u64 {
    mix64(mix_str(seed ^ amplitude.to_bits(), tag))
}

/// Uniform deviate in `[0, 1)`.
pub(crate) fn unit01(word: u64) -> f64 {
    (word >> 11) as f64 * 2f64.powi(-53)
}

/// Uniform deviate in `[-magnitude, magnitude]` (LTspice `rand(x)`).
fn signed_uniform(word: u64, magnitude: f64) -> f64 {
    (unit01(word) * 2.0 - 1.0) * magnitude
}

/// One standard normal via Box–Muller on two draw words (LTspice `gauss(x)`).
fn standard_normal(word: u64) -> f64 {
    let u = unit01(word).max(1e-12);
    let v = unit01(mix64(word ^ 0xD1B5_4A32_D192_ED03));
    (-2.0 * u.ln()).sqrt() * (std::f64::consts::TAU * v).cos()
}

/// Per-parse draw seed: the sorted (parameter, value) overrides mixed into
/// one 64-bit word. A `.step` of `param run 1 100 1` therefore gives 100
/// different, bit-reproducible Monte-Carlo runs; an empty override map
/// hashes to 0, the constant of an unstepped deck.
pub fn seed_from_overrides(overrides: &HashMap<String, f64>) -> u64 {
    let mut keys: Vec<&String> = overrides.keys().collect();
    keys.sort_unstable();
    let mut h = 0u64;
    for key in keys {
        h = mix_str(h, &key.to_ascii_lowercase());
        h = mix64(h ^ overrides[key].to_bits());
    }
    h
}

/// A stable tag word for one element name, mixed into keyed draws
/// (`white()`/`flicker()` on B sources) so two sources never share a roll
/// bit pattern. Names are unique per deck by the letter rule, so equality
/// here can only be a false collision — and the name is folded verbatim,
/// which also separates `X1.B1` from `X2.B1`.
pub fn tag_word(name: &str) -> u64 {
    mix_str(mix64(0), name)
}

// ─── Substitution pass ───────────────────────────────────────────────────────

/// The keyword tokens an ABM card can open its tail with (`E`/`F`/`G`/`H`
/// after their two output nodes): a classic gain spelling cannot produce
/// `KEYWORD {expr}` — control names are bare tokens, never `=` with a
/// brace group behind them. Shared with the element parsers (single
/// source of truth for the keyword set).
pub const ABM_KEYWORDS: &[&str] =
    &["value", "equation", "table", "laplace", "freq", "chebyshev"];

/// True when a line looks like an ABM card: one of the keywords spelled at
/// the start of (or glued as `KEYWORD=` inside) a whitespace token, with a
/// brace group on the same line — behind an `=`, or in the following token.
///
/// The substitution pass must **leave such a line untouched** (best-effort
/// path only): the brace groups belong to the card's own grammar even when
/// their text evaluates as a plain numeric constant (`LAPLACE {V(in)} = {1}`
/// would otherwise lose its numerator's braces before the card parser ran),
/// and the ABM parser folds deck parameters itself.
pub fn is_abm_card_line(line: &str) -> bool {
    let tokens = line.split_whitespace().collect::<Vec<&str>>();
    for (i, tok) in tokens.iter().enumerate() {
        let lower = tok.to_ascii_lowercase();
        for keyword in ABM_KEYWORDS {
            let glued = format!("{keyword}=");
            let brace_glued = format!("{keyword}{{");
            let hit = lower == *keyword
                || lower.starts_with(&glued)
                || lower.starts_with(&brace_glued);
            if hit {
                // The group can ride the token itself (`VALUE={…}`), sit in
                // the next token (`TABLE {…}`), or detach across a spaced
                // `=` (`LAPLACE = {…}` — two token positions of slack).
                let groomed = tok.contains('{')
                    || tokens.get(i + 1).is_some_and(|n| n.starts_with('{'))
                    || tokens.get(i + 1).is_some_and(|n| {
                        n.starts_with('=')
                            && (n.contains('{')
                                || tokens.get(i + 2).is_some_and(|m| m.starts_with('{')))
                    });
                return groomed;
            }
        }
    }
    false
}

/// Replace every `{expr}` and single-quoted `'expr'` in a line with its
/// evaluated value. Double quotes pass through untouched (string literals on
/// `.param` cards); unbalanced `'` stays literal (an apostrophe in a comment
/// is already gone by this point — this line carries no comment text).
///
/// Best-effort: a group the parameter language cannot evaluate (behavioral
/// probe text like `V={V(in)*gain}`) stays verbatim for the card-level
/// parser to judge; a genuinely broken value still reaches a loud error
/// there. See [`expand_line_strict`] for the variant `.param` cards use.
pub fn expand_line(line: &str, env: &ParamEnv) -> Result<String, EvalError> {
    expand_evaluated(line, env, true)
}

/// [`expand_line`] with the parameter language strictly enforced: a group
/// that fails to evaluate is an error instead of verbatim text.
///
/// `.param` values are pure parameter language — a missing name must fail
/// loudly here with that name. Passing an unresolved `'…'` group through
/// would only move the failure into the assignment parser, which would then
/// reject the quote characters and never mention the real cause.
pub fn expand_line_strict(line: &str, env: &ParamEnv) -> Result<String, EvalError> {
    expand_evaluated(line, env, false)
}

fn expand_evaluated(line: &str, env: &ParamEnv, verbatim: bool) -> Result<String, EvalError> {
    // An ABM card's braces are grammatical, not grouping (see
    // `is_abm_card_line`): the card parser folds parameters itself, so the
    // substitution pass moves aside completely.
    if verbatim && is_abm_card_line(line) {
        return Ok(line.to_string());
    }
    let chars: Vec<char> = line.chars().collect();
    let mut out = String::new();
    let mut i = 0usize;
    while i < chars.len() {
        match chars[i] {
            '{' => {
                let mut depth = 1usize;
                let mut j = i + 1;
                while j < chars.len() {
                    if chars[j] == '{' {
                        depth += 1;
                    } else if chars[j] == '}' {
                        depth -= 1;
                        if depth == 0 {
                            break;
                        }
                    }
                    j += 1;
                }
                if j >= chars.len() {
                    return Err("unbalanced '{' without closing '}'".into());
                }
                let expr: String = chars[i + 1..j].iter().collect();
                match env.eval(expr.trim()) {
                    Ok(v) => out.push_str(&v.to_text()),
                    // Best-effort substitution: a group the parameter
                    // language cannot evaluate stays verbatim and the
                    // card-level parser judges it — B/default expressions
                    // (`V={V(in)*gain}`) carry probe text the parameter
                    // language must not touch, and a genuinely broken value
                    // still reaches a loud error there.
                    Err(_) if verbatim => {
                        out.extend(chars[i..=j].iter().copied());
                    }
                    Err(e) => return Err(e),
                }
                i = j + 1;
            }
            '\'' => {
                let close = chars[i + 1..].iter().position(|&c| c == '\'');
                match close {
                    Some(k) => {
                        let expr: String = chars[i + 1..i + 1 + k].iter().collect();
                        match env.eval(expr.trim()) {
                            Ok(v) => out.push_str(&v.to_text()),
                            // Same best-effort rule: `'V(a)*gain'` belongs
                            // to the device expression.
                            Err(_) if verbatim => out.extend(chars[i..=i + 1 + k].iter().copied()),
                            Err(e) => return Err(e),
                        }
                        i = i + k + 2;
                    }
                    None => {
                        out.push('\'');
                        i += 1;
                    }
                }
            }
            c => {
                out.push(c);
                i += 1;
            }
        }
    }
    Ok(out)
}

/// Is this token a bare identifier (candidate for a string parameter)?
fn is_plain_word(s: &str) -> bool {
    let mut chars = s.chars();
    matches!(chars.next(), Some(c) if c.is_ascii_alphabetic() || c == '_')
        && chars.all(|c| c.is_ascii_alphanumeric() || c == '_')
}

/// Split a `.param` body into tokens on whitespace, but never inside a
/// `"quoted"` value — `note="five; six"` stays a single token.
fn param_tokens(body: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut cur = String::new();
    let mut in_quote = false;
    for c in body.chars() {
        if c == '"' {
            in_quote = !in_quote;
            cur.push(c);
        } else if c.is_whitespace() && !in_quote {
            if !cur.is_empty() {
                out.push(std::mem::take(&mut cur));
            }
        } else {
            cur.push(c);
        }
    }
    if !cur.is_empty() {
        out.push(cur);
    }
    out
}

/// Parse the body of a `.param` line (already `{}`/`'..'`-expanded) into the
/// environment. Forms: `name=value`, `name= value`, `name = value`, several
/// assignments at once. A value may be `"quoted text"` (string parameter) or
/// `'quoted expression'` (the quotes are stripped before evaluation).
/// Existing entries keep the *first* spelling like a node name would — late
/// redefinitions replace the value.
pub fn parse_param_body(
    body: &str,
    env: &mut ParamEnv,
    overrides: &HashMap<String, f64>,
) -> Result<(), EvalError> {
    let toks = param_tokens(body);
    let mut i = 0usize;
    while i < toks.len() {
        let t = &toks[i];
        let (name, consumed, value) = match t.find('=') {
            None if toks.get(i + 1).map(String::as_str) == Some("=") => {
                let v = toks
                    .get(i + 2)
                    .ok_or_else(|| format!(".param '{t}' is missing its value"))?;
                (t.clone(), 3, v.clone())
            }
            None => return Err(format!(".param expects name=value, got '{t}'")),
            Some(0) => return Err(".param assignment is missing a name".into()),
            Some(pos) => {
                let inline = &t[pos + 1..];
                let name = t[..pos].to_string();
                if inline.is_empty() {
                    let v = toks
                        .get(i + 1)
                        .ok_or_else(|| format!(".param '{name}' is missing its value"))?;
                    (name, 2, v.clone())
                } else {
                    (name, 1, inline.to_string())
                }
            }
        };
        if !is_plain_word(&name) {
            return Err(format!(".param '{name}' is not a valid parameter name"));
        }

        let key = name.to_ascii_lowercase();
        let pv = if let Some(&v) = overrides.get(&key) {
            ParamValue::Num(v)
        } else if value.len() >= 2 && value.starts_with('"') && value.ends_with('"') {
            ParamValue::Str(value[1..value.len() - 1].to_string())
        } else {
            // `'…'` wraps an expression that escapes expansion in some flows;
            // strip the wrapper before evaluating.
            let stripped = if value.len() >= 2
                && value.starts_with('\'')
                && value.ends_with('\'')
            {
                value[1..value.len() - 1].to_string()
            } else {
                value.clone()
            };
            match env.eval_num(&stripped) {
                Ok(n) => ParamValue::Num(n),
                Err(e) => {
                    if is_plain_word(&stripped) {
                        ParamValue::Str(stripped)
                    } else {
                        return Err(format!(".param {name}: {e}"));
                    }
                }
            }
        };
        env.params.insert(key, pv);
        i += consumed;
    }
    Ok(())
}

/// Parse the body of a `.func` line: `.func name(arg,…) {body}` — the body
/// braces are optional and the body is NOT expanded at definition time
/// (it references the function's own arguments).
pub fn parse_func_body(body: &str, env: &mut ParamEnv) -> Result<(), EvalError> {
    let body = body.trim();
    // Name up to '(' or the first space.
    let name_len = body
        .find(|c: char| c == '(' || c.is_whitespace())
        .unwrap_or(body.len());
    let name = body[..name_len].trim().to_ascii_lowercase();
    if name.is_empty() {
        return Err(".func requires a function name".into());
    }
    let rest = body[name_len..].trim_start();

    let (args, def_body) = if let Some(open) = rest.find('(') {
        // Argument list, possibly spanning whitespace: up to the matching ')'.
        if !rest[..open].trim().is_empty() {
            return Err(format!(".func {name}: unexpected text between name and '('"));
        }
        let close = rest[open + 1..]
            .find(')')
            .ok_or_else(|| format!(".func {name}: missing ')'"))?;
        let args_text = &rest[open + 1..open + 1 + close];
        let args: Vec<String> = args_text
            .split(',')
            .map(|a| a.trim().to_ascii_lowercase())
            .filter(|a| !a.is_empty())
            .collect();
        for a in &args {
            if !is_plain_word(a) {
                return Err(format!(".func {name}: invalid argument '{a}'"));
            }
        }
        (args, rest[open + 1 + close + 1..].trim())
    } else {
        (Vec::new(), "")
    };

    // The definition body is the remainder; require it now.
    let def_body = def_body.trim();
    if def_body.is_empty() {
        return Err(format!(".func {name}: missing body expression"));
    }
    // Optional `{...}` wrapper.
    let cleaned = if def_body.starts_with('{') && def_body.ends_with('}') && def_body.len() >= 2 {
        def_body[1..def_body.len() - 1].trim()
    } else {
        def_body
    };
    if cleaned.is_empty() {
        return Err(format!(".func {name}: missing body expression"));
    }
    // Reject the body lexically now so errors surface at definition, not call.
    lex(cleaned)?;
    env.funcs.insert(name, FuncDef { args, body: cleaned.to_string() });
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn env_of(entries: &[(&str, f64)]) -> ParamEnv {
        let mut env = ParamEnv::default();
        for (k, v) in entries {
            env.params.insert(k.to_string(), ParamValue::Num(*v));
        }
        env
    }

    #[test]
    fn evaluates_arithmetic() {
        let env = env_of(&[("a", 2.0), ("b", 3.0)]);
        assert_eq!(env.eval_num("a+b").unwrap(), 5.0);
        assert_eq!(env.eval_num("a*b - b^2").unwrap(), -3.0);
        assert_eq!(env.eval_num("(a+b)/(a*b)").unwrap(), 5.0 / 6.0);
        assert_eq!(env.eval_num("-a + 1").unwrap(), -1.0);
        assert_eq!(env.eval_num("2*3^2").unwrap(), 18.0); // ^ binds tighter
    }

    #[test]
    fn suffix_numbers_inside_expressions() {
        let env = ParamEnv::default();
        assert_eq!(env.eval_num("1k*2").unwrap(), 2000.0);
        assert_eq!(env.eval_num("4.7meg/2").unwrap(), 2.35e6);
    }

    #[test]
    fn math_library() {
        let env = ParamEnv::default();
        assert_eq!(env.eval_num("sqrt(9)").unwrap(), 3.0);
        assert_eq!(env.eval_num("abs(-3)").unwrap(), 3.0);
        assert_eq!(env.eval_num("log(100)").unwrap(), 2.0); // base 10
        assert_eq!(env.eval_num("ln(exp(2))").unwrap(), 2.0);
        assert_eq!(env.eval_num("max(1, 5, 3)").unwrap(), 5.0);
        assert_eq!(env.eval_num("min(1, 5, 3)").unwrap(), 1.0);
        assert_eq!(env.eval_num("hypot(3,4)").unwrap(), 5.0);
        assert_eq!(env.eval_num("pow(2,10)").unwrap(), 1024.0);
        assert_eq!(env.eval_num("sign(-9)").unwrap(), -1.0);
    }

    #[test]
    fn user_functions_shadow_and_recursion_is_bounded() {
        let mut env = ParamEnv::default();
        parse_func_body("f(x,y) {x*y+1}", &mut env).unwrap();
        assert_eq!(env.eval_num("f(2,3)").unwrap(), 7.0);
        parse_func_body("sin(x) {x}", &mut env).unwrap(); // shadows the library
        assert_eq!(env.eval_num("sin(5)").unwrap(), 5.0);
        parse_func_body("loop(x) {loop(x)}", &mut env).unwrap();
        assert!(env.eval_num("loop(1)").is_err());
    }

    #[test]
    fn string_params_substitute_into_lines() {
        let mut env = ParamEnv::default();
        env.params.insert("mymod".into(), ParamValue::Str("D1N4148".into()));
        let line = expand_line("D1 1 0 {mymod}", &env).unwrap();
        assert_eq!(line, "D1 1 0 D1N4148");
        // Arithmetic on a string parameter must fail loudly.
        assert!(env.eval("mymod*2").is_err());
    }

    #[test]
    fn quoted_expressions_expand() {
        let env = env_of(&[("t", 2.0)]);
        assert_eq!(expand_line("R1 1 0 '1k*(1+t/100)'", &env).unwrap(), "R1 1 0 1020");
        assert_eq!(expand_line("R2 1 0 {1e3} {2}", &env).unwrap(), "R2 1 0 1000 2");
    }

    #[test]
    fn nested_braces_expand_innermost_out() {
        let mut env = env_of(&[("a", 3.0)]);
        env.params.insert("b".into(), ParamValue::Num(4.0));
        assert_eq!(expand_line("V1 1 0 {a*b}", &env).unwrap(), "V1 1 0 12");
        // `{ {a} + 2 }` — the outer scan counts braces before evaluating.
        assert_eq!(expand_line("V2 1 0 { {a} + 2 }", &env).unwrap(), "V2 1 0 5");
    }

    /// ABM cards' braces are grammatical, not grouping — the substitution
    /// pass leaves such a line alone (a plain-numeric LAPLACE group would
    /// otherwise pre-evaluate into bare text ahead of the card parser).
    #[test]
    fn abm_cards_skip_the_substitution_pass() {
        let env = env_of(&[("gain", 4.0)]);
        let line = "E1 out 0 LAPLACE {V(in)} = {1} {1, gain}";
        assert_eq!(expand_line(line, &env).unwrap(), line.to_string());
        // The shape detector covers every spelling split_abm accepts.
        assert!(is_abm_card_line("E1 o 0 VALUE={V(1)*2}"));
        assert!(is_abm_card_line("E1 o 0 TABLE {V(1)} = (0,0)"));
        assert!(is_abm_card_line("G1 o 0 VALUE ={V(1)*2}"));
        assert!(is_abm_card_line("F1 o 0 CHEBYSHEV{V(1)} 1"));
        // A keyword token with no brace group behind it is not an ABM card
        // (a `freq`-named node still rides the classic grammar there).
        assert!(!is_abm_card_line("E1 o 0 freq 2"));
        // Non-ABM lines substitute exactly as before — the carve-out is
        // scoped to the keyword+group shape.
        let line = expand_line("R1 a 0 {1k*gain}", &env).unwrap();
        assert_eq!(line, "R1 a 0 4000");
    }

    /// Best-effort vs strict expansion: element lines pass unevaluated
    /// groups (probe text like `V={V(in)*gain}`) through verbatim for the
    /// device parser to judge; `.param` card values are pure parameter
    /// language, so the strict variant fails with the missing name.
    #[test]
    fn expand_verbatim_and_strict_split() {
        let env = env_of(&[("t", 2.0)]);

        // Probe text the parameter language must not touch — stays verbatim.
        let line = expand_line("B1 out 0 V={V(in)*t}", &env).unwrap();
        assert_eq!(line, "B1 out 0 V={V(in)*t}");
        let line = expand_line("B2 out 0 V={'V(in)*gain'}", &env).unwrap();
        assert_eq!(line, "B2 out 0 V={'V(in)*gain'}");

        // The same quote text on a `.param` card is strict.
        let err = expand_line_strict("r = '1k*gain'", &env).unwrap_err();
        assert!(err.to_string().contains("gain"), "error was: {err}");

        // And it resolves once the name is defined above.
        let mut env = env_of(&[("t", 2.0)]);
        env.params.insert("gain".into(), ParamValue::Num(4.0));
        assert_eq!(expand_line_strict("r = '1k*gain'", &env).unwrap(), "r = 4000");
    }

    #[test]
    fn param_cards_assign_and_override_wins() {
        let mut env = ParamEnv::default();
        parse_param_body("rl=1k gain = -3 vtol='0.5*2'", &mut env, &HashMap::new()).unwrap();
        assert_eq!(env.lookup_num("rl"), Some(1000.0));
        assert_eq!(env.lookup_num("gain"), Some(-3.0));
        assert_eq!(env.lookup_num("vtol"), Some(1.0));

        let mut ov = HashMap::new();
        ov.insert("rl".to_string(), 47e3);
        env.params.clear();
        parse_param_body("rl=1k", &mut env, &ov).unwrap();
        assert_eq!(env.lookup_num("rl"), Some(47e3));
    }

    #[test]
    fn func_parsing_variants() {
        let mut env = ParamEnv::default();
        parse_func_body("f(a,b) { a * b }", &mut env).unwrap();
        parse_func_body("g(x) x*2", &mut env).unwrap();          // bare body
        parse_func_body("h (p, q ) { p - q }", &mut env).unwrap(); // detached parens
        assert_eq!(env.eval_num("g(5)").unwrap(), 10.0);
        assert_eq!(env.eval_num("h(9, 4)").unwrap(), 5.0);
        assert!(parse_func_body("k(x)", &mut env).is_err());
    }

    #[test]
    fn division_by_zero_and_unknown_names_error() {
        let env = ParamEnv::default();
        assert!(env.eval_num("1/0").is_err());
        assert!(env.eval_num("nope").is_err());
        assert!(env.eval("1 +").is_err());
        assert!(env.eval_num("sqrt(-1)").is_err());
    }

    // ─── Monte-Carlo draws (TODO §8) ─────────────────────────────────────────

    #[test]
    fn mc_draws_are_stable_within_a_parse_and_bounded() {
        let mut env = ParamEnv::default();
        env.mc_seed = 0x5EED_CAFE;
        let a = env.eval_num("random(5)").unwrap();
        assert_eq!(a, env.eval_num("random(5)").unwrap(), "stable within a parse");
        assert!((0.0..5.0).contains(&a), "random(5) in [0,5): {a}");

        let r = env.eval_num("rand(2)").unwrap();
        assert!((-2.0..=2.0).contains(&r), "rand(2) inside ±2: {r}");

        let g = env.eval_num("gauss(0.05)").unwrap();
        assert!(g.abs() < 0.4, "gauss(0.05) within 8σ: {g}");
        assert_eq!(env.eval_num("gauss(0)").unwrap(), 0.0);

        // Wrong arity is still a loud error.
        assert!(env.eval_num("random()").is_err());
        assert!(env.eval_num("rand(1, 2)").is_err());
    }

    #[test]
    fn mc_draws_shift_with_seed_and_are_reproducible() {
        let mut e1 = ParamEnv::default();
        e1.mc_seed = 1;
        let mut e2 = ParamEnv::default();
        e2.mc_seed = 2;
        let draw = |e: &ParamEnv| e.eval_num("random(1)").unwrap();
        assert_ne!(draw(&e1).to_bits(), draw(&e2).to_bits());
        assert_eq!(draw(&e1), draw(&e1), "same seed reproduces the same draw");
    }

    /// Statistical shape over many independent seeds: zero mean, unit
    /// standard deviation, bounded tails.
    #[test]
    fn mc_gauss_distribution_is_statistically_sane() {
        let n = 2500u32;
        let mut sum = 0.0f64;
        let mut sum_sq = 0.0f64;
        let mut max_abs = 0.0f64;
        let mut env = ParamEnv::default();
        for i in 0..n {
            env.mc_seed = u64::from(i) * 0x9E37_79B9; // scattered seeds
            let z = env.eval_num("gauss(1)").unwrap();
            sum += z;
            sum_sq += z * z;
            max_abs = max_abs.max(z.abs());
        }
        let mean = sum / n as f64;
        let var = sum_sq / n as f64;
        // SE of the mean at n=2500 is ~0.02 → allow 3 SE.
        assert!(mean.abs() < 0.06, "mean {mean}");
        assert!((var - 1.0).abs() < 0.12, "variance {var}");
        assert!(max_abs < 7.5, "tail |z| = {max_abs} exceeds the Box–Muller bound");
    }

    #[test]
    fn mc_seed_mixes_override_map_deterministically() {
        let mut one: HashMap<String, f64> = HashMap::new();
        one.insert("run".into(), 3.0);
        let mut two = one.clone();
        two.insert("run".into(), 4.0);
        assert_ne!(seed_from_overrides(&one), seed_from_overrides(&two));
        assert_eq!(
            seed_from_overrides(&one),
            seed_from_overrides(&one),
            "pure function of the map"
        );
        assert_eq!(seed_from_overrides(&HashMap::new()), 0);
    }

    /// Element-name tags for keyed (B-source) draws: stable per name,
    /// different across names.
    #[test]
    fn tag_words_separate_names_and_are_stable() {
        assert_eq!(tag_word("B1"), tag_word("B1"), "stable per name");
        assert_ne!(tag_word("B1"), tag_word("B2"));
        assert_ne!(tag_word("X1.B1"), tag_word("X2.B1"), "instance paths count");
    }
}
