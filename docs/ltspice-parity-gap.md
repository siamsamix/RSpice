# LTspice → RSpice Parity Gap Analysis

A comprehensive, implementation-oriented catalog of every capability LTspice ships that RSpice does not yet provide (or provides only in weakened form), plus concrete guidance on how to add each one. Written so that any AI agent (or contributor) can pick an item, find where it belongs in the RSpice codebase, and implement it following existing patterns.

## 1. Purpose, method, and evidence base

**Goal.** RSpice's stated ambition (see `TODO.md` and `CONTEXT.md`) is full product parity with LTspice and OrCAD/PSpice — engine, device models, parser language, analyses, waveform viewer, schematic GUI, model libraries and interop. This document lists the concrete gaps against LTspice so they can be tracked, prioritized, and implemented.

**Evidence base (shipped LTspice artifact).** All LTspice behavior described here was read from the LTspice installation's own help system, i.e. the shipped artifact at:

```
/home/cachy/.wine/drive_c/Program Files/ADI/LTspice/
```

Key files consulted:

- `LTspice.exe` (68 MB PE binary — version 26.0.2 per `LTspice.json`, installed 2026-05-07)
- `LTspiceChangeLog.txt` (release notes through 4/29/26, LTspice 26.0.2)
- `LTspiceHelp/*.htm` — the full HTML help: `dotcommands.htm`, `circuitelements.htm`, `aspecialfunctions.htm`, `barbitrarybehavioralvoltageorcur.htm`, `machine--arbitrarystatemachine.htm`, `measure--evaluateuser-definedele.htm`, `net--computenetworkparametersina.htm`, `dotfra.html`, `fra_device.html`, `fra_probe.html`, `olossytransmissionline.htm`, `uuniformrc-line.htm`, `zmesfetandigbt.htm`, `save--limitthequantityofsaveddat.htm`, `savebias--saveoperatingpointtodi.htm`, `loadbias--loadapreviouslysolvedd.htm`, `loadstate.htm`, `savestate.htm`, `dotkeep.htm`, `wave--writeselectednodestoawavfi.htm`, `options--setsimulatoroptions.htm`, `tran--doanonlineartransientanaly.htm`, `ac--performansmallsignalacanalys.htm`, `noise--performanoiseanalysis.htm`, `tf--findthedcsmall-signaltransfe.htm`, `dc--performadcsourcesweepanalysi.htm`, `op--findthedcoperatingpoint.htm`, `temp--temperaturesweeps.htm`, `step--parametersweeps.htm`, `func--userdefinedfunctions.htm`, `param--user-definedparameters.htm`, `model--defineaspicemodel.htm`, `ic--setinitialconditions.htm`, `commandlineswitches.htm`, plus device pages for `ccapacitor`, `ddiode`, `mmosfet`, `qbipolartransistor`, `jjfet`, `linductor`, `rresistor`, `svoltagecontrolledswitch`, `tlosslesstransmissionline`, `vvoltagesource`, `wcurrentcontrolledswitch`, `xsubcircuit`, `evoltagedependentvoltagesource`, `fcurrentdependentcurrentsource`, `gvoltagedependentcurrentsource`, `hcurrentdependentvoltagesource`, `icurrentsource`, `global--declareglobalnodes`, `backanno--annotatethesubcircuitp`, `include--includeanotherfile`, `lib--includealibrary`, `subckt--defineasubcircuit`, `param--user-definedparameters`, `dotkeep`, `exportingwaveformdata`, `waveformviewer`, `waveformarithmetic`, `plotpanes`, `attachedcursors`, `datatraceselection`, `schematiccapture`, `placingcomponents`, `creatingnewsymbols`, `customsymbols`, `symbolandlibrarysearchpaths`, `hierarchy`, `navigatingthehierarchy`, `rulesofhierarchy`, `convergence`, `transientanalysisoptions`, `integrationmethods`, `temperature`, etc.

**How LTspice behavior was established.** These are the shipped tool's *documented* behaviors (syntax, defaults, model parameter tables). I did **not** execute `LTspice.exe` (it is a Windows GUI binary under Wine; running it reliably is outside this task's scope). Where the help is vague, the item is flagged "verify against shipped docs before implementing" — exactly the discipline `TODO.md`'s footnote demands. Implementation guidance below is derived by mapping each LTspice behavior onto RSpice's existing architecture vocabulary (`CONTEXT.md`): element, model card, analysis command, directive, stamp, MNA, NR, branch layout, `ParamEnv`, `BehavioralRuntime`, `RunConfig`, `AnalysisCommands`.

**Companion documents.** Read `CONTEXT.md` (glossary/vocabulary), `TODO.md` (the existing roadmap, which this doc supplements), and `docs/adr/0001-circuit-boundary-contract.md` (the `Circuit` boundary contract) before implementing anything here. Use the terms from `CONTEXT.md` — element, model card, analysis command, directive, stamp, MNA — do not invent synonyms.

## 2. How to use this document

1. Pick an unclaimed gap from the tables in §4–§9.
2. Read the "LTspice behavior" column to understand what to build (syntax, defaults, semantics).
3. Read the "RSpice status" column to see what (if anything) exists.
4. Follow the "Where to implement" pointer — it names the crate, module, and the existing pattern to imitate (e.g. "add a new element family like `Switch` in `circuitsim/src/netlist/elements.rs`, stamp it in `circuitsim/src/mna.rs`, report it in `circuitsim/src/circuit.rs`").
5. Write a regression test in `circuitsim/tests/regression.rs` (or a new `.cir` fixture under `examples/`) pinned to an analytic reference or to LTspice `.raw` output — the "fast" claim must be measured, not asserted (`TODO.md` §11).
6. When the item lands, mark it `[done]` here and add a corresponding `TODO.md` entry if the roadmap doesn't already cover it.

## 3. RSpice today (what exists)

Source of truth: `CONTEXT.md` §"Where we stand" and `TODO.md` §2–§5. Summarized here so the gap tables below can be read standalone.

**Element families** (`CURRENT_FAMILIES = R C L V D M Q J I B`, plus `E F G H S W T K`): resistor, capacitor, inductor, voltage source, current source, diode, BJT (`Q`), JFET (`J`), MOSFET (`M`), voltage-controlled voltage source (`E`), current-controlled current source (`F`), voltage-controlled current source (`G`), current-controlled voltage source (`H`), behavioral source (`B`), voltage-controlled switch (`S`), current-controlled switch (`W`), lossless transmission line (`T`), mutual inductance (`K`), op-amp (`U`). Subcircuit instantiation (`X`) with flattening and mangling.

**Missing element letters vs LTspice:** `O` (lossy T-line/LTRA), `U` (uniform RC line), `Z` (MESFET/IGBT), `A` (special functions / digital gates), `@` (FRA device), `&` (FRA probe).

**Analyses:** `.op`, `.dc` (one/two sweeps, linear + LIST), `.tran` (adaptive trapezoidal/Gear, UIC, STARTUP, tmax, breakpoints), `.ac` (oct/dec/lin), `.noise` (R/diode/MOSFET), `.tf`, `.four`, `.step` (linear/oct/decade/LIST, nested, temperature), `.meas`, `.mc` (Monte Carlo, LOT/DEV, histogram/yield), `.temp`.

**Dot commands parsed:** `.model`, `.param`, `.func`, `.subckt`/`.ends`, `.include`, `.lib` (section-aware), `.global`, `.options`, `.temp`, `.ic`, `.nodeset`, `.dc`, `.ac`, `.tran`, `.tf`, `.noise`, `.four`, `.step`, `.meas`, `.mc`, `.end`.

**GUI:** egui schematic editor (`R C L V Gnd` symbols only), waveform tabs (Voltage/Current/Differential, Frequency magnitude/phase/branch), DC tab, Overview tab, CSV export, `--raw`/`.raw` writer + reader (binary + ASCII, stepped runs).

**CLI:** `-b/--batch`, `-a/--analysis`, `--output`, `--waves`, `-I`, warnings on stderr, exit codes.

## 4. Gap catalog — circuit elements (§5 of LTspice help)

Each row: LTspice element, how it works (from LTspice help), RSpice status, where to implement.

| # | LTspice element | LTspice behavior (help summary) | RSpice status | Where to implement |
|---|-----------------|---------------------------------|---------------|--------------------|
| 4.1 | `Oxxx L+ L- R+ R- <model>` — lossy transmission line (LTRA) | Single-conductor lossy line. Port 1 = L+/L-, port 2 = R+/R-. Requires `.model <name> LTRA(len= R= L= G= C= Rel= Abs= NoStepLimit NoControl LinInterp MixedInterp CompactRel CompactAbs TruncNr TruncDontCut)`. R per-unit-length, L per-unit-length, G, C, Len. History compaction, breakpoints on derivative change, interpolation flags. Implemented as LTRA convolution (LTspice proprietary). | **Missing.** RSpice has only lossless `T` (method of characteristics, `TlineHistory`). | New element family `O` / model type `LTRA`. Add `LtraModel` to `circuitsim/src/models.rs` (or a `tline.rs`), parse `.model … LTRA(…)` in `dotcards.rs`/`models.rs`, element in `circuitsim/src/netlist/elements.rs`, stamp as a frequency-dependent/convolution companion in `circuitsim/src/mna.rs` transient path, report `I(O…)` current. AC: exact lossy two-port or rational approximation. Verify against the parameter table in `olossytransmissionline.htm`. |
| 4.2 | `Uxxx N1 N2 Ncom <model> L=<len> [N=<lumps>]` — uniform RC line | Distributed RC line expanded into geometrically-progressed lumped RC segments (Gertzberrg 1974). Model: `URC` with K (propagation constant, 2.0), Fmax (1G), Rperl (1k), Cperl (1e-15), ISPERL (diode replacement), RSPERL. | **Missing.** | New element family `U`, `UrcModel` with the parameter table in `uuniformrc-line.htm`. Implement as a lumped-segment expansion (internally generated nodes, geometric progression) or a distributed RC companion model. Stamp in `mna.rs`. |
| 4.3 | `Zxxx D G S <model>` — MESFET (NMF/PMF) | Statz GaAs FET model. Parameters: Vto, Beta, B, Alpha, Lambda, Rd, Rs, Cgs, Cgd, Pb, Kf, Af, Fc, Is. Charge storage = total gate charge vs Vgd/Vgs. | **Missing.** | New element family `Z`, `MosfetModel`/`MefetModel` (model types NMF/PMF). Reuse the JFET/MOSFET quadratic-channel structure; add gate capacitance. Stamp in `mna.rs`; `I(Z…)`, `Id(Z…)`, `Ig(Z…)`, `Is(Z…)` probes. |
| 4.4 | `Zxxx C G E <model>` — IGBT (NIGBT/PIGBT) | Hefner/NIST-based IGBT with subthreshold conduction and stochastic noise. Parameters: Agd, area, BVF, BVN, Cgs, Coxd, Jsne, KF, KP, MUN, MUP, NB, Tau, Theta, Vt, Vtd, WB, subthres, Kfn, Afn, tnom. | **Missing.** | As for MESFET but with the IGBT parameter set and model types NIGBT/PIGBT. Larger effort; verify physics against `zmesfetandigbt.htm`. |
| 4.5 | `Annn … <model>` — special functions / digital gates | INV, BUF, AND, OR, XOR, SCHMITT, SCHMTBUF, SCHMTINV, DFLOP, VARISTOR, MODULATE. 8-terminal idealized behavioral gates (terminals 6/7 = complementary outputs, 8 = common). Parameters: Vhigh, Vlow, Trise, Tfall, Tau, Cout, Rout, Rhigh, Rlow, Td, Ref, Vt, Vh. OTA family: OTA, OTA2, MOTA, MOTA2, MOTA3, MOTA8 — transconductance amplifier with tanh (or linear) transfer, noise densities (en, enk, in, ink, incm, incmk), compliance clamps. | **Missing.** | This is a mixed-signal/digital primitive family. New element family `A`. Implement gates as idealized behavioral elements with the instance-parameter table from `aspecialfunctions.htm`. OTA = a `G`-like element with tanh transfer, output clamps, noise. This is the largest single element gap; scope carefully (TODO §5 lists digital primitives as P2/PS). |
| 4.6 | `@xxx …` — FRA device + `&xxx …` — FRA probe | Frequency Response Analyzer (`.fra` analysis). Applies sinusoidal stimuli, measures loop gain/impedance. `@` = analyzer (gain or impedance via `zm`), `&` = probe (differential gain between two points). Parameters: fstart, fend, oct, fcoarse, nmax, pp0/pp1/f0/f1/pp, tavgmin, tsettle, rpar, acmag, acphase, enabled, refnode, intnode, delay. | **Missing.** Goes hand-in-hand with `.fra` (§4.12 below). | New element families `@` and `&`, plus the `.fra` analysis driver. Stamp stimulus current sources and measure response per frequency point. Large effort; P2. |

### Notes on elements RSpice already has (but LTspice extends)

- **Resistor/Capacitor/Inductor:** LTspice supports intrinsic parasitics on every R/L/C (`rser`, `cpar`, `lser`, `rpar`) plus `m` multiplier. RSpice CONTEXT says passive model cards + TC1/TC2 + parasitic split landed (nonzero slots materialize into real series/parallel arms with internal nodes). Confirm `m` multiplier and `rpar`/`cpar` on all three are fully wired.
- **Diode:** LTspice `D` supports `off`, `area`, temp; model params RS, TT, CJO/M/VJ, EG/XTI, BV/IBV, KF/AF, N+, etc. RSpice CONTEXT says RS/CJO/TT/BV/IBV/NBV/EG/XTI/TNOM/KF/AF done with charge-based capacitance. Confirm `off` and `temp` instance params.
- **BJT/Q:** LTspice Gummel-Poon with IKF/IKR, XTB, Early, ISE/ISC, CJE/CJC/TF/TR, RB/RC/RE internal nodes. RSpice CONTEXT says done. Confirm `off`, `IC=Vbe,Vce`, `temp`, area.
- **JFET/J:** done per CONTEXT. Confirm `off`, `IC`, `temp`, area.
- **MOSFET/M:** levels done; LTspice levels 1–3 + BSIM3/BSIM4 differ per tool (TODO flags this as verify). LTspice instance params: L, W, AD, AS, PD, PS, NRD, NRS, off, IC=Vds,Vgs,Vbs, temp, m.
- **Switch S/W:** done (`SW(RON ROFF VT VH)` model). Confirm `on`/`off` initial state and `temp`.
- **Op-amp U:** done (single-pair VCVS, GAIN/RIN/ROUT/VOS). LTspice universal op-amp is a subckt macromodel, not a primitive — covered by `.include`/libraries.
- **Behavioral B:** done (`V=<expr>|I=<expr>`). LTspice adds `R=<expr>` (behavioral resistor), `P=<expr>` (behavioral power), `ic`, `tripdv`, `tripdt`, `laplace=`, `window=`, `nfft`, `mtol`, `Rpar`. RSpice B source lacks `R=`, `P=`, `tripdv/tripdt`, `laplace/window/nfft/mtol` on transient (it has ABM Laplace/FREQ/CHEBYSHEV on E/F/G/H). See §4.8.
- **Sources V/I:** LTspice waveform family PULSE/SIN/EXP/PWL(FILE,REPEAT)/SFFM/AM plus `load` flag on I, `Rser` on V. RSpice CONTEXT says full family done. Confirm `PWL FILE=<file>`, repeat blocks, `SFFM`, `AM`, `load` flag, `Rser`.

## 5. Gap catalog — dot commands / directives (§5.2 of LTspice help)

| # | Directive | LTspice behavior | RSpice status | Where to implement |
|---|-----------|------------------|---------------|--------------------|
| 5.1 | `.save V(out) [V(in) [I(L1) …]]` | Restricts saved waveforms to listed traces (huge .tran files). Wildcards `*`/`?` (`.save V(*) Id(*)`). Hierarchy with `:` (`.save V(x23:*)`). Single-terminal device currents: `Ic(Q2)`. | **Missing.** | New `.save` card in `dotcards.rs` → `SaveCommand` in `mod.rs` (`AnalysisCommands`). Store the trace filter (wildcards + hierarchy). The analysis runner / raw writer (`circuitsim/src/analysis.rs`, `rawwave.rs`) honors it: only emit selected traces. CLI `--raw` unchanged. |
| 5.2 | `.keepnode <node> [, …]` | Keeps nodes LTspice would otherwise optimize away (mostly useful for `.noise`). | **Missing.** | New `.keepnode` card → `HashSet<String>` kept nodes on `AnalysisCommands`/netlist. During node-plan/compaction (`nodeplan.rs`), exclude kept nodes from removal. |
| 5.3 | `.savebias <file> [internal] [temp=] [time= [repeat]] [step=] [DC1=] [DC2=] [DC3=]` | Writes DC operating point as text (`.nodeset` form) to disk; reloadable via `.loadbias`. `internal` keeps internal device nodes. `time=` saves the first solved point after that time in `.tran`; `repeat` rewrites every period. DC1/2/3 extract one point from a `.dc` sweep. | **Missing.** (Superseded by `.savestate` in LTspice ≥24.1, but still documented.) | New `.savebias`/`.loadbias` cards. `.savebias` solves `.op`, writes a `.nodeset`-form text file (node names + voltages) next to the deck (resolve via `deck_dir`). `.loadbias` reads it back into `.nodeset`/`.ic` entries. Reuse `NodesetEntry` parse/emit. |
| 5.4 | `.savestate [<file>] [time=]` / `.loadstate [<file>] [reset]` | Save/restore *complete transient state* (all components) to a proprietary `.state` file; simulation continues at saved time (`reset` plots from t=0 relative to saved time). Must be same circuit/solver/method. | **Partially missing.** RSpice `.tran` accepts `savestate`/`loadstate`/`savestatetime` modifiers per the `.tran` help, but standalone `.savestate`/`.loadstate` directives and state serialization are not in `AnalysisCommands`. | Add standalone `.savestate`/`.loadstate` cards + state snapshot format. The snapshot must capture integrator history (trapezoidal/Gear companion state), B-source `idt`/`sdt`/`delay` history (`BehavioralRuntime`), T-line histories (`TlineHistory`), and NR state. Serialize to a portable format (JSON or binary) in a new `circuitsim/src/state.rs`. |
| 5.5 | `.wave <file.wav> <Nbits> <SampleRate> V(out) …` | Writes selected node voltages/device currents to a .wav audio file (1–32 bits, sample rate, up to 65535 channels, full scale ±1 V/A). Usable as input to another simulation (see V/I source `PWL FILE`). | **Missing.** | New `.wave` card → `WaveCommand`. After `.tran`, resample the selected traces at `<SampleRate>`, quantize to `<Nbits>`, write RIFF/WAV. Reuse trace selection from `.save` (5.1). CLI can also expose `--wave`. |
| 5.6 | `.machine … .state … .rule … .output … .endmachine` | Arbitrary state machine (5 commands). `.state name value` (first declared = initial). `.rule old new condition` (fires when condition > 0.5; `*` matches any state, checked first; one rule per timestep). `.output (pos[,neg]) expression` — current sources requiring external readout resistors; expression sees `state` and state names. | **Missing.** | New analysis/directive block. Parse `.machine`/`.state`/`.rule`/`.output`/`.endmachine` into an `ArbitraryStateMachine` struct (`AnalysisCommands.machine`). During transient, each accepted step: evaluate rules in order, fire at most one, update state; inject `.output` expressions as current sources into the MNA RHS (branch-row or injected current). See `machine--arbitrarystatemachine.htm` for the inverter / divide-by-2 examples to pin as tests. |
| 5.7 | `.net [V(out[,ref])\|I(Rout)] <Vin\|Iin> [Rin=] [Rout=]` | 1-port/2-port network parameters (Y, Z, H, S) computed during `.AC`. Input = independent V or I source; output = node or resistor; terminations default 1 Ω (or device impedance). | **Missing.** | New `.net` card → `NetCommand` on `AnalysisCommands`. During AC solve, post-process the complex nodal solution into Y/Z/H/S parameters using terminations Rin/Rout. Emit as `.raw` traces (like `.tf`/`.noise` output). P2 (LTspice `.NET`, rarely used). |
| 5.8 | `.fra …` | Time-domain frequency response analysis for SMPS loop stability. Requires `@` FRA devices. See §4.6 and `dotfra.html`. | **Missing.** | New analysis mode `Fra`. Requires `@` element (4.6). Drives sinusoidal stimulus at fstart…fend, measures gain/impedance, writes `<circuit>.fra_<name>.raw`. Large effort; P2. |
| 5.9 | `.csparam` | Feeds `.meas` results across stepped runs (LTspice). | **Missing.** | `.meas` results are already per-step (`measurements`). Add a `.csparam` card that exposes prior-step measurement values as parameters to later steps. Cross-references `Measurement` results. P2. |
| 5.10 | `.out` / `.log` / `.probe` (PSpice) | Output-file directives and simulation log with device operating-point tables. | **Missing.** | RSpice logs warnings to stderr and `.raw` has data. Add `.log <file>` to echo analysis output (op table, `.meas` results, `.tf`, `.noise` totals) to a file. `.probe` = PS trace selection (analogous to `.save`). P2/PS. |
| 5.11 | `.alias` / `.endaliases` (PSpice) | Node aliasing blocks for probe names. | **Missing.** | Named-node aliases resolved during parse into canonical node ids. P2/PS. |
| 5.12 | `.backanno` | Automatically included in LTspice-generated netlists; records subcircuit pin names in `.raw` so port currents can be cross-probed by pin name. | **Missing.** | When writing `.raw`, include subcircuit instance path → port name mapping so `I(X1@pin)` style probes work. Tie into `rawwave.rs` trace naming and GUI cross-probing (§7). P2. |

**Directives RSpice already has that LTspice extends (verify/complete):**
- `.options` — RSpice parses: `method`, `reltol`, `abstol`, `vntol`, `gmin`, `gminsteps`, `srcsteps`/`itl6`, `itl1`, `itl4`, `seed`. LTspice's full table (`options--setsimulatoroptions.htm`) adds: `chgtol`, `cshunt`, `cshuntintern`, `debugtran`, `defad`, `defas`, `defl`, `defw`, `delay`, `fastaccess`, `flagloads`, `gfarad`, `gfloat`, `itl2`, `itl6`, `list`, `logparams`, `logopinfo`, `maxclocks`, `maxstep`, `meascplxfmt`, `measdgt`, `minclocks`, `mindeltagmin`, `no_caret_warning`, `nomarch`, `noopiter`, `numdgt`, `pivrel`, `pivtol`, `plotreltol`, `plotvntol`, `plotabstol`, `plotwinsize`, `ptrantau`, `ptranmax`, `reject_number_tails`, `solver`, `srcstepmethod`, `sstol`, `ststclocks`, `ststdelay`, `temp`, `thev_induc`, `tnom`, `topologycheck`, `trtol`, `trytocompact`. RSpice should round-trip the ones it consumes and warn-and-ignore the rest (already the pattern: unknown keys warn, `not recognized — ignored`). Add the missing knobs to `RunConfig` as they're needed by the engine (e.g. `plotwinsize`/`plotreltol` for waveform compression, `maxstep` already on `.tran`, `temp`/`tnom` exist).
- `.ac` — RSpice does oct/dec/lin; LTspice adds `list <freq…>` and `file=<filename>`. TODO §4 lists `.ac` LIST beyond dec/oct/lin as P2.
- `.dc` — RSpice does lin ramp + LIST; LTspice adds oct/dec sweeps and `file=<filename>`. Implemented (DcSweepValues::Linear/List). Add oct/dec if desired.
- `.tran` — RSpice has Tstep/Tstop/[Tstart/dTmax], UIC, STARTUP, tmax, loadstate/savestate modifiers. LTspice also lists `convreport` (`.fra`/`.tran` modifier) and `startup`. Fine.
- `.measure`/`.meas` — implemented (MeasKind: Scalar/FindWhen/Cross/TrigTarg; AVG/MAX/MIN/PP/RMS/INTEG; FIND/WHEN/AT/RISE/FALL/CROSS/TD/TRIG/TARG). LTspice also saves results to a SQLite `.db` (Parameters/Steps/Measurements/Results tables, since 24.1.9). RSpice could add a `--meas-db` or `.measdb` option emitting SQLite. P2.
- `.four` — implemented (frequency + outputs).
- `.temp` / `.step temp` — implemented.
- `.op` / `.op pon|all` — implemented (`OpMode`, `device_op_points`).
- `.noise` — implemented (R/diode/MOSFET; BJT/JFET noise arrives with those devices per CONTEXT).
- `.include`/`.lib` — implemented (deck_dir + search_paths + RSPICE_LIB, sections, no leakage). LTspice `.lib <section>` inside a library that's missing = section boundary (quiet); RSpice already handles.
- `.global` — implemented (collect_global_nets, globals HashSet, mangling skip). LTspice adds: node 0 always global; `$G_`-prefixed names auto-global. RSpice should add `$G_` auto-global.
- `.subckt` / `.ends` / `X` — implemented with PARAMS:, bare key=value, instance overrides, nesting, forward refs. LTspice `PARAMS:` + `&lt;string expression&gt;` model names (`<modname>\|<string expression>`) and `ako:` model derivation (`model--defineaspicemodel.htm`) — RSpice `.model` should support `ako:` (derive one model from another overriding params) and string-expression model names.
- `.param` / `.func` — implemented. LTspice 24.1+: no braces needed except ambiguity; defaults PI, BOLTZ, ECHARGE, PLANCK, KELVIN, GMIN; reserved `temp`; string params; functions incl. `flat`, `mc`, `select`, `table(…, .include "file")`; `select()` for string indexing. RSpice param language should add these missing functions and defaults, and the `mc(a,tol)` Monte-Carlo function.
- `.model` — implemented for diode/MOSFET. Missing types: SW/CSW (done? Switch model `SW(RON ROFF VT VH)` exists per CONTEXT — check it's a `.model` type), URC, LTRA, MESFET (NMF/PMF), IGBT (NIGBT/PIGBT), VDMOS, plus `ako:` derivation. See §4.
- `.ic` — implemented (V(n)= and I(L)= per `ic--setinitialconditions.htm` — confirm inductor current `I(L1)=` is parsed; RSpice `parse_ic_entries` may only do V(n)). LTspice `.ic` supports `I(<inductor>)=<current>`; treat inductor as infinite-impedance current source at op. Verify RSpice handles `I(L1)=`.
- `.nodeset` — implemented.

## 5.5. Gap catalog — numerical / optimization techniques reverse-engineered from `LTspice.exe`

This section lists numerical and optimization techniques found by reverse-engineering the shipped `LTspice.exe` binary (version 26.0.2, SHA-256 `a94eb1789084db9f46375cce05110e03578f9cdb931867a0faaca5b200793f06`) using REA with Rizin and Ghidra. Evidence IDs are cited per finding; all are observed from the shipped artifact, not from documentation.

### Evidence base

- **PE imports** (`inspect_pe_imports`, ev_af890a52): LTspice links only Windows system DLLs (`KERNEL32`, `USER32`, `ADVAPI32`, `WS2_32`, `OLE32`, `SHELL32`, `RPCRT4`, `VERSION`). Notably absent: no BLAS/LAPACK, no MKL, no KLU/UMFPACK/SuperLU shared library, no Eigen, no OpenMP runtime DLL. The sparse solver and threading are implemented in-house.
- **Rizin string search of `.data`/`.rsrc`** (`/z`): found algorithm-related strings embedded in the binary.
- **Rizin trace strings** (`iz~avx`): found `.trace` source filenames for AVX2-optimized math.

### Techniques found in LTspice (missing from RSpice)

| # | Technique | Binary evidence | RSpice status | Where to implement |
|---|-----------|-----------------|---------------|--------------------|
| 5.5.1 | **Sparse direct solver (KLU)** | String `klu::ltspice::KLUSolver<ltspice::solver_traits::UFloat64>` found in `.data` (ev_b1428b27). SuiteSparse KLU — sparse LU factorization with Markowitz ordering, factorization reuse across NR iterations. | **Missing.** RSpice uses dense `nalgebra` LU, rebuilt/cloned per iterate (TODO §3 §3.1). | New sparse matrix module in `circuitsim/src/mna.rs` — `SparseMnaSystem` generified like `MnaSystem<T>`, symbolic Markowitz ordering at netlist load, numeric factorization reuse across NR iterations/timesteps. Interface: `analyze()` (symbolic, once) + `factorize()` (numeric, per iterate) + `solve()`. |
| 5.5.2 | **Gmin stepping** | 9 string hits for `gmin` across `.text`/`.rdata`/`.rsrc` (ev_95472647). Adaptive conductance-stepping retry ladder for DC convergence (shrinking `gmin` decades). | **Partially present.** RSpice has `gmin` floor and `gminsteps` ladder in `RunConfig` (CONTEXT §3), but the binary confirms it's a core convergence technique LTspice relies on. | Confirm `gminsteps` ladder is fully wired in `analysis.rs` NR retry loop; add `mindeltagmin` termination limit (`.options mindeltagmin=1e-4`). |
| 5.5.3 | **Source stepping** | 2 string hits for `srcstep` (ev_67569775). Source-scaling retry ladder as fallback when gmin stepping fails. | **Partially present.** `srcsteps`/`itl6` in `RunConfig`; CONTEXT says ladder is pinned structurally with budget-exhaustion errors in tests. | Verify source-stepping rungs actually solve decks plain stage fails on; add `srcstepmethod` knob to select algorithm variant. |
| 5.5.4 | **Relative/absolute tolerances (`reltol`/`abstol`)** | `reltol` (6 hits, ev_ccd5e501), `abstol` (6 hits, ev_99e6df01) embedded in binary. | **Present.** `reltol`/`abstol` in `RunConfig` defaults (1e-3 / 1e-12). | Already implemented; confirm they're consumed by NR and LTE controllers. |
| 5.5.5 | **Transient tolerance (`trtol`)** | 4 string hits for `trtol` (ev_fccffd03). Transient error tolerance (estimate of truncation-error overestimation factor, default 2.0 per LTspice 24 changelog). | **Partially present.** RSpice has LTE adaptive stepping with `reltol`/`abstol`/`vntol`; `trtol` not in `RunConfig`. | Add `trtol` to `RunConfig`; use as the truncation-error overestimation factor in the adaptive step controller (`analysis.rs` LTE). |
| 5.5.6 | **Trapezoidal integration** | 1 string hit for `trapezoid` (ev_eaee4497). | **Present.** Trapezoidal is the default companion model; `gear1`/`gear2` selectable (CONTEXT §3). | Already implemented; `trtol` (5.5.5) is the missing knob for it. |
| 5.5.7 | **AVX2-optimized transcendental math** | `.trace` source filenames: `sin_avx2.c`, `cos_avx2.c`, `tan_avx2.c`, `asin_avx2.c`, `acos_avx2.c`, `atan_avx2.c`, `sinh_avx2.c`, `cosh_avx2.c`, `tanh_avx2.c`, `exp_avx2.c`, `log_avx2.c`, `log10_avx2.c`, `exp2_avx2.c`, `pow_avx2.c`, `log2f_avx2.c` (ev_746c1aea). SIMD-optimized vector math for device equations and behavioral sources. | **Missing.** RSpice uses scalar `libm`/`libm`-equivalent `f64` math. | Add `std::simd`-based (or `arch`-targeted) fast math for `exp`/`log`/`sin`/`cos`/`pow` used in device model eval (`models.rs`) and B-source expression eval (`params.rs`). Gate on `target_feature = "avx2"` with scalar fallback. |
| 5.5.8 | **In-house multithreading (Win32, not OpenMP)** | Strings `CWinThread`, `GetCurrentThreadId`, SRW lock/R6016/R6017 multithread errors (ev_93a4c532); PE imports `CreateThread`, `AcquireSRWLock*`, `GetLogicalProcessorInformationEx`, `SetProcessAffinityMask`. No OpenMP DLL linked. | **Missing.** RSpice is single-threaded; `.step`/`.mc` runs run sequentially (CONTEXT §11 beyond-parallelism is a separate goal, but basic threading is absent). | Add a thread pool in `circuitsim/src/analysis.rs` or a new `circuitsim/src/threadpool.rs`: distribute `.step`/`.mc` runs across worker threads, each with its own `MnaSystem`/NR. Use `std::thread` + crossbeam or std mpsc; pin CPU affinity optionally. |
| 5.5.9 | **LLVM JIT for behavioral-source expressions** | Strings `llvm::orc::ThreadPool`, `DynamicThreadPoolTaskDispatcher`, `OpenMPOptPass`, `OpenMPIRBuilder`, `OpenMPOptCGSCCPass` (ev_93a4c532, ev_622f68d5). LTspice JIT-compiles behavioral-source (`B`) expressions via LLVM ORC for fast evaluation. | **Missing.** RSpice evaluates B expressions via an AST interpreter (`params.rs`, `BehavioralRuntime`). | Optional advanced item: JIT-compile `BExpr` AST to native code via a lightweight expression compiler or `cranelift`/`llvm`; otherwise keep AST but add expression caching. P2. |

### Techniques NOT found in the binary

- **"newton"** — 0 string hits. LTspice may not embed the literal word; NR is implemented without the string.
- **"adaptive"** — 0 hits. Adaptive timestep control exists (per help docs) but the string is absent.
- **"vntol"** — 0 hits. Absolute voltage tolerance exists as `.options vntol` but the string may not be embedded.

### Implementation priority

1. **Sparse KLU-style solver** (5.5.1) — biggest performance gap; dense LU is infeasible beyond ~few thousand nodes (TODO §3 §3.1).
2. **AVX2 fast math** (5.5.7) — measurable speedup for device/B-source eval; low risk with scalar fallback.
3. **Multithreaded `.step`/`.mc`** (5.5.8) — parallelize independent runs; required for the "advanced and fast" claim (TODO §11).
4. **`trtol` knob** (5.5.5) — small addition to complete the tolerance set.

## 6. Gap catalog — `.MODEL` types and model-card language

| # | LTspice model feature | RSpice status | Where to implement |
|---|-----------------------|---------------|--------------------|
| 6.1 | `.model … SW` / `CSW` (voltage/current-controlled switch model) | Switch elements S/W exist with `SW(RON ROFF VT VH)` per CONTEXT; confirm it's backed by a `.model SW` type with full param list (Vt, Hyst, Ron, Roff). | `models.rs` `SwitchModel`, `dotcards.rs` model parsing, `elements.rs` S/W. |
| 6.2 | `.model … URC` / `.model … LTRA` | Missing (lossy/uniform RC lines). | `models.rs`, `dotcards.rs`, elements `U`/`O`, `mna.rs`. |
| 6.3 | `.model … NMF/PMF` (MESFET), `.model … NIGBT/PIGBT` (IGBT), `.model … VDMOS` | Missing element families `Z` (+ these model types). | `models.rs`, elements `Z`, `mna.rs`. |
| 6.4 | `.model <new> ako: <source> [type] (params)` — derive a model from another, overriding parameters | Missing. | `dotcards.rs` model-card walk: resolve `ako:` source model, copy, apply overrides. Validate type matches. |
| 6.5 | `<modname>\|<string expression>` model names (model selection via string param, used with `.step`/`.param select()`) | Missing. | `dotcards.rs`/`.model` parser: allow a string expression that resolves at expansion time to a model name. |
| 6.6 | Duplicate `.model`/`.func`/`.param`/`.subckt` accepted if identical (same file/location); unknown `.model` params silent warnings by default (`.options allow_ambiguous_models`) | RSpice warns on unknown directives/elements and rejects unknown model names (`validate()`). Add `allow_ambiguous_models` option and identical-duplicate dedupe. | `dotcards.rs`, `validate()`. |

## 7. Gap catalog — analyses

| # | Analysis | LTspice behavior | RSpice status | Where to implement |
|---|----------|------------------|---------------|--------------------|
| 7.1 | `.FRA` (time-domain frequency response) | Swept-sine loop-stability analysis driven by `@` FRA devices; writes `<circuit>.fra_<name>.raw`. | **Missing.** (Requires 4.6, 5.7.) | New `Fra` analysis mode, `@`/`&` elements, `.fra` directive. P2. |
| 7.2 | `.NET` (network parameters Y/Z/H/S, 1- and 2-port) | Computed during `.AC` with terminations. | **Missing.** | `.net` card + AC post-processing (`analysis.rs`). P2. |
| 7.3 | `.ac` LIST / `file=` frequency lists | Missing (oct/dec/lin only). | `AcCommand` add `List(Vec<f64>)`/`File(PathBuf)` scale; reader in `rawwave.rs`. |
| 7.4 | `.dc` oct/dec sweeps, `file=` | Missing (lin ramp + LIST only). | `DcSweepValues` add Oct/Dec/File. |
| 7.5 | `.sens` (DC sensitivity), `.pz` (pole-zero), `.disto` (harmonic distortion) | PSpice small-signal secondary analyses. | **Missing.** P2. New analysis modes. |
| 7.6 | `.meas` results → SQLite `.db` (Parameters/Steps/Measurements/Results) | LTspice ≥24.1.9 saves `.meas` into SQLite; plottable via "Plot .step'ed .meas data". | **Missing.** Add `.measdb`/CLI option emitting SQLite with the four-table schema (`measure--evaluateuser-definedele.htm` shows the schema diagram). P2. |
| 7.7 | Noise models for BJT/JFET | `.noise` done for R/diode/MOSFET; BJT/JFET noise arrives with those devices. | Add BJT (`Q`) and JFET (`J`) shot/flicker/thermal noise to the adjoint `.noise` path (`analysis.rs` noise, `mna.rs` noise stamps). |

## 8. Gap catalog — `.OPTIONS`, `.PARAM`/`.FUNC`, and B-source language

### 8.1 `.options` keys missing from `RunConfig`

From `options--setsimulatoroptions.htm` not yet in `RunConfig`: `chgtol`, `cshunt`, `cshuntintern`, `debugtran`, `defad`, `defas`, `defl`, `defw`, `delay`, `fastaccess`, `flagloads`, `gfarad`, `gfloat`, `itl2`, `itl6` (alias of `srcsteps`), `list`, `logparams`, `logopinfo`, `maxclocks`, `maxstep` (already on `.tran`), `meascplxfmt`, `measdgt`, `minclocks`, `mindeltagmin`, `no_caret_warning`, `nomarch`, `noopiter`, `numdgt`, `pivrel`, `pivtol`, `plotreltol`, `plotvntol`, `plotabstol`, `plotwinsize`, `ptrantau`, `ptranmax`, `reject_number_tails`, `solver`, `srcstepmethod`, `sstol`, `ststclocks`, `ststdelay`, `temp`, `thev_induc`, `tnom`, `topologycheck`, `trtol`, `trytocompact`.

**Implementation:** Add to `RunConfig` as needed (the ones the engine actually consumes: tolerances, iteration budgets, gmin ladders, integration method, plot compression tolerances `plotreltol/plotvntol/plotabstol/plotwinsize`, temperature `temp`/`tnom`). Unknown keys keep the existing warn-and-ignore pattern (`dotcards.rs:470`). Defaults per LTspice help table. Emit round-trip in `dotcards.rs` emit (the `~.options method= reltol= abstol= vntol=` line already exists; extend it).

### 8.2 `.param` / `.func` functions missing from the parameter grammar

LTspice `param--user-definedparameters.htm` numerical functions not clearly in RSpice: `flat(x)`, `mc(x,y)`, `select(x,a,b,…)` (numerical and string), `table(x, .include "file")`, plus defaults PI, BOLTZ, ECHARGE, PLANCK, KELVIN, GMIN, reserved `temp`. RSpice CONTEXT says math library + user functions live in `params.rs`. Add the missing functions and defaults there.

### 8.3 B-source (`B`) extensions missing

LTspice `barbitrarybehavioralvoltageorcur.htm`: `R=<expr>` (behavioral resistor), `P=<expr>` (behavioral power, `vprx` smoothing), `ic`, `tripdv`, `tripdt`, `Rpar`, `laplace= [window=] [nfft=] [mtol=]`. RSpice B source has `V=`/`I=` + ABM `laplace`/`FREQ`/`CHEBYSHEV` on E/F/G/H, and `idt`/`sdt`/`delay`/`table()`/`white`/`flicker` on B. Add `R=`, `P=`, `tripdv/tripdt`, and transient-domain `laplace`/`window`/`nfft`/`mtol` to the `B` element (`circuit.rs` `BehavioralSource`/`BehavioralKind`, `elements.rs` B parse/emit, `mna.rs` B stamp, `rawwave.rs` reporting).

## 9. Gap catalog — waveform viewer, GUI, and interop

| # | Area | LTspice behavior | RSpice status | Where to implement |
|---|------|------------------|---------------|--------------------|
| 9.1 | Cursors | Crosshair cursors with readouts and measured deltas (dual cursors; cursor measurements). | **Missing.** | `circuitsim-gui/src/plot.rs`/`plot_frame.rs` — cursor state + readout + delta annotations. |
| 9.2 | Waveform arithmetic | Add arbitrary expression traces (`V(out)*I(V1)` power, scaled, offset, combined). | **Missing.** | GUI plot: expression trace type, evaluate against raw data per point. |
| 9.3 | FFT | FFT of transient traces with configurable windowing (Blackman-Harris et al.). | **Missing.** | GUI: FFT panel + window function; reuse `.four` or a standalone FFT. |
| 9.4 | Stepped/parametric overlays | All `.step` runs on one plot with per-run identification; PSpice performance-analysis curves (measurement vs swept parameter). | Partially (results tagged per step; GUI overlays open). | GUI `plot.rs` overlays + legend per run; measurement-vs-parameter curve from `.meas`/`.step`. |
| 9.5 | Axis completeness | linear/log both axes, physical unit auto-detection & formatting (Hz/s/V/A/dB/°), multi-pane layouts. | Partially (log grid, dB mode, AcMagScale/AcCursor exist). | `plot.rs` axis formatting, multi-pane. |
| 9.6 | Cross-probing | Click wire/part in schematic → add `V(net)`/`I(device)` in viewer; and vice versa. | **Missing.** | GUI: schematic↔plot wire-up; probe a pin/net adds a trace. |
| 9.7 | Waveform file I/O | Read/write binary & ASCII `.raw` (LTspice) / `.dat` (PSpice), incl. step-run metadata. | `.raw` done (binary + ASCII, stepped runs). `.dat` (PSpice) missing. | `rawwave.rs` add PSpice `.dat` reader/writer. |
| 9.8 | Full export surface | CSV of arbitrary traces (voltages, currents, measurements); image export. | Only `.tran` node voltages exportable today. | CLI `--output`/GUI export: all traces + measurements; image export (PNG/SVG). |
| 9.9 | Waveform compression/decimation | `.options plotwinsize`, `plotreltol`, `plotvntol`, `plotabstol`; LTspice fast-access format. | **Missing.** | `rawwave.rs`/analysis: compression during `.tran`; `.options plot*`. |
| 9.10 | Persist plot configurations | Trace sets, scales, window layouts saved/reused (`.plt` plot settings). | **Missing.** | GUI settings persistence. |
| 9.11 | Schematic symbols | Symbols for every device family (diode, MOSFET, BJT, I, controlled/behavioral sources, switches, transformers, T-lines, opamps). Editor is `R C L V Gnd`-only. | **Missing.** | `circuitsim-gui` symbol library: new symbols + `CompKind` per family; attach `model=`/`.subckt` references. |
| 9.12 | Symbol library format + component picker | `.asy`/symbol with attached model/spiceline data; picker. | **Missing.** | GUI: symbol format, library search paths (`symbolandlibrarysearchpaths.htm`), picker dialog. |
| 9.13 | Net/wire labels | Clean node names governing netlist & plot names. | Partially (named nodes parsed). | GUI: wire labels → node names in `build_netlist`. |
| 9.14 | SPICE text directives as schematic annotations | `.tran`, `.step`, `.meas`, `.include` as first-class schematic objects (comment/directive toggle). | **Missing.** | GUI: directive text objects on the schematic; toggle comment/directive; `Configure Analysis` toolbar mapping tabs ↔ schematic text. |
| 9.15 | Hierarchical schematics | Map to `.subckt` definitions/instances. | **Missing.** | GUI: hierarchy blocks ↔ `.subckt`. |
| 9.16 | ERC | Dangling-pin, shorted-output, floating-node diagnostics + connectivity highlighting; undo/redo completeness. | **Missing.** | GUI: `topologycheck`-style checks, DRC/ERC pass. |
| 9.17 | Persistent configuration/preferences | Window state, library paths, theme (forced dark today). | **Missing.** | GUI: settings save/load (`.ini`-like), theme. |

## 10. Gap catalog — advanced analysis (PSpice-centric, P2)

| # | Feature | LTspice/PSpice behavior | RSpice status | Where to implement |
|---|---------|----------------------|---------------|--------------------|
| 10.1 | Monte Carlo | Landed (`.mc`, LOT/DEV, seed, histogram, yield, `random()`/`gauss()`). | **Done.** | — |
| 10.2 | Worst-case analysis | Ranked component/tolerance contributions to a measurement. | **Missing.** | `mc.rs`: sensitivity ranking across MC runs. |
| 10.3 | Sensitivity analysis | Ranked parameter sensitivities of a target measurement. | **Missing.** | `.sens`-style adjoint or finite-difference sensitivities. |
| 10.4 | Optimizer | Curve-fitting parameter tuning against measurement goals (goal-function library, user-defined goals). | **Missing.** | Optimizer loop over `.step`/`.meas`. |
| 10.5 | Smoke test | Device stress checks (max V/I/power/temperature from model cards) + derating tables. | **Missing.** | Post-run check against `.model` max ratings. |
| 10.6 | Performance analysis | Measurement-vs-parameter curves across parametric runs. | **Missing.** | GUI (9.4) + `.meas`/`.step` integration. |
| 10.7 | Digital primitives & mixed signal | Gates, flip-flops, `PINDLY`/`CONSTRAINT`, `DIGSTIM`/`DIGCLOCK`/`STIM*`/`FILESTIM`, A/D & D/A bridges, analog co-simulation during `.tran`. | **Missing.** | Digital element family + event-driven co-solver. Large; P2/PS. |
| 10.8 | IBIS | IBIS I/O model translation & simulation. | **Missing.** | IBIS parser + I/O buffer model. P2/PS. |

## 11. Gap catalog — model libraries & interop

| # | Feature | LTspice behavior | RSpice status | Where to implement |
|---|---------|------------------|---------------|--------------------|
| 11.1 | Bundled model libraries | `standard.lib`, `opamps.lib`, `nom.lib` shipped; include/lib search + user dirs. | **Done** (`libs/standard.lib`, `opamps.lib`, `nom.lib`; `-I`, `RSPICE_LIB`, bundled tree, deck dir). | — |
| 11.2 | Model authoring/import tooling | Model Editor-style tooling (datasheet parameters → model cards); LTspice symbol authoring (`.asy`). | **Missing.** P2. | GUI/model-authoring tool (out of engine scope). |
| 11.3 | Encrypted/protected models | LTspice-encrypted subcircuits; PSpice protected libraries. `-encrypt` CLI (`commandlineswitches.htm`). At minimum: detect and fail loudly with actionable messages. | **Missing.** | Detect encrypted blocks at parse; `validate()`/parse error with a clear message. CLI `-encrypt` not needed (RSpice isn't ADI). |
| 11.4 | `.raw` fast-access format | `.options fastaccess`; `FastAccess` flag; batch conversion `-FastAccess`. | **Missing.** `rawwave.rs` reader tolerates `FastAccess` column-major payloads; writer doesn't emit it. | `rawwave.rs` fast-access writer. |

## 12. Gap catalog — CLI & documentation

| # | Feature | LTspice behavior | RSpice status | Where to implement |
|---|---------|------------------|---------------|--------------------|
| 12.1 | Batch mode `-b` | `ltspice.exe -b deck.cir` leaves `deck.raw`; `-sync` writes to stdout instead of dialog; `-Run` simulates on open. | **Done** (`-b/--batch`, exit codes, `--output`, `--waves`). | — |
| 12.2 | Netlist extraction | `-netlist` (schematic→netlist), `-PCBnetlist` (schematic→PCB netlist) batch conversion. | **Missing.** | CLI: `--netlist` to emit a flattened SPICE netlist from a schematic/deck. |
| 12.3 | `-version` | Print LTspice version. | CLI version flag? Check `circuitsim-cli`. | `circuitsim-cli/src/main.rs` `--version`. |
| 12.4 | `.meas` SQLite db | See 7.6. | **Missing.** | `mc.rs`/new `measdb.rs` emitting SQLite. |
| 12.5 | Manual / command reference | Professional-grade documentation (command reference, netlist format rules, worked examples). | **Missing.** P2. | `docs/` manual + worked examples. |

## 13. Implementation template (pick up an item)

For any gap above, follow this checklist:

1. **Read the LTspice help page** named in §1 for the exact syntax, defaults, and parameter table. Treat the help page as the spec; where it's silent, flag "verify against shipped docs."
2. **Locate the integration point** using `CONTEXT.md` vocabulary:
   - New element letter → `circuitsim/src/netlist/elements.rs` (parse + emit), add family to `Circuit`/`ElementFamily`, stamp in `circuitsim/src/mna.rs`, report currents in `circuitsim/src/circuit.rs` (`BranchCurrents`), symbol in `circuitsim-gui`.
   - New dot command → `circuitsim/src/netlist/dotcards.rs` (parse + emit), `AnalysisCommands` field in `circuitsim/src/netlist/mod.rs`, analysis driver in `circuitsim/src/analysis.rs`, `.raw` traces in `circuitsim/src/rawwave.rs`.
   - New `.options` key → `RunConfig` in `mod.rs`, parse in `dotcards.rs` `.options` walk (warn-and-ignore for unknowns), emit round-trip.
   - New `.param`/`.func` function → `circuitsim/src/netlist/params.rs` math library.
   - New `.model` type → `circuitsim/src/models.rs` + `dotcards.rs` model-card dispatch.
   - GUI → `circuitsim-gui/src/` (`schematic.rs`, `plot.rs`, `app.rs`, symbols).
   - CLI → `circuitsim-cli/src/`.
3. **Write a regression test** — `.cir` fixture under `examples/` + pinned assertion in `circuitsim/tests/regression.rs`, ideally cross-checked against LTspice `.raw` output (`--read-raw` exists) or an analytic reference.
4. **Update `TODO.md`** (add a `[ ]` item tagged by source tool `(LT)`/`(PS)`/`(both)`) and mark the item `[done]` here when landed.
5. **Update this doc** — move implemented items to a "Done" subsection and keep the evidence file paths accurate.

## 14. Evidence index (files consulted)

LTspice install root: `/home/cachy/.wine/drive_c/Program Files/ADI/LTspice/`
- `LTspice.json` — version/install manifest (26.0.2, installed 2026-05-07)
- `LTspiceChangeLog.txt` — release notes through 2026-04-29
- `LTspiceHelp/dotcommands.htm` — full dot-command index
- `LTspiceHelp/circuitelements.htm` — element list + syntax
- `LTspiceHelp/options--setsimulatoroptions.htm` — full `.options` keyword table (defaults)
- `LTspiceHelp/param--user-definedparameters.htm` — `.param` defaults + function table (incl. `flat`, `mc`, `select`, `string` ops)
- `LTspiceHelp/func--userdefinedfunctions.htm` — `.func` dynamic scoping
- `LTspiceHelp/model--defineaspicemodel.htm` — model types table (SW, CSW, URC, LTRA, D, NPN/PNP, NJF/PJF, NMOS/PMOS, NMF/PMF, NIGBT/PIGBT, VDMOS) + `ako:` derivation
- `LTspiceHelp/barbitrarybehavioralvoltageorcur.htm` — B source syntax + function table (incl. `white`, `random`, `idt`, `sdt`, `delay`, `table`, `laplace`, `tripdv/tripdt`)
- `LTspiceHelp/aspecialfunctions.htm` — A-device gates (INV/BUF/AND/OR/XOR/SCHMITT/…/VARISTOR/MODULATE/OTA) + parameter tables
- `LTspiceHelp/machine--arbitrarystatemachine.htm` — `.machine`/`.state`/`.rule`/`.output`/`.endmachine`
- `LTspiceHelp/measure--evaluateuser-definedele.htm` — `.MEAS` syntax + SQLite schema (4 tables)
- `LTspiceHelp/net--computenetworkparametersina.htm` — `.NET`
- `LTspiceHelp/dotfra.html` + `fra_device.html` + `fra_probe.html` — `.FRA`, `@`, `&`
- `LTspiceHelp/olossytransmissionline.htm` — `O` LTRA model parameter table
- `LTspiceHelp/uuniformrc-line.htm` — `U` URC model parameter table
- `LTspiceHelp/zmesfetandigbt.htm` — `Z` MESFET/IGBT parameter tables
- `LTspiceHelp/save--limitthequantityofsaveddat.htm` — `.save` (wildcards, `: hierarchy`)
- `LTspiceHelp/savebias--saveoperatingpointtodi.htm` + `loadbias--loadapreviouslysolvedd.htm` — `.savebias`/`.loadbias`
- `LTspiceHelp/savestate.htm` + `loadstate.htm` — `.savestate`/`.loadstate`
- `LTspiceHelp/dotkeep.htm` — `.keepnode`
- `LTspiceHelp/wave--writeselectednodestoawavfi.htm` — `.wave`
- `LTspiceHelp/commandlineswitches.htm` — `-b`, `-ascii`, `-netlist`, `-PCBnetlist`, `-encrypt`, `-FastAccess`, `-version`, `-I`
- `LTspiceHelp/ic--setinitialconditions.htm` — `.ic V(n)= / I(L)=`
- `LTspiceHelp/global--declareglobalnodes.htm` — `.global` (`$G_` auto-global)
- `LTspiceHelp/backanno--annotatethesubcircuitp.htm` — `.backanno`

RSpice repository: `/mnt/G/files/Programming/RSpice/`
- `CONTEXT.md` — glossary & architecture vocabulary (single source of truth for terms)
- `TODO.md` — existing roadmap with status checkboxes (the place to add new P0/P1/P2 items)
- `circuitsim/src/netlist/mod.rs` — `AcCommand`, `DcCommand`, `DcSweepSpec/Values`, `StepSpec/Values`, `McCommand`, `RunConfig`, `AnalysisCommands`, `TfCommand`, `FourCommand`, `NoiseCommand`, `Measurement/MeasKind`
- `circuitsim/src/netlist/dotcards.rs` — parse/emit for every dot command (pattern to imitate)
- `circuitsim/src/netlist/elements.rs` — parse/emit per element letter (pattern for new families)
- `circuitsim/src/netlist/models.rs` — `DiodeModel`, `MosfetModel`, device equations (pattern for new model types)
- `circuitsim/src/netlist/params.rs` — `ParamEnv`, math library, user functions (`.param`/`.func`)
- `circuitsim/src/netlist/imports.rs` — `.include`/`.lib` resolution (deck_dir + search_paths + RSPICE_LIB)
- `circuitsim/src/netlist/subckts.rs` — flattening, mangling, `.global` nets, subckt bodies
- `circuitsim/src/netlist/waveforms.rs` — source waveform generators (PULSE/SIN/EXP/PWL/…)
- `circuitsim/src/netlist/rawwave.rs` — `.raw` writer/reader (binary + ASCII, stepped runs)
- `circuitsim/src/mna.rs` — MNA stamping, companion models, integrators (`Companion`, `BehavioralRuntime`)
- `circuitsim/src/analysis.rs` — NR loop, `.op`/`.dc`/`.tran`/`.ac`/`.noise`/`.tf`/`.four`/`.meas`/`.mc`
- `circuitsim/src/circuit.rs` — `Circuit`, `BranchCurrents`, device op points
- `circuitsim-gui/src/` — egui schematic + plot tabs (`plot.rs`, `plot_frame.rs`, `schematic.rs`, `app.rs`)
- `circuitsim-cli/src/` — CLI batch flags
- `circuitsim/tests/regression.rs` — golden-result regression corpus

## 15. Done items (migrated here as they land)

*(none yet — this document is the baseline; move completed items here with the commit that lands them)*
