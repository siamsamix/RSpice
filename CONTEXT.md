# RSpice — Domain Glossary

Words used consistently across the three modules (`circuitsim`, `circuitsim-gui`, `circuitsim-cli`).
Use these terms in code names, tests, and reviews. Marked `(cut #N)` where a name was agreed for a
deepening that has not landed yet.

## Circuit language

- **Netlist** — SPICE-style text: element lines, directives (`.op` `.dc` `.tran` `.ac` `.model` `.param` `.func`), ends with `.end`. `*` comments only at line start, `;` inline, `+` continues the previous line. Parsed into a `Netlist` (`Circuit` + `AnalysisCommands` + `source` + `params` + `funcs` + `mc_seed` + `options`/`overrides`) by the `netlist/` module family: `mod.rs` holds the vocabulary and the parse/emit pipeline, and every card family lives in its own module with its parse and its emit co-located (`elements`, `models`, `dotcards`, `waveforms`, `subckts`, `imports`, `nodeplan`) — a card's grammar is one file, not parse here emit there. `Netlist::param_env()` seeds the deck's parameter/function environment, so a run and its re-parses share one env; `mc_seed` travels on the deck for reproducible Monte Carlo.
- **Source value (bridge)** — the value of a `V`/`I` card (`DC 5`, `AC 1 30`, `PULSE(…)`), typed as `netlist::SourceValue` with the round trip `parse_source_value` / `emit_source_value` at the tail of `netlist/mod.rs`. Frontends edit and display source values through this bridge, so whatever a GUI writes back is text the deck parser reads identically; the writer prints canonical decimal (no SPICE suffixes), which always re-parses to the same number.
- **Netlist.warnings** — best-effort import diagnostics, never fatal: unknown directives (echoed with their arguments), unsupported element letters, stray `.endl` markers, unknown `.model` parameters. A third-party deck imports and simulates with warnings surfaced in both front-ends; ordering follows the parse pipeline (flatten first, card walk second).
- **Element** — one component line. The first letter names the kind: `R` resistor, `C` capacitor, `L` inductor, `V` voltage source, `I` independent current source, `D` diode, `M` MOSFET. `X` is now also a kind: subcircuit instantiation (unmarshalled at parse time into its body's elements — `Circuit` never sees an `X`).
- **Element family** — `ElementFamily`, the *named kind* of an element; `CURRENT_FAMILIES` fixes the canonical order `R C L V D M Q J I B`. Builders and report walkers name families instead of pattern-matching raw structs, and the frontends hold the `Circuit` itself — adding a device letter means one family + its model, not edits scattered across screens and tabs.
- **Subckt definition** — a `.subckt name p1 p2 … / … / .ends` block, collected before any card walk (`collect_subckts`) so bodies may be referenced before their text appears. The header carries default parameters after the ports (`name a b PARAMS: rr=1k` and bare `key=value` forms both accepted); the body may nest definitions and `.param`/`.func` cards.
- **Subcircuit instance (X line)** — `XName p1 … pn defname key=value …`: the port tokens bind to the caller's nets (not mangled), the trailing assignments override the defaults and may reference the parent env, and instance keys beat a body `.param` card re-assigning the same name (the reference tools' `PARAMS=` precedence).
- **Mangling (instance path)** — flattening rewrites a body's internal nets and element names to the instance path (`X1.mid`, `X1.R1`, nested `X1.X2.mid`), so two instances of one definition never share a net or a branch-report slot. `.global` nets, `gnd` and `0` never mangle; `element_letter` reads the kind from the segment after the last dot.
- **Node / ground** — a connection point, addressed as `NodeId`. Node `0` is ground and owns no matrix row; node `0` and `gnd` are the same net.
- **Node plan / node names** — the parser pre-scans every node field of the deck (`scan_node_plan`) so alphanumeric names (`in`, `out`) resolve case-insensitively on first spelling, numeric nodes keep their decimal text as their name, and the final `Circuit` carries `node_names` indexed by id. After parsing, `compact_node_ids` renumbers used ids to `0..n-1` — hole-free ids keep MNA rows one-per-node (an unused id would be a singular unstamped row); names survive compaction.
- **Parameter** — a named value from `.param`: `Num` or `Str`; every element line expands `{expr}` and `'expr'` against the `ParamEnv` in deck order, so same-card references work and `.func` bodies stay unexpanded until called. The math library and user functions live in `params.rs`.
- **ParseOptions** — where file-reading directives look: `deck_dir` (the parsed `.cir`'s own directory, set by the CLI) and `search_paths` (`RSPICE_LIB` split-paths, prepend order). `.include <file>` and `.lib <file>` (with optional section) resolve against deck_dir then the search list; a missing file or section is a loud parse error, an unresolvable `.lib <section>` *inside a library* is just a section boundary.
- **Stamp** — writing one element's contribution into the MNA matrix/right-hand side at build time. Not a runtime mutation of results.
- **Model card** — a `.model` line defining device physics (`DiodeModel`, `MosfetModel` level-1 Shichman–Hodges). Elements reference cards by name; `validate()` rejects unknown names.
- **Model card grammar** — the full card: fused-paren (`NMOS(…)`) and paren-less (`D IS=…`) forms, type-word slicing safe against names that contain the search string, keys case-normalized, LEVEL validated against the device type, values read via `parse_value` (trailing `)`/`%`/`,` tolerated), complete diode/MOSFET parameter lists, `DEV`/`LOT` slots parsed into `ModelTolerance` (staged for the Monte Carlo work, TODO §8), and unknown families kept as `GenericModel` cards for future element letters.
- **Sizing fields** — per-instance junction geometry: diode `AREA=…` (bare trailing number also accepted) and MOSFET `W/L` plus `AD/AS/PS/PD`, carried onto element fields and consumed by the device model (`eval_for(vd, area)`), not just stored.
- **DeviceModel** — the module (`model.rs`, cut #1) owning device equations. Interface: `eval` takes f64 biases, returns f64 linearization (gm, gds, gbs, current equivalent); AC lifts values into Complex64 at the stamp, never inside the model.

## Analysis language

- **MNA** — modified nodal analysis. First rows are node voltages, then **branch rows**: voltage-source currents, inductor currents (in that order).
- **BranchLayout** — the branch-row layout as a named structure built with the system (cut #3); accessors replace raw `n_nodes + i` arithmetic. Not owned by `Circuit`.
- **MnaSystem(T)** — the solver system `a`, `z` generified over the field: `f64` for DC/transient, `Complex64` for AC (cut #2). `ac_mna.rs` folds into this.
- **Field** — the small trait (`from_real`, arithmetic ops) letting one stamping implementation serve both fields.
- **Mode** — `Dc`, `Tran { dt }`, `Ac { omega }`: three modes of one system build, replacing three builders.
- **Operating point (`.op`)** — DC solve; nonlinear elements linearize via Newton–Raphson. Also the `DcResult` that seeds `.tran` and `.ac`.
- **DC sweep (`.dc`)** — a real sweep: one or two `DcSweepSpec`s over a `V`/`I` source or a parameter, values from linear start/stop/increment or `LIST`. Runs per point (`run_dc_sweep`), the second sweep nested inside the first, capped at `MAX_SWEEP_POINTS`. Results are `DcSweepResult`/`DcSweepPoint`; arguments are validated or rejected — never dropped.
- **Newton–Raphson (NR)** — the nonlinear convergence loop, tolerance 1e-6, 100 iterations. One loop shared by DC and transient (cut #4); `analysis` owns it, `mna` owns stamping.
- **Transient (`.tran`)** — backward-Euler companion models stepped in time; starts from the operating point, not zeros (fix lands with cut #4). The `UIC` keyword replaces that start: the first sample is the `.ic`-pinned state (inductor rows collapsed), the DC op is skipped entirely.
- **Initial conditions (`.ic` / `.nodeset`)** — `NodesetEntry` lists (`V(net)=…`, split spellings accepted, `I()` entries warn), names resolved case-insensitively against real nets — an unknown or ground entry is a loud error, never a silent seed. Inside a subckt body the entries are scope-mangled (`V(mid)=3` → `V(X1.mid)=3`). Distribution of roles: `.nodeset` only moves the DC Newton starting iterate (converges to the true operating point either way); `.ic` pins the UIC t=0 state.
- **AC sweep (`.ac`)** — frequency-domain small-signal analysis about the DC bias; stimulus amplitude comes from the voltage source's `AC` field, not a separate command setting.
- **Analysis command** — parsed directive config (`.tran`/`.ac` parameters, `AcCommand`, `AcScale`). Lives with the netlist format, not with analyses (cut #3 resolves the current mutual import).

## GUI language

- **Schematic** — the grid-drawn circuit in the egui app. `schematic.rs` owns the editor's state machine — `SchematicState` with its mutations (place by tool, rotate with wire re-attach, drag with wire stretch, wire open/close, select, cancel, deletes) and both deck conversions: `build_netlist` (schematic → `Netlist` through the shared value grammar) and `from_netlist` (deck → schematic through the one `parse`, drawing `R C L V` and counting the rest instead of dropping them). The canvas computes pointer geometry and asks the module to mutate. Node numbering comes from a union-find over grid connectivity — one implementation, shared by the writer and the painter.
- **Element kind / CompKind** — the GUI enum paralleling element letters; import draws `R C L V` and counts the rest (`D`/`M`/`I` have no symbol yet) instead of dropping silently.
- **Plot series / PlotState** — the tab plotting adapter: typed bounds state replacing the stringly scale fields, one `apply_bounds`, one element→series interface used by all tabs. Every result tab draws through `plot_frame` (axis labels, legend, gestures, the one-time bounds apply), and the four AC panels share `ac_plot_frame` with the frequency-cursor plumbing — log grid and 10ˣ→Hz hover labels in dB mode, plain ticks in linear mode; `AcMagScale` (default `Db`) and `AcCursor` live beside it. dB/phase math is GUI-internal, computed once, not re-derived per tab.
- **Tabs** — Overview, DC (operating point; `.dc` sweep as a plot when flat, a table when nested), Waveforms (Voltage / Current / Differential), Frequency (magnitude / phase / branch plots).

## Roadmap language

- **Integration method** — how `.tran` discretizes time: `Trapezoidal` (default), `Gear1`
  (backward Euler), `Gear2` (BDF2, variable-step coefficients). Selected through
  `.options method=…`. The integrator owns its companion models in one place (`mna.rs`):
  `Companion` (α/β₁/β₂/γ per method) is built by `Companion::for_step` — the only remaining
  `match method` — and `derivative(k, new, prev, older, &history)` is the one history consumer;
  `BehavioralRuntime` owns its own delay-queue history advance (`seeded`/`advance`), with
  `MAX_DELAY_SAMPLES` living beside the queue law it caps.
- **Adaptive step / LTE** — the transient controller doubles each proposed step and compares
  the full-step solution with the two-half-step one; the normalized difference is the local
  truncation error, accepted under `reltol`/`vntol`/`abstol`. Source edges and print-grid
  instants are hard boundaries.
- **Breakpoint** — an instant where a source waveform changes slope (`PULSE` edges, `EXP`
  corners, `PWL` points including repeats). The stepper clips to every breakpoint.
- **Temperature system** — `Circuit.temperature` (°C) plus model `TNOM`/`EG`/`XTI`/`TCV`/`BEX`
  and passive `TC1`/`TC2`; `.temp` with several values and `.step temp` produce one run per
  temperature. Device equations recompute `Vt = kT/q` and the bandgap law per evaluation.
- **Step run** — one cell of the `.step` cartesian product. `StepRun` carries the swept values
  and that run's analyses; `SimulationResult.runs` holds them all, with the first mirrored into
  the top-level fields for single-run consumers.
- **Measurement** — a `.meas` card parsed into `Measurement`/`MeasKind` and evaluated against
  sample points built from `.tran`/`.dc`/`.ac`/`.op`. A measurement parses once into a compiled
  program — `MeasProgram::compile` → `MeasPart::{Text, Node, NodeDiff, Current}` — and
  re-evaluates per sample: probes (`V(n)`, `I(el)`) become per-sample values (`MeasSample`,
  carrying `currents: BranchCurrents`; AC magnitudes collapse through `collapse_ac` in one
  place) and the rest evaluates through the parameter grammar. Failures are
  reported per measurement, never fatal.
- **Noise analysis (`.noise`)** — output-referred PSD from the adjoint system at each frequency
  (`Aᵀy = e_out`), summing resistor thermal, diode shot/flicker, and MOSFET channel noise;
  input-referred by dividing by |gain|², integrated over the sweep for RMS totals.
- **Controlled source** — `E`/`F`/`G`/`H` with one control per `POLY(n)` term and a constant
  first coefficient; `E`/`H` add a branch row, `F`/`G` inject a dependent current. Current
  controls read the controlling voltage source's branch unknown (element convention).
- **Switch** — `S` (voltage-controlled) or `W` (current-controlled) with an `SW(RON ROFF VT VH)`
  model, stamped as a conductance and hysteresis-fed by the previous state in transient.
- **Mutual inductance** — `Kname L1 L2 k`; `M = k√(L1L2)`, stamped as cross terms in the two
  inductor branch equations for every integrator.
- **Passive model card** — `.model … R/C/L` supplying a value and `TC1`/`TC2`; the element
  card may omit its value when the model supplies it. `Circuit::resistance_at` (and the C/L
  siblings) is the one temperature-scaling point.
- **Bundled library** — files under `libs/` (`standard.lib`, `opamps.lib`, `nom.lib`) resolved
  after the deck directory, `-I` paths, and `RSPICE_LIB`; the CLI adds the installed/checkout
  tree automatically.

- **Parity baseline** — the combined feature set of LTspice and OrCAD/PSpice (incl. Advanced Analysis)
  that the roadmap in `TODO.md` measures RSpice against.
- **Parity gap** — a capability in a parity-baseline tool that RSpice lacks, or provides in weakened
  form; each gap is one `TODO.md` item tagged by source tool (`both` / `LT` / `PS`).
- **Degenerate directive** — an analysis command that is accepted but silently loses semantics, e.g.
  the old `.dc` with sweep arguments degrading to a plain operating point. Contrasts with a
  *rejected* directive, which errors. A degenerate directive is a correctness bug — the `.dc`
  trap was one, and is fixed: sweep arguments now produce points or an error.
- **Beyond parity** — goals in neither reference tool; RSpice's own "advanced and fast" track, kept
  separate from parity gaps in `TODO.md`.
