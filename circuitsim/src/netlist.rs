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
    expand_line, parse_func_body, parse_param_body, seed_from_overrides, ParamEnv, ParamValue,
};
use crate::units::parse_value;

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
        warnings,
        options: options.clone(),
        overrides: overrides.clone(),
    })
}

// ─── Library files (.include / .lib) ────────────────────────────────────────

/// Resolve a `.include`/`.lib` argument against the containing file's
/// directory, the configured search paths, and finally the process
/// directory — the PSpice `nom.lib`-respecting search order spelled out in
/// TODO §2. `None` when no candidate exists as a file.
fn find_library_file(
    spec: &str,
    base: Option<&Path>,
    options: &ParseOptions,
) -> Option<PathBuf> {
    let bare = spec.trim().trim_matches(|c| c == '"' || c == '\'');
    if bare.is_empty() {
        return None;
    }
    let raw = PathBuf::from(bare);
    if raw.is_absolute() {
        return raw.is_file().then_some(raw);
    }
    if let Some(dir) = base {
        let candidate = dir.join(&raw);
        if candidate.is_file() {
            return Some(candidate);
        }
    }
    for dir in &options.search_paths {
        let candidate = dir.join(&raw);
        if candidate.is_file() {
            return Some(candidate);
        }
    }
    // Relative to the process working directory last.
    if raw.is_file() {
        return Some(raw);
    }
    None
}

/// Read a deck or library file. Gated: the wasm build has no file system,
/// so it reports the platform limitation as the read error.
#[cfg(not(target_arch = "wasm32"))]
fn open_deck_file(path: &Path) -> std::io::Result<(String, Option<PathBuf>)> {
    Ok((
        std::fs::read_to_string(path)?,
        path.parent().map(Path::to_path_buf),
    ))
}

#[cfg(target_arch = "wasm32")]
fn open_deck_file(path: &Path) -> std::io::Result<(String, Option<PathBuf>)> {
    Err(std::io::Error::other(format!(
        "library file '{}' is not readable on the web build",
        path.display()
    )))
}

/// One library file opened and read, with the containing directory for the
/// next level of `.include`/`.lib` recursion.
struct LibLoad {
    content: String,
    base: Option<PathBuf>,
    path: PathBuf,
}

fn open_library(
    spec: &str,
    base: Option<&Path>,
    options: &ParseOptions,
) -> Result<Option<LibLoad>> {
    match find_library_file(spec, base, options) {
        Some(path) => {
            let (content, child_base) = open_deck_file(&path).map_err(|e| {
                SimError::Parse(format!("library file '{}': {e}", path.display()))
            })?;
            Ok(Some(LibLoad { content, base: child_base, path }))
        }
        None => Ok(None),
    }
}

/// Splice file-backed directives into a logical-line stream.
struct SpliceCtx<'a> {
    options: &'a ParseOptions,
    /// Canonical paths of files currently open — a chain, not a set: sibling
    /// decks may load the same library twice, but a file may not contain
    /// itself.
    visited: Vec<String>,
    base: Option<PathBuf>,
}

fn splice_libraries(
    logical: Vec<(usize, String)>,
    options: &ParseOptions,
) -> Result<Vec<(usize, String)>> {
    let mut ctx = SpliceCtx {
        options,
        visited: Vec::new(),
        base: options.deck_dir.clone(),
    };
    splice_lines(logical, &mut ctx, false)
}

/// Splice file-backed directives into a logical-line stream:
///
/// * `.include <file>` / `.inc <file>` — the file's logical lines are inserted
///   in the directive's place and processed recursively (nested
///   `.include`/`.lib` lines resolve relative to the *containing* file's
///   directory).
/// * `.lib <file> [<section>]` — same, but the PSpice section selection cuts
///   the `.lib <name>` … `.endl [<name>]` block out of the file first.
/// * A `.lib <word>` that names no file opens a section *boundary* inside a
///   library file. Boundaries never enter the deck stream: a whole-file read
///   merges every section (the LTspice behavior), a selected one is extracted
///   before the recursion.
/// * A library file's own `.end` terminates the file, not the importing deck.
/// * Resolution follows the containing directory, [`ParseOptions::search_paths`],
///   then the process directory; cycles fail loudly.
fn splice_lines(
    lines: Vec<(usize, String)>,
    ctx: &mut SpliceCtx<'_>,
    in_lib: bool,
) -> Result<Vec<(usize, String)>> {
    let mut out = Vec::with_capacity(lines.len());
    for (line_no, line) in lines {
        let lower = line.to_ascii_lowercase();
        let cmd = lower.split_whitespace().next().unwrap_or("").to_string();
        let is_directive = matches!(
            cmd.as_str(),
            ".include" | ".inc" | ".lib"
        );
        if !is_directive {
            // A library file's own `.end` terminates the file, not the
            // importing deck. `.endl` marks the end of a section in the
            // same way: it is library structure, never deck content.
            if !in_lib || (cmd != ".end" && cmd != ".endl") {
                out.push((line_no, line));
            }
            continue;
        }

        if cmd == ".lib" {
            // One important overload: a `.lib` line we cannot resolve to a
            // file is a section boundary INSIDE a library (`.lib <name>` …
            // `.endl`). It defines structure, never content.
            let filespec = rest_args(&line).first().cloned().unwrap_or_default();
            if filespec.is_empty() {
                return Err(SimError::Parse(format!(
                    "line {line_no}: .lib requires a file argument"
                )));
            }
            let section = rest_args(&line).get(1).map(|s| {
                s.trim_matches(|c| c == '"' || c == '\'').to_ascii_lowercase()
            });
            if let Some(load) = open_library(&filespec, ctx.base.as_deref(), ctx.options)? {
                splice_loaded_library(load, section.as_deref(), line_no, ctx, &mut out)?;
            } else {
                if in_lib {
                    continue; // a section boundary inside the library
                }
                let message = if section.is_some() {
                    format!(
                        "line {line_no}: .lib '{}' (section {:?}) — library file not found in search paths",
                        filespec,
                        section.unwrap_or_default()
                    )
                } else {
                    format!(
                        "line {line_no}: .lib '{}' — library file not found in search paths",
                        filespec
                    )
                };
                return Err(SimError::Parse(message));
            }
            continue;
        }

        // `.include`/`.inc`: splice the file's logical lines in place.
        let filespec = rest_args(&line).first().cloned().unwrap_or_default();
        if filespec.is_empty() {
            return Err(SimError::Parse(format!(
                "line {line_no}: .include requires a file argument"
            )));
        }
        let Some(load) = open_library(&filespec, ctx.base.as_deref(), ctx.options)? else {
            return Err(SimError::Parse(format!(
                "line {line_no}: .include '{}' — file not found in search paths",
                filespec
            )));
        };
        splice_loaded_library(load, None, line_no, ctx, &mut out)?;
    }
    Ok(out)
}

fn rest_args(line: &str) -> Vec<String> {
    line.split_whitespace()
        .skip(1)
        .map(|t| t.trim_matches(|c| c == '"' || c == '\'').to_string())
        .collect()
}

/// Read, section-select, cycle-check, recursively splice one loaded file.
fn splice_loaded_library(
    load: LibLoad,
    section: Option<&str>,
    deck_line: usize,
    ctx: &mut SpliceCtx<'_>,
    out: &mut Vec<(usize, String)>,
) -> Result<()> {
    let canonical = load
        .path
        .canonicalize()
        .map(|p| p.to_string_lossy().to_string())
        .unwrap_or_else(|_| load.path.to_string_lossy().to_string());
    if ctx.visited.iter().any(|v| v == &canonical) {
        return Err(SimError::Parse(format!(
            "circular library chain at '{}'",
            load.path.display()
        )));
    }
    ctx.visited.push(canonical);

    // Note: the loaded file goes through the same logical-line cleaning as
    // the deck, so comments and `+` continuations inside libraries behave.
    let child_lines = logical_lines(&load.content)?;

    let child_lines = match section {
        Some(sec) => extract_section(&child_lines, sec, &load.path, deck_line)?,
        None => child_lines,
    };

    let parent_base = std::mem::replace(&mut ctx.base, load.base.clone());
    let spliced = splice_lines(child_lines, ctx, true)?;
    ctx.base = parent_base;
    ctx.visited.pop();

    out.extend(spliced);
    Ok(())
}

/// The lines of the `.lib <name>` … `.endl [<name>]` section. A nested
/// `.lib` boundary while the target section is open is a structural error.
fn extract_section(
    lines: &[(usize, String)],
    name: &str,
    file: &Path,
    deck_line: usize,
) -> Result<Vec<(usize, String)>> {
    let mut body: Vec<(usize, String)> = Vec::new();
    let mut open = false;
    for (line_no, line) in lines {
        let first = line
            .split_whitespace()
            .next()
            .unwrap_or("")
            .to_ascii_lowercase();
        if !open {
            if first == ".lib" {
                let boundary = rest_args(line).first().map(|s| {
                    s.trim_matches(|c| c == '"' || c == '\'').to_ascii_lowercase()
                });
                if boundary.as_deref() == Some(name) {
                    open = true;
                }
            }
            continue;
        }
        if first == ".endl" {
            return Ok(body);
        }
        if first == ".lib" {
            return Err(SimError::Parse(format!(
                ".lib '{}' from line {deck_line}: nested .lib inside section '{name}' of {} — .lib sections do not nest",
                file.display(),
                name
            )));
        }
        body.push((*line_no, line.clone()));
    }
    Err(SimError::Parse(format!(
        ".lib file {}: section '{name}' has no .endl",
        file.display()
    )))
}

// ─── Global nets ────────────────────────────────────────────────────────────

/// Every net named by a `.global` card, anywhere in the deck — definitions
/// inside `.subckt` bodies count, since a body must be able to say "this net
/// is the top-level one". Parameter references (`{name}`) are not collected:
/// those names resolve later, in the scopes that use them.
fn collect_global_nets(logical: &[(usize, String)]) -> HashSet<String> {
    let mut set = HashSet::new();
    for (_, line) in logical {
        let lower = line.to_ascii_lowercase();
        let mut words = lower.split_whitespace();
        if words.next() != Some(".global") {
            continue;
        }
        for name in words {
            if name.contains('{') || name.contains('}') {
                continue;
            }
            set.insert(name.to_string());
        }
    }
    set
}

// ─── Subcircuit definitions ─────────────────────────────────────────────────

/// One `.subckt` definition lifted out of the deck before instantiation.
/// Ports and default parameters are lowercased name text; default parameter
/// *values* stay raw so each instance evaluates them in its own scope.
#[derive(Debug, Clone)]
struct SubcktDef {
    name: String,
    /// The definition name as first written (display only).
    display_name: String,
    ports: Vec<String>,
    default_params: Vec<(String, String)>,
    body: Vec<(usize, String)>,
    /// Source line of the `.subckt` card, for diagnostics.
    line_no: usize,
}

/// `.subckt <name> <port…> [PARAMS:] <key=value…>` — ports are the tokens
/// before the first `key=value` pair (LTspice puts default parameter
/// assignments directly on the header, PSpice introduces them with the
/// `PARAMS:` marker; both forms land here).
fn parse_subckt_header(
    line_no: usize,
    line: &str,
) -> std::result::Result<(String, String, Vec<String>, Vec<(String, String)>), String> {
    let tokens: Vec<&str> = line.split_whitespace().collect();
    if tokens.len() < 2 {
        return Err(format!("line {line_no}: .subckt requires a definition name"));
    }
    let display = tokens[1].to_string();
    let name = display.to_ascii_lowercase();

    let mut ports = Vec::new();
    let mut default_params: Vec<(String, String)> = Vec::new();
    let mut after_marker = false;

    for tok in &tokens[2..] {
        let lower = tok.to_ascii_lowercase();

        // Bare `PARAMS`/`PARAMS:` tokens introduce the pair region.
        if lower == "params" || lower == "params:" {
            after_marker = true;
            continue;
        }
        // Fused `PARAMS:key=value` (no whitespace): strip the 7-character
        // marker from the original token — `params:` and its lowercase form
        // have identical ASCII length, so the byte offsets agree.
        if lower.starts_with("params:") {
            let fused = &tok[7..];
            match kv_pair_text(fused) {
                Some((key, value)) => default_params.push((key, value)),
                None => {
                    return Err(format!(
                        "line {line_no}: .subckt '{name}': 'PARAMS:{fused}' must be a key=value pair"
                    ))
                }
            }
            after_marker = true;
            continue;
        }
        match kv_pair_text(tok) {
            Some((key, value)) => default_params.push((key, value)),
            None if after_marker => {
                return Err(format!(
                    "line {line_no}: .subckt '{name}': '{tok}' after PARAMS: must be a key=value pair"
                ))
            }
            None => ports.push(lower),
        }
    }
    Ok((name, display, ports, default_params))
}

/// Split `key=value` into (key lowercased, value text as written) — `None`
/// when the '=' is missing or the key side is empty.
fn kv_pair_text(token: &str) -> Option<(String, String)> {
    let eq = token.find('=')?;
    let key = token[..eq].to_ascii_lowercase();
    (!key.is_empty()).then_some((key, token[eq + 1..].to_string()))
}

/// Lift `.subckt`/`.ends` definitions out of the deck. Returns the flow of
/// cards outside any definition plus every definition (nested definitions
/// inside bodies join the same global namespace). Everything after the first
/// top-level `.end` is ignored, as SPICE reads, and a `.subckt` without its
/// matching `.ends` (or a stray `.ends`) is a structural error.
fn collect_subckts(
    logical: &[(usize, String)],
) -> std::result::Result<(Vec<(usize, String)>, Vec<SubcktDef>), String> {
    let mut flow: Vec<(usize, String)> = Vec::new();
    let mut defs: Vec<SubcktDef> = Vec::new();
    let mut open: Vec<SubcktDef> = Vec::new();

    for (line_no, line) in logical {
        let first = line
            .split_whitespace()
            .next()
            .unwrap_or("")
            .to_ascii_lowercase();

        if open.is_empty() && first == ".end" {
            // Content after `.end` is not part of the deck.
            break;
        }

        match first.as_str() {
            ".subckt" => {
                let (name, display, ports, default_params) = parse_subckt_header(*line_no, line)?;
                open.push(SubcktDef {
                    name,
                    display_name: display,
                    ports,
                    default_params,
                    body: Vec::new(),
                    line_no: *line_no,
                });
            }
            ".ends" => {
                let def = open
                    .pop()
                    .ok_or_else(|| format!("line {line_no}: .ends outside any .subckt"))?;
                defs.push(def);
            }
            _ => match open.last_mut() {
                Some(def) => def.body.push((*line_no, line.to_string())),
                None => flow.push((*line_no, line.to_string())),
            },
        }
    }

    if let Some(def) = open.last() {
        return Err(format!(
            "line {}: .subckt '{}' is missing its .ends",
            def.line_no, def.display_name
        ));
    }
    Ok((flow, defs))
}

// ─── Scope flattening: `X` instantiation and net mapping ────────────────────

/// Shared state while flattening: definition table, global nets, the `.dc`
/// sweep overrides, and diagnostics collected along the way.
struct ScopeCtx<'a> {
    defs: &'a HashMap<String, SubcktDef>,
    globals: &'a HashSet<String>,
    overrides: &'a HashMap<String, f64>,
    warnings: Vec<String>,
}

/// Node-field positions per element letter — one table for the plan pre-scan
/// and the scope mapper, so they can never drift apart. `K` has no nodes at
/// all (an empty slice stops the generic `[1, 2]` fallback from treating its
/// inductance names as nets).
fn node_slot_indices(kind: char) -> Option<&'static [usize]> {
    match kind.to_ascii_uppercase() {
        'M' => Some(&[1, 2, 3, 4]),
        'J' => Some(&[1, 2, 3]),
        'E' | 'G' | 'S' | 'T' => Some(&[1, 2, 3, 4]),
        'R' | 'C' | 'L' | 'V' | 'I' | 'D' | 'F' | 'H' | 'W' | 'B' => Some(&[1, 2]),
        'K' => Some(&[]),
        _ => None,
    }
}

/// Node fields for one card, including the `POLY(n)` control pairs of an
/// `E`/`G` source, whose positions depend on the card's own text.
fn node_slots_for_tokens(tokens: &[&str]) -> Vec<usize> {
    let mut slots = node_slot_indices(element_letter(tokens[0]))
        .unwrap_or(&[1, 2])
        .to_vec();
    let kind = element_letter(tokens[0]).to_ascii_uppercase();
    if kind == 'Q' {
        let mut slots = vec![1, 2, 3];
        if q_is_four_terminal(tokens) {
            slots.push(4);
        }
        return slots;
    }
    if matches!(kind, 'E' | 'G') && tokens.get(3).map(|t| t.to_ascii_lowercase()).is_some_and(|t| t.starts_with("poly")) {
        if let Some(n) = poly_count(tokens[3]) {
            for k in 0..n {
                slots.push(4 + 2 * k);
                slots.push(5 + 2 * k);
            }
        }
    }
    slots
}

/// Parse the `n` in `POLY(n)` (the caller already checked the prefix).
fn poly_count(token: &str) -> Option<usize> {
    let open = token.find('(')?;
    let close = token[open..].find(')').map(|c| open + c)?;
    token[open + 1..close].trim().parse().ok()
}

/// Does this `Q` card carry the optional substrate node?
///
/// Three-terminal: `Qname c b e model …`; four-terminal:
/// `Qname c b e substrate model …`. The model token is the first trailing
/// word that is not a number, an `AREA=`/`OFF` field, or `IC=`.
fn q_is_four_terminal(tokens: &[&str]) -> bool {
    tokens.len() >= 6 && tokens.get(5).is_some_and(|t| {
        let lower = t.to_ascii_lowercase();
        lower != "off"
            && !lower.contains('=')
            && parse_value(t).is_err()
    })
}

/// Token positions that name *other elements* rather than nodes or values.
/// Inside a subckt these names carry the instance path, like the card's own
/// name (`X1.F1` must control `X1.V1`).
fn reference_slots_for_tokens(tokens: &[&str]) -> Vec<usize> {
    let kind = element_letter(tokens[0]).to_ascii_uppercase();
    let is_poly = tokens
        .get(3)
        .is_some_and(|t| t.to_ascii_lowercase().starts_with("poly"));
    match kind {
        'F' | 'H' => {
            if is_poly {
                let n = poly_count(tokens[3]).unwrap_or(0);
                (4..4 + n).collect()
            } else {
                vec![3]
            }
        }
        'W' => vec![3],
        'K' => vec![1, 2],
        _ => Vec::new(),
    }
}

/// Rewrite one node token to its physical net name in the current scope:
/// ports bind to the instance's connection tokens, `.global` nets carry over
/// everywhere, ground is world-wide ground, and anything else is the instance
/// path plus the body's spelling (`X1.XR2.mid`).
fn scope_token(
    token: &str,
    bindings: &HashMap<String, String>,
    globals: &HashSet<String>,
    prefix: &str,
) -> String {
    let lower = token.to_ascii_lowercase();
    if lower == "0" || lower == "gnd" {
        return token.to_string();
    }
    if let Some(bound) = bindings.get(&lower) {
        return bound.clone();
    }
    if globals.contains(&lower) {
        return token.to_string();
    }
    if prefix.is_empty() {
        return token.to_string();
    }
    format!("{prefix}.{token}")
}

/// Rewrite the `V(…)`/`I(…)` probe arguments of one `B` expression into the
/// current scope's naming.
///
/// A probe boundary check guards against rewriting function arguments (`int(`
/// / `hypot(` do not become probes): a probe grabs only when the character
/// before the letter is not a name character. `V(…)` arguments are nets —
/// each top-level comma argument scopes like a node token; `I(…)` names an
/// element and follows the element-reference rule (a path prefix rides along,
/// a bare name gains the instance's own). Unbalanced text passes through for
/// the expression parser to reject loudly.
fn rewrite_b_probes(
    expr: &str,
    bindings: &HashMap<String, String>,
    globals: &HashSet<String>,
    prefix: &str,
) -> String {
    let chars: Vec<char> = expr.chars().collect();
    let mut out = String::with_capacity(expr.len());
    let mut i = 0usize;
    while i < chars.len() {
        let c = chars[i];
        let boundary = i == 0
            || !(chars[i - 1].is_ascii_alphanumeric() || chars[i - 1] == '_');
        if (c == 'v' || c == 'V' || c == 'i' || c == 'I')
            && chars.get(i + 1) == Some(&'(')
            && boundary
        {
            let mut depth = 1usize;
            let mut j = i + 2;
            while j < chars.len() && depth > 0 {
                match chars[j] {
                    '(' => depth += 1,
                    ')' => depth -= 1,
                    _ => {}
                }
                j += 1;
            }
            j -= 1; // position of the balanced ')' (or last char when unbalanced)
            if depth == 0 {
                let inner: String = chars[i + 2..j].iter().collect();
                let is_voltage = c.to_ascii_lowercase() == 'v';
                let rewritten = if is_voltage {
                    inner
                        .split(',')
                        .map(|arg| scope_token(arg.trim(), bindings, globals, prefix))
                        .collect::<Vec<String>>()
                        .join(",")
                } else {
                    let arg = inner.trim();
                    if !prefix.is_empty() && !arg.contains('.') {
                        format!("{prefix}.{arg}")
                    } else {
                        arg.to_string()
                    }
                };
                out.push(c);
                out.push('(');
                out.push_str(&rewritten);
                out.push(')');
                i = j + 1;
                continue;
            }
        }
        out.push(c);
        i += 1;
    }
    out
}

/// Expand one scope's lines into physical element/dot cards.
///
/// `.param`/`.func` execute against the scope's own environment (deck order
/// preserved; instance scopes start from their instantiation snapshot and
/// never leak back out). `.global` was registered before flattening and is
/// consumed. `X` cards instantiate their definition in place. Every other
/// card is `{}`-expanded — into the instance scope — with known element
/// kinds having their node tokens rewritten by [`scope_token`].
fn flatten_scope(
    lines: &[(usize, String)],
    prefix: &str,
    label: &str,
    bindings: &HashMap<String, String>,
    env: &mut ParamEnv,
    scope: &mut ScopeCtx<'_>,
    skip_keys: Option<&HashSet<String>>,
) -> std::result::Result<Vec<(usize, String)>, String> {
    let mut out = Vec::with_capacity(lines.len());

    for (line_no, line) in lines {
        let lower = line.to_ascii_lowercase();
        match lower.split_whitespace().next().unwrap_or("") {
            ".param" => {
                // Expansion runs first: a multi-token brace/quote value
                // (`x = '1k * gain'`) must resolve before the assignment
                // token split sees the line (the contract on
                // `parse_param_body`).
                let expanded =
                    expand_line(line, env).map_err(|e| format!("line {line_no}: {e}"))?;
                // Instance-scoped assignments may replay here: a body card
                // re-assigning a key the `X` instance line provided is
                // rolled back, because the instance line wins (the same
                // precedence the reference tools give `PARAMS=` values).
                let snapshot = env.params.clone();
                parse_param_body(&expanded[6..], env, scope.overrides)
                    .map_err(|e| format!("line {line_no}: {e}"))?;
                if let Some(skip) = skip_keys {
                    for key in env.params.keys().cloned().collect::<Vec<String>>() {
                        if skip.contains(&key) {
                            match snapshot.get(&key) {
                                Some(value) => { env.params.insert(key, value.clone()); }
                                None => { env.params.remove(&key); }
                            }
                        }
                    }
                }
                continue;
            }
            ".func" => {
                parse_func_body(&line[5..], env)
                    .map_err(|e| format!("line {line_no}: .func: {e}"))?;
                continue;
            }
            ".global" => continue, // registered by collect_global_nets
            ".ic" | ".nodeset" => {
                // Initial conditions carry node names, so inside a body the
                // entries must be translated to the body's net space
                // (`V(mid)=…` → `V(X1.mid)=…`).
                let expanded = expand_line(line, env).map_err(|e| format!("line {line_no}: {e}"))?;
                let expanded = expanded.trim();
                if !expanded.is_empty() {
                    out.push((*line_no, mangle_ic_entries(expanded, bindings, scope.globals, prefix)));
                }
                continue;
            }
            ".end" => {
                if prefix.is_empty() {
                    // The deck terminator travels to the card walk.
                    out.push((*line_no, line.to_string()));
                    continue;
                }
                return Err(format!(
                    "line {line_no}: '.end' inside subckt '{label}' — end definitions with .ends"
                ));
            }
            ".ends" => {
                return Err(format!(
                    "line {line_no}: '.ends' reached while flattening '{label}' — unbalanced .subckt body"
                ))
            }
            _ => {}
        }

        let expanded = expand_line(line, env).map_err(|e| format!("line {line_no}: {e}"))?;
        let expanded = expanded.trim();
        if expanded.is_empty() {
            continue;
        }

        if expanded.starts_with('.') {
            out.push((*line_no, expanded.to_string()));
            continue;
        }

        let tokens: Vec<&str> = expanded.split_whitespace().collect();
        let kind = tokens[0]
            .chars()
            .next()
            .ok_or_else(|| format!("line {line_no}: empty element"))?;

        if kind.eq_ignore_ascii_case(&'x') {
            let child = instantiate_subckt(&tokens, *line_no, prefix, label, bindings, env, scope)?;
            out.extend(child);
            continue;
        }

        // `B` cards carry an expression whose `V(…)`/`I(…)` probes name nets
        // and elements of the same instance; those are rewritten here instead
        // of the generic slot pass (which knows nothing about subexpressions).
        if kind.eq_ignore_ascii_case(&'b') {
            if let Ok(split) = split_b_card(&tokens) {
                let mut head: Vec<String> = Vec::with_capacity(4);
                head.push(if prefix.is_empty() {
                    tokens[0].to_string()
                } else {
                    format!("{prefix}.{}", tokens[0])
                });
                head.push(scope_token(tokens[1], bindings, scope.globals, prefix));
                head.push(scope_token(tokens[2], bindings, scope.globals, prefix));
                let letter = match split.kind {
                    BehavioralKind::Voltage => 'V',
                    BehavioralKind::Current => 'I',
                };
                let expr = rewrite_b_probes(&split.expr, bindings, scope.globals, prefix);
                head.push(format!("{letter}={expr}"));
                out.push((*line_no, head.join(" ")));
                continue;
            }
            // Malformed split: fall through with the generic rewrite so the
            // card walk raises the loud parse error for it.
        }

        if node_slot_indices(kind).is_some() {
            // Rewrite this card's node tokens in the scope's net space; the
            // card text keeps its non-node fields (values, model names,
            // waveform parameters).
            let mut rewritten: Vec<String> = tokens.iter().map(|t| (*t).to_string()).collect();
            if !prefix.is_empty() {
                // Subckt-internal element names carry their instance path
                // (`X1.R1`), so branch reports never confuse two instances'
                // cards that share a name.
                rewritten[0] = format!("{prefix}.{}", rewritten[0]);
            }
            for slot in node_slots_for_tokens(&tokens) {
                if let Some(tok) = rewritten.get_mut(slot) {
                    *tok = scope_token(tok, bindings, scope.globals, prefix);
                }
            }
            // Element references (controlling voltage sources, coupled
            // inductors) live in the same instance namespace as element
            // names, so they carry the instance path too.
            if !prefix.is_empty() {
                for slot in reference_slots_for_tokens(&tokens) {
                    if let Some(tok) = rewritten.get_mut(slot) {
                        if !tok.contains('.') {
                            *tok = format!("{prefix}.{tok}");
                        }
                    }
                }
            }
            out.push((*line_no, rewritten.join(" ")));
        } else {
            out.push((*line_no, expanded.to_string()));
        }
    }
    Ok(out)
}

/// Instantiate one `X` card: locate the definition, bind its ports, build the
/// instance scope's parameter environment (definition defaults, then the
/// instance's own assignments), and flatten the body at this deck position.
///
/// The definition name is the first token that both names a definition and
/// leaves only `key=value` tokens after the node list — the same match the
/// reference tools make once they know a definition's port count.
fn instantiate_subckt(
    tokens: &[&str],
    line_no: usize,
    parent_prefix: &str,
    parent_label: &str,
    parent_bindings: &HashMap<String, String>,
    parent_env: &ParamEnv,
    scope: &mut ScopeCtx<'_>,
) -> std::result::Result<Vec<(usize, String)>, String> {
    let instance = tokens[0].to_string();

    // Locate the definition-tag token.
    let mut tag_idx: Option<usize> = None;
    let mut name_hit: Option<String> = None;
    for j in 1..tokens.len() {
        let cand = tokens[j].to_ascii_lowercase();
        if scope.defs.contains_key(&cand) {
            // Remember the leftmost name match for the error text, but only
            // commit to a tag when every trailing token is key=value
            // (i.e. that token sits after the node list).
            if name_hit.is_none() {
                name_hit = Some(cand);
            }
            if tokens[j + 1..].iter().all(|t| t.contains('=')) {
                tag_idx = Some(j);
                break;
            }
        }
    }
    let Some(tag) = tag_idx else {
        let guess = name_hit.unwrap_or_else(|| {
            tokens
                .iter()
                .skip(1)
                .find(|t| !t.contains('='))
                .map(|t| t.to_ascii_lowercase())
                .unwrap_or_else(|| "?".into())
        });
        let _ = parent_label;
        return Err(format!(
            "line {line_no}: instance '{instance}' references an unknown .subckt name ('{guess}')"
        ));
    };
    let def = &scope.defs[&tokens[tag].to_ascii_lowercase()];

    // Port tokens belong to the caller scope — they were subjects of its
    // bindings, may itself be `.global` names, and keep their spelling.
    let ports: Vec<String> = tokens[1..tag]
        .iter()
        .map(|t| scope_token(t, parent_bindings, scope.globals, parent_prefix))
        .collect();

    if ports.len() != def.ports.len() {
        return Err(format!(
            "line {line_no}: instance '{instance}' of '{}' needs {} port(s), got {}",
            def.display_name,
            def.ports.len(),
            ports.len()
        ));
    }

    let bindings_now: HashMap<String, String> = def
        .ports
        .iter()
        .zip(&ports)
        .map(|(p, t)| (p.clone(), t.clone()))
        .collect();

    // Instance scope: caller env snapshot, definition defaults first, then
    // the instance's own assignments — which may reference the defaults.
    let mut env = parent_env.clone();
    for (key, raw) in &def.default_params {
        let value = eval_param_value(raw, &env).map_err(|e| {
            format!(
                "line {line_no}: instance '{instance}': default '{key}={raw}': {e}"
            )
        })?;
        env.params.insert(key.clone(), value);
    }
    // The X line's own assignments, remembered so a body `.param` sandbox
    // replaying for the same name cannot clobber them.
    let mut instance_keys: HashSet<String> = HashSet::new();
    for token in &tokens[(tag + 1)..] {
        let Some((key, value_text)) = kv_pair_text(token) else {
            return Err(format!(
                "line {line_no}: instance '{instance}': parameter '{token}' must be key=value"
            ));
        };
        let value = eval_param_value(&value_text, &env).map_err(|e| {
            format!(
                "line {line_no}: instance '{instance}': parameter '{key}={value_text}': {e}"
            )
        })?;
        env.params.insert(key.clone(), value);
        instance_keys.insert(key);
    }

    // The instance's node prefix for internal nets. Nested instances extend
    // the path, so two instances of one definition cannot share a net.
    let ins_prefix = if parent_prefix.is_empty() {
        instance
    } else {
        format!("{parent_prefix}.{instance}")
    };
    let ins_label = def.display_name.clone();
    let def_body = def.body.clone();
    flatten_scope(
        &def_body,
        &ins_prefix,
        &ins_label,
        &bindings_now,
        &mut env,
        scope,
        Some(&instance_keys),
    )
}

/// Evaluate one parameter-value text (`1k`, `{a*b}`, an identifier) in `env`.
fn eval_param_value(
    text: &str,
    env: &ParamEnv,
) -> std::result::Result<ParamValue, String> {
    env.eval(text.trim())
}

/// The `.ic`/`.nodeset` rewrite for one scope: each entry's node text goes
/// through [`scope_token`] so entries written inside a body address the
/// body's nets; the letter (`V`/`I`) and the value text are copied verbatim.
fn mangle_ic_entries(
    line: &str,
    bindings: &HashMap<String, String>,
    globals: &HashSet<String>,
    prefix: &str,
) -> String {
    line.split_whitespace()
        .enumerate()
        .map(|(idx, tok)| {
            if idx == 0 || !tok.contains('(') {
                return tok.to_string();
            }
            match (tok.find('('), tok.find(')')) {
                (Some(o), Some(c)) if o > 0 && c > o + 1 => {
                    let inner = &tok[o + 1..c];
                    format!(
                        "{}{}{}",
                        &tok[..=o],
                        scope_token(inner, bindings, globals, prefix),
                        &tok[c..]
                    )
                }
                _ => tok.to_string(),
            }
        })
        .collect::<Vec<String>>()
        .join(" ")
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
fn element_letter(card_name: &str) -> char {
    card_name
        .rsplit('.')
        .next()
        .unwrap_or(card_name)
        .chars()
        .next()
        .unwrap_or('?')
}

/// Reserve node ids up front: numeric nodes own their number, alphanumeric
/// nodes take ids above every numeric reference in first-appearance order,
/// so the two name spaces can never collide however the deck interleaves.
fn scan_node_plan(logical: &[(usize, String)]) -> HashMap<String, usize> {
    let mut max_numeric = 0usize;
    let mut named_order: Vec<String> = Vec::new();

    for (_, line) in logical {
        if line.to_ascii_lowercase() == ".end" {
            break;
        }
        if line.starts_with('.') {
            continue;
        }
        let tokens: Vec<&str> = line.split_whitespace().collect();
        if tokens.is_empty() {
            continue;
        }
        let slots = node_slots_for_tokens(&tokens);
        // Unknown kinds still look node-ish in slots 1–2; whatever the
        // real grammar rejects surfaces again at parse time.
        for slot in slots {
            let Some(tok) = tokens.get(slot) else { continue };
            let lower = tok.to_ascii_lowercase();
            if lower == "gnd" {
                continue;
            }
            if let Ok(n) = lower.parse::<usize>() {
                max_numeric = max_numeric.max(n);
            } else if !named_order.contains(&lower) {
                named_order.push(lower);
            }
        }
    }

    let mut plan = HashMap::new();
    let mut next = max_numeric + 1;
    for name in named_order {
        plan.insert(name, next);
        next += 1;
    }
    plan
}

// ─── Node resolution ─────────────────────────────────────────────────────────

/// Resolve one node token: ground aliases (`0`, `gnd`), numeric ids, or an
/// alphanumeric net name from the pre-scan plan (case-insensitive; the first
/// spelling seen becomes the display name).
fn parse_node(
    token: &str,
    circuit: &mut Circuit,
    plan: &HashMap<String, usize>,
) -> Result<NodeId> {
    let lower = token.to_ascii_lowercase();
    if lower == "0" || lower == "gnd" {
        return Ok(NodeId(0));
    }

    if let Ok(id) = lower.parse::<usize>() {
        circuit.ensure_node(id);
        let existing = circuit.node_name(id).to_string();
        if existing != id.to_string() {
            return Err(SimError::Parse(format!(
                "node '{token}' collides with named node '{existing}'"
            )));
        }
        return Ok(NodeId(id));
    }

    let id = match plan.get(&lower) {
        Some(&id) => id,
        // The pre-scan did not see this token position (unsupported element
        // kind); allocating above everything seen is the safe fallback.
        None => circuit.nodes,
    };
    circuit.ensure_node(id);
    circuit.set_node_name(id, token);
    Ok(NodeId(id))
}

// ─── Dot commands ────────────────────────────────────────────────────────────

/// One dot-command card from the flattened deck.
///
/// Tolerance split: malformed arguments of a *known* directive are loud
/// errors (`.dc` sweeps, `.tran` timings), while directives RSpice does not
/// implement at all are recorded in `warnings` and skipped — third-party
/// decks import as much as the engine can simulate (TODO §2).
fn parse_dot_command(
    line: &str,
    analysis: &mut AnalysisCommands,
    circuit: &mut Circuit,
    warnings: &mut Vec<String>,
    line_no: usize,
) -> Result<()> {
    let tokens: Vec<&str> = line.split_whitespace().collect();
    let cmd = tokens[0].to_ascii_lowercase();
    match cmd.as_str() {
        ".model" => parse_model_card(line, &tokens, circuit, warnings, line_no)?,
        ".op" => {
            analysis.dc_op = true;
            if tokens.len() > 1 {
                warnings.push(ignored_tokens(line_no, ".op", &tokens[1..]));
            }
        }
        ".tran" => {
            if tokens.len() < 3 {
                return Err(SimError::Parse(
                    ".tran requires: .tran <tstep> <tstop> [tstart] [UIC]".into(),
                ));
            }
            analysis.tran_step = Some(parse_value(tokens[1]).map_err(SimError::Parse)?);
            analysis.tran_stop = Some(parse_value(tokens[2]).map_err(SimError::Parse)?);
            let mut t_start: Option<f64> = None;
            let mut t_max: Option<f64> = None;
            for tok in &tokens[3..] {
                if tok.eq_ignore_ascii_case("uic") {
                    analysis.tran_uic = true;
                    continue;
                }
                if tok.eq_ignore_ascii_case("startup") {
                    analysis.tran_startup = true;
                    continue;
                }
                match parse_value(tok) {
                    Ok(v) if t_start.is_none() => t_start = Some(v),
                    Ok(v) if t_max.is_none() => t_max = Some(v),
                    Ok(_) => warnings.push(ignored_tokens_owned(line_no, ".tran", tok)),
                    Err(_) => warnings.push(format!(
                        "line {line_no}: .tran ignores unsupported token '{tok}'"
                    )),
                }
            }
            analysis.tran_start = t_start.unwrap_or(0.0);
            analysis.tran_maxstep = t_max;
        }
        ".ac" => {
            if tokens.len() < 5 {
                return Err(SimError::Parse(
                    ".ac requires: .ac <DEC|OCT|LIN> <points> <fstart> <fstop> [src_idx [amplitude]]"
                        .into(),
                ));
            }
            let scale = match tokens[1].to_ascii_lowercase().as_str() {
                "dec" => AcScale::Dec,
                "oct" => AcScale::Oct,
                "lin" => AcScale::Lin,
                other => {
                    return Err(SimError::Parse(format!(
                        ".ac sweep type must be DEC, OCT, or LIN — got '{}'", other
                    )));
                }
            };
            let points: usize = tokens[2]
                .parse()
                .map_err(|_| SimError::Parse(format!("invalid .ac point count '{}'", tokens[2])))?;
            let f_start = parse_value(tokens[3]).map_err(SimError::Parse)?;
            let f_stop  = parse_value(tokens[4]).map_err(SimError::Parse)?;
            let stimulus_source = tokens
                .get(5)
                .map(|s| s.parse::<usize>())
                .transpose()
                .map_err(|_| SimError::Parse("invalid .ac source index".into()))?;
            analysis.ac = Some(AcCommand {
                scale, points, f_start, f_stop, stimulus_source,
            });
            if tokens.len() > 6 {
                warnings.push(ignored_tokens(line_no, ".ac", &tokens[6..]));
            }
        }
        ".dc" => parse_dc(&tokens, analysis, line_no)?,
        ".tf" => {
            if tokens.len() < 3 {
                return Err(SimError::Parse(format!(
                    "line {line_no}: .tf requires: .tf <V(out)|I(Vsrc)> <input source>"
                )));
            }
            if tokens.len() > 3 {
                warnings.push(ignored_tokens(line_no, ".tf", &tokens[3..]));
            }
            analysis.tf = Some(TfCommand {
                output: parse_tf_output(tokens[1], line_no)?,
                input: tokens[2].to_string(),
            });
        }
        ".options" | ".option" => {
            parse_options(&tokens, analysis, warnings, line_no)?;
        }
        ".temp" => {
            if tokens.len() < 2 {
                return Err(SimError::Parse(format!(
                    "line {line_no}: .temp requires at least one temperature in °C"
                )));
            }
            for tok in &tokens[1..] {
                let t = parse_value(tok).map_err(SimError::Parse)?;
                if !(-273.15..=1000.0).contains(&t) {
                    return Err(SimError::Parse(format!(
                        "line {line_no}: .temp {t} °C is outside the physical range"
                    )));
                }
                analysis.temperatures.push(t);
            }
            if let Some(&first) = analysis.temperatures.first() {
                circuit.temperature = first;
            }
        }
        ".step" => parse_step(&tokens, analysis, warnings, line_no)?,
        ".meas" | ".measure" => {
            analysis
                .measurements
                .push(parse_measurement(&tokens, line_no)?);
        }
        ".noise" => {
            if tokens.len() < 7 {
                return Err(SimError::Parse(format!(
                    "line {line_no}: .noise requires: .noise V(out) <input src> DEC|OCT|LIN <points> <fstart> <fstop>"
                )));
            }
            let output = parse_tf_output(tokens[1], line_no)?;
            if matches!(output, TfOutput::Current(_)) {
                return Err(SimError::Parse(format!(
                    "line {line_no}: .noise output must be a node voltage V(out) or V(a,b)"
                )));
            }
            let scale = match tokens[3].to_ascii_lowercase().as_str() {
                "dec" => AcScale::Dec,
                "oct" => AcScale::Oct,
                "lin" => AcScale::Lin,
                other => {
                    return Err(SimError::Parse(format!(
                        "line {line_no}: .noise sweep type '{other}' must be DEC, OCT, or LIN"
                    )))
                }
            };
            let points: usize = tokens[4].parse().map_err(|_| {
                SimError::Parse(format!("line {line_no}: invalid .noise point count '{}'", tokens[4]))
            })?;
            let f_start = parse_value(tokens[5]).map_err(SimError::Parse)?;
            let f_stop = parse_value(tokens[6]).map_err(SimError::Parse)?;
            if tokens.len() > 7 {
                warnings.push(ignored_tokens(line_no, ".noise", &tokens[7..]));
            }
            analysis.noise = Some(NoiseCommand {
                output,
                input: tokens[2].to_string(),
                scale,
                points,
                f_start,
                f_stop,
            });
        }
        ".four" => {
            if tokens.len() < 3 {
                return Err(SimError::Parse(format!(
                    "line {line_no}: .four requires: .four <frequency> <output…>"
                )));
            }
            let frequency = parse_value(tokens[1]).map_err(SimError::Parse)?;
            if frequency <= 0.0 {
                return Err(SimError::Parse(format!(
                    "line {line_no}: .four frequency must be > 0"
                )));
            }
            analysis.four = Some(FourCommand {
                frequency,
                outputs: tokens[2..].iter().map(|s| (*s).to_string()).collect(),
            });
        }
        ".ic" => {
            let entries = parse_ic_entries(&tokens, ".ic", line_no, warnings)?;
            analysis.ic.extend(entries);
        }
        ".nodeset" => {
            let entries = parse_ic_entries(&tokens, ".nodeset", line_no, warnings)?;
            analysis.nodeset.extend(entries);
        }
        other => {
            // Echo the arguments so the user can find the card in their deck.
            let shown = if tokens.len() > 1 {
                format!("{other} {}", tokens[1..].join(" "))
            } else {
                other.to_string()
            };
            warnings.push(format!(
                "line {line_no}: directive '{shown}' is not recognized — skipped with a warning, never fatal"
            ));
        }
    }
    Ok(())
}

/// The standard shape of a "these cards were carried, not applied" warning.
fn ignored_tokens(line_no: usize, directive: &str, tokens: &[&str]) -> String {
    format!("line {line_no}: {directive} ignores unsupported tokens: {}", tokens.join(" "))
}

/// The same warning for owned tokens (`.tran` numeric extras).
fn ignored_tokens_owned(line_no: usize, directive: &str, rest: &str) -> String {
    format!("line {line_no}: {directive} ignores the extra token '{rest}'")
}

/// Parse the output operand of `.tf`: `V(node)`, `V(a,b)`, or `I(Vname)`.
fn parse_tf_output(token: &str, line_no: usize) -> Result<TfOutput> {
    let open = token.find('(').ok_or_else(|| {
        SimError::Parse(format!(
            "line {line_no}: .tf output '{token}' must be V(node) or I(Vsrc)"
        ))
    })?;
    let close = token[open..]
        .find(')')
        .map(|c| open + c)
        .ok_or_else(|| {
            SimError::Parse(format!(
                "line {line_no}: .tf output '{token}' is missing ')'"
            ))
        })?;
    let letter = token[..open].trim().to_ascii_uppercase();
    let inner = token[open + 1..close].trim();
    if inner.is_empty() {
        return Err(SimError::Parse(format!(
            "line {line_no}: .tf output '{token}' names nothing"
        )));
    }
    match letter.as_str() {
        "V" => {
            let mut parts = inner.split(',').map(str::trim);
            let node = parts.next().unwrap_or("").to_string();
            let reference = parts
                .next()
                .filter(|s| !s.is_empty())
                .map(str::to_string);
            if node.is_empty() {
                return Err(SimError::Parse(format!(
                    "line {line_no}: .tf output '{token}' names no node"
                )));
            }
            Ok(TfOutput::Voltage { node, reference })
        }
        "I" => Ok(TfOutput::Current(inner.to_string())),
        _ => Err(SimError::Parse(format!(
            "line {line_no}: .tf output '{token}' must be V(…) or I(…)"
        ))),
    }
}

/// Parse the `V(node)=value` entry list behind an `.ic`/`.nodeset` card.
///
/// The reference spelling puts one entry per token (`V(a)=1 V(b)=2`); the
/// assignment and its value may also be split across tokens (`V(a) = 1`,
/// `V(a)= 1`, `V(a) =1`), which several tools emit. Current entries
/// (`I(…)`) are recognized but not modeled yet, so they warn and are
/// skipped (inductor seeding is not supported at all); anything that is
/// not a V/I entry warns too.
fn parse_ic_entries(
    tokens: &[&str],
    directive: &str,
    line_no: usize,
    warnings: &mut Vec<String>,
) -> Result<Vec<NodesetEntry>> {
    let mut entries = Vec::new();
    let mut i = 1usize; // tokens[0] is the directive itself
    while i < tokens.len() {
        let tok = tokens[i];
        if !tok.contains('(') {
            warnings.push(format!(
                "line {line_no}: {directive} ignores stray token '{tok}'"
            ));
            i += 1;
            continue;
        }

        let open_rel = tok.find('(').expect("'(' checked above");
        if open_rel == 0 {
            return Err(SimError::Parse(format!(
                "line {line_no}: {directive} entry '{tok}' is missing its device letter (V/I)"
            )));
        }
        let dev = tok[..open_rel].to_ascii_uppercase();
        let close_rel = tok[open_rel..]
            .find(')')
            .map(|c| open_rel + c)
            .ok_or_else(|| {
                SimError::Parse(format!(
                    "line {line_no}: {directive} entry '{tok}' is missing ')'"
                ))
            })?;
        let node = tok[open_rel + 1..close_rel].trim().to_string();
        if node.is_empty() {
            return Err(SimError::Parse(format!(
                "line {line_no}: {directive} entry '{tok}' names no node"
            )));
        }

        // The value rides fused after ')' (`V(n)=5`), or lands in later
        // tokens (`V(n) = 5`, `V(n)= 5`, `V(n) =5`).
        let after = &tok[close_rel + 1..];
        if !after.is_empty() && !after.starts_with('=') {
            return Err(SimError::Parse(format!(
                "line {line_no}: {directive} entry '{tok}' must match V(node)=value"
            )));
        }
        let mut value_tok = after.strip_prefix('=').unwrap_or("").to_string();
        let mut next = i + 1;
        while value_tok.is_empty() {
            let candidate = tokens.get(next).ok_or_else(|| {
                SimError::Parse(format!(
                    "line {line_no}: {directive} entry '{tok}' is missing its value"
                ))
            })?;
            if candidate.contains('(') {
                // The next entry arrived while the value was still pending.
                return Err(SimError::Parse(format!(
                    "line {line_no}: {directive} entry '{tok}' is missing its value"
                )));
            }
            let stripped = candidate.trim_start_matches('=');
            if stripped.is_empty() {
                value_tok = String::new();
                next += 1;
                continue; // a lone '=' spacer — keep looking
            }
            value_tok = stripped.to_string();
            next += 1;
            break;
        }
        let value = parse_value(&value_tok).map_err(|e| {
            SimError::Parse(format!(
                "line {line_no}: {directive} entry '{tok}': {e}"
            ))
        })?;

        match dev.as_str() {
            "V" => entries.push(NodesetEntry { node, value }),
            "I" => warnings.push(format!(
                "line {line_no}: {directive} entry I({node}) is not supported — skipped (inductor currents are not seeded)"
            )),
            _ => warnings.push(format!(
                "line {line_no}: {directive} only carries V()/I() entries — '{tok}' skipped"
            )),
        }
        i = next;
    }
    Ok(entries)
}

// ─── Model cards ─────────────────────────────────────────────────────────────

/// `.model <name> <TYPE>(<KEY=VALUE …>)` — the complete card grammar.
///
/// The keyword and the parameter list may run together (`NMOS(KP=…)`), sit
/// apart (`NMOS (…)`) with whitespace inside the list, and `%`/`)`
/// decorations on values are tolerated. Model names are lowercased so the
/// element-card lookup is case-insensitive. Waveform-card conventions:
///
/// * `D` → a [`DiodeModel`] — every SPICE diode parameter is parsed, and
///   the ones the engine stamps (`IS`, `N`) drive the equations.
/// * `NMOS`/`PMOS` → a [`MosfetModel`] — level-1 parameters parsed, plus
///   the `LEVEL` keyword the element validator checks.
/// * anything else (`NPN`, `RES`, …) → a `GenericModel` in
///   `circuit.generic_models`, so decks from other tools import cleanly.
///
/// `DEV=`/`LOT=` spell the PSpice Monte-Carlo tolerance slot on every
/// family (TODO §8 consumes it); unsupported parameters warn and are
/// carried as far as storage allows, but always skip silently *only* at
/// the parameter level — structural breakage (no name, no type, unbalanced
/// parentheses) is an error.
fn parse_model_card(
    line: &str,
    tokens: &[&str],
    circuit: &mut Circuit,
    warnings: &mut Vec<String>,
    line_no: usize,
) -> Result<()> {
    if tokens.len() < 3 {
        return Err(SimError::Parse(
            ".model requires at least a name and a type".into(),
        ));
    }
    let name = tokens[1].to_ascii_lowercase();

    // The type keyword lives after the name and ends at '(' or whitespace.
    // Slice the source line by byte span — a `.find(name)` would also hit
    // earlier text like `.model od D(...)`.
    let name_end = nth_token_span(line, 1).ok_or_else(|| {
        SimError::Parse(format!("line {line_no}: broken .model card: {line}"))
    })?.end;
    let tail = line[name_end..].trim_start();
    if tail.is_empty() {
        return Err(SimError::Parse(
            format!("line {line_no}: .model '{name}' is missing its type"),
        ));
    }
    let (type_word, param_body) = match tail.find('(') {
        Some(open_rel) => {
            let after = &tail[open_rel + 1..];
            let close_rel = after.rfind(')').ok_or_else(|| {
                SimError::Parse(format!(
                    "line {line_no}: .model '{name}': unbalanced parentheses"
                ))
            })?;
            let trailing = after[close_rel + 1..].trim();
            if !trailing.is_empty() {
                warnings.push(format!(
                    "line {line_no}: .model '{name}': tokens after the parameter list are ignored: {trailing}"
                ));
            }
            (tail[..open_rel].trim(), after[..close_rel].to_string())
        }
        // Paren-less form (`D IS=1e-14 N=1.05`): every token after the type
        // word is parameter text.
        None => match tail.find(char::is_whitespace) {
            Some(ws) => (tail[..ws].trim(), tail[ws + 1..].trim().to_string()),
            None => (tail.trim(), String::new()),
        },
    };
    if type_word.is_empty() {
        return Err(SimError::Parse(
            format!("line {line_no}: .model '{name}' is missing its type"),
        ));
    }

    let (mut pairs, strays) = parse_model_params(&param_body);
    // A generic `JFET(NJF …)` card carries the polarity as a bare token —
    // that spelling is legal, so it does not warn as a stray.
    let type_word_lower = type_word.to_ascii_lowercase();
    let jfet_polarity_in_body = type_word_lower == "jfet"
        && strays
            .iter()
            .chain(pairs.iter().filter(|(k, _)| k == "type").map(|(_, v)| v))
            .any(|s| matches!(s.to_ascii_lowercase().as_str(), "njf" | "pjf"));
    for stray in &strays {
        if jfet_polarity_in_body && matches!(stray.to_ascii_lowercase().as_str(), "njf" | "pjf") {
            continue;
        }
        warnings.push(format!(
            "line {line_no}: .model '{name}': token '{stray}' is not a KEY=VALUE pair — ignored"
        ));
    }

    // Value reader with the line context baked in; stray punctuation from
    // the parameter list spelling (`5)` after the closing paren) is
    // tolerated ahead of the numeric parse.
    let value = |raw: &str| {
        parse_model_value(raw).map_err(|e| {
            SimError::Parse(format!("line {line_no}: .model '{name}': {e}"))
        })
    };

    let type_lower = type_word.to_ascii_lowercase();
    if type_lower == "d" || type_lower == "diode" {
        let mut model = DiodeModel::default();
        for (key, raw) in pairs {
            match key.as_str() {
                "is" => model.is = value(&raw)?,
                "n" => model.n = value(&raw)?,
                "rs" => model.rs = value(&raw)?,
                "cjo" => model.cjo = value(&raw)?,
                "vj" => model.vj = value(&raw)?,
                "m" => model.m = value(&raw)?,
                "tt" => model.tt = value(&raw)?,
                "eg" => model.eg = value(&raw)?,
                "xti" => model.xti = value(&raw)?,
                "bv" => model.bv = value(&raw)?,
                "ibv" => model.ibv = value(&raw)?,
                "kf" => model.kf = value(&raw)?,
                "af" => model.af = value(&raw)?,
                "tnom" => model.tnom = value(&raw)?,
                "nbv" => model.nbv = value(&raw)?,
                "fc" => model.fc = value(&raw)?,
                "dev" => model.tolerance.dev = Some(parse_tolerance_value(&raw, &value)),
                "lot" => model.tolerance.lot = Some(parse_tolerance_value(&raw, &value)),
                "level" => warnings.push(format!(
                    "line {line_no}: .model '{name}': LEVEL on a diode model is ignored"
                )),
                other => warnings.push(format!(
                    "line {line_no}: .model '{name}': diode parameter '{other}' is not supported — ignored"
                )),
            }
        }
        circuit.diode_models.insert(name, model);
        return Ok(());
    }

    if type_lower == "nmos" || type_lower == "pmos" {
        let kind = if type_lower == "nmos" {
            MosfetType::Nmos
        } else {
            MosfetType::Pmos
        };
        let mut model = MosfetModel {
            kind,
            ..MosfetModel::default()
        };
        for (key, raw) in pairs {
            match key.as_str() {
                "level" => match raw
                    .trim()
                    .trim_end_matches(|c| c == ')' || c == '%' || c == ',')
                    .parse::<u32>()
                {
                    Ok(level) => model.level = level,
                    Err(_) => {
                        return Err(SimError::Parse(format!(
                            "line {line_no}: .model '{name}': invalid LEVEL '{raw}'"
                        )))
                    }
                },
                "kp" => model.kp = value(&raw)?,
                "vth0" | "vto" => model.vth0 = value(&raw)?,
                "lambda" => model.lambda = value(&raw)?,
                "gamma" => model.gamma = value(&raw)?,
                "phi" => model.phi = value(&raw)?,
                "tnom" => model.tnom = value(&raw)?,
                "tcv" => model.tcv = value(&raw)?,
                "bex" => model.bex = value(&raw)?,
                "tox" => model.tox = Some(value(&raw)?),
                "cgdo" => model.cgdo = Some(value(&raw)?),
                "cgso" => model.cgso = Some(value(&raw)?),
                "cgbo" => model.cgbo = Some(value(&raw)?),
                "cj" => model.cj = Some(value(&raw)?),
                "cjsw" => model.cjsw = Some(value(&raw)?),
                "mj" => model.mj = Some(value(&raw)?),
                "mjsw" => model.mjsw = Some(value(&raw)?),
                "pb" => model.pb = Some(value(&raw)?),
                "fc" => model.fc = Some(value(&raw)?),
                "nsub" => model.nsub = Some(value(&raw)?),
                "nss" => model.nss = Some(value(&raw)?),
                "tpg" => model.tpg = Some(value(&raw)?),
                "dev" => model.tolerance.dev = Some(parse_tolerance_value(&raw, &value)),
                "lot" => model.tolerance.lot = Some(parse_tolerance_value(&raw, &value)),
                other => warnings.push(format!(
                    "line {line_no}: .model '{name}': MOSFET parameter '{other}' is not supported — ignored"
                )),
            }
        }
        circuit.mosfet_models.insert(name, model);
        return Ok(());
    }

    if type_lower == "npn" || type_lower == "pnp" {
        let kind = if type_lower == "npn" {
            BjtType::Npn
        } else {
            BjtType::Pnp
        };
        let mut model = BjtModel { kind, ..BjtModel::default() };
        for (key, raw) in pairs {
            match key.as_str() {
                "is" => model.is = value(&raw)?,
                "bf" => model.bf = value(&raw)?,
                "br" => model.br = value(&raw)?,
                "nf" => model.nf = value(&raw)?,
                "nr" => model.nr = value(&raw)?,
                "vaf" => model.vaf = value(&raw)?,
                "var" => model.var = value(&raw)?,
                "ikf" => model.ikf = value(&raw)?,
                "ikr" => model.ikr = value(&raw)?,
                "ise" => model.ise = value(&raw)?,
                "ne" => model.ne = value(&raw)?,
                "isc" => model.isc = value(&raw)?,
                "nc" => model.nc = value(&raw)?,
                "rb" => model.rb = value(&raw)?,
                "rc" => model.rc = value(&raw)?,
                "re" => model.re = value(&raw)?,
                "cje" => model.cje = value(&raw)?,
                "vje" => model.vje = value(&raw)?,
                "mje" => model.mje = value(&raw)?,
                "cjc" => model.cjc = value(&raw)?,
                "vjc" => model.vjc = value(&raw)?,
                "mjc" => model.mjc = value(&raw)?,
                "tf" => model.tf = value(&raw)?,
                "tr" => model.tr = value(&raw)?,
                "xti" => model.xti = value(&raw)?,
                "xtb" => model.xtb = value(&raw)?,
                "eg" => model.eg = value(&raw)?,
                "tnom" => model.tnom = value(&raw)?,
                "kf" => model.kf = value(&raw)?,
                "af" => model.af = value(&raw)?,
                "dev" => model.tolerance.dev = Some(parse_tolerance_value(&raw, &value)),
                "lot" => model.tolerance.lot = Some(parse_tolerance_value(&raw, &value)),
                "level" => {}
                other => warnings.push(format!(
                    "line {line_no}: .model '{name}': BJT parameter '{other}' is not supported — ignored"
                )),
            }
        }
        circuit.bjt_models.insert(name, model);
        return Ok(());
    }

    // JFET card: either bare `NJF`/`PJF`, or the generic `JFET(...)` spelling
    // where the polarity arrives in the body (a leading stray `NJF`/`PJF`
    // token, or a `TYPE=` pair), defaulting to NJF.
    if type_lower == "njf" || type_lower == "pjf" || type_lower == "jfet" {
        let mut rest_strays = strays;
        let polarity_word = if type_lower == "jfet" {
            // The bare-token spelling (`JFET(NJF VTO=…)`) lost the token in
            // the pairs/strays split; the pairs-only variant keeps `TYPE=`.
            let mut word = "njf".to_string();
            let mut rest_pairs = pairs;
            if let Some(idx) = rest_strays.iter().position(|s| {
                matches!(s.to_ascii_lowercase().as_str(), "njf" | "pjf")
            }) {
                word = rest_strays.remove(idx).to_ascii_lowercase();
            } else if let Some(idx) = rest_pairs.iter().position(|(k, _)| k == "type") {
                let (_, v) = rest_pairs.remove(idx);
                let v = v.to_ascii_lowercase();
                if matches!(v.as_str(), "njf" | "pjf") {
                    word = v;
                }
            }
            // Only `pairs` are consumed by the parameter walk below; the
            // remaining strays were already warned above and stay unread.
            pairs = rest_pairs;
            word
        } else {
            type_lower.clone()
        };
        let kind = if polarity_word == "pjf" {
            JfetType::Pjf
        } else {
            JfetType::Njf
        };
        let mut model = JfetModel { kind, ..JfetModel::default() };
        for (key, raw) in pairs {
            match key.as_str() {
                "vto" => model.vto = value(&raw)?,
                "beta" => model.beta = value(&raw)?,
                "lambda" => model.lambda = value(&raw)?,
                "b" => model.b = value(&raw)?,
                "is" => model.is = value(&raw)?,
                "rd" => model.rd = value(&raw)?,
                "rs" => model.rs = value(&raw)?,
                "cgs" => model.cgs = value(&raw)?,
                "cgd" => model.cgd = value(&raw)?,
                "pb" => model.pb = value(&raw)?,
                "fc" => model.fc = value(&raw)?,
                "xti" => model.xti = value(&raw)?,
                "eg" => model.eg = value(&raw)?,
                "tnom" => model.tnom = value(&raw)?,
                "kf" => model.kf = value(&raw)?,
                "af" => model.af = value(&raw)?,
                "dev" => model.tolerance.dev = Some(parse_tolerance_value(&raw, &value)),
                "lot" => model.tolerance.lot = Some(parse_tolerance_value(&raw, &value)),
                "level" => {}
                other => warnings.push(format!(
                    "line {line_no}: .model '{name}': JFET parameter '{other}' is not supported — ignored"
                )),
            }
        }
        circuit.jfet_models.insert(name, model);
        return Ok(());
    }

    // Passive model cards: `.model m R(R=1k TC1=…)`, `CAP(…)`, `IND(…)`.    // Passive model cards: `.model m R(R=1k TC1=…)`, `CAP(…)`, `IND(…)`.
    let passive = matches!(
        type_lower.as_str(),
        "r" | "res" | "resistor" | "c" | "cap" | "capacitor" | "l" | "ind" | "inductor"
    );
    if passive {
        let mut model = crate::circuit::PassiveModel::default();
        for (key, raw) in pairs {
            match key.as_str() {
                "r" | "res" | "resistance" | "c" | "cap" | "capacitance" | "l" | "ind"
                | "inductance" => model.value = Some(value(&raw)?),
                "tc1" => model.tc1 = value(&raw)?,
                "tc2" => model.tc2 = value(&raw)?,
                // LTspice-style parasitic slots (TODO §5). They are read
                // here and materialized into element arms by
                // `expand_parasitics` after the full deck walk.
                "rser" => model.rser = value(&raw)?,
                "lser" => model.lser = value(&raw)?,
                "cpar" => model.cpar = value(&raw)?,
                "rpar" => model.rpar = value(&raw)?,
                "dev" | "lot" => {}
                other => warnings.push(format!(
                    "line {line_no}: .model '{name}': passive parameter '{other}' is not supported — ignored"
                )),
            }
        }
        match type_lower.as_str() {
            "r" | "res" | "resistor" => {
                circuit.resistor_models.insert(name, model);
            }
            "c" | "cap" | "capacitor" => {
                circuit.capacitor_models.insert(name, model);
            }
            _ => {
                circuit.inductor_models.insert(name, model);
            }
        }
        return Ok(());
    }

    if type_lower == "sw" || type_lower == "switch" || type_lower == "s" {
        let mut model = SwitchModel::default();
        for (key, raw) in pairs {
            match key.as_str() {
                "ron" => model.ron = value(&raw)?,
                "roff" => model.roff = value(&raw)?,
                "vt" | "vth" => model.vt = value(&raw)?,
                "vh" | "vhy" => model.vh = value(&raw)?,
                "dev" | "lot" => {} // tolerance slots are not physical here
                other => warnings.push(format!(
                    "line {line_no}: .model '{name}': switch parameter '{other}' is not supported — ignored"
                )),
            }
        }
        circuit.switch_models.insert(name, model);
        return Ok(());
    }

    // Unknown device family — store the card so the deck still imports, and
    // a future element letter can consume it (TODO §2).
    let mut generic = GenericModel {
        spice_type: type_lower,
        level: None,
        params: Vec::new(),
        tolerance: ModelTolerance::default(),
    };
    for (key, raw) in pairs {
        match key.as_str() {
            "level" => match value(&raw) {
                Ok(l) => generic.level = Some(l as u32),
                Err(_) => warnings.push(format!(
                    "line {line_no}: .model '{name}': non-numeric LEVEL '{raw}' is kept out of the card"
                )),
            },
            "dev" => generic.tolerance.dev = Some(parse_tolerance_value(&raw, &value)),
            "lot" => generic.tolerance.lot = Some(parse_tolerance_value(&raw, &value)),
            _ => match value(&raw) {
                Ok(v) => generic.params.push((key, v)),
                Err(_) => warnings.push(format!(
                    "line {line_no}: .model '{name}': parameter '{key}={raw}' is not numeric — ignored"
                )),
            },
        }
    }
    circuit.generic_models.insert(name, generic);
    Ok(())
}

/// Byte span of the `index`-th whitespace-separated token of `line`.
fn nth_token_span(line: &str, index: usize) -> Option<std::ops::Range<usize>> {
    let bytes = line.as_bytes();
    let mut i = 0usize;
    let mut count = 0usize;
    while i < bytes.len() {
        while i < bytes.len() && bytes[i].is_ascii_whitespace() {
            i += 1;
        }
        if i >= bytes.len() {
            break;
        }
        let start = i;
        while i < bytes.len() && !bytes[i].is_ascii_whitespace() {
            i += 1;
        }
        if count == index {
            return Some(start..i);
        }
        count += 1;
    }
    None
}

/// Split a `.model` parameter body into `(key, value_text)` pairs.
///
/// The reference spellings run pairs together (`KP=20e-6 VTH0=0.7`), with
/// optional whitespace (or `,`) either side of `=`; a `=` without value or
/// a stray word lands in the returned "strays" list so the caller can warn
/// with card-level context.
fn parse_model_params(body: &str) -> (Vec<(String, String)>, Vec<String>) {
    let cleaned = body.replace(',', " ");
    let bytes = cleaned.as_bytes();
    let mut pairs = Vec::new();
    let mut strays = Vec::new();
    let mut i = 0usize;
    while i < bytes.len() {
        if bytes[i].is_ascii_whitespace() {
            i += 1;
            continue;
        }
        let key_start = i;
        while i < bytes.len() && bytes[i] != b'=' && !bytes[i].is_ascii_whitespace() {
            i += 1;
        }
        if i == bytes.len() || bytes[i].is_ascii_whitespace() {
            // No '=' before the pair ends: a key-less leftover word.
            strays.push(cleaned[key_start..i].to_string());
            continue;
        }
        let key = cleaned[key_start..i].to_ascii_lowercase();
        i += 1; // the '='
        while i < bytes.len() && bytes[i].is_ascii_whitespace() {
            i += 1;
        }
        let value_start = i;
        while i < bytes.len() && !bytes[i].is_ascii_whitespace() {
            i += 1;
        }
        let value = cleaned[value_start..i].to_string();
        if value.is_empty() {
            strays.push(key);
        } else {
            pairs.push((key, value));
        }
    }
    (pairs, strays)
}

/// One model parameter value: trailing `%`/`)`/`,` decorations are
/// tolerated (the tolerance `5%` keeps its percent flag via
/// [`parse_tolerance_value`]; model values simply drop it).
fn parse_model_value(raw: &str) -> std::result::Result<f64, String> {
    parse_value(raw.trim().trim_end_matches(|c| c == ')' || c == '%' || c == ','))
        .map_err(|e| format!("value '{raw}' does not parse: {e}"))
}

/// A `DEV=`/`LOT=` tolerance slot: the number plus whether the `5%`
/// percent-flag spelling was used.
fn parse_tolerance_value(
    raw: &str,
    value: &impl Fn(&str) -> std::result::Result<f64, SimError>,
) -> (f64, bool) {
    let percent = raw.trim_end().ends_with('%');
    let parsed = value(raw.trim().trim_end_matches('%')).unwrap_or(0.0);
    (parsed, percent)
}

/// Parse `.options` keys the engine consumes. Unknown keys warn (the full
/// reference-tool option surface is a TODO §2 item) rather than failing an
/// otherwise simulatable deck.
fn parse_options(
    tokens: &[&str],
    analysis: &mut AnalysisCommands,
    warnings: &mut Vec<String>,
    line_no: usize,
) -> Result<()> {
    if tokens.len() < 2 {
        warnings.push(format!("line {line_no}: .options card has no options"));
        return Ok(());
    }
    for tok in &tokens[1..] {
        let Some((key, value)) = tok.split_once('=') else {
            warnings.push(format!(
                "line {line_no}: .options token '{tok}' is not KEY=VALUE — ignored"
            ));
            continue;
        };
        let key = key.to_ascii_lowercase();
        match key.as_str() {
            "method" => {
                analysis.method = match value.to_ascii_lowercase().as_str() {
                    "trap" | "trapezoidal" => crate::circuit::IntegrationMethod::Trapezoidal,
                    "gear" | "gear2" => crate::circuit::IntegrationMethod::Gear2,
                    "gear1" | "be" | "euler" => crate::circuit::IntegrationMethod::Gear1,
                    other => {
                        return Err(SimError::Parse(format!(
                            "line {line_no}: .options method={other} must be trap, gear2, or gear1"
                        )))
                    }
                };
            }
            "reltol" => analysis.reltol = parse_value(value).map_err(SimError::Parse)?,
            "abstol" => analysis.abstol = parse_value(value).map_err(SimError::Parse)?,
            "vntol" => analysis.vntol = parse_value(value).map_err(SimError::Parse)?,
            "maxstep" | "tmax" => {
                analysis.tran_maxstep = Some(parse_value(value).map_err(SimError::Parse)?)
            }
            // TODO §3 convergence suite knobs: they drive the DC operating
            // point (gmin floor, the gmin/source-stepping ladder depths) and
            // the Newton iteration budgets at DC (`itl1`) and per transient
            // point (`itl4`).
            "gmin" => analysis.gmin = parse_value(value).map_err(SimError::Parse)?,
            "gminsteps" => {
                analysis.gminsteps =
                    parse_value(value).map_err(SimError::Parse)?.max(0.0) as usize
            }
            "srcsteps" => {
                analysis.srcsteps =
                    parse_value(value).map_err(SimError::Parse)?.max(0.0) as usize
            }
            "itl1" => analysis.itl1 = parse_value(value).map_err(SimError::Parse)?.max(1.0) as usize,
            "itl4" => analysis.itl4 = parse_value(value).map_err(SimError::Parse)?.max(1.0) as usize,
            // Parsed but not applied yet; keep them as warnings instead of
            // pretending the engine honours them.
            "itl2" | "itl6" | "maxord" | "tnom" | "temp"
            | "accurate" | "solvehold" | "plotwinsize" | "numdgt" => {
                warnings.push(format!(
                    "line {line_no}: .options {key}={value} is parsed but not applied yet"
                ));
            }
            other => warnings.push(format!(
                "line {line_no}: .options '{other}' is not recognized — ignored"
            )),
        }
    }
    Ok(())
}

/// Parse one `.step` card.
///
/// Accepted spellings (LTspice/PSpice-compatible):
/// `.step param x 0 10 1`, `.step dec param x 1 1k 10`,
/// `.step oct param x 1 1k 5`, `.step param x list 1 2 3`,
/// and the `PARAM`-less `.step x …` shorthand.
fn parse_step(
    tokens: &[&str],
    analysis: &mut AnalysisCommands,
    warnings: &mut Vec<String>,
    line_no: usize,
) -> Result<()> {
    if tokens.len() < 3 {
        return Err(SimError::Parse(format!(
            "line {line_no}: .step requires: .step [DEC|OCT|LIN] PARAM <name> <values>"
        )));
    }
    let mut i = 1usize;
    let scale = match tokens[i].to_ascii_lowercase().as_str() {
        "dec" => {
            i += 1;
            Some(AcScale::Dec)
        }
        "oct" => {
            i += 1;
            Some(AcScale::Oct)
        }
        "lin" => {
            i += 1;
            Some(AcScale::Lin)
        }
        _ => None,
    };
    if tokens.get(i).is_some_and(|t| t.eq_ignore_ascii_case("param")) {
        i += 1;
    }
    let target = tokens
        .get(i)
        .ok_or_else(|| {
            SimError::Parse(format!("line {line_no}: .step is missing its parameter name"))
        })?
        .to_ascii_lowercase();
    i += 1;
    if !analysis
        .steps
        .iter()
        .all(|s| s.target != target)
    {
        return Err(SimError::Parse(format!(
            "line {line_no}: .step already sweeps '{target}' — each parameter may be stepped once"
        )));
    }

    let (values, consumed) = parse_step_values(&tokens[i..], scale, line_no, &target)?;
    if i + consumed < tokens.len() {
        warnings.push(ignored_tokens(line_no, ".step", &tokens[i + consumed..]));
    }
    analysis.steps.push(StepSpec { target, values });
    Ok(())
}

/// The value part of a `.step` card, returning the parsed set and how many
/// tokens it consumed. Malformed values are loud errors.
fn parse_step_values(
    rest: &[&str],
    scale: Option<AcScale>,
    line_no: usize,
    target: &str,
) -> Result<(StepValues, usize)> {
    let Some(first) = rest.first() else {
        return Err(SimError::Parse(format!(
            "line {line_no}: .step {target} needs values"
        )));
    };
    if first.eq_ignore_ascii_case("list") {
        let mut values = Vec::new();
        for tok in &rest[1..] {
            values.push(parse_value(tok).map_err(SimError::Parse)?);
        }
        if values.is_empty() {
            return Err(SimError::Parse(format!(
                "line {line_no}: .step {target} LIST needs at least one value"
            )));
        }
        return Ok((StepValues::List(values), rest.len()));
    }
    if rest.len() < 3 {
        return Err(SimError::Parse(format!(
            "line {line_no}: .step {target} needs start, stop, and increment/points"
        )));
    }
    let start = parse_value(rest[0]).map_err(SimError::Parse)?;
    let stop = parse_value(rest[1]).map_err(SimError::Parse)?;
    match scale {
        Some(AcScale::Dec) | Some(AcScale::Oct) => {
            let points: usize = rest[2].parse().map_err(|_| {
                SimError::Parse(format!(
                    "line {line_no}: .step {target}: '{}' is not a point count",
                    rest[2]
                ))
            })?;
            if start <= 0.0 || stop <= 0.0 || stop < start {
                return Err(SimError::Parse(format!(
                    "line {line_no}: .step {target}: logarithmic steps need 0 < start <= stop"
                )));
            }
            if points == 0 {
                return Err(SimError::Parse(format!(
                    "line {line_no}: .step {target}: points per decade/octave must be > 0"
                )));
            }
            let values = if matches!(scale, Some(AcScale::Dec)) {
                StepValues::Decade { start, stop, points }
            } else {
                StepValues::Octave { start, stop, points }
            };
            Ok((values, 3))
        }
        _ => {
            let step = parse_value(rest[2]).map_err(SimError::Parse)?;
            if step == 0.0 {
                return Err(SimError::Parse(format!(
                    "line {line_no}: .step {target}: increment cannot be zero"
                )));
            }
            if (stop - start) * step < 0.0 {
                return Err(SimError::Parse(format!(
                    "line {line_no}: .step {target}: increment moves away from stop"
                )));
            }
            Ok((StepValues::Linear { start, stop, step }, 3))
        }
    }
}

/// Parse one `.meas`/`.measure` card.
///
/// `.meas <tran|ac|dc|op> <name> <function…>` with the reference tools'
/// function set: `avg rms max min pp max_at min_at`, `find … when …`,
/// `cross … val=…`, and `trig … targ …`. Options are `from=`, `to=`, `val=`,
/// and `rise[=n]`/`fall[=n]`/`cross[=n]`.
fn parse_measurement(tokens: &[&str], line_no: usize) -> Result<Measurement> {
    if tokens.len() < 4 {
        return Err(SimError::Parse(format!(
            "line {line_no}: .meas requires: .meas <tran|ac|dc|op> <name> <function>"
        )));
    }
    let analysis = match tokens[1].to_ascii_lowercase().as_str() {
        "tran" => MeasAnalysis::Tran,
        "ac" => MeasAnalysis::Ac,
        "dc" => MeasAnalysis::Dc,
        "op" => MeasAnalysis::Op,
        other => {
            return Err(SimError::Parse(format!(
                "line {line_no}: .meas analysis '{other}' must be tran, ac, dc, or op"
            )))
        }
    };
    let name = tokens[2].to_string();
    let function = tokens[3].to_ascii_lowercase();
    let func = match function.as_str() {
        "avg" => MeasFunc::Avg,
        "rms" => MeasFunc::Rms,
        "max" => MeasFunc::Max,
        "min" => MeasFunc::Min,
        "pp" => MeasFunc::Pp,
        "max_at" => MeasFunc::MaxAt,
        "min_at" => MeasFunc::MinAt,
        "find" => {
            return parse_find_when(tokens, analysis, name, line_no);
        }
        "cross" => return parse_cross(tokens, analysis, name, line_no),
        "trig" => return parse_trig_targ(tokens, analysis, name, line_no),
        other => {
            return Err(SimError::Parse(format!(
                "line {line_no}: .meas function '{other}' is not supported \
                 (use avg/rms/max/min/pp/max_at/min_at/find/cross/trig)"
            )))
        }
    };
    let expr = tokens
        .get(4)
        .ok_or_else(|| {
            SimError::Parse(format!("line {line_no}: .meas {name}: '{function}' needs an expression"))
        })?
        .to_string();
    let opts = parse_meas_options(&tokens[5..], line_no, &name)?;
    if let Some(value) = opts.value {
        return Err(SimError::Parse(format!(
            "line {line_no}: .meas {name}: VAL={value} is only meaningful for find/cross/trig"
        )));
    }
    if opts.edge.is_some() {
        return Err(SimError::Parse(format!(
            "line {line_no}: .meas {name}: an edge selector is only meaningful for find/cross/trig"
        )));
    }
    Ok(Measurement {
        analysis,
        name,
        kind: MeasKind::Scalar { func, expr, from: opts.from, to: opts.to },
    })
}

fn parse_find_when(
    tokens: &[&str],
    analysis: MeasAnalysis,
    name: String,
    line_no: usize,
) -> Result<Measurement> {
    let expr = tokens
        .get(4)
        .ok_or_else(|| SimError::Parse(format!("line {line_no}: .meas {name}: FIND needs an expression")))?
        .to_string();
    if !tokens.get(5).is_some_and(|t| t.eq_ignore_ascii_case("when")) {
        return Err(SimError::Parse(format!(
            "line {line_no}: .meas {name}: FIND must be followed by WHEN <expr>=<value>"
        )));
    }
    let condition = tokens.get(6).ok_or_else(|| {
        SimError::Parse(format!("line {line_no}: .meas {name}: WHEN needs a condition"))
    })?;
    let (when, when_value, consumed) = split_condition(condition, tokens.get(7), line_no, &name)?;
    let opts = parse_meas_options(&tokens[6 + consumed..], line_no, &name)?;
    Ok(Measurement {
        analysis,
        name,
        kind: MeasKind::FindWhen {
            expr,
            when,
            when_value,
            edge: opts.edge,
            from: opts.from,
            to: opts.to,
        },
    })
}

/// Split `V(in)=1.5` (or the spaced `V(in) = 1.5`) into expression, target,
/// and how many tokens the condition consumed.
fn split_condition(
    first: &str,
    next: Option<&&str>,
    line_no: usize,
    name: &str,
) -> Result<(String, f64, usize)> {
    if let Some(eq) = first.find('=') {
        let expr = first[..eq].trim().to_string();
        let value = parse_value(&first[eq + 1..]).map_err(SimError::Parse)?;
        if expr.is_empty() {
            return Err(SimError::Parse(format!(
                "line {line_no}: .meas {name}: WHEN condition has no expression"
            )));
        }
        return Ok((expr, value, 1));
    }
    // `V(in) = 1.5`.
    if next.is_some_and(|t| *t == "=") {
        let value_tok = next.unwrap();
        let _ = value_tok;
        // The caller passed the token after the condition; `= 1.5` needs one
        // more token than the signature can see, so support the compact form
        // only and report the spaced form clearly.
    }
    Err(SimError::Parse(format!(
        "line {line_no}: .meas {name}: WHEN condition '{first}' must be <expr>=<value>"
    )))
}

fn parse_cross(
    tokens: &[&str],
    analysis: MeasAnalysis,
    name: String,
    line_no: usize,
) -> Result<Measurement> {
    let expr = tokens
        .get(4)
        .ok_or_else(|| SimError::Parse(format!("line {line_no}: .meas {name}: CROSS needs an expression")))?
        .to_string();
    let opts = parse_meas_options(&tokens[5..], line_no, &name)?;
    let value = opts.value.ok_or_else(|| {
        SimError::Parse(format!("line {line_no}: .meas {name}: CROSS needs VAL=<level>"))
    })?;
    Ok(Measurement {
        analysis,
        name,
        kind: MeasKind::Cross { expr, value, edge: opts.edge, from: opts.from, to: opts.to },
    })
}

fn parse_trig_targ(
    tokens: &[&str],
    analysis: MeasAnalysis,
    name: String,
    line_no: usize,
) -> Result<Measurement> {
    let trig_expr = tokens
        .get(4)
        .ok_or_else(|| SimError::Parse(format!("line {line_no}: .meas {name}: TRIG needs an expression")))?
        .to_string();
    let mut i = 5usize;
    let mut trig_opts = MeasOpts::default();
    while i < tokens.len() && !tokens[i].eq_ignore_ascii_case("targ") {
        trig_opts.merge_token(tokens[i], line_no, &name)?;
        i += 1;
    }
    if i >= tokens.len() {
        return Err(SimError::Parse(format!(
            "line {line_no}: .meas {name}: TRIG must be followed by TARG"
        )));
    }
    i += 1;
    let targ_expr = tokens
        .get(i)
        .ok_or_else(|| SimError::Parse(format!("line {line_no}: .meas {name}: TARG needs an expression")))?
        .to_string();
    i += 1;
    let targ_opts = parse_meas_options(&tokens[i..], line_no, &name)?;
    let trig_value = trig_opts.value.ok_or_else(|| {
        SimError::Parse(format!("line {line_no}: .meas {name}: TRIG needs VAL=<level>"))
    })?;
    let targ_value = targ_opts.value.ok_or_else(|| {
        SimError::Parse(format!("line {line_no}: .meas {name}: TARG needs VAL=<level>"))
    })?;
    Ok(Measurement {
        analysis,
        name,
        kind: MeasKind::TrigTarg {
            trig_expr,
            trig_value,
            trig_edge: trig_opts.edge,
            targ_expr,
            targ_value,
            targ_edge: targ_opts.edge,
            from: trig_opts.from.or(targ_opts.from),
            to: trig_opts.to.or(targ_opts.to),
        },
    })
}

#[derive(Debug, Default, Clone, Copy)]
struct MeasOpts {
    value: Option<f64>,
    edge: Option<(MeasEdge, usize)>,
    from: Option<f64>,
    to: Option<f64>,
}

impl MeasOpts {
    fn merge_token(&mut self, token: &str, line_no: usize, name: &str) -> Result<()> {
        let parsed = parse_one_meas_option(token, line_no, name)?;
        match parsed {
            MeasOption::Value(v) => self.value = Some(v),
            MeasOption::Edge(e) => self.edge = Some(e),
            MeasOption::From(v) => self.from = Some(v),
            MeasOption::To(v) => self.to = Some(v),
        }
        Ok(())
    }
}

enum MeasOption {
    Value(f64),
    Edge((MeasEdge, usize)),
    From(f64),
    To(f64),
}

fn parse_one_meas_option(token: &str, line_no: usize, name: &str) -> Result<MeasOption> {
    let lower = token.to_ascii_lowercase();
    if let Some(rest) = lower.strip_prefix("val=") {
        return parse_value(rest)
            .map(MeasOption::Value)
            .map_err(SimError::Parse);
    }
    if let Some(rest) = lower.strip_prefix("from=") {
        return parse_value(rest)
            .map(MeasOption::From)
            .map_err(SimError::Parse);
    }
    if let Some(rest) = lower.strip_prefix("to=") {
        return parse_value(rest)
            .map(MeasOption::To)
            .map_err(SimError::Parse);
    }
    let (word, count) = match lower.split_once('=') {
        Some((word, n)) => {
            let parsed = n.parse::<usize>().map_err(|_| {
                SimError::Parse(format!(
                    "line {line_no}: .meas {name}: '{token}' needs an occurrence count"
                ))
            })?;
            (word.to_string(), parsed)
        }
        None => (lower.clone(), 1),
    };
    let edge = match word.as_str() {
        "rise" => MeasEdge::Rise,
        "fall" => MeasEdge::Fall,
        "cross" => MeasEdge::Cross,
        _ => {
            return Err(SimError::Parse(format!(
                "line {line_no}: .meas {name}: unsupported option '{token}' \
                 (expected from=, to=, val=, or rise/fall/cross[=n])"
            )))
        }
    };
    if count == 0 {
        return Err(SimError::Parse(format!(
            "line {line_no}: .meas {name}: occurrence count must be >= 1"
        )));
    }
    Ok(MeasOption::Edge((edge, count)))
}

fn parse_meas_options(tokens: &[&str], line_no: usize, name: &str) -> Result<MeasOpts> {
    let mut opts = MeasOpts::default();
    for tok in tokens {
        opts.merge_token(tok, line_no, name)?;
    }
    Ok(opts)
}


/// Parse the sweep arguments of `.dc`. Malformed arguments are errors —
/// sweep arguments are never silently dropped.
fn parse_dc(tokens: &[&str], analysis: &mut AnalysisCommands, line_no: usize) -> Result<()> {
    // A bare `.dc` stays a plain operating point (historical behavior).
    if tokens.len() == 1 {
        analysis.dc_op = true;
        return Ok(());
    }

    let mut sweeps = Vec::new();
    let mut i = 1usize;
    while i < tokens.len() {
        let (spec, next) = parse_dc_sweep_spec(tokens, i, line_no)?;
        sweeps.push(spec);
        i = next;
    }
    if sweeps.len() > 2 {
        return Err(SimError::Parse(format!(
            "line {line_no}: .dc supports at most two nested sweeps, got {}",
            sweeps.len()
        )));
    }
    analysis.dc = Some(DcCommand { sweeps });
    Ok(())
}

/// One sweep spec starting at token `i` (`target [start stop step]` or
/// `target LIST v1 v2 …`), returning the index of the next unconsumed token.
fn parse_dc_sweep_spec(
    tokens: &[&str],
    i: usize,
    line_no: usize,
) -> Result<(DcSweepSpec, usize)> {
    let target = tokens.get(i).copied().ok_or_else(|| {
        SimError::Parse(format!("line {line_no}: .dc is missing its sweep source name"))
    })?;
    let first = target
        .chars()
        .next()
        .filter(|c| c.is_ascii_alphabetic())
        .ok_or_else(|| {
            SimError::Parse(format!(
                "line {line_no}: .dc sweep target '{}' must be a source or parameter name",
                target
            ))
        })?;
    let _ = first;
    let target = target.to_string();

    let after = i + 1;
    if tokens
        .get(after)
        .map(|t| t.eq_ignore_ascii_case("list"))
        .unwrap_or(false)
    {
        // Values run until the first token that is not a number — which is
        // where the next sweep spec (if any) begins.
        let mut vals = Vec::new();
        let mut j = after + 1;
        while j < tokens.len() {
            match parse_value(tokens[j]) {
                Ok(v) => vals.push(v),
                Err(_) => break,
            }
            j += 1;
        }
        if vals.is_empty() {
            return Err(SimError::Parse(format!(
                ".dc {} LIST needs at least one value",
                target
            )));
        }
        return Ok((DcSweepSpec { target, values: DcSweepValues::List(vals) }, j));
    }

    if tokens.len() < after + 3 {
        return Err(SimError::Parse(format!(
            "line {line_no}: .dc {} requires <start> <stop> <increment>, or LIST with values",
            target
        )));
    }
    let start = parse_value(tokens[after]).map_err(SimError::Parse)?;
    let stop = parse_value(tokens[after + 1]).map_err(SimError::Parse)?;
    let step = parse_value(tokens[after + 2]).map_err(SimError::Parse)?;

    if step == 0.0 {
        return Err(SimError::Parse(format!(
            "line {line_no}: .dc {} increment cannot be zero",
            target
        )));
    }
    if (stop > start && step < 0.0) || (stop < start && step > 0.0) {
        return Err(SimError::Parse(format!(
            "line {line_no}: .dc {} increment {} must march from {} toward {}",
            target, step, start, stop
        )));
    }
    Ok((
        DcSweepSpec {
            target,
            values: DcSweepValues::Linear { start, stop, step },
        },
        after + 3,
    ))
}

// ─── Element cards ───────────────────────────────────────────────────────────

/// SPICE syntax:
///   Dname  Anode  Cathode  ModelName  [AREA=<a>] [<a>] [OFF] [M=<m>]
///
/// The junction area multiplier scales the saturation current in the stamp
/// (TODO §2); the bare-numeric form (area as the 5th token) and the
/// `AREA=` keyword are both accepted.
fn parse_diode(
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
fn parse_mosfet(
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
/// variable and `n+1` coefficients (constant first).
fn parse_controlled_source(
    tokens: &[&str],
    circuit: &mut Circuit,
    plan: &HashMap<String, usize>,
    line_no: usize,
    warnings: &mut Vec<String>,
) -> Result<()> {
    let kind = element_letter(tokens[0]).to_ascii_uppercase();
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

/// Parse a bipolar transistor:
/// `Qname c b e [substrate] model [AREA=…] [OFF]`.
fn parse_bjt(
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
fn parse_jfet(
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
fn parse_switch(
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

/// Parse a mutual-inductance card: `Kname L1 L2 <k>`.
fn parse_mutual_inductance(
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
fn parse_passive_card<'a>(
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

fn parse_resistor(
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

fn parse_capacitor(
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

fn parse_inductor(
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

/// A two-terminal `V`/`I` source card, after the name and the two nodes.
/// The syntax is shared between both element letters, so one grammar owns it:
/// `DC <v>`, bare value, `AC <mag> [phase]`, and the waveform family
/// `PULSE`/`SIN`/`EXP`/`PWL`/`SFFM`/`AM`.
#[derive(Debug, Default)]
struct SourceSpec {
    dc: Option<f64>,
    waveform: Option<Waveform>,
    ac: Option<Ac>,
}

fn parse_source_value_specs(rest: &[&str], options: &ParseOptions) -> Result<SourceSpec> {
    let mut spec = SourceSpec::default();
    let mut i = 0usize;
    while i < rest.len() {
        let token = rest[i].to_ascii_lowercase();
        if token == "dc" {
            i += 1;
            let value = rest.get(i).copied().ok_or_else(|| {
                SimError::Parse("source missing DC value".into())
            })?;
            spec.dc = Some(parse_value(value).map_err(SimError::Parse)?);
            i += 1;
        } else if token == "ac" {
            i += 1;
            let mag_tok = rest.get(i).copied().ok_or_else(|| {
                SimError::Parse("source missing AC magnitude".into())
            })?;
            let mag = parse_value(mag_tok).map_err(SimError::Parse)?;
            i += 1;
            let mut phase = 0.0;
            if let Some(next) = rest.get(i) {
                if let Ok(p) = parse_value(next) {
                    phase = p;
                    i += 1;
                }
            }
            spec.ac = Some(Ac { mag, phase });
        } else if token.starts_with("pulse") {
            let p = parenthesised_params(&rest[i..], "pulse")?;
            if p.len() < 7 {
                return Err(SimError::Parse(
                    "PULSE source requires 7 parameters: V1 V2 TD TR TF PW PER".into(),
                ));
            }
            let v1  = parse_value(&p[0]).map_err(SimError::Parse)?;
            let v2  = parse_value(&p[1]).map_err(SimError::Parse)?;
            let td  = parse_value(&p[2]).map_err(SimError::Parse)?;
            let tr  = parse_value(&p[3]).map_err(SimError::Parse)?;
            let tf  = parse_value(&p[4]).map_err(SimError::Parse)?;
            let pw  = parse_value(&p[5]).map_err(SimError::Parse)?;
            let per = parse_value(&p[6]).map_err(SimError::Parse)?;
            set_waveform(&mut spec, Waveform::Pulse(Pulse { v1, v2, td, tr, tf, pw, per }))?;
            return Ok(spec);
        } else if token.starts_with("sin") && !token.starts_with("sffm") {
            let s = parenthesised_params(&rest[i..], "sin")?;
            if s.len() < 3 {
                return Err(SimError::Parse(
                    "SIN source requires at least 3 parameters: VO VA FREQ [TD [THETA]]".into(),
                ));
            }
            let vo   = parse_value(&s[0]).map_err(SimError::Parse)?;
            let va   = parse_value(&s[1]).map_err(SimError::Parse)?;
            let freq = parse_value(&s[2]).map_err(SimError::Parse)?;
            let td    = if s.len() > 3 { parse_value(&s[3]).map_err(SimError::Parse)? } else { 0.0 };
            let theta = if s.len() > 4 { parse_value(&s[4]).map_err(SimError::Parse)? } else { 0.0 };
            set_waveform(&mut spec, Waveform::Sine(Sine { vo, va, freq, td, theta }))?;
            return Ok(spec);
        } else if token.starts_with("exp") {
            let e = parenthesised_params(&rest[i..], "exp")?;
            if e.len() < 6 {
                return Err(SimError::Parse(
                    "EXP source requires 6 parameters: V1 V2 TD1 TAU1 TD2 TAU2".into(),
                ));
            }
            let v1 = parse_value(&e[0]).map_err(SimError::Parse)?;
            let v2 = parse_value(&e[1]).map_err(SimError::Parse)?;
            let td1 = parse_value(&e[2]).map_err(SimError::Parse)?;
            let tau1 = parse_value(&e[3]).map_err(SimError::Parse)?;
            let td2 = parse_value(&e[4]).map_err(SimError::Parse)?;
            let tau2 = parse_value(&e[5]).map_err(SimError::Parse)?;
            set_waveform(&mut spec, Waveform::Exp(Exp { v1, v2, td1, tau1, td2, tau2 }))?;
            return Ok(spec);
        } else if token.starts_with("sffm") {
            let s = parenthesised_params(&rest[i..], "sffm")?;
            if s.len() < 5 {
                return Err(SimError::Parse(
                    "SFFM source requires 5 parameters: VO VA FC MDI FS [TD]".into(),
                ));
            }
            let vo = parse_value(&s[0]).map_err(SimError::Parse)?;
            let va = parse_value(&s[1]).map_err(SimError::Parse)?;
            let fc = parse_value(&s[2]).map_err(SimError::Parse)?;
            let mdi = parse_value(&s[3]).map_err(SimError::Parse)?;
            let fs = parse_value(&s[4]).map_err(SimError::Parse)?;
            let td = if s.len() > 5 { parse_value(&s[5]).map_err(SimError::Parse)? } else { 0.0 };
            set_waveform(&mut spec, Waveform::Sffm(Sffm { vo, va, fc, mdi, fs, td }))?;
            return Ok(spec);
        } else if token.starts_with("am") {
            let a = parenthesised_params(&rest[i..], "am")?;
            if a.len() < 4 {
                return Err(SimError::Parse(
                    "AM source requires 4 parameters: VA VO FM FC [TD]".into(),
                ));
            }
            let va = parse_value(&a[0]).map_err(SimError::Parse)?;
            let vo = parse_value(&a[1]).map_err(SimError::Parse)?;
            let fm = parse_value(&a[2]).map_err(SimError::Parse)?;
            let fc = parse_value(&a[3]).map_err(SimError::Parse)?;
            let td = if a.len() > 4 { parse_value(&a[4]).map_err(SimError::Parse)? } else { 0.0 };
            set_waveform(&mut spec, Waveform::Am(Am { va, vo, fm, fc, td }))?;
            return Ok(spec);
        } else if token.starts_with("pwl") {
            let pwl = parse_pwl(&rest[i..], options)?;
            set_waveform(&mut spec, Waveform::Pwl(pwl))?;
            return Ok(spec);
        } else if spec.dc.is_none() {
            // Bare value — SPICE allows the DC keyword to be omitted.
            spec.dc = Some(parse_value(rest[i]).map_err(SimError::Parse)?);
            i += 1;
        } else {
            return Err(SimError::Parse(format!(
                "unexpected token '{}' in source definition", rest[i]
            )));
        }
    }
    Ok(spec)
}

fn set_waveform(spec: &mut SourceSpec, waveform: Waveform) -> Result<()> {
    if spec.waveform.is_some() {
        return Err(SimError::Parse(
            "a source card may carry only one waveform (PULSE/SIN/EXP/PWL/SFFM/AM)".into(),
        ));
    }
    spec.waveform = Some(waveform);
    Ok(())
}

/// Parse a `PWL(…)` card, including `PWL FILE=<file>` and LTspice's
/// `REPEAT FOR <n> (…) ENDREPEAT` / `REPEAT FOREVER (…) ENDREPEAT` blocks.
fn parse_pwl(rest: &[&str], options: &ParseOptions) -> Result<Pwl> {
    let text = rest.join(" ");
    let trimmed = text.trim();
    let after = if trimmed.len() >= 3 && trimmed[..3].eq_ignore_ascii_case("pwl") {
        &trimmed[3..]
    } else {
        return Err(SimError::Parse("broken PWL card".into()));
    };
    let after = after.trim();
    let after = after.strip_prefix('=').unwrap_or(after).trim();

    if after.len() >= 4 && after[..4].eq_ignore_ascii_case("file") {
        let file_part = after[4..].trim_start_matches(['=', ' ', '\t']);
        let name = file_part
            .split(|c: char| c.is_whitespace() || c == ')' || c == ',')
            .next()
            .unwrap_or("")
            .trim_matches(|c| c == '"' || c == '\'');
        if name.is_empty() {
            return Err(SimError::Parse("PWL FILE requires a file name".into()));
        }
        let path = find_library_file(name, options.deck_dir.as_deref(), options)
            .ok_or_else(|| SimError::Parse(format!(
                "PWL FILE '{name}' not found in the deck directory or search paths"
            )))?;
        let (content, _) = open_deck_file(&path).map_err(|e| {
            SimError::Parse(format!("PWL FILE '{}': {e}", path.display()))
        })?;
        let mut points = Vec::new();
        for (n, line) in content.lines().enumerate() {
            let line = strip_comment(line);
            let line = line.trim();
            if line.is_empty() || line.starts_with('.') {
                continue;
            }
            let mut words = line.split(|c: char| c.is_whitespace() || c == ',');
            let t = words.next().unwrap_or("");
            let v = words.next().ok_or_else(|| {
                SimError::Parse(format!(
                    "PWL FILE '{}': line {} needs a time and a value",
                    path.display(),
                    n + 1
                ))
            })?;
            let t = parse_value(t).map_err(|e| {
                SimError::Parse(format!("PWL FILE '{}': line {} time: {e}", path.display(), n + 1))
            })?;
            let v = parse_value(v).map_err(|e| {
                SimError::Parse(format!("PWL FILE '{}': line {} value: {e}", path.display(), n + 1))
            })?;
            points.push((t, v));
        }
        let pwl = Pwl { points, repeat_from: None, repeat_count: None };
        pwl.validate().map_err(SimError::Parse)?;
        return Ok(pwl);
    }

    // Tokenize the inline form: commas/parentheses are separators, and the
    // repeat markers are ordinary words in the stream.
    let cleaned: String = after
        .chars()
        .map(|c| if matches!(c, '(' | ')' | ',') { ' ' } else { c })
        .collect();
    let tokens: Vec<&str> = cleaned.split_whitespace().collect();
    let mut points: Vec<(f64, f64)> = Vec::new();
    let mut repeat_start: Option<usize> = None;
    let mut repeat_count: Option<Option<usize>> = None;
    let mut i = 0usize;
    while i < tokens.len() {
        match tokens[i].to_ascii_lowercase().as_str() {
            "repeat" => {
                i += 1;
                let mut count = None;
                if tokens.get(i).is_some_and(|t| t.eq_ignore_ascii_case("for")) {
                    i += 1;
                }
                if tokens.get(i).is_some_and(|t| t.eq_ignore_ascii_case("forever")) {
                    i += 1;
                } else if let Some(tok) = tokens.get(i) {
                    if let Ok(n) = tok.parse::<usize>() {
                        count = Some(n);
                        i += 1;
                    }
                }
                repeat_start = Some(points.len());
                repeat_count = Some(count);
            }
            "endrepeat" => i += 1,
            _ => {
                let tok = tokens[i];
                let time = parse_value(tok).map_err(|e| {
                    SimError::Parse(format!("PWL time '{tok}': {e}"))
                })?;
                let value_tok = tokens.get(i + 1).ok_or_else(|| {
                    SimError::Parse(format!(
                        "PWL needs a value after time '{tok}' (pairs are time value)"
                    ))
                })?;
                let value = parse_value(value_tok).map_err(|e| {
                    SimError::Parse(format!("PWL value '{value_tok}': {e}"))
                })?;
                points.push((time, value));
                i += 2;
            }
        }
    }

    let repeat_from = match repeat_start {
        Some(start) if start < points.len() => Some(points[start].0),
        Some(_) => {
            return Err(SimError::Parse(
                "PWL REPEAT block contains no (time, value) pairs".into(),
            ))
        }
        None => None,
    };
    let pwl = Pwl { points, repeat_from, repeat_count };
    pwl.validate().map_err(SimError::Parse)?;
    Ok(pwl)
}

/// Join the tail of a source card from `start`, strip the function name,
/// parentheses and commas, and split into parameters.
fn parenthesised_params(rest: &[&str], name: &'static str) -> Result<Vec<String>> {
    let remainder = rest.join(" ");
    let cleaned = remainder
        .to_ascii_lowercase()
        .replace(name, "")
        .replace('(', "")
        .replace(')', "")
        .replace(',', " ");
    Ok(cleaned.split_whitespace().map(|s| s.to_string()).collect())
}

fn parse_voltage_source(
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

fn parse_current_source(
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
struct BCardSplit {
    kind: BehavioralKind,
    /// Expression as written after the `V=`/`I=` keyword (the keyword's own
    /// letter is stripped: the compiled grammar treats `V(…)` as a probe).
    expr: String,
}

/// Find the `V=`/`I=` keyword in a `B` card and rejoin the expression.
///
/// Both keyword shapes are legal: glued (`V=V(in)*2`, possibly the '=' with
/// the expression in the same token) and split (`V = V(in) * V(out)`), and
/// the expression itself may carry spaces — so everything after the keyword
/// is rejoined with single spaces. Scanning starts at token 3 (name, and two
/// nodes, come first); an expression can never contain '=' otherwise.
fn split_b_card(tokens: &[&str]) -> Result<BCardSplit> {
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

fn parse_behavioral_source(
    tokens: &[&str],
    circuit: &mut Circuit,
    plan: &HashMap<String, usize>,
    line_no: usize,
) -> Result<()> {
    let split = split_b_card(tokens)
        .map_err(|e| SimError::Parse(format!("line {line_no}: {e}")))?;
    let expr = split.expr.trim();
    if expr.is_empty() {
        return Err(SimError::Parse(format!(
            "line {line_no}: behavioral source '{}' has an empty expression",
            tokens[0]
        )));
    }
    let behavior =
        crate::behavior::parse(expr).map_err(|e| SimError::Parse(format!("line {line_no}: {e}")))?;
    let out_p = parse_node(tokens[1], circuit, plan)?;
    let out_n = parse_node(tokens[2], circuit, plan)?;
    circuit.behavioral.push(BehavioralSource {
        name: tokens[0].to_string(),
        out_p,
        out_n,
        kind: split.kind,
        expr: expr.to_string(),
        behavior,
    });
    Ok(())
}

/// Canonical text for one source waveform (the inverse of the source-card
/// grammar). PWL repeat blocks are emitted as a linear point list — the
/// waveform values are identical even though the block markers are lost.
fn waveform_text(w: &Waveform) -> String {
    match w {
        Waveform::Pulse(p) => format!(
            "PULSE({} {} {} {} {} {} {})",
            p.v1, p.v2, p.td, p.tr, p.tf, p.pw, p.per
        ),
        Waveform::Sine(s) => format!("SIN({} {} {} {} {})", s.vo, s.va, s.freq, s.td, s.theta),
        Waveform::Exp(e) => format!(
            "EXP({} {} {} {} {} {})",
            e.v1, e.v2, e.td1, e.tau1, e.td2, e.tau2
        ),
        Waveform::Pwl(p) => {
            let mut text = String::from("PWL(");
            for (i, (t, v)) in p.points.iter().enumerate() {
                if i > 0 {
                    text.push(' ');
                }
                text.push_str(&format!("{t} {v}"));
            }
            text.push(')');
            text
        }
        Waveform::Sffm(s) => format!(
            "SFFM({} {} {} {} {} {})",
            s.vo, s.va, s.fc, s.mdi, s.fs, s.td
        ),
        Waveform::Am(a) => format!("AM({} {} {} {} {})", a.va, a.vo, a.fm, a.fc, a.td),
    }
}

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

    // ── Model cards (sorted for deterministic output) ────────────────────
    let mut diode_names: Vec<&String> = circuit.diode_models.keys().collect();
    diode_names.sort();
    for name in diode_names {
        let m = &circuit.diode_models[name];
        // `RS` is deliberately not reprinted: `expand_diode_series_resistance`
        // has already materialized it as resistor arms, and printing the slot
        // a second time would make `parse(emit(…))` split it twice.
        let mut fields = format!("IS={} N={} CJO={} VJ={} M={} TT={} BV={} IBV={}",
            m.is, m.n, m.cjo, m.vj, m.m, m.tt, m.bv, m.ibv);
        if m.eg != 1.11 { fields.push_str(&format!(" EG={}", m.eg)); }
        if m.xti != 3.0 { fields.push_str(&format!(" XTI={}", m.xti)); }
        if m.kf != 0.0 { fields.push_str(&format!(" KF={} AF={}", m.kf, m.af)); }
        if m.tnom != 27.0 { fields.push_str(&format!(" TNOM={}", m.tnom)); }
        if m.nbv != 1.0 { fields.push_str(&format!(" NBV={}", m.nbv)); }
        if m.fc != 0.5 { fields.push_str(&format!(" FC={}", m.fc)); }
        out.push_str(&format!(".model {} D({})\n", name, fields));
    }
    let mut mosfet_names: Vec<&String> = circuit.mosfet_models.keys().collect();
    mosfet_names.sort();
    for name in mosfet_names {
        let m = &circuit.mosfet_models[name];
        let kind = match m.kind {
            MosfetType::Nmos => "NMOS",
            MosfetType::Pmos => "PMOS",
        };
        out.push_str(&format!(
            ".model {} {}(KP={} VTH0={} LAMBDA={} GAMMA={} PHI={}",
            name, kind, m.kp, m.vth0, m.lambda, m.gamma, m.phi
        ));
        if m.level != 1 {
            out.push_str(&format!(" LEVEL={}", m.level));
        }
        if m.tnom != 27.0 { out.push_str(&format!(" TNOM={}", m.tnom)); }
        if m.tcv != 2.0e-3 { out.push_str(&format!(" TCV={}", m.tcv)); }
        if m.bex != -1.5 { out.push_str(&format!(" BEX={}", m.bex)); }
        if let Some(tox) = m.tox { out.push_str(&format!(" TOX={tox}")); }
        if let Some(v) = m.cgdo { out.push_str(&format!(" CGDO={v}")); }
        if let Some(v) = m.cgso { out.push_str(&format!(" CGSO={v}")); }
        if let Some(v) = m.cgbo { out.push_str(&format!(" CGBO={v}")); }
        if let Some(v) = m.cj { out.push_str(&format!(" CJ={v}")); }
        if let Some(v) = m.cjsw { out.push_str(&format!(" CJSW={v}")); }
        if let Some(v) = m.mj { out.push_str(&format!(" MJ={v}")); }
        if let Some(v) = m.mjsw { out.push_str(&format!(" MJSW={v}")); }
        if let Some(v) = m.pb { out.push_str(&format!(" PB={v}")); }
        if let Some(v) = m.fc { out.push_str(&format!(" FC={v}")); }
        if let Some(v) = m.nsub { out.push_str(&format!(" NSUB={v}")); }
        if let Some(v) = m.nss { out.push_str(&format!(" NSS={v}")); }
        if let Some(v) = m.tpg { out.push_str(&format!(" TPG={v}")); }
        out.push_str(")\n");
    }
    // Passive model cards. Parasitic slots (`RSER`/`LSER`/`CPAR`/`RPAR`) are
    // deliberately not reprinted: `expand_parasitics` has already materialized
    // them as element arms, and printing the slots a second time would make
    // `parse(emit(…))` split them twice.
    let mut passive_names: Vec<&String> = circuit
        .resistor_models
        .keys()
        .chain(circuit.capacitor_models.keys())
        .chain(circuit.inductor_models.keys())
        .collect();
    passive_names.sort();
    passive_names.dedup();
    for name in passive_names {
        let (type_word, m) = if let Some(m) = circuit.resistor_models.get(name) {
            ("R", m)
        } else if let Some(m) = circuit.capacitor_models.get(name) {
            ("C", m)
        } else {
            ("L", circuit.inductor_models.get(name).expect("key from a passive map"))
        };
        out.push_str(&format!(".model {} {}(", name, type_word));
        let mut fields = Vec::new();
        if let Some(v) = m.value {
            fields.push(format!("{}={v}", type_word));
        }
        if m.tc1 != 0.0 {
            fields.push(format!("TC1={}", m.tc1));
        }
        if m.tc2 != 0.0 {
            fields.push(format!("TC2={}", m.tc2));
        }
        out.push_str(&fields.join(" "));
        out.push_str(")\n");
    }
    // BJT model cards. `VAF`/`VAR` (and `IKF`/`IKR`) print only when finite —
    // the infinite defaults would emit `inf`, which `parse_value` rejects —
    // and `XTB` joins only when it moves β at all.
    let mut bjt_names: Vec<&String> = circuit.bjt_models.keys().collect();
    bjt_names.sort();
    for name in bjt_names {
        let m = &circuit.bjt_models[name];
        let kind = match m.kind {
            BjtType::Npn => "NPN",
            BjtType::Pnp => "PNP",
        };
        let mut fields = vec![
            format!("IS={}", m.is),
            format!("BF={}", m.bf),
            format!("BR={}", m.br),
            format!("NF={}", m.nf),
            format!("NR={}", m.nr),
            format!("ISE={}", m.ise),
            format!("NE={}", m.ne),
            format!("ISC={}", m.isc),
            format!("NC={}", m.nc),
            // `RB`/`RC`/`RE` are deliberately not reprinted: the BJT
            // series-resistance expansion has already materialized them as
            // collector/base/emitter resistor arms, and printing the slots a
            // second time would split them twice.
        ];
        for (label, val) in [("VAF", m.vaf), ("VAR", m.var), ("IKF", m.ikf), ("IKR", m.ikr)] {
            if val.is_finite() {
                fields.push(format!("{label}={val}"));
            }
        }
        fields.extend([
            format!("CJE={}", m.cje),
            format!("VJE={}", m.vje),
            format!("MJE={}", m.mje),
            format!("CJC={}", m.cjc),
            format!("VJC={}", m.vjc),
            format!("MJC={}", m.mjc),
            format!("TF={}", m.tf),
            format!("TR={}", m.tr),
            format!("XTI={}", m.xti),
        ]);
        if m.xtb != 0.0 {
            fields.push(format!("XTB={}", m.xtb));
        }
        fields.extend([
            format!("EG={}", m.eg),
            format!("TNOM={}", m.tnom),
            format!("KF={}", m.kf),
            format!("AF={}", m.af),
        ]);
        out.push_str(&format!(".model {} {}({})\n", name, kind, fields.join(" ")));
    }
    // JFET model cards.
    let mut jfet_names: Vec<&String> = circuit.jfet_models.keys().collect();
    jfet_names.sort();
    for name in jfet_names {
        let m = &circuit.jfet_models[name];
        let kind = match m.kind {
            JfetType::Njf => "NJF",
            JfetType::Pjf => "PJF",
        };
        out.push_str(&format!(
            // `RD`/`RS` are deliberately not reprinted: the series-resistance
            // expansion has already materialized them as resistor arms, and
            // printing the slots a second time would split them twice.
            ".model {} {}(VTO={} BETA={} LAMBDA={} B={} IS={} CGS={} CGD={} PB={} FC={} XTI={} EG={} TNOM={} KF={} AF={})\n",
            name, kind, m.vto, m.beta, m.lambda, m.b, m.is, m.cgs, m.cgd, m.pb,
            m.fc, m.xti, m.eg, m.tnom, m.kf, m.af
        ));
    }
    // Switch model cards.
    let mut switch_names: Vec<&String> = circuit.switch_models.keys().collect();
    switch_names.sort();
    for name in switch_names {
        let m = &circuit.switch_models[name];
        out.push_str(&format!(
            ".model {} SW(RON={} ROFF={} VT={} VH={})\n",
            name, m.ron, m.roff, m.vt, m.vh
        ));
    }
    // Device families without an element letter yet: emitted with their
    // stored numeric parameters so a re-parse reproduces the card.
    let mut generic_names: Vec<&String> = circuit.generic_models.keys().collect();
    generic_names.sort();
    for name in generic_names {
        let m = &circuit.generic_models[name];
        let mut params: Vec<String> = Vec::new();
        if let Some(level) = m.level {
            params.push(format!("LEVEL={}", level));
        }
        for (key, value) in &m.params {
            params.push(format!("{}={}", key, value));
        }
        out.push_str(&format!(
            ".model {} {}({})\n",
            name,
            m.spice_type.to_ascii_uppercase(),
            params.join(" ")
        ));
    }

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
    for b in &circuit.behavioral {
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

    // ── Analysis commands ────────────────────────────────────────────────
    if let Some(dc) = &analysis.dc {
        let mut line = String::from(".dc");
        for spec in &dc.sweeps {
            match &spec.values {
                DcSweepValues::Linear { start, stop, step } => {
                    line.push_str(&format!(" {} {} {} {}", spec.target, start, stop, step));
                }
                DcSweepValues::List(values) => {
                    line.push_str(&format!(" {} LIST", spec.target));
                    for v in values {
                        line.push_str(&format!(" {v}"));
                    }
                }
            }
        }
        out.push_str(&line);
        out.push('\n');
    }
    if analysis.dc_op {
        out.push_str(".op\n");
    }
    if let (Some(step), Some(stop)) = (analysis.tran_step, analysis.tran_stop) {
        let mut line = format!(".tran {} {}", step, stop);
        if analysis.tran_start != 0.0 {
            line.push_str(&format!(" {}", analysis.tran_start));
        }
        if let Some(tmax) = analysis.tran_maxstep {
            line.push_str(&format!(" {}", tmax));
        }
        if analysis.tran_uic {
            line.push_str(" UIC");
        }
        if analysis.tran_startup {
            line.push_str(" STARTUP");
        }
        out.push_str(&line);
        out.push('\n');
    }
    if let Some(ac) = &analysis.ac {
        let scale = match ac.scale {
            AcScale::Dec => "DEC",
            AcScale::Oct => "OCT",
            AcScale::Lin => "LIN",
        };
        let src = match ac.stimulus_source {
            Some(i) => format!(" {}", i),
            None => String::new(),
        };
        out.push_str(&format!(".ac {} {} {} {}{}\n", scale, ac.points, ac.f_start, ac.f_stop, src));
    }
    if let Some(tf) = &analysis.tf {
        let output = match &tf.output {
            TfOutput::Voltage { node, reference } => match reference {
                Some(reference) => format!("V({node},{reference})"),
                None => format!("V({node})"),
            },
            TfOutput::Current(name) => format!("I({name})"),
        };
        out.push_str(&format!(".tf {} {}\n", output, tf.input));
    }
    let mut options = format!(
        ".options method={} reltol={} abstol={} vntol={}",
        match analysis.method {
            crate::circuit::IntegrationMethod::Trapezoidal => "trap",
            crate::circuit::IntegrationMethod::Gear1 => "gear1",
            crate::circuit::IntegrationMethod::Gear2 => "gear2",
        },
        analysis.reltol,
        analysis.abstol,
        analysis.vntol
    );
    // Convergence knobs re-print only when they differ from the engine
    // defaults, so an uncustomized deck's emitted options line is unchanged.
    if analysis.gmin != 1e-12 {
        options.push_str(&format!(" gmin={}", analysis.gmin));
    }
    if analysis.gminsteps != 10 {
        options.push_str(&format!(" gminsteps={}", analysis.gminsteps));
    }
    if analysis.srcsteps != 10 {
        options.push_str(&format!(" srcsteps={}", analysis.srcsteps));
    }
    if analysis.itl1 != 100 {
        options.push_str(&format!(" itl1={}", analysis.itl1));
    }
    if analysis.itl4 != 100 {
        options.push_str(&format!(" itl4={}", analysis.itl4));
    }
    out.push_str(&options);
    out.push('\n');
    if !analysis.temperatures.is_empty() {
        let values: Vec<String> = analysis.temperatures.iter().map(|t| format!("{t}")).collect();
        out.push_str(&format!(".temp {}\n", values.join(" ")));
    }
    if let Some(noise) = &analysis.noise {
        let output = match &noise.output {
            TfOutput::Voltage { node, reference } => match reference {
                Some(reference) => format!("V({node},{reference})"),
                None => format!("V({node})"),
            },
            TfOutput::Current(name) => format!("I({name})"),
        };
        let scale = match noise.scale {
            AcScale::Dec => "DEC",
            AcScale::Oct => "OCT",
            AcScale::Lin => "LIN",
        };
        out.push_str(&format!(
            ".noise {} {} {} {} {} {}\n",
            output, noise.input, scale, noise.points, noise.f_start, noise.f_stop
        ));
    }
    if let Some(four) = &analysis.four {
        out.push_str(&format!(".four {} {}\n", four.frequency, four.outputs.join(" ")));
    }
    for step in &analysis.steps {
        match &step.values {
            StepValues::Linear { start, stop, step: inc } => {
                out.push_str(&format!(".step param {} {} {} {}\n", step.target, start, stop, inc));
            }
            StepValues::Decade { start, stop, points } => {
                out.push_str(&format!(".step dec param {} {} {} {}\n", step.target, start, stop, points));
            }
            StepValues::Octave { start, stop, points } => {
                out.push_str(&format!(".step oct param {} {} {} {}\n", step.target, start, stop, points));
            }
            StepValues::List(values) => {
                let text: Vec<String> = values.iter().map(|v| format!("{v}")).collect();
                out.push_str(&format!(".step param {} list {}\n", step.target, text.join(" ")));
            }
        }
    }
    // Initial conditions: node names are stored as written, so these cards
    // reprint verbatim (the entries' node text resolves again on re-parse).
    for e in &analysis.nodeset {
        out.push_str(&format!(".nodeset V({})={}\n", e.node, e.value));
    }
    for e in &analysis.ic {
        out.push_str(&format!(".ic V({})={}\n", e.node, e.value));
    }
    out.push_str(".end\n");
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use approx::assert_relative_eq;

    /// `parse(emit(x))` preserves the language-relevant facts of a
    /// representative netlist: elements, model cards, waveforms, commands.
    #[test]
    fn emit_round_trips_through_parse() {
        let src = "* roundtrip\n\
                   .model dino D(IS=1e-14 N=1.05)\n\
                   .model nh NMOS(KP=2e-4 VTH0=0.7 LAMBDA=0.02 GAMMA=0.5 PHI=0.6)\n\
                   .model jn JFET(NJF VTO=-2 BETA=1e-4 IS=1e-14 CGS=2p CGD=1p PB=1)\n\
                   .model qp NPN(IS=1e-14 BF=150 NF=1.02 IKF=5m IKR=2m XTB=0.5\n\
                   + VAF=50 VAR=2)\n\
                   R1 1 2 1k\n\
                   C1 1 2 1u\n\
                   L1 2 0 1m\n\
                   D1 1 0 dino\n\
                   M1 2 1 0 0 nh W=2u L=0.18u\n\
                   J1 2 1 0 jn AREA=2\n\
                   Q1 2 1 0 qp\n\
                   VS 1 0 DC 5 AC 1 30 PULSE(0 5 1u 2u 2u 5u 10u)\n\
                   IS 2 0 DC 1m AC 1u PULSE(0 1m 1u 2u 2u 5u 10u)\n\
                   B1 2 0 V=V(1)*2\n\
                   .op\n\
                   .tran 1u 5m\n\
                   .ac DEC 10 1 10k\n\
                   .end\n";
        let first = parse(src).expect("first parse");
        let text = emit(&first);
        let second = parse(&text).expect("re-parse of emitted text");

        assert_eq!(second.circuit.nodes, first.circuit.nodes);
        assert_eq!(second.circuit.resistors.len(), 1);
        assert_eq!(second.circuit.capacitors.len(), 1);
        assert_eq!(second.circuit.inductors.len(), 1);
        assert_eq!(second.circuit.diodes.len(), 1);
        assert_eq!(second.circuit.mosfets.len(), 1);
        assert_eq!(second.circuit.jfets.len(), 1);
        assert_eq!(second.circuit.bjts.len(), 1);
        assert_eq!(second.circuit.voltage_sources.len(), 1);
        assert_eq!(second.circuit.current_sources.len(), 1);
        assert_eq!(second.circuit.behavioral.len(), 1);

        let q = &second.circuit.bjts[0];
        assert_eq!(q.model, "qp");
        let qm = &second.circuit.bjt_models["qp"];
        assert!(matches!(qm.kind, crate::circuit::BjtType::Npn));
        assert_eq!(qm.is, 1e-14);
        assert_eq!(qm.bf, 150.0);
        assert_eq!(qm.nf, 1.02);
        assert_eq!(qm.ikf, 5e-3);
        assert_eq!(qm.ikr, 2e-3);
        assert_eq!(qm.xtb, 0.5);
        assert_eq!(qm.vaf, 50.0);
        assert_eq!(qm.var, 2.0);
        // The infinite defaults survive an emit that skips them.
        let bare = parse("Q2 1 0 2 qb\n.model qb PNP(RB=10)\n.op\n.end\n").unwrap();
        assert_eq!(bare.circuit.bjt_models["qb"].vaf, f64::INFINITY);
        assert_eq!(bare.circuit.bjt_models["qb"].ikf, f64::INFINITY);
        assert_eq!(bare.circuit.bjt_models["qb"].xtb, 0.0);

        let j = &second.circuit.jfets[0];
        assert_eq!(j.model, "jn");
        assert_eq!(j.area, 2.0);
        let jm = &second.circuit.jfet_models["jn"];
        assert!(matches!(jm.kind, JfetType::Njf));
        assert_eq!(jm.vto, -2.0);
        assert_eq!(jm.beta, 1e-4);
        assert_eq!(jm.cgs, 2e-12);
        assert_eq!(jm.cgd, 1e-12);

        let bs = &second.circuit.behavioral[0];
        assert_eq!(bs.kind, BehavioralKind::Voltage);
        assert_eq!(bs.expr, "V(1)*2");
        assert_eq!(bs.behavior.probes.len(), 1);

        let m = &second.circuit.mosfets[0];
        assert_eq!(m.model, "nh");
        assert_eq!(m.w, parse_value("2u").unwrap());
        assert_eq!(m.l, parse_value("0.18u").unwrap());

        let v = &second.circuit.voltage_sources[0];
        assert_eq!(v.voltage, 5.0);
        let ac = v.ac.as_ref().unwrap();
        assert_eq!(ac.mag, 1.0);
        assert_eq!(ac.phase, 30.0);
        let Some(Waveform::Pulse(pulse)) = v.waveform.as_ref() else {
            panic!("expected a PULSE waveform");
        };
        assert_eq!(pulse.v1, 0.0);
        assert_eq!(pulse.v2, 5.0);
        // 10×1e-6 is not bit-identical to the 10e-6 literal, so compare against
        // what the parser actually produced: the round trip must be exact.
        assert_eq!(pulse.per, parse_value("10u").unwrap());

        let i_src = &second.circuit.current_sources[0];
        assert_eq!(i_src.current, 1e-3);
        assert_eq!(i_src.ac.as_ref().unwrap().mag, 1e-6);
        let Some(Waveform::Pulse(pulse)) = i_src.waveform.as_ref() else {
            panic!("expected a PULSE waveform");
        };
        assert_eq!(pulse.pw, parse_value("5u").unwrap());

        let a = second.analysis.ac.as_ref().unwrap();
        assert!(matches!(a.scale, AcScale::Dec));
        assert_eq!(a.points, 10);
        assert_eq!(a.f_start, 1.0);
        assert_eq!(a.f_stop, parse_value("10k").unwrap());
        assert_eq!(second.analysis.tran_step, Some(parse_value("1u").unwrap()));
        assert_eq!(second.analysis.tran_stop, Some(parse_value("5m").unwrap()));
        assert!(second.analysis.dc_op);
    }

    /// TODO §3: the `.options` convergence knobs survive `parse(emit(parse))`
    /// — they re-emit only when they differ from the engine defaults (so an
    /// untouched deck's emitted text is unchanged) and the re-parse recovers
    /// the very same field values. Compared as values, not text: f64 Display
    /// writes 1e-10 as `0.0000000001` and the re-parse must read it back
    /// exactly.
    #[test]
    fn options_convergence_knobs_round_trip() {
        let netlist = parse(
            "* stepped convergence knobs\n\
             V1 a 0 DC 1\n\
             R1 a b 1k\n\
             .options gmin=1e-10 gminsteps=12 srcsteps=3 itl1=200 itl4=5\n\
             .op\n\
             .end\n",
        )
        .unwrap();
        assert_eq!(netlist.analysis.gmin, 1e-10);
        assert_eq!(netlist.analysis.gminsteps, 12);
        assert_eq!(netlist.analysis.srcsteps, 3);
        assert_eq!(netlist.analysis.itl1, 200);
        assert_eq!(netlist.analysis.itl4, 5);

        let text = emit(&netlist);
        for knob in ["gmin=", "gminsteps=", "srcsteps=", "itl1=", "itl4="] {
            assert!(text.contains(knob), "emit lost {knob}: {text}");
        }
        let reparsed = parse(&text).unwrap();
        assert_eq!(reparsed.analysis.gmin, netlist.analysis.gmin);
        assert_eq!(reparsed.analysis.gminsteps, netlist.analysis.gminsteps);
        assert_eq!(reparsed.analysis.srcsteps, netlist.analysis.srcsteps);
        assert_eq!(reparsed.analysis.itl1, netlist.analysis.itl1);
        assert_eq!(reparsed.analysis.itl4, netlist.analysis.itl4);

        // Default knobs stay silent in the emitted options line.
        let plain = parse("* plain deck\n.op\n.end\n").unwrap();
        let text = emit(&plain);
        for knob in ["gmin=", "gminsteps=", "srcsteps=", "itl1=", "itl4="] {
            assert!(!text.contains(knob), "default deck emitted {knob}: {text}");
        }
    }

    /// `*` only comments out a line from its start: mid-line `*` is data
    /// (the multiplication inside a quoted or braced expression).
    #[test]
    fn star_comments_are_line_initial_only() {
        let netlist = parse(
            "* leading comment\n\
             V1 1 0 '2*1.5'\n\
             R1 1 2 '{ 1k * 2 }'\n\
             .op\n\
             .end\n",
        )
        .unwrap();
        assert_eq!(netlist.circuit.voltage_sources[0].voltage, 3.0);
        assert_eq!(netlist.circuit.resistors[0].resistance, 2000.0);

        // `;` still truncates inline, outside quotes.
        let netlist = parse("V1 1 0 5 ; inline note\n.op\n.end\n").unwrap();
        assert_eq!(netlist.circuit.voltage_sources[0].voltage, 5.0);
        // …but never inside quoted text.
        let netlist = parse(".param note=\"five; six\"\n.op\n.end\n").unwrap();
        assert_eq!(
            netlist.params["note"],
            ParamValue::Str("five; six".to_string())
        );
    }

    #[test]
    fn leading_plus_continues_the_previous_line() {
        let netlist = parse(
            ".model mod1 NMOS(KP=20e-6 VTH0=0.7\n\
             + LAMBDA=0.02 GAMMA=0.5)\n\
             .tran 1u\n\
             + 5m\n\
             V1 1 0 DC 5\n\
             R1 1 2 1k\n\
             M1 2 1 0 0 mod1\n\
             .op\n\
             .end\n",
        )
        .unwrap();
        let m = &netlist.circuit.mosfet_models["mod1"];
        assert_eq!(m.kp, 20e-6);
        assert_eq!(m.lambda, 0.02);
        assert_eq!(netlist.analysis.tran_stop, Some(5e-3));
    }

    #[test]
    fn continuation_without_a_predecessor_is_an_error() {
        assert!(parse("+ 5m\n.end\n").is_err());
    }

    #[test]
    fn alphanumeric_nodes_are_resolved_case_insensitively() {
        let netlist = parse(
            "V1 IN 0 DC 5\n\
             R1 in OUT 1k\n\
             R2 out gnd 1k\n\
             .op\n\
             .end\n",
        )
        .unwrap();
        let circuit = &netlist.circuit;
        // Ground: "0" and "gnd" agree.
        assert_eq!(circuit.node_name(0), "0");
        let r1_n1 = circuit.resistors[0].n1.0;
        let r1_n2 = circuit.resistors[0].n2.0;
        let r2_n1 = circuit.resistors[1].n1.0;
        let r2_n2 = circuit.resistors[1].n2.0;
        // V1's "IN" and R1's "in" are the same net.
        assert_eq!(circuit.voltage_sources[0].n1.0, r1_n1);
        assert_eq!(r1_n2, r2_n1); // out == OUT
        assert_eq!(r2_n2, 0); // gnd == ground
        // Canonical names keep the first spelling seen.
        let names: Vec<String> = circuit.node_names.clone();
        assert!(names.contains(&"IN".to_string()));
        assert!(names.contains(&"OUT".to_string()));
        // Emitted text keeps the names.
        assert!(emit(&netlist).contains("R1 IN OUT"));
    }

    #[test]
    fn mixed_numeric_and_named_nodes_compact_with_names() {
        let netlist = parse(
            "V1 in 0 DC 5\n\
             R1 in 5 1k\n\
             R2 5 out 1k\n\
             R3 6 0 1k\n\
             .op\n\
             .end\n",
        )
        .unwrap();
        let circuit = &netlist.circuit;
        // The deck uses {0, 5, 6, in, out}; `parse` compacts ids to 0..4 and
        // keeps each node's original spelling in `node_names` (numeric ids
        // included), so the two address spaces can never collide.
        assert_eq!(circuit.resistors[0].n2.0, 1); // numeric "5"
        assert_eq!(circuit.resistors[2].n1.0, 2); // numeric "6"
        assert_eq!(circuit.resistors[0].n1.0, circuit.voltage_sources[0].n1.0);
        assert_eq!(circuit.resistors[1].n1.0, 1); // R2 starts on "5"
        let in_id = circuit.voltage_sources[0].n1.0;
        let out_id = circuit.resistors[1].n2.0;
        assert_eq!(in_id, 3);
        assert_eq!(out_id, 4);
        assert_eq!(circuit.resistors[2].n2.0, 0); // gnd == ground
        assert_eq!(circuit.node_names[1], "5");
        assert_eq!(circuit.node_names[2], "6");
        assert_eq!(circuit.node_names[3], "in");
        assert_eq!(circuit.node_names[4], "out");
        // The spacing one would exploit for a collision is gone: ids are dense.
        assert_eq!(circuit.nodes, 5);
    }

    #[test]
    fn parameter_language_flows_through_elements() {
        let netlist = parse(
            ".param rl=1k gain=3\n\
             .func scaled(x) { x * {gain} }\n\
             R1 in out {rl}\n\
             V1 in 0 '4 + {gain}'\n\
             R2 out 0 {scaled({rl})}\n\
             .op\n\
             .end\n",
        )
        .unwrap();
        let circuit = &netlist.circuit;
        assert_eq!(circuit.resistors[0].resistance, 1000.0);
        assert_eq!(circuit.voltage_sources[0].voltage, 7.0);
        assert_eq!(circuit.resistors[1].resistance, 3000.0); // function of a parameter
    }

    #[test]
    fn dc_progresses_from_trap_to_real_command() {
        // Plain `.dc` with no arguments still degrades to an operating point…
        let bare = parse(".dc\n.end\n").unwrap();
        assert!(bare.analysis.dc_op);
        assert!(bare.analysis.dc.is_none());
        // …but arguments are stored, never silently discarded.
        let linear = parse(".dc V1 0 5 1\n.end\n").unwrap();
        let cmd = linear.analysis.dc.as_ref().unwrap();
        assert_eq!(cmd.sweeps.len(), 1);
        assert_eq!(cmd.sweeps[0].target, "V1");
        assert!(matches!(
            cmd.sweeps[0].values,
            DcSweepValues::Linear { start: 0.0, stop: 5.0, step: 1.0 }
        ));

        let both = parse(".dc V1 0 5 1 V2 LIST 0 2.5 5\n.end\n").unwrap();
        let cmd = both.analysis.dc.as_ref().unwrap();
        assert_eq!(cmd.sweeps.len(), 2);
        assert!(matches!(
            cmd.sweeps[1].values,
            DcSweepValues::List(ref v) if v == &vec![0.0, 2.5, 5.0]
        ));
        // Round trip through emit keeps the command.
        assert!(emit(&both).contains(".dc V1 0 5 1 V2 LIST 0 2.5 5"));
    }

    #[test]
    fn malformed_dc_arguments_are_loud_errors() {
        assert!(parse(".dc V1 0 5\n.end\n").is_err()); // missing increment
        assert!(parse(".dc V1 0 5 0\n.end\n").is_err()); // zero increment
        assert!(parse(".dc V1 5 0 1\n.end\n").is_err()); // marches backwards
        assert!(parse(".dc V1 LIST\n.end\n").is_err()); // empty list
        assert!(parse(".dc V1 0 5 1 I1 0 1m\n.end\n").is_err()); // truncated second sweep
    }

    #[test]
    fn current_source_cards_parse_like_voltage_cards() {
        let netlist = parse(
            "I1 1 0 5m\n\
             I2 2 0 DC 1m AC 10u 45\n\
             I3 3 0 PULSE(0 5m 1u 1n 1n 5u 10u)\n\
             I4 4 0 SIN(0 2m 1k)\n\
             .op\n\
             .end\n",
        )
        .unwrap();
        let srcs = &netlist.circuit.current_sources;
        assert_eq!(srcs.len(), 4);
        assert_eq!(srcs[0].current, 5e-3);
        assert_eq!(srcs[1].ac.as_ref().unwrap().phase, 45.0);
        // `5u` sits one ulp from the literal `5e-6`; compare against the
        // shared value parser, which is what the card was read through.
        let Some(Waveform::Pulse(pulse)) = srcs[2].waveform.as_ref() else {
            panic!("expected a PULSE waveform");
        };
        assert_eq!(pulse.pw, crate::units::parse_value("5u").unwrap());
        let Some(Waveform::Sine(sine)) = srcs[3].waveform.as_ref() else {
            panic!("expected a SIN waveform");
        };
        assert_eq!(sine.freq, 1e3);
    }

    // ─── Subcircuit hierarchy ─────────────────────────────────────────────────

    /// `X` instantiation: ports bind to caller nets, internal nodes get the
    /// instance path (`X1.mid`), element names carry the path too, and body
    /// `.param` values evaluate per instance.
    #[test]
    fn subckt_expansion_binds_ports_and_mangles_internal_nets() {
        let netlist = parse(
            ".subckt rdiv a b\n\
             .param rr=1k\n\
             R1 a mid {rr}\n\
             R2 mid b {rr}\n\
             .ends\n\
             V1 in 0 DC 5\n\
             Rload out 0 1k\n\
             X1 in out rdiv rr=2k\n\
             X2 out 0 rdiv\n\
             .op\n\
             .end\n",
        )
        .unwrap();
        let circuit = &netlist.circuit;

        let names: Vec<String> = circuit.resistors.iter().map(|r| r.name.clone()).collect();
        assert_eq!(
            names,
            vec!["Rload", "X1.R1", "X1.R2", "X2.R1", "X2.R2"]
        );

        // Values: the instance override beats the body default; the default
        // stays for the instance that did not assign.
        let values: Vec<f64> = circuit.resistors.iter().map(|r| r.resistance).collect();
        assert_eq!(
            values,
            vec![
                1000.0,
                2000.0,
                2000.0,
                1000.0,
                1000.0,
            ]
        );

        // Internal nets are distinct per instance; port bindings match the
        // caller nets.
        let node_by_name = |name: &str| {
            circuit
                .node_names
                .iter()
                .position(|n| n == name)
                .unwrap_or_else(|| panic!("node '{}' missing", name))
        };
        let in_id = node_by_name("in");
        let out_id = node_by_name("out");
        assert_eq!(node_by_name("X1.mid"), 3);
        assert_eq!(node_by_name("X2.mid"), 4);
        // Deck order: Rload comes first, so it is resistors[0].
        assert_eq!(circuit.resistors[0].n1.0, out_id); // Rload's "out"
        assert_eq!(circuit.resistors[0].n2.0, 0); // ground
        assert_eq!(circuit.resistors[1].n1.0, in_id); // X1.R1's "a"
        assert_eq!(circuit.resistors[1].n2.0, 3); // X1.mid
        assert_eq!(circuit.resistors[2].n2.0, out_id); // X1.R2's "b"
        assert_eq!(circuit.resistors[4].n2.0, 0); // X2.R2's "b" -> ground
    }

    /// Fields may be separated by tabs just as by spaces.
    #[test]
    fn tabs_act_as_field_separators() {
        let netlist = parse("V1\tin\t0\tDC\t5\nR1\tin\tout\t1k\n.op\n.end\n").unwrap();
        let circuit = &netlist.circuit;
        assert_eq!(circuit.resistors[0].resistance, 1000.0);
        let names: Vec<String> = circuit.node_names.clone();
        assert!(names.iter().any(|n| n == "in"));
        assert!(names.iter().any(|n| n == "out"));
    }

    /// `.subckt` definitions may appear after the instances that use them —
    /// the definitions are lifted out before flattening, in deck order.
    #[test]
    fn subckt_references_resolve_through_forward_definitions() {
        let netlist = parse(
            "V1 in 0 5\n\
             X1 in out rdiv\n\
             Rload out 0 1k\n\
             .subckt rdiv a b\n\
             R1 a mid 1k\n\
             R2 mid b 1k\n\
             .ends\n\
             .op\n\
             .end\n",
        )
        .unwrap();
        assert_eq!(netlist.circuit.resistors.len(), 3);
    }

    /// The instance line wins over a body `.param` card re-assigning the
    /// same name (the reference tools' precedence for `PARAMS=` values),
    /// while a body card defining *new* names still evaluates — and may
    /// reference instance assignments.
    #[test]
    fn subckt_instance_overrides_beat_body_param_cards() {
        let netlist = parse(
            ".subckt rdiv a b\n\
             .param rr=1000.0\n\
             .param rr2={rr * 2}\n\
             R1 a mid {rr}\n\
             R2 mid b {rr2}\n\
             .ends\n\
             V1 in 0 DC 5\n\
             Rload out 0 1k\n\
             X1 in out rdiv rr=2k\n\
             X2 out 0 rdiv\n\
             .op\n\
             .end\n",
        )
        .unwrap();
        // Key on element names (deck order puts Rload first).
        let by_name = |name: &str| {
            netlist
                .circuit
                .resistors
                .iter()
                .find(|r| r.name == name)
                .unwrap_or_else(|| panic!("no resistor '{name}'"))
                .resistance
        };
        // X1: instance rr=2k wins over the body card; rr2 sees it.
        assert_eq!(by_name("X1.R1"), 2000.0);
        assert_eq!(by_name("X1.R2"), 4000.0);
        // X2: no instance assignment, body card applies as written.
        assert_eq!(by_name("X2.R1"), 1000.0);
        assert_eq!(by_name("X2.R2"), 2000.0);
        // The caller-side load is untouched by the subckt environment.
        assert_eq!(by_name("Rload"), 1000.0);
    }

    /// A body may define new subckts and share nets through `.global`.
    #[test]
    fn nested_subckts_and_global_nets_flatten_together() {
        let netlist = parse(
            ".global rail\n\
             V1 rail 0 DC 5\n\
             X1 rail 0 stage\n\
             .subckt stage a c\n\
             .subckt inner r s\n\
             Rmatch r s 10\n\
             .ends\n\
             Xc a c inner\n\
             Rpath a mid 1k\n\
             Rmid mid c 2k\n\
             .ends\n\
             .op\n\
             .end\n",
        )
        .unwrap();
        let circuit = &netlist.circuit;
        // The nested definition lives in the same global namespace: `X1.Xc`
        // is built from `inner` even though it is declared inside `stage`.
        let names: Vec<String> = circuit.resistors.iter().map(|r| r.name.clone()).collect();
        assert_eq!(names, vec!["X1.Xc.Rmatch", "X1.Rpath", "X1.Rmid"]);
        // `.global rail` inside the deck keeps its name everywhere.
        let node_by_name = |name: &str| {
            circuit
                .node_names
                .iter()
                .position(|n| n == name)
                .unwrap_or_else(|| panic!("node '{name}' missing"))
        };
        let rail_id = node_by_name("rail");
        assert_eq!(circuit.resistors[0].n1.0, rail_id);
    }

    #[test]
    fn subckt_structural_mistakes_are_loud_errors() {
        // Port-count mismatch.
        let deck = ".subckt rdiv a b\nR1 a b 1k\n.ends\nV1 in 0 5\nX1 in 0 out rdiv\n.op\n.end\n";
        assert!(parse(deck).unwrap_err().to_string().contains("needs 2 port(s)"));

        // Unknown definition.
        let deck = "V1 in 0 5\nX1 in 0 nonexistent\n.op\n.end\n";
        assert!(parse(deck)
            .unwrap_err()
            .to_string()
            .contains("unknown .subckt"));

        // Missing .ends.
        let deck = ".subckt rdiv a b\nR1 a b 1k\nV1 in 0 5\n.op\n.end\n";
        assert!(parse(deck)
            .unwrap_err()
            .to_string()
            .contains("missing its .ends"));

        // Stray .ends.
        let deck = "R1 1 0 1k\n.ends\n.op\n.end\n";
        assert!(parse(deck)
            .unwrap_err()
            .to_string()
            .contains(".ends outside any .subckt"));
    }

    /// A body card may only reference parameters defined at or above the
    /// instance — a top-level `.param` later in the deck order is not
    /// visible and fails loudly with the missing name.
    #[test]
    fn subckt_body_uses_late_top_level_param_is_loud() {
        let deck = "V1 in 0 5\n\
                    Rload out 0 1k\n\
                    X1 in out rdiv\n\
                    .param gain=4\n\
                    .subckt rdiv a b\n\
                    .param r = '1k * gain'\n\
                    R1 a b {r}\n\
                    .ends\n\
                    .op\n\
                    .end\n";
        let err = parse(deck).unwrap_err().to_string();
        assert!(err.contains("gain"), "error was: {err}");
    }

    // ─── Library files ────────────────────────────────────────────────────────

    /// A unique temp file for library tests — `tempfile` is not a
    /// dependency, so each test namespaces by name.
    fn write_temp(label: &str, content: &str) -> std::path::PathBuf {
        let mut path = std::env::temp_dir();
        path.push(format!("rspice_test_{}_{}", std::process::id(), label));
        std::fs::write(&path, content).expect("writing the temp library file");
        path
    }

    /// `.include` resolves relative to the deck's own directory first, then
    /// through the configured search paths.
    #[test]
    fn include_resolves_deck_directory_then_search_paths() {
        let dir = std::env::temp_dir().join(format!("rspice_test_{}_dir", std::process::id()));
        std::fs::create_dir_all(&dir).expect("temp deck directory");
        let sub_dir = dir.join("libs");
        std::fs::create_dir_all(&sub_dir).expect("temp lib directory");

        std::fs::write(dir.join("sub.cir"), "R1 in out 1k\n").unwrap();
        std::fs::write(sub_dir.join("far.cir"), "R2 out 0 2k\n").unwrap();

        let deck = format!(
            "V1 in 0 5\n\
             .include {sub}\n\
             .include far.cir\n\
             .op\n\
             .end\n",
            sub = dir.join("sub.cir").display(),
        );
        let options = ParseOptions {
            deck_dir: Some(dir.clone()),
            search_paths: vec![sub_dir],
        };
        let netlist = parse_with_options(&deck, &options).unwrap();
        let circuit = &netlist.circuit;
        assert_eq!(circuit.resistors.len(), 2);
        assert_eq!(circuit.resistors[1].resistance, 2000.0);
        assert!(netlist.warnings.is_empty(), "{:?}", netlist.warnings);

        let _ = std::fs::remove_dir_all(&dir);
    }

    /// A whole-file `.include` of a `.lib` file merges every section (the
    /// boundaries stay invisible), while `.lib <file> <section>` selects one.
    #[test]
    fn lib_sections_select_between_whole_file_and_one_part() {
        let lib = write_temp(
            "sections.lib",
            "* two model sections\n\
             .lib sec1\n\
             .model m1 D(IS=1e-12)\n\
             .endl sec1\n\
             \n\
             .lib sec2\n\
             .model m2 D(IS=2e-12)\n\
             .endl sec2\n",
        );
        let dir = lib.parent().unwrap().to_path_buf();
        let options = ParseOptions {
            deck_dir: Some(dir),
            search_paths: Vec::new(),
        };

        // Whole file: both sections merge into the deck.
        let deck = format!(
            ".include {}\nV1 1 0 1\nD1 1 0 m1\n.op\n.end\n",
            lib.display()
        );
        let netlist = parse_with_options(&deck, &options).unwrap();
        assert_eq!(netlist.circuit.diode_models.len(), 2);
        assert!(
            netlist.warnings.is_empty(),
            "boundaries must not surface as warnings: {:?}",
            netlist.warnings
        );

        // Named section: only sec2 arrives.
        let deck = format!(
            ".lib {} sec2\nV1 1 0 1\nD1 1 0 m2\n.op\n.end\n",
            lib.display()
        );
        let netlist = parse_with_options(&deck, &options).unwrap();
        assert_eq!(netlist.circuit.diode_models.len(), 1);
        assert!(netlist.circuit.diode_models.contains_key("m2"));

        // A deck that asked for a model inside the *other* section fails
        // loudly at validation.
        let deck = format!(
            ".lib {} sec2\nD1 1 0 m1\n.op\n.end\n",
            lib.display()
        );
        assert!(parse_with_options(&deck, &options)
            .unwrap_err()
            .to_string()
            .contains("unknown model"));

        let _ = std::fs::remove_file(&lib);
    }

    /// A library file that includes itself fails with a cycle error, not a
    /// stack overflow.
    #[test]
    fn library_cycle_is_a_loud_error() {
        let dir = std::env::temp_dir().join(format!("rspice_test_{}_cyc", std::process::id()));
        std::fs::create_dir_all(&dir).expect("temp library directory");
        let lib = dir.join("cycle.lib");
        std::fs::write(&lib, "* a library that cannot end this way\n.include cycle.lib\n")
            .expect("writing the cyclic library");

        let options = ParseOptions {
            deck_dir: Some(dir.clone()),
            search_paths: Vec::new(),
        };
        let deck = ".include cycle.lib\nV1 1 0 1\n.op\n.end\n";
        let err = parse_with_options(deck, &options).unwrap_err().to_string();
        assert!(err.contains("circular library chain"), "was: {err}");
        let _ = std::fs::remove_dir_all(&dir);
    }

    // ─── Model cards ──────────────────────────────────────────────────────────

    /// The complete card grammar: every diode parameter lands, the fused
    /// `TYPE(` spelling and the spaced one both parse, MOSFET `LEVEL` and
    /// the optional process fields read through, and unknown families are
    /// stored generically with their `DEV`/`LOT` slots.
    #[test]
    fn model_card_grammar_completeness() {
        let netlist = parse(
            ".model dmod D(IS=1.2e-13 N=1.8 RS=0.5 CJO=2p VJ=0.7 M=0.3 TT=1n EG=1.15 XTI=3.1 BV=80 IBV=1p KF=0 AF=1 TNOM=30 DEV=2% LOT=1)\n\
             .model nmos2 NMOS(KP=22e-6 VTO=0.65 LAMBDA=0.03 GAMMA=0.4 PHI=0.55 LEVEL=2 TOX=4n NSUB=1e16 NSS=1e10 TPG=1 CGDO=1p CGSO=1p CGBO=1p)\n\
             .model nmos1 NMOS(KP=2e-4)\n\
             .model pmos3 PMOS (KP=10e-6 VTH0=-0.7)\n\
             .model npn1 NPN(LEVEL=1 BF=100 IS=8e-15 DEV=5%)\n\
             V1 a 0 1\n\
             D1 a 0 DMOD\n\
             M1 a 0 0 0 nmos1\n\
             M2 a 0 0 0 pMOS3\n\
             .op\n\
             .end\n",
        )
        .unwrap();
        let circuit = &netlist.circuit;

        let d = &circuit.diode_models["dmod"];
        assert_eq!(d.is, 1.2e-13);
        assert_eq!(d.n, 1.8);
        assert_eq!(d.rs, 0.5);
        assert_eq!(d.cjo, 2e-12);
        assert_eq!(d.vj, 0.7);
        assert_eq!(d.m, 0.3);
        assert_eq!(d.tt, 1e-9);
        assert_eq!(d.eg, 1.15);
        assert_eq!(d.xti, 3.1);
        assert_eq!(d.bv, 80.0);
        assert_eq!(d.ibv, 1e-12);
        assert_eq!(d.kf, 0.0);
        assert_eq!(d.af, 1.0);
        assert_eq!(d.tnom, 30.0);
        assert_eq!(d.tolerance.dev, Some((2.0, true)));
        assert_eq!(d.tolerance.lot, Some((1.0, false)));

        let m = &circuit.mosfet_models["nmos2"];
        assert_eq!(m.level, 2);
        assert_eq!(m.kp, 22e-6);
        assert_eq!(m.vth0, 0.65);
        assert_eq!(m.tox, Some(4e-9));
        assert_eq!(m.cgdo, Some(1e-12));
        assert_eq!(m.nsub, Some(1e16));
        assert_eq!(m.nss, Some(1e10));
        assert_eq!(m.tpg, Some(1.0));

        // Spaced spelling, NMOS's sibling alias (VTO) and PMOS kind.
        let p = &circuit.mosfet_models["pmos3"];
        assert!(matches!(p.kind, MosfetType::Pmos));
        assert_eq!(p.vth0, -0.7);

        // BJT cards land in the typed slot with their tolerance.
        let b = &circuit.bjt_models["npn1"];
        assert!(matches!(b.kind, BjtType::Npn));
        assert_eq!(b.bf, 100.0);
        assert_eq!(b.is, 8e-15);
        assert_eq!(b.tolerance.dev, Some((5.0, true)));

        // Element cards reference models case-insensitively.
        assert_eq!(circuit.diodes[0].model, "dmod");
        assert_eq!(circuit.mosfets[0].model, "nmos1");
        assert_eq!(circuit.mosfets[1].model, "pmos3");
        assert!(
            netlist.warnings.is_empty(),
            "{:?}",
            netlist.warnings
        );
    }

    /// A model card on a level the engine cannot stamp is accepted, but its
    /// *use* is rejected at parse time (validation runs inside `parse`).
    #[test]
    fn referenced_level_above_one_is_rejected() {
        let deck = ".model m NMOS(KP=1u LEVEL=3)\n\
                    V1 a 0 1\n\
                    M1 a 0 0 0 m\n\
                    .op\n\
                    .end\n";
        let err = parse(deck).unwrap_err().to_string();
        assert!(err.contains("only level 1 is supported"));
    }

    #[test]
    fn malformed_model_cards_are_loud_errors() {
        // No type.
        assert!(parse(".model lonely\n.op\n.end\n").is_err());
        // Unbalanced parentheses.
        assert!(parse(".model m1 D(IS=1e-12\n.op\n.end\n")
            .unwrap_err()
            .to_string()
            .contains("unbalanced"));
        // A value that is not a number on a diode card.
        assert!(parse(".model m1 D(IS=fuzzy)\n.op\n.end\n").is_err());
        // Garbage LEVEL.
        assert!(parse(".model m1 NMOS(K=1 LEVEL=high)\n.op\n.end\n").is_err());
    }

    /// Passive model parasitics (`RSER`/`LSER`/`CPAR`/`RPAR`) materialize
    /// into real element arms with internal nodes, and the emitted deck keeps
    /// the same graph without re-splitting (`parse(emit(…))` is stable).
    #[test]
    fn passive_parasitics_expand_to_arms() {
        let netlist = parse(
            ".model cm CAP(C=1n RSER=2 CPAR=4p)\n\
             .model rm R(RES=10 LSER=1 RPAR=100 TC1=0.1)\n\
             C1 a b cm\n\
             R1 b 0 rm\n\
             V1 a 0 5\n\
             .op\n\
             .end\n",
        )
        .unwrap();
        let circuit = &netlist.circuit;
        // One resistor arm carries the capacitor's RSER between the card
        // terminal and an internal node; the carrier keeps the internal node.
        let arm = circuit
            .resistors
            .iter()
            .find(|r| r.name == "RC1_RSER")
            .expect("capacitor RSER arm");
        assert_relative_eq!(arm.resistance, 2.0, epsilon = 1e-15);
        // Carrier capacitor now hangs off the arm's internal node.
        let carrier = &circuit.capacitors[0];
        assert_eq!(carrier.capacitance, 0.0, "value comes from the model card");
        assert_relative_eq!(
            circuit.capacitance_at(carrier),
            parse_value("1n").unwrap(),
            epsilon = 1e-24
        );
        assert_eq!(
            circuit.node_name(carrier.n1.0),
            "C1.int",
            "carrier moved to the internal junction"
        );
        assert_eq!(circuit.node_name(arm.n1.0), "a");
        assert_eq!(circuit.node_name(arm.n2.0), "C1.int");
        // CPAR rides the untouched external pair as a real capacitor.
        let cpar = circuit
            .capacitors
            .iter()
            .find(|c| c.name == "CC1_CPAR")
            .expect("CPAR arm");
        assert_relative_eq!(cpar.capacitance, parse_value("4p").unwrap(), epsilon = 1e-24);

        // Inductor arm for the resistor carrier (LSER), parallel R.
        let lser = circuit
            .inductors
            .iter()
            .find(|l| l.name == "LR1_LSER")
            .expect("resistor LSER arm");
        assert_relative_eq!(lser.inductance, 1.0, epsilon = 1e-15);
        let rpar = circuit
            .resistors
            .iter()
            .find(|r| r.name == "RR1_RPAR")
            .expect("RPAR arm");
        assert_relative_eq!(rpar.resistance, 100.0, epsilon = 1e-15);
        assert_eq!(circuit.node_name(rpar.n1.0), "b");
        assert_eq!(circuit.node_name(rpar.n2.0), "0");
        assert_eq!(
            circuit.resistance_at(&circuit.resistors[0]),
            10.0,
            "carrier resistance unchanged"
        );

        // Round trip: the arms re-parse as plain cards and the parasitic
        // slots are not reprinted, so the graph is stable.
        let text = emit(&netlist);
        assert!(text.contains("RC1_RSER"), "{text}");
        assert!(!text.contains("RSER=2"), "slots must not double-print: {text}");
        let second = parse(&text).unwrap();
        let arm2 = second
            .circuit
            .resistors
            .iter()
            .find(|r| r.name == "RC1_RSER")
            .expect("arm survives the round trip");
        assert_relative_eq!(arm2.resistance, 2.0, epsilon = 1e-15);
        assert_eq!(second.circuit.resistors.len(), circuit.resistors.len());
        assert_eq!(second.circuit.capacitors.len(), circuit.capacitors.len());
        assert_eq!(second.circuit.inductors.len(), circuit.inductors.len());
    }

    /// Unsupported parameters are carried as warnings, never fatal.
    #[test]
    fn unknown_model_parameters_warn() {
        let netlist = parse(
            ".model d1 D(IS=1p WOOD=FIRM)\n\
             .model m1 NMOS(SHIFT=3)\n\
             .op\n\
             .end\n",
        )
        .unwrap();
        assert!(netlist
            .warnings
            .iter()
            .any(|w| w.contains("wood") && w.contains("not supported")));
        // Keys normalize to lowercase before the unknown-parameter branch.
        assert!(
            netlist
                .warnings
                .iter()
                .any(|w| w.contains("shift") && w.contains("not supported")),
            "{:?}",
            netlist.warnings
        );
        assert_eq!(netlist.circuit.diode_models["d1"].is, 1e-12);
    }

    // ─── Element sizing fields ────────────────────────────────────────────────

    /// Diode `AREA` (bare-value and keyword spellings) and MOSFET per-instance
    /// junction sizing parse onto the stored element fields.
    #[test]
    fn element_sizing_fields_parse() {
        let netlist = parse(
            ".model dm D(IS=1e-12)\n\
             .model nm NMOS(KP=2e-4)\n\
             D1 a 0 dm 2\n\
             D2 b 0 dm AREA=3\n\
             M1 a 0 0 0 nm W=2u L=0.5u AD=1p AS=2p PS=1u PD=2u\n\
             M2 b 0 0 0 nm\n\
             .op\n\
             .end\n",
        )
        .unwrap();
        let circuit = &netlist.circuit;
        assert_eq!(circuit.diodes[0].area, 2.0);
        assert_eq!(circuit.diodes[1].area, 3.0);
        let m = &circuit.mosfets[0];
        assert_eq!(m.ad, crate::units::parse_value("1p").unwrap());
        assert_eq!(m.asr, crate::units::parse_value("2p").unwrap());
        assert_eq!(m.ps, crate::units::parse_value("1u").unwrap());
        assert_eq!(m.pd, crate::units::parse_value("2u").unwrap());
        // Defaults survive on an unannotated card.
        assert_eq!(circuit.mosfets[1].ad, 0.0);
        assert_eq!(circuit.mosfets[1].w, 1e-6);

        // Sizing fields ride the canonical emission (`f64` Display prints
        // plain decimal, which `parse_value` always re-parses).
        let text = emit(&netlist);
        assert!(text.contains("D1 a 0 dm AREA=2"), "{text}");
        assert!(text.contains("AREA=3"), "{text}");
        assert!(
            text.contains("AD=0.000000000001 AS=0.000000000002 PS=0.000001 PD=0.000002"),
            "{text}"
        );
    }

    // ─── Tolerant import ──────────────────────────────────────────────────────

    /// Unknown directives, unknown element letters and stray `.endl`
    /// markers import with warnings instead of failing the deck.
    #[test]
    fn third_party_leftovers_become_warnings() {
        let netlist = parse(
            ".probe v(1)\n\
             Q1 c be 0 qmod\n\
             .model qmod NPN(LEVEL=1 BF=200)\n\
             .endl stray\n\
             V1 in 0 5\n\
             R1 in c 1k\n\
             .op\n\
             .end\n",
        )
        .unwrap();
        assert_eq!(netlist.circuit.resistors.len(), 1);
        assert!(netlist.analysis.dc_op);
        assert_eq!(netlist.warnings.len(), 2, "{:?}", netlist.warnings);
        assert!(netlist.warnings[0].contains(".probe"));
        assert!(netlist.warnings[1].contains("stray"));
        // The `Q` card now parses as a real BJT against its model.
        assert_eq!(netlist.circuit.bjts.len(), 1);
        assert_eq!(netlist.circuit.bjts[0].model, "qmod");
        assert!(netlist.circuit.bjt_models.contains_key("qmod"));
    }

    // ─── .ic / .nodeset ───────────────────────────────────────────────────────

    /// Entry spellings: fused, split, and mixed on one line; I() entries and
    /// stray tokens warn and skip.
    #[test]
    fn ic_and_nodeset_entries_parse_in_every_spelling() {
        let netlist = parse(
            ".ic V(in)=1.5 V(mid) = 2.5 V(out) =3 V(tap)=1m\n\
             .nodeset V(mid) = 2\n\
             .ic I(L1)=2 A(bq)=1 remain\n\
             .op\n\
             .end\n",
        )
        .unwrap();
        let analysis = &netlist.analysis;

        let flat: Vec<(String, f64)> = analysis
            .ic
            .iter()
            .map(|e| (e.node.to_ascii_lowercase(), e.value))
            .collect();
        assert_eq!(
            flat,
            vec![
                ("in".into(), 1.5),
                ("mid".into(), 2.5),
                ("out".into(), 3.0),
                ("tap".into(), 1e-3),
            ]
        );
        assert_eq!(analysis.nodeset.len(), 1);
        assert_eq!(analysis.nodeset[0].node.to_ascii_lowercase(), "mid");
        assert_eq!(analysis.nodeset[0].value, 2.0);

        // One warning for the I() entry, one for the machine letter 'A',
        // one for the trailing stray word.
        assert_eq!(netlist.warnings.len(), 3, "{:?}", netlist.warnings);
        assert!(netlist.warnings[0].contains("I(L1)"));
        assert!(netlist.warnings[1].contains("A(bq)"));
        assert!(netlist.warnings[2].contains("remain"));

        // And the cards survive the canonical emission.
        let text = emit(&netlist);
        assert!(text.contains(".ic V(in)=1.5"));
        assert!(text.contains(".nodeset V(mid)=2"));
    }

    #[test]
    fn malformed_ic_entries_are_loud_errors() {
        // No ')'.
        assert!(parse(".ic V(in=5\n.op\n.end\n").is_err());
        // No value.
        assert!(parse(".ic V(in)=\n.op\n.end\n").is_err());
        // Fused value before '='.
        assert!(parse(".ic V(in)5=2\n.op\n.end\n").is_err());
    }
}
