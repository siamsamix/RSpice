use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};

use crate::circuit::{
    Ac, Am, BehavioralKind, BehavioralSource, Bjt, BjtModel, BjtType, Capacitor, Circuit,
    ControlVar, ControlledSource, CurrentSource, Diode, DiodeModel, Exp, GenericModel, Inductor,
    Jfet, JfetModel, JfetType, Mosfet, MosfetModel, MosfetType, NodeId, Pwl, Pulse, Resistor,
    Sffm, Sine, Switch, SwitchControl, SwitchModel, VoltageSource, Waveform,
};
use crate::error::{Result, SimError};
use crate::model::ModelTolerance;
use crate::params::{
    expand_line, parse_func_body, parse_param_body, seed_from_overrides, FuncDef, ParamEnv,
    ParamValue,
};
use crate::units::parse_value;

// The card grammar submodules (each family's parse beside its emit).
use self::dotcards::parse_dot_command;
use self::models::parse_model_card;
use self::elements::{
    parse_behavioral_source, parse_bjt, parse_capacitor, parse_controlled_source,
    parse_current_source, parse_diode, parse_inductor, parse_jfet, parse_mosfet,
    parse_mutual_inductance, parse_resistor, parse_switch, parse_voltage_source,
};
use self::imports::splice_libraries;
use self::nodeplan::{collect_global_nets, element_letter, scan_node_plan};
use self::subckts::{collect_subckts, flatten_scope, ScopeCtx, SubcktDef};

/// Supported sweep scale (mirrors SPICE `.ac` syntax).
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum AcScale {
    /// Logarithmically spaced points per decade.
    Dec,
    /// Logarithmically spaced points per octave.
    Oct,
    /// Linearly spaced points.
    Lin,
}

/// Parameters extracted from a `.ac` command.
#[derive(Debug, Clone)]
pub struct AcCommand {
    pub scale: AcScale,
    /// Points per decade/octave, or total linear points.
    pub points: usize,
    /// Start frequency (Hz).
    pub f_start: f64,
    /// Stop frequency (Hz).
    pub f_stop: f64,
    /// Index of the voltage source to use as the AC stimulus (0-based into
    /// `circuit.voltage_sources`).  `None` = drive the first source.
    pub stimulus_source: Option<usize>,
}

/// One `.dc` sweep: an independent source or a `.param`, driven over a
/// linear ramp or an explicit value list.
#[derive(Debug, Clone)]
pub struct DcSweepSpec {
    /// Name of a `V`/`I` source card or a `.param` key.
    pub target: String,
    pub values: DcSweepValues,
}

#[derive(Debug, Clone)]
pub enum DcSweepValues {
    /// Start/stop/increment ramp.
    Linear { start: f64, stop: f64, step: f64 },
    /// Explicit value list (`.dc V1 LIST 0 1 2`).
    List(Vec<f64>),
}

/// The full `.dc` command: one sweep, or two (the second nests inside the
/// first — SPICE steps the *second* source fastest).
#[derive(Debug, Clone)]
pub struct DcCommand {
    pub sweeps: Vec<DcSweepSpec>,
}

/// One `.ic`/`.nodeset` assignment: a node name and the value it seeds.
/// The node text is resolved after parsing (node names are final then).
#[derive(Debug, Clone)]
pub struct NodesetEntry {
    pub node: String,
    pub value: f64,
}

/// Output variable of a `.tf` card: a node voltage (optionally differential)
/// or the current through a voltage source.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TfOutput {
    Voltage { node: String, reference: Option<String> },
    Current(String),
}

/// `.tf <V(out)|I(Vsrc)> <input source>` — DC transfer function.
#[derive(Debug, Clone)]
pub struct TfCommand {
    pub output: TfOutput,
    pub input: String,
}

/// How one `.step` sweep produces its values.
#[derive(Debug, Clone)]
pub enum StepValues {
    /// Linear ramp (start, stop, increment).
    Linear { start: f64, stop: f64, step: f64 },
    /// Logarithmic points per decade (start, stop, points/decade).
    Decade { start: f64, stop: f64, points: usize },
    /// Logarithmic points per octave (start, stop, points/octave).
    Octave { start: f64, stop: f64, points: usize },
    /// Explicit value list (`.step param x list 1 2 3`).
    List(Vec<f64>),
}

/// One `.step` card: a parameter driven over a value set. Several cards nest
/// (the first is the outer loop), exactly like `.dc`.
#[derive(Debug, Clone)]
pub struct StepSpec {
    /// `.param` key (lowercased).
    pub target: String,
    pub values: StepValues,
}

/// Which analysis result a `.meas` card reads.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MeasAnalysis {
    Tran,
    Ac,
    Dc,
    Op,
}

/// Scalar statistic family of `.meas`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MeasFunc {
    Avg,
    Rms,
    Max,
    Min,
    Pp,
    /// Abscissa (time/frequency/sweep value) at the maximum.
    MaxAt,
    /// Abscissa at the minimum.
    MinAt,
}

/// Edge selector for a crossing (rise, fall, or either).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MeasEdge {
    Rise,
    Fall,
    Cross,
}

/// `.four <fundamental Hz> <output…>` — Fourier decomposition of the last
/// period of a transient waveform.
#[derive(Debug, Clone)]
pub struct FourCommand {
    pub frequency: f64,
    pub outputs: Vec<String>,
}

/// `.noise V(out) <input source> DEC|OCT|LIN <points> <fstart> <fstop>`.
#[derive(Debug, Clone)]
pub struct NoiseCommand {
    pub output: TfOutput,
    pub input: String,
    pub scale: AcScale,
    pub points: usize,
    pub f_start: f64,
    pub f_stop: f64,
}

/// One `.meas` card, parsed into its evaluation shape.
#[derive(Debug, Clone)]
pub struct Measurement {
    pub analysis: MeasAnalysis,
    pub name: String,
    pub kind: MeasKind,
}

#[derive(Debug, Clone)]
pub enum MeasKind {
    /// `AVG/RMS/MAX/MIN/PP/MAX_AT/MIN_AT <expr> [FROM=…] [TO=…]`.
    Scalar {
        func: MeasFunc,
        expr: String,
        from: Option<f64>,
        to: Option<f64>,
    },
    /// `FIND <expr> WHEN <cond> [RISE|FALL|CROSS [=n]]`.
    FindWhen {
        expr: String,
        when: String,
        when_value: f64,
        edge: Option<(MeasEdge, usize)>,
        from: Option<f64>,
        to: Option<f64>,
    },
    /// `CROSS <expr> VAL=<v> [RISE|FALL|CROSS [=n]]`.
    Cross {
        expr: String,
        value: f64,
        edge: Option<(MeasEdge, usize)>,
        from: Option<f64>,
        to: Option<f64>,
    },
    /// `TRIG <expr> VAL=<v> [edge] TARG <expr> VAL=<v> [edge]` — returns the
    /// target time minus the trigger time.
    TrigTarg {
        trig_expr: String,
        trig_value: f64,
        trig_edge: Option<(MeasEdge, usize)>,
        targ_expr: String,
        targ_value: f64,
        targ_edge: Option<(MeasEdge, usize)>,
        from: Option<f64>,
        to: Option<f64>,
    },
}

#[derive(Debug, Clone)]
pub struct AnalysisCommands {
    pub dc_op: bool,
    pub dc: Option<DcCommand>,
    pub tran_step: Option<f64>,
    pub tran_stop: Option<f64>,
    pub tran_start: f64,
    /// `.tran … UIC`: the operating point is skipped and the transient
    /// starts from the `.ic` values (everything else at 0).
    pub tran_uic: bool,
    /// `.tran … STARTUP`: the DC operating point is solved and run with every
    /// source suppressed (zero) and the sources ramp to their card values
    /// over the first 10 µs of the transient (the LTspice startup ramp).
    pub tran_startup: bool,
    /// Optional `.tran … tmax` cap on the internal timestep.
    pub tran_maxstep: Option<f64>,
    /// `.options method=` integration method (trapezoidal default).
    pub method: crate::circuit::IntegrationMethod,
    /// Relative Newton/LTE tolerance (`reltol`, default 1e-3).
    pub reltol: f64,
    /// Absolute current tolerance (`abstol`, default 1e-12 A).
    pub abstol: f64,
    /// Absolute voltage tolerance (`vntol`, default 1e-6 V).
    pub vntol: f64,
    /// `.options gmin=` — the conductance floor (S) the engine keeps across
    /// every pn junction and capacitor at DC (TODO §3). Default 1e-12, the
    /// classic SPICE value.
    pub gmin: f64,
    /// `.options gminsteps=` — depth of the DC gmin-stepping retry ladder
    /// (one decade per rung). Default 10.
    pub gminsteps: usize,
    /// `.options srcsteps=` — depth of the DC source-stepping retry ladder
    /// (one equal fraction per rung). Default 10.
    pub srcsteps: usize,
    /// `.options itl1=` — Newton-Raphson iteration budget per DC operating
    /// point. Default 100, the SPICE classic.
    pub itl1: usize,
    /// `.options itl4=` — Newton-Raphson iteration budget per transient time
    /// point. Default 100 (kept from the pre-options engine; the SPICE
    /// classic default of 10 starved the damped-Newton line search).
    pub itl4: usize,
    pub ac: Option<AcCommand>,
    /// `.tf <output> <input>` — DC transfer function.
    pub tf: Option<TfCommand>,
    /// `.step` sweeps, first card outermost.
    pub steps: Vec<StepSpec>,
    /// `.meas`/`.measure` cards, evaluated against the analysis results.
    pub measurements: Vec<Measurement>,
    /// `.four <freq> <output…>` Fourier analysis of the last period.
    pub four: Option<FourCommand>,
    /// `.noise` output/input-referred noise sweep.
    pub noise: Option<NoiseCommand>,
    /// `.temp` values in °C. One value sets the run temperature; several act
    /// like `.step temp LIST …` runs.
    pub temperatures: Vec<f64>,
    /// `.ic V(n)=…` entries: bias/transient startup conditions. Without
    /// `UIC` they act as an operating-point seed; with `UIC` they pin the
    /// t=tstart state.
    pub ic: Vec<NodesetEntry>,
    /// `.nodeset V(n)=…` entries: seeds for the DC Newton-Raphson iteration.
    pub nodeset: Vec<NodesetEntry>,
}

impl Default for AnalysisCommands {
    fn default() -> Self {
        Self {
            dc_op: false,
            dc: None,
            tran_step: None,
            tran_stop: None,
            tran_start: 0.0,
            tran_uic: false,
            tran_startup: false,
            tran_maxstep: None,
            method: crate::circuit::IntegrationMethod::Trapezoidal,
            reltol: 1e-3,
            abstol: 1e-12,
            vntol: 1e-6,
            gmin: 1e-12,
            gminsteps: 10,
            srcsteps: 10,
            itl1: 100,
            itl4: 100,
            ac: None,
            tf: None,
            steps: Vec::new(),
            measurements: Vec::new(),
            four: None,
            noise: None,
            temperatures: Vec::new(),
            ic: Vec::new(),
            nodeset: Vec::new(),
        }
    }
}

/// How the parser reads files for directives that pull other files in
/// (`.include`, `.lib`).
///
/// The GUI builds for wasm, where files cannot be opened: file-backed values
/// there cannot be resolved from the platform. Options are transported with
/// the parsed [`Netlist`] so `.dc` parameter sweeps re-read the deck the
/// same way.
#[derive(Debug, Clone, Default)]
pub struct ParseOptions {
    /// The deck file's own directory; relative `.include`/`.lib` paths are
    /// tried here first. `None` for text the user pasted (GUI editor).
    pub deck_dir: Option<PathBuf>,
    /// Library directories searched after the deck's own directory —
    /// CLI-provided model directories, the `RSPICE_LIB` env entries, and the
    /// `nom.lib`/user-lib conventions of the reference tools.
    pub search_paths: Vec<PathBuf>,
}

#[derive(Debug, Clone)]
pub struct Netlist {
    pub circuit: Circuit,
    pub analysis: AnalysisCommands,
    /// The deck exactly as it was read — `.dc` sweeps of a `.param`
    /// re-expand the deck per sweep point, and that needs the original text.
    pub source: String,
    /// Final parameter table, for sweep-target resolution.
    pub params: HashMap<String, ParamValue>,
    /// The final `.func` table. `.meas`/`.four` expressions evaluate through
    /// it — it travels with the deck instead of being rebuilt (and dropped)
    /// by each consumer.
    pub funcs: HashMap<String, FuncDef>,
    /// The Monte-Carlo seed this parse rolled (the step/sweep overrides are
    /// mixed in), so runtime expression evaluation draws exactly the way the
    /// parse-time expansion did.
    pub mc_seed: u64,
    /// Best-effort import diagnostics: unknown directives and unsupported
    /// device kinds are recorded here and skipped, never fatal (TODO §2).
    pub warnings: Vec<String>,
    /// The options this deck was parsed with (needed for file resolution on
    /// `.dc` sweep re-parses).
    pub options: ParseOptions,
    /// Parameter values pinned by an enclosing `.step` run (empty for a
    /// top-level parse). Nested re-parses (`.dc` of a parameter) merge these
    /// so a stepped value is never lost mid-run.
    pub overrides: HashMap<String, f64>,
}

pub fn parse(source: &str) -> Result<Netlist> {
    parse_with_options(source, &ParseOptions::default())
}

/// Parse with library search options — what a deck read from disk wants
/// (its own directory next to the search paths) whenever it uses `.include`
/// or `.lib`.
pub fn parse_with_options(source: &str, options: &ParseOptions) -> Result<Netlist> {
    parse_with_overrides(source, options, &HashMap::new())
}

/// Parse honoring per-parameter overrides — the `.dc` sweep of a `.param`
/// path re-expands the deck with each swept value pinned, carrying the
/// surrounding parse's options so file-backed libraries resolve alike.
pub fn parse_with_overrides(
    source: &str,
    options: &ParseOptions,
    overrides: &HashMap<String, f64>,
) -> Result<Netlist> {
    parse_featured(source, options, overrides)
}

/// The one parser pipeline every entry point funnels through:
///
/// 1. comments/continuations → logical lines,
/// 2. `.include`/`.lib` spliced in as lines,
/// 3. `.global` net names collected (needed before bodies are flattened),
/// 4. `.subckt`/`.ends` definitions lifted out of the deck,
/// 5. `X` instances expanded in place — scopes get their own parameter
///    environment and internal nets are name-mangled to the instance path,
/// 6. the plan pre-scan and the deck-order card walk over the flat result.
fn parse_featured(
    source: &str,
    options: &ParseOptions,
    overrides: &HashMap<String, f64>,
) -> Result<Netlist> {
    let logical = logical_lines(source)?;
    let logical = splice_libraries(logical, options)?;
    let globals = collect_global_nets(&logical);
    let (flow, def_list) = collect_subckts(&logical).map_err(SimError::Parse)?;

    let mut warnings: Vec<String> = Vec::new();
    let mut defs: HashMap<String, SubcktDef> = HashMap::new();
    for def in def_list {
        if defs.contains_key(&def.name) {
            // Logged as a warning, not an error: the later definition wins,
            // matching how a re-spoken `.model` card behaves.
            warnings.push(format!(
                "line {}: .subckt '{}' redefined — the later definition wins",
                def.line_no, def.display_name
            ));
        }
        defs.insert(def.name.clone(), def);
    }

    // Flatten: scope-aware expansion and `X` instantiation. `.param`/`.func`
    // execute here (deck order preserved, instance scopes get fresh
    // environments); `.global` is already registered; element cards come out
    // node-mangled and `{}`-expanded, dot commands just `{}`-expanded.
    let mut env = ParamEnv::default();
    // Monte-Carlo draws key off the sweep overrides, so each stepped point
    // re-rolls deterministically (TODO §8).
    env.mc_seed = seed_from_overrides(overrides);
    let mut scope = ScopeCtx {
        defs: &defs,
        globals: &globals,
        overrides,
        warnings: Vec::new(),
    };
    let empty_bindings: HashMap<String, String> = HashMap::new();
    let flat = flatten_scope(&flow, "", "", &empty_bindings, &mut env, &mut scope, None)
        .map_err(SimError::Parse)?;
    warnings.extend(scope.warnings);

    let plan = scan_node_plan(&flat);

    let mut circuit = Circuit::new("untitled");
    let mut analysis = AnalysisCommands::default();

    for &(line_no, ref line) in &flat {
        let lower = line.to_ascii_lowercase();
        if lower == ".end" {
            break;
        }

        if line.starts_with('.') {
            parse_dot_command(
                line,
                &mut analysis,
                &mut circuit,
                &mut warnings,
                line_no,
            )?;
            continue;
        }

        let tokens: Vec<&str> = line.split_whitespace().collect();
        if tokens.len() < 4 {
            return Err(SimError::Parse(format!(
                "line {line_no}: expected at least 4 fields"
            )));
        }

        let kind = element_letter(tokens[0]);
        match kind.to_ascii_uppercase() {
            'R' => parse_resistor(&tokens, &mut circuit, &plan, &mut warnings, line_no)?,
            'C' => parse_capacitor(&tokens, &mut circuit, &plan, &mut warnings, line_no)?,
            'L' => parse_inductor(&tokens, &mut circuit, &plan, &mut warnings, line_no)?,
            'D' => parse_diode(&tokens, &mut circuit, &plan, &mut warnings, line_no)?,
            'V' => parse_voltage_source(&tokens, &mut circuit, &plan, options)?,
            'I' => parse_current_source(&tokens, &mut circuit, &plan, options)?,
            'M' => parse_mosfet(&tokens, &mut circuit, &plan, &mut warnings, line_no)?,
            'E' | 'G' | 'F' | 'H' => {
                parse_controlled_source(&tokens, &mut circuit, &plan, line_no, &mut warnings)?
            }
            'Q' => parse_bjt(&tokens, &mut circuit, &plan, &mut warnings, line_no)?,
            'J' => parse_jfet(&tokens, &mut circuit, &plan, &mut warnings, line_no)?,
            'S' | 'W' => parse_switch(&tokens, &mut circuit, &plan, line_no)?,
            'K' => parse_mutual_inductance(&tokens, &mut circuit, line_no)?,
            'B' => parse_behavioral_source(&tokens, &mut circuit, &plan, line_no)?,
            _ => {
                // Device families RSpice does not model yet are skipped with
                // a warning instead of failing the import (tolerant parsing).
                warnings.push(format!(
                    "line {line_no}: element type '{kind}' is not supported — card skipped: {line}"
                ));
            }
        }
    }

    // Series resistances need the final model table, so they split after the
    // card walk; the internal nodes then join compaction below.
    circuit.expand_diode_series_resistance();
    circuit.expand_jfet_series_resistance();
    circuit.expand_bjt_series_resistance();
    // Passive parasitics (`RSER`/`LSER`/`CPAR`/`RPAR`) materialize into real
    // element arms the same way, just before the node renumbering.
    circuit.expand_parasitics();
    // Node ids must be hole-free: row addressing below assumes ids 0..n-1 are
    // all stamped (a named id above an unused numeric id would otherwise leave
    // a singular row). Compact first, then validate.
    circuit.compact_node_ids();
    // Behavioral probes bind against the final node space / element tables;
    // they never create nodes, so this runs before `validate`.
    circuit.resolve_behavioral()?;
    circuit.validate()?;
    Ok(Netlist {
        circuit,
        analysis,
        source: source.to_string(),
        params: env.params,
        funcs: env.funcs,
        mc_seed: env.mc_seed,
        warnings,
        options: options.clone(),
        overrides: overrides.clone(),
    })
}

impl Netlist {
    /// The complete parameter environment this deck parsed to: values, user
    /// `.func` tables, and the Monte-Carlo seed. Measurement expressions
    /// evaluate through exactly this — the hand-built copies that dropped
    /// `funcs` are what made `.meas` lose user functions.
    pub fn param_env(&self) -> ParamEnv {
        ParamEnv {
            params: self.params.clone(),
            funcs: self.funcs.clone(),
            mc_seed: self.mc_seed,
        }
    }
}

// ─── Preprocessing: comments and continuations ──────────────────────────────

/// `*` opens a comment only where it *starts a line* (SPICE card rule — a
/// `*` inside a value or quoted text is data). `;` opens an inline comment
/// anywhere outside double quotes. Comments are removed first, so the
/// continuation logic below never sees comment text.
fn strip_comment<'a>(line: &'a str) -> std::borrow::Cow<'a, str> {
    if line.trim_start().starts_with('*') {
        return std::borrow::Cow::Borrowed("");
    }
    let mut in_string = false;
    for (byte, ch) in line.char_indices() {
        match ch {
            '"' => in_string = !in_string,
            ';' if !in_string => {
                return std::borrow::Cow::Owned(line[..byte].to_string());
            }
            _ => {}
        }
    }
    std::borrow::Cow::Borrowed(line)
}

/// Read the deck as logical lines: comments stripped, `+` continuations
/// appended verbatim (leading whitespace after the `+` is kept, so word
/// boundaries survive: `.tran 1u` / `+ 5m` → `.tran 1u 5m`).
fn logical_lines(source: &str) -> Result<Vec<(usize, String)>> {
    let mut out: Vec<(usize, String)> = Vec::new();
    for (idx, raw) in source.lines().enumerate() {
        let line_no = idx + 1;
        let stripped = strip_comment(raw);
        let text = stripped.trim();
        if text.is_empty() {
            continue;
        }

        if text.starts_with('+') {
            let mut chars = stripped.char_indices().peekable();
            let plus_pos = loop {
                match chars.peek() {
                    Some((_, c)) if !c.is_whitespace() => break chars.next().unwrap().0,
                    Some(_) => {
                        chars.next();
                    }
                    None => break stripped.len(),
                }
            };
            match out.last_mut() {
                Some((_, prev)) => prev.push_str(&stripped[plus_pos + 1..]),
                None => {
                    return Err(SimError::Parse(format!(
                        "line {line_no}: '+' continuation with no preceding line"
                    )));
                }
            }
            continue;
        }

        out.push((line_no, text.to_string()));
    }
    Ok(out)
}

/// The element letter of a card name. Subckt-internal names carry their
/// instance path (`X1.R1`), so the property letter comes from the segment
/// after the last dot — the name part the user actually wrote.
// ─── Card grammar submodules — each family's parse beside its emit ─────────
mod dotcards;
mod elements;
mod imports;
mod models;
mod nodeplan;
mod subckts;
mod waveforms;

#[cfg(test)]
mod tests;

/// Emit the canonical textual form of a parsed netlist — the inverse of
/// [`parse`]: model cards, element cards, dot commands, `.end`.
///
/// Numeric values print through `f64` `Display` (plain decimal notation,
/// always re-parseable by `parse_value`, unlike a value the user typed with a
/// suffix); non-GUI source waveforms (`PULSE`, `SIN`) carry their stored
/// parameters so a round trip preserves them. Model cards and the parameter
/// table are emitted sorted by name so the output is deterministic despite
/// the HashMap storage. Nodes print by their canonical name — a decimal id
/// for numeric nodes, the first-seen spelling for alphanumeric nets, so a
/// round trip keeps the net names the deck promised.
pub fn emit(netlist: &Netlist) -> String {
    let circuit = &netlist.circuit;
    let analysis = &netlist.analysis;
    let mut out = String::from("* Canonical netlist\n");

    // ── Parameter table (sorted; definitions the deck may sweep) ─────────
    let mut param_names: Vec<&String> = netlist.params.keys().collect();
    param_names.sort();
    for name in param_names {
        let value = &netlist.params[name];
        let text = match value {
            ParamValue::Num(v) => format!("{v}"),
            ParamValue::Str(s) => {
                if s.contains(|c: char| c.is_whitespace()) {
                    format!("{s:?}")
                } else {
                    s.clone()
                }
            }
        };
        out.push_str(&format!(".param {name}={text}\n"));
    }

    models::emit_models(circuit, &mut out);
    elements::emit_elements(circuit, &mut out);
    dotcards::emit_commands(analysis, &mut out);
    out.push_str(".end\n");
    out
}

// ─── Source-value bridge (frontends edit through the core grammar) ──────────

/// The value of an independent `V`/`I` source, in the deck's own vocabulary:
/// `DC <value>`, `AC <magnitude> <phase>`, or one of the waveform families
/// (`PULSE`, `SIN`, `EXP`, `PWL`, `SFFM`, `AM`). Kept public so frontends
/// route source-value editing and display through it instead of hand-splitting
/// the card text.
#[derive(Debug, Clone)]
pub enum SourceValue {
    /// `DC <value>` (a bare number parses as this too).
    Dc { value: f64 },
    /// `AC <magnitude> <phase>`.
    Ac { magnitude: f64, phase: f64 },
    /// A stored waveform family (`Sine`, `Pulse`, …).
    Waveform(Waveform),
}

/// Parse one source-value specification as written on a `V`/`I` card —
/// the same grammar the deck parser applies to source values, so a
/// frontend's edited text never drifts from what `parse` would read.
pub fn parse_source_value(text: &str, options: &ParseOptions) -> Result<SourceValue> {
    let tokens: Vec<&str> = text.split_whitespace().collect();
    if tokens.is_empty() {
        return Err(SimError::Parse("source value is empty".into()));
    }
    let spec = self::waveforms::parse_source_value_specs(&tokens, options)?;
    if let Some(w) = spec.waveform {
        return Ok(SourceValue::Waveform(w));
    }
    if let Some(ac) = spec.ac {
        return Ok(SourceValue::Ac { magnitude: ac.mag, phase: ac.phase });
    }
    if let Some(value) = spec.dc {
        return Ok(SourceValue::Dc { value });
    }
    Err(SimError::Parse("source value is empty".into()))
}

/// Emit one source value back to its canonical card text — the inverse of
/// [`parse_source_value`]. `AC` always prints its phase (eliding a zero is
/// what kept `1e-3` phases from surviving a round trip).
pub fn emit_source_value(value: &SourceValue) -> String {
    match value {
        SourceValue::Dc { value } => format!("DC {value}"),
        SourceValue::Ac { magnitude, phase } => format!("AC {magnitude} {phase}"),
        SourceValue::Waveform(w) => self::waveforms::waveform_text(w),
    }
}
