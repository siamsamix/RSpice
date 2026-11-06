//! The schematic symbol library: a small line-based text format that gives a
//! part its pins, its drawing, and the SPICE data placing it attaches — the
//! model or `.subckt` it references, the library file that supplies it, and a
//! default `spiceline` of extra card text (TODO §7).
//!
//! ```text
//! * comment (a `*` first on the line; `;` starts an inline comment)
//! SYMBOL nmos M              name, SPICE prefix letter
//!   DESC    N-channel MOSFET
//!   CATEGORY MOSFETs         the picker's group
//!   PIN D 0 0                name, then x y in grid units (y grows down)
//!   PIN G -2 2               the FIRST pin is the symbol's origin: (0 0)
//!   LINE   0 0 0 1           body drawing: LINE x1 y1 x2 y2
//!   RECT   0 0 2 1           RECT x1 y1 x2 y2
//!   CIRCLE 1 1 0.25          CIRCLE cx cy r      (drawing units are grid units)
//!   MODEL  NMOS1             default `.model` card the part references
//!   SUBCKT opamp             — or the `.subckt` it instantiates (one of the two)
//!   LIB    standard.lib      file that defines the reference (`.include`d once)
//!   SPICELINE W=10u L=1u     default extra text after the reference
//! ENDS
//! ```
//!
//! Pins are authored in the symbol's own frame (rotation 0); the editor turns
//! the frame in quarter turns. The pin order **is** the SPICE card's node
//! order, so the card for a placed part is simply
//!
//! ```text
//! <instance> <node of pin 1> … <node of pin n> [<reference>] [<spiceline>]
//! ```
//!
//! which covers the families whose cards share that shape: `D` `Q` `J` `M`
//! `S` `W` `X` (reference = model or subckt) and `E` `G` `B` `I` `U`-style
//! cards whose tail is free text (reference empty, everything in the
//! spiceline). `R`, `C`, `L` and `V` are drawn by the editor's own tools and
//! are refused as symbol prefixes. This format is RSpice's own — modelled on
//! the idea of LTspice's `.asy` symbol files, not byte-compatible with them.

use crate::error::{Result, SimError};

/// The shipped symbols, kept as one `.sym` file next to the shipped `.lib`s.
const BUILTIN: &str = include_str!("../../libs/standard.sym");

/// Prefix letters the editor draws with its own tools; a library symbol may
/// not claim them (their cards come from typed values, not from symbols).
const RESERVED_PREFIXES: [char; 4] = ['R', 'C', 'L', 'V'];

/// Prefixes whose card is malformed without a model/subckt name.
const NEEDS_REFERENCE: [char; 7] = ['D', 'M', 'Q', 'J', 'S', 'W', 'X'];

/// What a symbol's reference names.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RefKind {
    /// A `.model` card (`D1N4148`, `NMOS1`).
    Model,
    /// A `.subckt` definition an `X` card instantiates.
    Subckt,
}

#[derive(Debug, Clone, PartialEq)]
pub struct SymbolPin {
    pub name: String,
    pub x: i32,
    pub y: i32,
}

/// One body-drawing primitive, in grid units of the symbol's own frame.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Shape {
    Line { x1: f32, y1: f32, x2: f32, y2: f32 },
    Rect { x1: f32, y1: f32, x2: f32, y2: f32 },
    Circle { cx: f32, cy: f32, r: f32 },
}

#[derive(Debug, Clone, PartialEq)]
pub struct Symbol {
    pub name: String,
    /// Uppercase SPICE prefix letter: the card's first letter.
    pub prefix: char,
    pub description: String,
    pub category: String,
    pub pins: Vec<SymbolPin>,
    pub shapes: Vec<Shape>,
    /// Default model or subckt name; empty when the user must supply one.
    pub reference: String,
    pub ref_kind: RefKind,
    /// Library file that defines `reference` (resolved like `.include`).
    pub lib: Option<String>,
    /// Default extra card text after the reference.
    pub spiceline: String,
}

impl Symbol {
    /// The SPICE card for one placed instance. `nodes` are the deck spellings
    /// of the pins' nets in pin order; `reference` and `spiceline` are the
    /// instance's attached data (the placement starts from this symbol's
    /// defaults, the user edits from there).
    pub fn card(
        &self,
        instance: &str,
        nodes: &[String],
        reference: &str,
        spiceline: &str,
    ) -> Result<String> {
        valid_instance_name(self.prefix, instance)?;
        if nodes.len() != self.pins.len() {
            return Err(SimError::Circuit(format!(
                "{instance}: symbol '{}' has {} pins but {} nets were given",
                self.name,
                self.pins.len(),
                nodes.len()
            )));
        }
        let reference = reference.trim();
        let spiceline = spiceline.trim();
        if reference.is_empty() && NEEDS_REFERENCE.contains(&self.prefix) {
            let what = match self.ref_kind {
                RefKind::Model => "model",
                RefKind::Subckt => "subckt",
            };
            return Err(SimError::Circuit(format!(
                "{instance}: needs a {what} name (set it in the part's properties)"
            )));
        }
        if reference.contains(char::is_whitespace) {
            return Err(SimError::Circuit(format!(
                "{instance}: the {} name '{reference}' must be one word — extra \
                 text belongs in the SpiceLine field",
                match self.ref_kind {
                    RefKind::Model => "model",
                    RefKind::Subckt => "subckt",
                }
            )));
        }
        // One part is one card: a line break would smuggle a second card (or
        // a dot command) into the deck.
        if reference.contains(['\n', '\r']) || spiceline.contains(['\n', '\r']) {
            return Err(SimError::Circuit(format!(
                "{instance}: model and SpiceLine text must be a single line"
            )));
        }
        let mut card = String::from(instance);
        for node in nodes {
            card.push(' ');
            card.push_str(node);
        }
        if !reference.is_empty() {
            card.push(' ');
            card.push_str(reference);
        }
        if !spiceline.is_empty() {
            card.push(' ');
            card.push_str(spiceline);
        }
        Ok(card)
    }
}

/// An instance name must start with its symbol's prefix letter (that letter
/// is what makes the card the right device) and stay one plain word: no
/// whitespace, no `.` (the hierarchy separator), no separators.
pub fn valid_instance_name(prefix: char, name: &str) -> Result<()> {
    let first = name.chars().next().map(|c| c.to_ascii_uppercase());
    if first != Some(prefix.to_ascii_uppercase()) {
        return Err(SimError::Circuit(format!(
            "instance name '{name}' must start with '{prefix}'"
        )));
    }
    if !name.chars().all(|c| c.is_ascii_alphanumeric() || c == '_') {
        return Err(SimError::Circuit(format!(
            "instance name '{name}' may only use letters, digits and '_'"
        )));
    }
    Ok(())
}

/// An ordered set of symbols, names unique case-insensitively.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct SymbolLibrary {
    pub symbols: Vec<Symbol>,
}

impl SymbolLibrary {
    /// The shipped library (`libs/standard.sym`).
    pub fn builtin() -> Self {
        Self::parse(BUILTIN).expect("libs/standard.sym is a valid symbol library")
    }

    pub fn get(&self, name: &str) -> Option<&Symbol> {
        self.symbols.iter().find(|s| s.name.eq_ignore_ascii_case(name))
    }

    /// Picker groups in first-seen order.
    pub fn categories(&self) -> Vec<&str> {
        let mut out: Vec<&str> = Vec::new();
        for s in &self.symbols {
            if !out.iter().any(|c| c.eq_ignore_ascii_case(&s.category)) {
                out.push(&s.category);
            }
        }
        out
    }

    /// Add `other`'s symbols; one with the same name replaces the earlier
    /// one in place (a user library overrides a shipped symbol).
    pub fn extend(&mut self, other: SymbolLibrary) {
        for sym in other.symbols {
            match self.symbols.iter().position(|s| s.name.eq_ignore_ascii_case(&sym.name)) {
                Some(i) => self.symbols[i] = sym,
                None => self.symbols.push(sym),
            }
        }
    }

    pub fn parse(text: &str) -> Result<Self> {
        let mut lib = SymbolLibrary::default();
        let mut open: Option<Symbol> = None;
        for (i, raw) in text.lines().enumerate() {
            let line_no = i + 1;
            let err = |msg: String| SimError::Parse(format!("symbol library line {line_no}: {msg}"));
            let line = raw.split(';').next().unwrap_or("").trim();
            if line.is_empty() || line.starts_with('*') {
                continue;
            }
            let (word, rest) = match line.split_once(char::is_whitespace) {
                Some((w, r)) => (w, r.trim()),
                None => (line, ""),
            };
            let keyword = word.to_ascii_uppercase();

            if keyword == "SYMBOL" {
                if let Some(prev) = &open {
                    return Err(err(format!("symbol '{}' is missing its ENDS", prev.name)));
                }
                let toks: Vec<&str> = rest.split_whitespace().collect();
                let [name, prefix] = toks[..] else {
                    return Err(err("SYMBOL needs a name and a prefix letter".into()));
                };
                let mut chars = prefix.chars();
                let prefix = match (chars.next(), chars.next()) {
                    (Some(c), None) if c.is_ascii_alphabetic() => c.to_ascii_uppercase(),
                    _ => return Err(err(format!("prefix '{prefix}' must be one letter"))),
                };
                if RESERVED_PREFIXES.contains(&prefix) {
                    return Err(err(format!(
                        "prefix '{prefix}' belongs to the editor's built-in R/C/L/V tools"
                    )));
                }
                if lib.get(name).is_some() {
                    return Err(err(format!("symbol '{name}' is defined twice")));
                }
                open = Some(Symbol {
                    name: name.to_string(),
                    prefix,
                    description: String::new(),
                    category: "Other".to_string(),
                    pins: Vec::new(),
                    shapes: Vec::new(),
                    reference: String::new(),
                    ref_kind: if prefix == 'X' { RefKind::Subckt } else { RefKind::Model },
                    lib: None,
                    spiceline: String::new(),
                });
                continue;
            }

            let Some(sym) = open.as_mut() else {
                return Err(err(format!("'{word}' outside a SYMBOL … ENDS block")));
            };
            match keyword.as_str() {
                "ENDS" => {
                    let done = open.take().expect("open symbol");
                    check_symbol(&done).map_err(err)?;
                    lib.symbols.push(done);
                }
                "DESC" => sym.description = rest.to_string(),
                "CATEGORY" => {
                    if rest.is_empty() {
                        return Err(err("CATEGORY needs a name".into()));
                    }
                    sym.category = rest.to_string();
                }
                "PIN" => {
                    let toks: Vec<&str> = rest.split_whitespace().collect();
                    let [name, x, y] = toks[..] else {
                        return Err(err("PIN needs: name x y".into()));
                    };
                    let int = |s: &str| {
                        s.parse::<i32>().map_err(|_| err(format!("'{s}' is not a whole grid coordinate")))
                    };
                    sym.pins.push(SymbolPin { name: name.to_string(), x: int(x)?, y: int(y)? });
                }
                "LINE" | "RECT" | "CIRCLE" => {
                    let nums: std::result::Result<Vec<f32>, _> =
                        rest.split_whitespace().map(str::parse::<f32>).collect();
                    let nums = nums
                        .ok()
                        .filter(|v| v.iter().all(|x| x.is_finite()))
                        .ok_or_else(|| err(format!("{keyword} takes numbers only")))?;
                    sym.shapes.push(match (keyword.as_str(), nums[..].len()) {
                        ("LINE", 4) => Shape::Line { x1: nums[0], y1: nums[1], x2: nums[2], y2: nums[3] },
                        ("RECT", 4) => Shape::Rect { x1: nums[0], y1: nums[1], x2: nums[2], y2: nums[3] },
                        ("CIRCLE", 3) if nums[2] > 0.0 => {
                            Shape::Circle { cx: nums[0], cy: nums[1], r: nums[2] }
                        }
                        ("CIRCLE", 3) => return Err(err("CIRCLE radius must be positive".into())),
                        ("CIRCLE", _) => return Err(err("CIRCLE needs: cx cy r".into())),
                        _ => return Err(err(format!("{keyword} needs: x1 y1 x2 y2"))),
                    });
                }
                "MODEL" | "SUBCKT" => {
                    if rest.is_empty() || rest.contains(char::is_whitespace) {
                        return Err(err(format!("{keyword} takes one name")));
                    }
                    sym.reference = rest.to_string();
                    sym.ref_kind = if keyword == "MODEL" { RefKind::Model } else { RefKind::Subckt };
                }
                "LIB" => {
                    if rest.is_empty() {
                        return Err(err("LIB needs a file name".into()));
                    }
                    sym.lib = Some(rest.to_string());
                }
                "SPICELINE" => sym.spiceline = rest.to_string(),
                other => return Err(err(format!("unknown keyword '{other}'"))),
            }
        }
        if let Some(prev) = open {
            return Err(SimError::Parse(format!(
                "symbol library: symbol '{}' is missing its ENDS",
                prev.name
            )));
        }
        Ok(lib)
    }

    /// The canonical text of the library; `parse(emit(l)) == l`.
    pub fn emit(&self) -> String {
        let mut out = String::new();
        for s in &self.symbols {
            out.push_str(&format!("SYMBOL {} {}\n", s.name, s.prefix));
            if !s.description.is_empty() {
                out.push_str(&format!("  DESC {}\n", s.description));
            }
            out.push_str(&format!("  CATEGORY {}\n", s.category));
            for p in &s.pins {
                out.push_str(&format!("  PIN {} {} {}\n", p.name, p.x, p.y));
            }
            for shape in &s.shapes {
                match *shape {
                    Shape::Line { x1, y1, x2, y2 } => out.push_str(&format!("  LINE {x1} {y1} {x2} {y2}\n")),
                    Shape::Rect { x1, y1, x2, y2 } => out.push_str(&format!("  RECT {x1} {y1} {x2} {y2}\n")),
                    Shape::Circle { cx, cy, r } => out.push_str(&format!("  CIRCLE {cx} {cy} {r}\n")),
                }
            }
            if !s.reference.is_empty() {
                let key = match s.ref_kind {
                    RefKind::Model => "MODEL",
                    RefKind::Subckt => "SUBCKT",
                };
                out.push_str(&format!("  {key} {}\n", s.reference));
            }
            if let Some(lib) = &s.lib {
                out.push_str(&format!("  LIB {lib}\n"));
            }
            if !s.spiceline.is_empty() {
                out.push_str(&format!("  SPICELINE {}\n", s.spiceline));
            }
            out.push_str("ENDS\n\n");
        }
        out
    }
}

/// Whole-symbol checks, run at `ENDS`: the placement and the card writer
/// both trust these, like `Circuit::validate` is trusted after parse.
fn check_symbol(s: &Symbol) -> std::result::Result<(), String> {
    let Some(first) = s.pins.first() else {
        return Err(format!("symbol '{}' has no PIN", s.name));
    };
    if (first.x, first.y) != (0, 0) {
        return Err(format!(
            "symbol '{}': the first pin '{}' must sit at the origin (0 0), not ({} {})",
            s.name, first.name, first.x, first.y
        ));
    }
    for (i, p) in s.pins.iter().enumerate() {
        if s.pins[..i].iter().any(|q| q.name.eq_ignore_ascii_case(&p.name)) {
            return Err(format!("symbol '{}': pin name '{}' is repeated", s.name, p.name));
        }
        if s.pins[..i].iter().any(|q| (q.x, q.y) == (p.x, p.y)) {
            return Err(format!(
                "symbol '{}': pins '{}' share the point ({} {}) — they would short",
                s.name, p.name, p.x, p.y
            ));
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    const DIODE: &str = "SYMBOL d1 D\n PIN A 0 0\n PIN K 3 0\n MODEL D1N4148\n LIB standard.lib\nENDS\n";

    #[test]
    fn builtin_library_parses_and_round_trips() {
        let lib = SymbolLibrary::builtin();
        assert!(lib.symbols.len() >= 8, "the shipped set covers the device families");
        assert_eq!(SymbolLibrary::parse(&lib.emit()).unwrap(), lib);
    }

    #[test]
    fn a_symbol_reads_its_pins_shapes_and_attached_data() {
        let lib = SymbolLibrary::parse(
            "* header\nSYMBOL nmos M ; name prefix\n DESC N-channel\n CATEGORY MOSFETs\n \
             PIN D 0 0\n PIN G -2 2\n PIN S 0 4\n PIN B 2 2\n LINE 0 0 0 1\n \
             RECT 0 0 1 1\n CIRCLE 1 1 0.5\n MODEL NMOS1\n LIB standard.lib\n \
             SPICELINE W=10u L=1u\nENDS\n",
        )
        .unwrap();
        let s = lib.get("NMOS").expect("case-insensitive lookup");
        assert_eq!(s.prefix, 'M');
        assert_eq!(s.pins.iter().map(|p| p.name.as_str()).collect::<Vec<_>>(), ["D", "G", "S", "B"]);
        assert_eq!(s.shapes.len(), 3);
        assert_eq!((s.reference.as_str(), s.ref_kind), ("NMOS1", RefKind::Model));
        assert_eq!(s.lib.as_deref(), Some("standard.lib"));
        assert_eq!(s.spiceline, "W=10u L=1u");
        assert_eq!(s.category, "MOSFETs");
    }

    #[test]
    fn an_x_symbol_defaults_to_a_subckt_reference() {
        let lib = SymbolLibrary::parse("SYMBOL amp X\n PIN P 0 0\n PIN N 0 2\n SUBCKT opamp\nENDS\n").unwrap();
        assert_eq!(lib.get("amp").unwrap().ref_kind, RefKind::Subckt);
    }

    #[test]
    fn the_card_follows_pin_order_then_reference_then_spiceline() {
        let lib = SymbolLibrary::parse(
            "SYMBOL nmos M\n PIN D 0 0\n PIN G 0 1\n PIN S 0 2\n PIN B 0 3\n MODEL NMOS1\nENDS\n",
        )
        .unwrap();
        let nodes: Vec<String> = ["d", "g", "0", "0"].iter().map(|s| s.to_string()).collect();
        let s = lib.get("nmos").unwrap();
        assert_eq!(s.card("M1", &nodes, "NMOS1", "W=10u L=1u").unwrap(), "M1 d g 0 0 NMOS1 W=10u L=1u");
        assert_eq!(s.card("MM1", &nodes, "BSIM", "").unwrap(), "MM1 d g 0 0 BSIM");
    }

    #[test]
    fn a_free_text_card_needs_no_reference() {
        let lib = SymbolLibrary::parse("SYMBOL bsrc B\n PIN P 0 0\n PIN N 0 2\n SPICELINE V=V(1)*2\nENDS\n").unwrap();
        let nodes = vec!["a".to_string(), "0".to_string()];
        let s = lib.get("bsrc").unwrap();
        assert_eq!(s.card("B1", &nodes, "", &s.spiceline).unwrap(), "B1 a 0 V=V(1)*2");
    }

    #[test]
    fn a_device_card_without_its_model_is_refused() {
        let lib = SymbolLibrary::parse(DIODE).unwrap();
        let nodes = vec!["a".to_string(), "0".to_string()];
        let err = lib.get("d1").unwrap().card("D1", &nodes, "  ", "").unwrap_err().to_string();
        assert!(err.contains("needs a model name"), "{err}");
    }

    #[test]
    fn attached_text_cannot_smuggle_a_second_card() {
        let lib = SymbolLibrary::parse(DIODE).unwrap();
        let s = lib.get("d1").unwrap();
        let nodes = vec!["a".to_string(), "0".to_string()];
        assert!(s.card("D1", &nodes, "D1N4148", "area=2\nV9 1 0 DC 5").is_err());
        assert!(s.card("D1", &nodes, "D1N4148\nR1 1 0 1", "").is_err());
        assert!(s.card("D1", &nodes, "two words", "").is_err());
    }

    #[test]
    fn instance_names_keep_the_prefix_and_one_plain_word() {
        assert!(valid_instance_name('M', "MM1").is_ok());
        assert!(valid_instance_name('M', "m_load2").is_ok());
        assert!(valid_instance_name('M', "Q1").is_err());
        assert!(valid_instance_name('M', "M 1").is_err());
        assert!(valid_instance_name('M', "M1.x").is_err());
        assert!(valid_instance_name('M', "").is_err());
    }

    #[test]
    fn malformed_libraries_fail_loudly_with_a_line_number() {
        let cases = [
            ("SYMBOL r1 R\n PIN A 0 0\nENDS\n", "R/C/L/V"),
            ("SYMBOL d D\n PIN A 1 0\nENDS\n", "origin"),
            ("SYMBOL d D\nENDS\n", "no PIN"),
            ("SYMBOL d D\n PIN A 0 0\n PIN A 3 0\nENDS\n", "repeated"),
            ("SYMBOL d D\n PIN A 0 0\n PIN K 0 0\nENDS\n", "short"),
            ("SYMBOL d D\n PIN A 0 0\n FOO 1\nENDS\n", "unknown keyword"),
            ("SYMBOL d D\n PIN A 0 0\n", "missing its ENDS"),
            ("PIN A 0 0\n", "outside a SYMBOL"),
            ("SYMBOL d D\n PIN A 0 x\nENDS\n", "whole grid"),
            ("SYMBOL d D\n PIN A 0 0\n CIRCLE 0 0 -1\nENDS\n", "positive"),
            ("SYMBOL d D\n PIN A 0 0\nENDS\nSYMBOL D D\n PIN A 0 0\nENDS\n", "twice"),
            ("SYMBOL d DD\n PIN A 0 0\nENDS\n", "one letter"),
        ];
        for (text, needle) in cases {
            let err = SymbolLibrary::parse(text).unwrap_err().to_string();
            assert!(err.contains(needle), "{text:?} -> {err}");
        }
    }

    /// Every shipped symbol, placed with its defaults, must be a card the
    /// deck grammar reads — the pin order, the reference position and the
    /// default spiceline are checked against the real parser, not by eye.
    #[test]
    fn every_shipped_symbol_places_as_a_card_the_parser_accepts() {
        use crate::netlist::{parse_with_options, ParseOptions};
        let options = ParseOptions {
            deck_dir: None,
            search_paths: vec![std::path::PathBuf::from(concat!(env!("CARGO_MANIFEST_DIR"), "/../libs"))],
        };
        for sym in &SymbolLibrary::builtin().symbols {
            let nodes: Vec<String> = (1..=sym.pins.len()).map(|i| format!("n{i}")).collect();
            // A generic symbol ships no reference; stand in a two-port
            // subcircuit from the shipped library.
            let generic = sym.reference.is_empty() && NEEDS_REFERENCE.contains(&sym.prefix);
            let reference = if generic { "lowpass" } else { sym.reference.as_str() };
            let card = sym
                .card(&format!("{}1", sym.prefix), &nodes, reference, &sym.spiceline)
                .unwrap_or_else(|e| panic!("{}: {e}", sym.name));
            let mut deck = String::from("* symbol check\n");
            if let Some(lib) = sym.lib.as_deref().or(generic.then_some("standard.lib")) {
                deck.push_str(&format!(".include {lib}\n"));
            }
            deck.push_str(&format!("{card}\nV9 n1 0 DC 1\n"));
            for i in 1..=sym.pins.len() {
                deck.push_str(&format!("Rpull{i} n{i} 0 1k\n"));
            }
            deck.push_str(".op\n.end\n");
            parse_with_options(&deck, &options)
                .unwrap_or_else(|e| panic!("symbol '{}' card `{card}` failed: {e}", sym.name));
        }
    }

    #[test]
    fn extending_replaces_a_same_name_symbol_in_place() {
        let mut lib = SymbolLibrary::builtin();
        let before = lib.symbols.len();
        let mine = SymbolLibrary::parse("SYMBOL diode D\n PIN A 0 0\n PIN K 2 0\n MODEL DLED\nENDS\n").unwrap();
        lib.extend(mine);
        assert_eq!(lib.symbols.len(), before);
        assert_eq!(lib.get("diode").unwrap().reference, "DLED");
    }
}
