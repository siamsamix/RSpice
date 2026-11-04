# RSpice — Roadmap to LTspice/PSpice parity

Things RSpice must gain to deliver "all the features of both LTspice and PSpice", tracked by category.
Terminology follows `CONTEXT.md` (element, model card, analysis command, directive, stamp, MNA, NR).

**Baseline (settled with author):** full product parity with LTspice and OrCAD/PSpice — engine, device
models, parser language, analyses, waveform viewer, schematic GUI, model libraries & interop — plus a
separate "beyond parity" track for speed/accuracy goals that neither reference tool covers.
Feature list compiled from built-in knowledge of LTspice 24.x and PSpice/Advanced Analysis 23.x;
no web research per decision. Niche claims flagged in the footnote at the bottom: verify against the
shipped tools' docs before implementing.

**Legend:** `[P0]` correctness/foundation — do first · `[P1]` core parity — everyday workflows ·
`[P2]` long tail — niche directives, advanced tooling.
Source tags: `(both)` present in both reference tools · `(LT)` LTspice-only · `(PS)` PSpice-only.

**Where we stand:** sim core — dense-LU MNA, `.op`, adaptive trapezoidal/Gear `.tran` with
source-edge breakpoints and `.options method=…`, small-signal `.ac`, one/two-sweep `.dc`,
`.tf`, `.noise`, `.four`, nested `.step` (parameters or temperature), `.meas`, the PSpice-style
`.mc` Monte Carlo driver (seeded runs over `LOT`/`DEV` model tolerances, uniform ±tol draws,
histogram/yield readouts), and multi-run
results; elements `R C L V I D M E F G H S W T K`, the full source-waveform family
(`PULSE/SIN/EXP/PWL[ FILE, REPEAT]/SFFM/AM`), temperature-dependent diode/MOSFET/passive
equations; parser with `*`/`;` comments, `+` continuation, `MEG`/`MIL` suffixes,
case-insensitive named nodes, subcircuits/libraries/initial conditions, and the
`{}`/`.param`/`.func` language; bundled `libs/` models and op-amp macromodels; egui GUI
(schematic + waveform tabs, symbols `R C L V Gnd`); CLI batch/report/CSV export. Every item
below is written against that state.

---

## 1. Correctness fixes in what already exists

- [x] **[P0] (spot)** Comment handling: `*` or `;` anywhere in a line truncates the netlist text —
  SPICE only starts a comment with `*` at the start of a line; quoted/element values can be destroyed
  (`netlist.rs` `strip_comment`).
- [x] **[P0] (spot)** README drift: Limitations section still claims "No nonlinear devices" while
  diode/MOSFET models exist; analysis table omits `.ac`; "inspired by" should become the parity goal.
- [x] **[P0] (spot)** Waveform/DC tabs omit MOSFET branch currents — `BranchCurrents.mosfets` is
  computed at `mna.rs:501-578` but never listed in `plot.rs`/`app.rs` plot or DC tables.
- [x] **[P0] (both)** `.dc` is a trap: it parses sweep arguments and silently ignores them, degrading to
  a plain operating point (see Analyses item below for the real feature).

## 2. Parser & netlist language

- [x] **[P0] (both)** Line continuation: leading `+` continues the previous line — required by nearly
  every real-world `.lib`/netlist file.
- [x] **[P0] (both)** Unit suffixes: support `MEG` (1e6) and `MIL`; current suffix parsing takes only
  the last character, so `4.7Meg` mis-parses (`units.rs`).
- [x] **[P0] (both)** Alphanumeric node names and net aliases (`in`, `out`, `V(out)`), case-insensitive;
  nodes are integers-only today.
- [x] **[P0] (both)** Parameter language: `{}` expression evaluation, `.param`, `.func`, math function
  library, string/numeric params, quoted values — no parametrization exists at all.
- [x] **[P1] (both)** Subcircuit hierarchy: `.subckt`/`.ends` (with `PARAMS=`), nesting, `.global` nets,
  `X` element instantiations — done: ports/bindings, `PARAMS:` and bare `key=value` defaults, instance
  overrides beat body `.param` re-assignments, internal nets/elements mangle to `X1.path` names,
  forward-referenced and nested definitions, `.global` shared through scopes.
- [x] **[P1] (both)** Library system directives: `.include` and `.lib` (with section selection), model
  search paths, bundled library directories (`nom.lib` convention in PSpice, user-lib dirs in LTspice)
  — done: `.include` and `.lib` (whole-file and section) resolve through the deck's directory plus
  `RSPICE_LIB`/`search_paths` (no bundled libraries shipped yet); library-structure cards inside a
  read library never leak into the deck.
- [x] **[P1] (both)** Full model-card grammar per device: complete parameter lists, model levels, and
  per-instance sizing (W/L, area/perimeter AD/AS/PS/PD); tolerance slots for Monte Carlo
  (`LOT`/`DEV` in PSpice's `.model` cards) reserved now — done: diode/MOSFET parameter lists with
  LEVEL validation, per-instance AD/AS/PS/PD + diode AREA carried into elements, DEV/LOT slots parsed
  into `ModelTolerance` (their consumers landed with the `.mc` Monte Carlo driver, TODO §8), unknown
  families kept as `GenericModel` cards.
- [x] **[P1] (both)** Tolerant import of third-party netlists: case-insensitivity everywhere, tabs/space
  chaos, unknown directives as warnings with best-effort parse instead of hard errors — done: unknown
  directives/element letters/stray `.endl` markers warn with the offending text and the deck still
  simulates.
- [x] **[P1] (both)** Initial-condition directives: `.ic` (bias/transient startup) and `.nodeset` —
  done: both parse (fused/split spellings), node names resolve loudly; `.ic` pins a `UIC` transient's
  t=0 state, `.nodeset` seeds (never forces) the DC iteration.
- [ ] **[P2] (both)** Full `.options` surface: `reltol`, `vntol`, `abstol`, `gmin`, `itl1..itl6`,
  `maxord`, `method`, PSpice `accurate`/`solvehold` style flags — parsed and honored.
- [ ] **[P2] (LT)** Waveform/data selection directives: `.save`, `.keep`, `.savebias`, `.loadbias`.
- [ ] **[P2] (LT)** `.csparam` support (feeds `.meas` results across stepped runs).
- [ ] **[P2] (PS)** Node aliasing blocks `.aliases`/`.endaliases` for probe names.
- [ ] **[P2] (both)** Output-file directives & simulation log: `.probe` (PS), `.out`/`.log` with
  device operating-point tables.

## 3. Engine & numerical core

- [x] **[P1] (both)** Sparse MNA solver replacing dense `nalgebra` LU — KLU-class with symbolic
  Markowitz ordering computed at netlist load; dense LU is infeasible beyond a few thousand nodes. *(done: slice 1
  landed `sparse.rs` + the `stamp_add`/`stamp_sub` seam (`MnaSystem.touched`, a zoo-tested superset record → CSR →
  unsymmetric full-Markowitz order with a no-stranding guard, `ADR 0002`); slice 2 landed the numeric factor:
  `SparsePlan` pools compiled orders per thread and `factor_solve` runs a fixed-order left-looking
  Gilbert–Peierls LU (unit-L columns / U rows) in elimination space — sparse answers, dense LU falls back on any
  sparse failure, zoo-tested sparse-vs-dense in all three arms (`ADR 0003`). Measured A/B on the benchmark suite:
  ladder-200 ~4× faster (778→191 ms), ladder-50 ~1.3×, tiny decks gated back to the dense kernel (`SMALL_SYSTEM`
  = 16). Remaining open, smaller: mark incremental the O(n·nnz) Markowitz scan, pool the per-solve factor
  workspace (retires the small-n gate), factor-once/solve-many across NR iterates and timesteps.)*
- [x] **[P1] (both)** LU factorization reuse across NR iterations and timesteps when the matrix changes
  only by values, not topology — today the matrix is rebuilt and cloned per iterate. *(Dense-buffer
  restamp reuse was prototyped against the NR loop and is a measured loss on the dense path: a reused
  `a` forces a full `n²` `fill(0.0)` per rebuild, while fresh `DMatrix::zeros` rides calloc's lazy
  zero pages (probe: reuse-fill 76.2 µs vs fresh-zeros 10.9 µs at n=210; ladder-200 regressed
  702→815 ms). The sparse solver (line 86 slice 2) eats part of this on the sparse path: `factor_solve`
  reads values straight off the stamped dense matrix — no clone, no `n²` restamp. **Done now:
  factor-once/solve-many** — `SparsePlan` keeps the four most recent numeric factorizations per
  field, keyed on the *bit-exact* pattern values, so an unchanged matrix (every Newton iterate of a
  linear circuit, constant-`dt` steps, the `dt`/`dt/2` pair of step doubling, an AC re-solve) skips
  the factor and runs the triangular solves only, with the identical factors a fresh factorization
  produces: results cannot change (all `examples/*.cir` decks byte-identical to the previous CLI over
  op/tran/ac). Measured against the previous commit: ladder-50 `.tran` 11.3→7.0 ms, ladder-200
  85→60 ms; 593/609 sparse solves reused (benchmark `reuse` column, `sparse::factor_stats`). ADR 0004.
  **Still open:** nonlinear decks refactor every iterate (a new Jacobian each time); modified Newton or a
  partial refactor of only the nonlinear rows would change iterate paths and needs its own decision;
  `touched.compress()` still re-runs per solve.)*
- [x] **[P1] (LT baseline)** Multithreaded matrix assembly/factorization and time-stepping loop;
  LTspice is threaded today, PSpice multithreading is unclear on this baseline. *(Measured, not
  asserted. Run-level parallelism is what LTspice's reserve threads (row 5.5.8 in the parity doc) do
  — per-run work spread over workers — and RSpice already has it for `.step`/`.mc`/`.temp`
  (`run_with_threads`, `RSPICE_THREADS`, CLI `-j/--jobs`, line 535). The in-loop shares, split by
  instrumentation on the benchmark (rc-ladder-200, n=201, 203 steps, 1216 builds): assembly
  stamping is 7–11 ms of a 148–161 ms wall (≈5%; a two-way stamp split pays ≤2.5% before sync
  overhead); LU is Gilbert–Peierls over a fixed Markowitz column order — inherently sequential —
  and 593/609 solves are served from cached factorizations (`factor_stats`), leaving ~16
  refactorizations; the time-step loop consumes each step's accepted state, a hard data
  dependency. The in-loop cost actually worth killing is not a parallelism problem: per-build
  `DMatrix::zeros(n²)` churns mmap/munmap — ~68–157 µs spikes on ~55–60% of builds, 58–73 ms of
  the wall; A/B proof `MALLOC_MMAP_THRESHOLD_`+`MALLOC_TRIM_THRESHOLD_`=1 GiB → wall 73–79 ms.
  Tracked as the next item.)*
- [ ] **[P2] (both)** Buffer pool instead of fresh `DMatrix::zeros`/`StampBits::zeros` per build
  (the mmap/munmap churn above costs rc-ladder-200 ~58–73 ms of a ~150 ms wall): thread-local
  spare buffers per type in `MnaSystem::for_circuit`, refill with `fill(T::zero())`, return on
  drop. Caution (line 96): reuse-`fill(0.0)` inside the NR loop there measured fill 76.2 µs vs
  fresh calloc-lazy zeros 10.9 µs and regressed ladder-200 — that probe predates the sparse path,
  so re-probe fill-vs-fresh over `examples/benchmark.rs` end-to-end before committing; the env
  knobs (1 GiB thresholds, ≈2× wall) are the no-code fallback if pooling loses.
- [x] **[P1] (both)** Trapezoidal integration as the default companion model with `.options
  method=gear` (Gear I/II) available — done: trapezoidal default, `gear1`/`gear2` selectable,
  variable-step BDF2 coefficients for Gear II.
- [x] **[P1] (both)** Adaptive timestep control from local truncation error with `reltol`/`abstol`,
  max-step and first-step control — done: step-doubling LTE estimate over node/current
  tolerances, `.tran … tmax` and `.options maxstep=`, first step sized from the print interval.
- [x] **[P1] (LT)** Discontinuity handling: zero-crossing refinement and pulse leading-edge thickening
  so narrow events are not sampled away — done: every source edge (`PULSE`/`EXP`/`PWL`, including
  repeated PWL blocks) is a hard step boundary, so 1 ns features are never skipped.
- [x] **[P1] (both)** Convergence suite: gmin stepping, source stepping, junction limiting
  (pn-junction/FET limiters), PSpice's Autoconverge-style retry ladder — done: at DC the engine
  keeps the `gmin=1e-12` floor across diode/BJT/JFET junctions and capacitors (`.options gmin`),
  and a failed plain solve retries over a stepped ladder of shrinking gmin (`gminsteps=` decades)
  then source scale fractions (`srcsteps=`), every stage carrying the SPICE2 junction limiter
  (`pnjlim` + `vcrit` at the emission-scaled thermal voltage, with the ngspice `arg > 0` guard so
  backward excursions snap to the knee instead of a NaN) and the `.options itl1=`/`itl4=` Newton
  budgets; the transient path keeps its pre-existing ±0.1 V clamp bit-exactly. MOSFETs are
  excluded from the limiter table — their `gds` floor already plays the junction-damping role.
  Two limiter bugs fixed along the way: the build-1 `used` table is now sized (`zeroed`) per
  element so recorded limits survive the prev/used swap, and the `.options` knobs round-trip
  through emit. An exhaustive hunt found no deck the plain stage fails but a stepped rung
  solves, so the ladder is pinned structurally (budget-exhaustion errors) in tests.
- [x] **[P1] (both)** Temperature system: `.temp`, `tnom`, per-device temperature coefficients, `VT`
  and bandgap terms recomputed from absolute temperature — done: diode `IS(T)`/`Vt` with
  `EG`/`XTI`/`TNOM`, MOSFET `TCV`/`BEX`/`TNOM`, passive `TC1`/`TC2` model cards, several
  `.temp` values or `.step temp` run the deck per temperature.
- [x] **[P1] (LT)** `.options trtol` transient error tolerance — LTspice uses `trtol` (default 2.0)
  as the truncation-error overestimation factor in its adaptive timestep controller; RSpice's LTE
  path has no `trtol` knob. Found as a string literal in `LTspice.exe` (reverse-engineered via
  REA/rizin; see `docs/ltspice-parity-gap.md` §5.5).
  Done: `RunConfig.trtol` default 2.0, `.options trtol=` parse arm (0.01 floor) + emit arm
  (non-default only), LTE estimate divided by `trtol` in `transient_error`. Tests: step-count
  coarsening + emit round-trip (`trtol_relaxes_the_step_controller`); jfet assertions loosened
  for the default relaxation (`epsilon 1e-3`, current floor 1e-9).
- [ ] **[P1] (LT)** AVX2/SIMD-optimized transcendental math — `LTspice.exe` ships `sin_avx2.c`,
  `cos_avx2.c`, `tan_avx2.c`, `exp_avx2.c`, `log_avx2.c`, `log10_avx2.c`, `pow_avx2.c` etc. for
  vectorized device/B-source evaluation; RSpice uses scalar `f64` math. Gate on
  `target_feature = "avx2"` with scalar fallback. (Same REA source as above.)

## 4. Analyses

- [x] **[P0] (both)** Real `.dc` sweep: sweep an independent V/I source (or model parameter) across
  start/stop/increment or a value LIST, with optional nested second sweeps. Under no circumstances may
  sweep arguments be silently ignored again.
- [x] **[P1] (both)** `.tran` completeness: adaptive stepping, `uic`, `tstart`, first argument as
  output/print interval rather than timestep, startup suppression of sources; pulse sources must be
  evaluated with edge refinement, not sampled at fixed instants — done: adaptive stepping, UIC,
  tstart, print-interval semantics, `tmax`, and edge breakpoints listed above; `.tran … STARTUP`
  ramps every independent source by `value·t/10µs` over the first 10 µs (waveform sources included,
  behavioral sources deliberately not) starting from a zero-source operating point.
- [x] **[P1] (both)** `.noise`: input-referred and output noise density vs frequency over `.ac`
  sweeps, integrated RMS totals; requires noise models for R (thermal), diode (shot/flicker), BJT, MOS,
  JFET — done for R/diode/MOSFET (adjoint method); BJT/JFET noise arrives with those devices.
- [x] **[P1] (both)** `.tf`: DC transfer function from source to node with input/output impedances.
- [x] **[P1] (both)** `.four`: Fourier decomposition of a transient waveform (directive) — done;
  viewer FFT with selectable windowing is tracked under §6.
- [x] **[P1] (both)** `.step`: linear/octave/decade/LIST sweeps over parameters or temperature, nested
  steps, run overlays, and results tagged per step — done: cartesian nesting, `runs` tagged with
  their values, GUI overlays still open under §6.
- [x] **[P1] (both)** `.measure`/`.meas` family: `avg/rms/max/min/pp/max_at/min_at/rise/fall/cross/
  find…when/from…to/trig…targ` with expressions over the results, for `.tran`/`.ac`/`.dc` — done
  (rise/fall spelled through `TRIG … TARG`, expressions use the probe/parameter evaluator).
- [x] **[P1] (both)** Operating-point presentation: annotated device operating points (LTspice plot/data
  view) and `.op` variants (`pon`, `all`) per PSpice — done (`analysis::device_op_points` prints the
  per-card R/C/L/D/M/Q/J/E/F/G/H/B and source `v/i/p` family table for `.op all`; `.op pon|all` parse
  through `OpMode`, plain bias print otherwise; PSpice's exact `pon`/`all` semantics are
  unverifiable, documented interpretation: `pon` plain bias print, `all` device detail on top).
- [ ] **[P2] (both)** `.ac` LIST frequency scales beyond dec/oct/lin.
- [ ] **[P2] (both)** Small-signal secondary analyses: `.sens` (DC sensitivity), `.pz` (pole-zero),
  `.disto` (harmonic distortion).

## 5. Devices & primitives

- [x] **[P0] (both)** Independent current source `I` — the most glaring single missing element; without
  it most textbook circuits can't even be drawn.
- [x] **[P1] (both)** Source waveform family: `EXP`, `SFFM`, `AM`, `PWL` (incl. `PWL FILE` and repeat
  blocks in LTspice); noise stimulus sources (LTspice's white/flicker functions) `(LT)`; PSpice
  synthesizes noise via `.noise` + IBIS interface nets instead `(PS)` — done: the waveform generators
  (incl. `PWL FILE=<file>` and repeat blocks) plus `white(arg)`/`flicker(arg)` keyed-noise functions
  on B sources: deterministic draws in `[−1, 1)` (flicker over six octave bands weighted for equal
  power per octave), re-rolled only by their pacing argument — a bare `time()` counts accepted
  steps, any other argument keys on its end-of-step `floor(arg)` — so a noise source is frozen
  within one Newton solve and amplitudes ride the user expression (`V=white(time())*10m`).
- [x] **[P1] (both)** Controlled sources `E/F/G/H` including `POLY(n)` multivariate forms.
- [x] **[P1] (LT)** Behavioral sources `B` (arbitrary expressions of node voltages/branch currents,
  `time()`, `idt()`/`sdt()`, `delay()`, `table()`, Laplace/frequency-domain hybrids).
- [x] **[P1] (PS)** ABM extensions on E/F/G/H: `VALUE=`, `TABLE=`, `FREQ=`, `LAPLACE=`, `CHEBYSHEV=`,
  `EQUATION=` blocks — done: E/H spell the voltage flavour, G/F the current flavour, each parsed
  into the same behavioral table (`BehavioralSource` + `abm: AbmForm`), emitting re-spells the
  canonical card shape. `TABLE` is an instant AST lookup (linear in the input, **0 outside the
  bracketing range** — PSpice ABM semantics, not LTspice clamping; the B-language `table()` call
  stays rejected — the table exists only in ABM). `EQUATION=` is accepted as the `VALUE=` alias.
  The dynamic forms respond in AC and stamp their DC gain H(0) in DC/transient (warned once per
  card): `LAPLACE = {num} {den}` takes descending coefficient lists or `s`-polynomial spellings
  (`2*s+s^2`), `FREQ` takes `<f, gain[, phase]>` rows interpolated in log f with `MEASURE=DB|
  RADS|DEGREES` units (gains plain, phases degrees otherwise), `CHEBYSHEV` takes the second-order
  gain coefficients and attenuates like `1/(1 + Σ|cᵢ·Tᵢ₊₂(f)|)`. Relaxed interpretation (the
  reference grammar details are unverifiable here): keyword may glue (`VALUE={…}`, `CHEBYSHEV{…}`)
  or carry a spaced/unglued `=`; braces are grammatical on ABM cards, so the substitution pass
  leaves those lines untouched and numeric `{1}` groups survive; `.param` names fold into the
  control expression.
- [x] **[P1] (both)** Diode model completion: `RS`, `TT`, `CJO/M/VJ`, `EG/XTI`, `BV/IBV`, `KF/AF`
  (flicker noise), recombination params — with junction capacitance so transients are physical —
  done: `RS` splits into an internal junction node, `CJO`/`TT` are stamped as charge-based
  transient capacitances and AC susceptances, `BV`/`IBV`/`NBV` implement reverse breakdown,
  `EG/XTI/TNOM` drive the temperature law, and `KF/AF` feed `.noise`.
- [x] **[P1] (both)** BJT Gummel-Poon: full parameter set, PNP polarity, junction capacitances, Early
  effect, temperature — done: Ebers–Moll/GP port equations with the IKF/IKR high-current quadrature
  (`qb = ½(q1 + √(q1² + 4q2))`), XTB beta-temperature exponent, Early effect, ISE/ISC leakages,
  CJE/CJC (+TF/TR) capacitances stamped in transient (companion history) and AC, `EG/XTI/XTB/TNOM`
  temperature law, RB/RC/RE expanded to internal-node resistor arms, parse/emit round-trip, and
  analytic operating-point tests for the stiff forward-active corner, IKF knee, XTB warmth and all
  three series slots.
- [ ] **[P2] (LT)** Advanced BJT models: VBIC and Mextram (level/type naming per LTspice's help).
- [x] **[P1] (both)** MOSFET levels 2 and 3 — done: `MosfetModel::eval_sized(w, l, …)` dispatches on
  `LEVEL` (level 1 stays `eval_at` bit for bit). *Level 2* is Grove–Frohman: the exact bulk-charge
  integral `(Vgs−Vfb−φ−Vde/2)·Vde − ⅔γ((φ+Vsb+Vde)^1.5 − (φ+Vsb)^1.5)` with the closed-form `Vdsat`
  root, `UCRIT/UEXP/UTRA` transverse-field mobility reduction (needs `TOX`), `λ` as the level-1
  multiplier; with `γ = 0` it *is* the level-1 square law (pinned in tests). *Level 3* is the
  semi-empirical model: `θ` mobility modulation, `ETA` drain-induced threshold lowering, `DELTA`
  narrow-width term, `VMAX` velocity-saturation `Vdsat` + `1/(1+Vde/Vc)`, and `KAPPA`/`NSUB`
  saturation-field length shortening (`LAMBDA` does not act on level 3, as in the reference SPICE;
  the `√` knee is smoothed over 10 µV so the finite-difference `gds` stays finite). `LD/WD` give the
  effective geometry. Missing `KP/GAMMA/PHI` derive from `UO·Cox`, `NSUB`+`TOX` (card values win);
  parameters a level ignores, and slots lacking `TOX`/`NSUB`/`UO`, warn instead of vanishing. Unmodelled
  and loudly warned: the short-channel `XJ` factor, `NFS` weak inversion, `NEFF`, level-2 `VMAX`. Level
  2/3 `gm/gds/gbs` are central differences of the one analytic current (1 µV step). Equations are written
  from the public SPICE2/3 model descriptions and pinned against independent Python references, **not**
  checked against the shipped LTspice/PSpice (see footnote). Levels above 3 are still a loud parse error.
- [ ] **[P1] (both)** BSIM3/BSIM4 families per tool coverage (level numbering differs between LTspice
  and PSpice — verify per target); `LEVEL=8/49/54` cards are rejected loudly today.
- [x] **[P1] (both)** MOSFET capacitance/charge model: gate overlap (CGDO/CGSO/CGBO), junction caps
  (CJ/CJSW/MJSW/PB), so switching transients carry realistic charge — done: overlap and zero-bias
  junction capacitances are stamped in transient (companion history) and AC; grading parameters are
  parsed for the bias-dependent upgrade.
- [x] **[P1] (both)** JFET with capacitance — done: `J` element with NJF/PJF model cards (quadratic
  channel in saturation/triode, λ modulation, AREA scaling, gate-junction diodes with IS/XTI/EG and
  0.8 V relaxation), depletion-law CGS/CGD (PB grading + Fc linearization) stated in tran (companion
  history) and AC, RD/RS expanded as internal node arms, `I(J1)`/`Id(J1)` probes, parse/emit
  round-trip. Integration tests pin NJF sink, PJF mirror, RD/RS degradation, AREA and capacitance
  loading, and the gate-step cutoff transient.
- [ ] `[P2] (LT/PS letters differ)` MESFET/GaAs FET (LTspice letter `Z`, PSpice letter `B`;
  Curtice/Statz/Raytheon variants).
- [ ] **[P2] (PS)** IGBT model; SCR/TRIAC/GTO/thyristor macromodels from the standard libraries.
- [x] **[P1] (both)** Switches: voltage-controlled (`S`) and current-controlled switch primitives with
  model cards (on/off resistance thresholds, hysteresis).
- [x] **[P1] (both)** Mutual inductance `K` and ideal transformers; `[P2] (PS)` nonlinear magnetic core
  models (XFRM_NONLINEAR/RM); `[P2] (LT)` behavioral inductor via `flux=` control equation.
- [x] **[P1] (both)** Transmission lines: lossless `T` and lossy `O` (LTRA with RLCG parameterization).
  *(the lossless `T` landed: `T<name> n1 n2 n3 n4 Z0=<Ω> TD=<s>` — `F=<Hz>` with `NL=<wavelengths>`
  (default 1) spells TD = NL/F and present `TD=` wins over them with a warning, `Z0 > 0`/`TD > 0`
  validated; no branch rows — one `I(T…)` report value per line, the port-A (`n1↔n2`) current.
  DC/op runs the wire-pair interpretation: `g = 1/(1e-6·Z0)` between each port's hot nets, a
  steady through-drop ≈ I·1e-6·Z0, `.op` rows z0/td/v/i; transient runs the method of
  characteristics off per-port `(t, v, i)` history rings (`TlineHistory`, seeded from the op point
  over [−TD, 0], pushed/pruned per accepted step) — each port draws Y = 1/Z0 against the far
  port's delayed incident wave E = v(t−TD) + Z0·i(t−TD); AC stamps the exact lossless two-port
  Y = 1/(Z0·j·sinθ)·[[cosθ,−1],[−1,cosθ]] with the |sinθ| < 1e-12 resonance clamp shared by stamp
  and extraction. `.model …TLINE(…)` cards import as silent `GenericModel`s (kept, unused).
  Integration tests pin the open-end TDR staircase (10/3 → 20/3 → 50/9 → 5 V), a
  matched-termination flat line, straight-through DC, and the frequency-flat matched AC transfer.)*
  *The lossy `O` landed as a **lumped ladder**, a documented interpretation of the reference tools' exact
  LTRA convolution:* `O<name> n1 n2 n3 n4 <model> [N=<sections>]` with `.model <model> LTRA(R= L= G=
  C= LEN= [N=])` (per-length RLGC, any order in the deck) expands after the card walk into `N` π-sections
  (default 20, max 1000; element `N=` beats the model's): series `R·LEN/N` then `L·LEN/N` along the signal
  conductor, `C·LEN/N` and `G·LEN/N` from each node to the return net with half weights at the two
  ports. Plain R/L/C arms mean DC, transient, AC and noise work with no new stamp, and the ladder is
  exact at DC (`R·LEN`, `G·LEN` sum exactly — pinned to 1e-8/1e-9) and low frequency (the open-line
  RC low-pass matches `C·LEN` to 1e-8), band-limited near `f ≈ N/(π·TD)`. Ports must share a return net
  (`n2 = n4`) or the card is a loud error; `R=0` (LC), `L=0` (RC) and `G` are all accepted;
  other LTRA knobs (`REL`, `ABS`, …) warn as inapplicable. Against the exact method-of-characteristics
  `T` element a 50 Ω/1 ns open line gives RMS 0.013 V at N=30 (0.109 V at N=3 — the test fails there on
  purpose); series loss lowers the matched level toward the DC divider. Subcircuit instance paths mangle
  the arms (`X1.RO1_s3`) so `element_letter` still reads plain passives.
- [ ] **[P2] (both)** Exact LTRA lossy line (impulse-response convolution, REL/ABS/compaction knobs,
  floating-return two-port semantics) beside the lumped `O` ladder — the ladder needs `N` raised for
  fast edges and long lines.
- [x] **[P1] (both)** Passive model cards: `.model R/L/C` with temperature coefficients `TC1/TC2`;
  LTspice-style intrinsic parasitics on every R/L/C (`rser`, `cpar`, `lser`, `rpar`). *(Model cards and
  TC1/TC2 plus the parasitic split were done: nonzero slots materialize into real series/parallel arms
  with internal nodes after the deck walk, so stamps/AC/noise see plain elements; emitted decks keep
  the expanded arms and drop the slots to stay round-trip stable.)*
- [x] **[P1] (both)** Opamp primitives/macromodels: ideal op-amp element and universal/multi-pole
  macromodel library entries, plus vendor `.subckt` opamps loadable from the library system. *(done:
  `Uname out+ out− in+ in− [model]` — one branch row enforces `V(out) = gain·(V(in+) − V(in−) + VOS)`;
  `.model <name> U(GAIN/RIN/ROUT/VOS)` with documented defaults 1e6/∞/0/0, bare-`U` resolution, and
  the reserved DEV/LOT slot; the stamp serves DC/tran/AC with RIN/ROUT admittance rungs when
  finite/non-zero, `I(U…)` rides the family reports, `.op all` annotates a/v/i/p, subckt flattening
  carries instance paths, and parse/emit round-trips (∞ RIN is not re-printed, like every
  non-finite slot). `libs/opamps.lib` also gained `opamp2`, a gm-RC two-pole macromodel
  (A0 = 1e5, f1 = 10 Hz → GBW 1 MHz, f2 = 400 kHz) whose AC map the regression corpus pins; an
  open-loop deck solves to the linear overdrive — the element is meant for feedback loops. Vendor
  `.subckt` opamps were already loadable through the `.include` system.)*
- [ ] **[P2] (PS)** Measurement/utility devices: wattmeter, fuse, varistor from the special libraries.

## 6. Waveform viewer & plotting

- [x] **[P1] (LT/PS)** Cursor infrastructure: crosshair cursors with readouts and measured deltas
  (LTspice dual cursors; PSpice cursor measurements). *(done: core `svg_document_with_cursors` +
  CLI `--cursor` (repeatable, SPICE-suffixed, max 2/pane, snaps to first kept trace, coordinate/
  delta readouts, `f = 1/Δt` on time axes); interactive dragging/ruler-to-cursor wiring is the
  later §7 GUI item)*
- [x] **[P1] (LT)** Waveform arithmetic: add arbitrary expression traces (`V(out)*I(V1)` power, scaled,
  offset, combined). *(done: core `arith.rs` — `TraceExpr::parse`/`evaluate` over a plot's `.raw`
  columns (`+ - * / ^`, SPICE-suffixed numbers, `V(a,b)` differences, `V(0)`/`V(gnd)` = 0,
  case-insensitive probes, the axis and step columns and numeric deck `.param`s as bare names,
  `abs mag ph arg re im db sgn sqrt exp ln log log10`, trig/hyperbolic, `floor ceil int round`,
  `pow pwr pwrs atan2 hypot min max`, and `d()` against the axis); real plots compute in `f64`
  (domain errors are NaN), `.ac` plots in complex arithmetic, so `V(out)/V(in)` is the transfer
  phasor; `frequency`/`time` name the plot's axis by kind (an AC axis column is `freq`), and
  expressions are bounded (256 operations, 64 nesting levels) so a runaway chain is a parse error,
  never a stack overflow. `add_expression_traces` appends the results as ordinary columns named
  by their expression minus whitespace (a `.raw` name cannot carry spaces; the CSV header quotes
  comma names like `V(a,b)`), so the exporters, FFT and `.raw` writer take them with no second path; a
  plot lacking a probe is skipped, an expression no plot can evaluate or a syntax error is a loud
  error. CLI `--expr` (repeatable) feeds `--waves/--image/--fft`; GUI Waveforms tab gains an
  **Expression** sub-tab (type, Add/Enter, per-trace ×, Clear; re-evaluated on every run, and
  kept dormant — dimmed, undrawn — through runs that cannot compute them) over the first run's
  transient plot. Function list written from memory of LTspice's help, not checked
  against the reference install; `.ac`/`.dc` plots in the GUI and LTspice's `x`/`y` axis-stat
  functions (`integ`, `avg`, `rms`, `Mag`-over-window, …) are not covered.)*
- [x] **[P1] (both)** FFT of transient traces with configurable windowing (LTspice Blackman-Harris et
  al.; PSpice Probe FFT). *(done: core `fft.rs` (13 windows incl. `--window`/aliases, unit-mean
  taper, next-power-of-two resample, bin = 1/span, `{trace} (FFT)` plots) + CLI `--fft` SVG;
  parameterized windows (Dolph-Chebyshev, Kaiser-Bessel, Gaussian, …) deferred; GUI viewing is a
  later §7 item)*
- [x] **[P1] (both)** Stepped/parametric overlays: all `.step`/parametric runs on one plot with per-run
  identification; PSpice performance-analysis curves of a measurement vs swept parameter belong here.
  *(done: CLI `--image` overlays — same-shape `.raw` plots group in first-occurrence order and draw
  one curve per (run, trace) on shared y/x scales, each legend chip naming the run's step values
  (run ordinals for reader-built files without them); step columns stay metadata and never draw,
  cursors snap to run 0's first kept trace, and a sub-1-s x axis no longer clamps into [0,1]; PSpice
  performance analysis landed as CLI `--perf` — `svg_performance` renders one `{meas} vs {step}`
  panel per distinct `.meas` name (measured runs only: a run whose meas failed is skipped, all-fail
  keeps just the frame, a single-point run draws a circle), axes span the plotted points with 5% pad
  on both ends and no zero anchor, titles only with no legend chips; unstepped results, multi-target
  sweeps and `.mc` runs are each rejected with their own error, as is a stepped deck with no
  `.meas`. GUI overlays of these remain a later §7 item)*
- [x] **[P1] (both)** Axis completeness: linear/log on both axes, physical unit auto-detection and
  formatting (Hz/s/V/A/dB/°), multi-pane layouts. *(done: CLI SVG exporter (`svg_panel`) — a
  `log`-flagged plot (`.ac dec/oct`) maps x logarithmically with decade ticks plus the data's own
  edge ticks and unlabeled minor log ticks, no pads (the first sample pins at the frame); linear
  axes keep the 5% pads, y spans the plotted data and no longer anchors at zero; log y, dB scales
  and the GUI stay §7. Columns carrying a physical unit (time/frequency/voltage/device_current/
  impedance) spell their tick ladders SI-style — `µ m k Meg G`, Meg for millions like the deck
  multipliers — with the unit letter kept off ticks: it rides the cursor readouts, whose Δt line
  spells Hz. Mixed-unit layers (a default all-traces panel's `V(...)` beside `I(...)`) agree on no
  shared unit and fall back to plain decimals; per-trace readouts keep their own units. Multi-pane
  layouts are served by the existing stacked-panel renderer. Fixture pins updated to the
  no-anchor scale and three tests added (`si_labels_follow_the_prefixes`,
  `ac_log_axis_spells_decades`, `transient_ticks_spell_si`); export suite 31 green)*
- [x] **[P1] (both)** Cross-probing: click a wire/part in the schematic to add/trace `V(net)` and
  `I(device)` in the viewer, and vice versa.
  *(done: `last_probe_click` click-probes in Select tool — wire → `V(node)`,
  R/C/L/V body → `I(device)` via `current_slot_targets` slot expansion; repeat click isolates
  that probe alone; waveforms Voltage/Current hovers map nearest series
  (`plot_frame_hover` ≤8px vertex distance) back to a red schematic flash on the owning wire
  (`probe_flash=Node`) or component (`probe_flash=Element`); red `V()/I()` hover pill preview on
  canvas; Differential/Expression null the flash; `plot_currents` mirrors family slots, reset on
  re-run; tests: `probe_target_*`, `current_slot_targets`, `waveforms_plot_visible` in app.rs,
  `tran_current_series` slot tests in plot.rs; export suite 31 green)*
- [ ] **[P1] (both)** Waveform file I/O: read/write binary and ASCII `.raw` (LTspice) / `.dat`
  (PSpice) *(the LTspice `.raw` half landed: `rawwave.rs` — a one-shot `rawwave::build(title,
  netlist, result)` turns every analysis of every step run into a plot (`RawDataset`:
  title/plotname/flags/variables/values), the operating point included with its synthetic
  `time = 0` axis so real plots stay axis-first; the writer emits both flavors — LTspice-style
  binary (UTF-16-LE headers, point-major payload: f64 axis + f32 values for real plots, f64
  `(re, im)` pairs for complex) and the ngspice-compatible ASCII `Values:` listing (one value
  per line, a variable-0 line opens each point, complex `re,im`); stepped runs concatenate
  self-headered plots and every run carries its step values as constant columns named after
  the step target (documented interpretation — a self-describing file, no `.log` sidecar
  invented); the reader takes either flavor either encoding, disambiguates the real layout by
  byte count (mixed wins even when the all-double count also fits, matching LTspice), re-rows
  `FastAccess` column-major payloads, and tolerates a truncated tail down to whole rows.
  Traces: every net as `V(net)` in id order (ground-less decks' node 0 rides like any other
  net), then one current trace per element family in the report probe order
  (`I(R1)`/`Ic(Q1)`/`I(T1)`), plus the `.tf`/`.noise` plots with byte-exact reference
  spellings: `Transfer Function` over a synthetic `freq = 0` row tracing the deck-spelled
  output plus `input_impedance`/`output_impedance_at_<src>`, and the noise plot
  `Noise Spectral Density - (V/Hz½ or A/Hz½)` with `V(onoise)` and the input-referred
  `V(inoise)` — the `i(inoise)` spelling for a current-source input is wired, but the
  analysis stage rejects a current `.noise` input today. CLI: `--raw <file>` (binary
  default) with `--raw-ascii`, plus `--read-raw` to dump any file's plots; the PSpice
  `.dat` format stays open.)*
- [x] **[P1] (spot)** Full export surface: CSV of arbitrary traces (node voltages, element currents,
  measurements — today only `.tran` node voltages are exportable), image export.
  *(Landed in `circuitsim::export`: `csv_document` renders every raw plot as a `# <Plotname>`
  section at `{:.9e}` with the `.meas` table in RFC 4180 quoting, complex (AC) columns as
  `<name>_mag`/`<name>_phase` degrees and the axis as the real part; `svg_document` draws the
  same plots as one stacked SVG panel per plot — linear axes, zero-anchored 5%-padded y scale,
  gridlines, legend chips, one-point traces as circles, XML-escaped text. Trace selection is
  shared: an empty list keeps every column, a comma list matches column names
  case-insensitively while the axis and the step columns always ride; the step columns carry
  each row's run provenance so stepped plots and CSVs stay self-describing. CLI: `--waves
  <file>` (CSV), `--image <file>` (SVG), optional `--traces a,b`. Logged axes, cursors, and
  real-time plotting remain per-item work elsewhere in this list.)*
- [ ] **[P2] (both)** Waveform data compression/decimation options (`.options plotwinsize` in LTspice,
  Probe compression in PSpice).
- [ ] **[P2] (both)** Persist and reuse plot configurations (trace sets, scales, window layouts).

## 7. Schematic GUI

- [ ] **[P1] (both)** Schematic symbols for every device family — at minimum diode, MOSFET, BJT,
  current/control/behavioral sources, switches, transformers, transmission lines, opamps; the editor is
  `R C L V Gnd`-only today.
- [ ] **[P1] (both)** Symbol library format with attached model/.subckt references and a component
  picker: placing `MM1` should be able to attach `model=...`/`spiceline` data.
- [x] **[P1] (both)** Net/wire labels with clean node names governing the generated netlist and plot
  names. *(done at the state level, GUI only — `NetLabel` in `schematic.rs`: Edit ▸ Label Net (F4)
  asks for a name, the next click drops it; same-name labels (case-insensitive) are one net even
  unwired, `0`/`gnd` ground a net, a bare number is refused (it would fight unlabeled nets'
  numbers), a net with two different labels or a named ground makes Generate Netlist fail loudly
  instead of guessing. The label becomes the node's deck name, so `V(name)` traces follow;
  import turns every named net into a label; labels travel with a dragged/rotated pin. Not
  verified visually: the flag drawing (name above a leader line) and F4 are written from memory of
  LTspice, not checked against the shipped help.)*
- [x] **[P1] (both)** SPICE text directives placed as schematic annotations (`.tran`, `.step`,
  `.meas`, `.include` as first-class schematic objects) — both reference tools run the whole show this
  way. *(done, GUI + core — `Directive` text objects (Edit ▸ SPICE Directive, `S`) are spliced into
  the generated deck verbatim; import keeps every dot card the schematic cannot draw
  (`circuitsim::deck_directives`; a `.subckt…​.ends` block is one object) and stacks them under the
  circuit, so a text → schematic → netlist round trip no longer drops `.model`/`.meas`/`.step`/
  `.param`. A `.tran`/`.ac`/`.op` directive replaces the analysis-strip command of its kind;
  import leaves those three to the strip (extra `.tran` args such as `uic` are still lost there).
  Not done: dragging a note, placing text without the dialog, drawn `.subckt` elements on import
  (D/M/I/X cards are still not drawn). Same unverified-visuals caveat as labels.)*
- [ ] **[P1] (both)** Hierarchical schematics mapping to `.subckt` definitions and instances.
- [ ] **[P2] (both)** ERC: dangling-pin, shorted-output, floating-node diagnostics with connectivity
  highlighting; undo/redo completeness.
- [ ] **[P2] (spot)** Persistent configuration/preferences (window state, library paths, theme — forced
  dark today).

## 8. Advanced Analysis & mixed signal (PSpice-centric parity)

- [x] **[P1] (PS; LT approximates)** Monte Carlo: statistical runs over `LOT`/`DEV` tolerances, seed
  control, multiple runs, histogram/yield readouts. First land the parametric `random()`/`gauss()`
  functions that LTspice uses to fake MC via `.step` `(LT)`. *(The parametric layer is landed:
  `random(x)` ∈ [0,x), `rand(x)` ∈ ±x, `gauss(x)` = x·N(0,1) as deterministic draws keyed off the
  sweep overrides — `.step param …` re-rolls them per run bit-reproducibly. The seed-control
  directive is landed too: `.options seed=<n>` raises the base seed before any element line
  expands (a pre-scan lifts the literal, so deck position is free) and round-trips through emit.
  Done as well (documented interpretation below the unverifiable reference grammar): the `.mc <runs>
  [seed=<n>] [hist=<measure> [bins=<n>]] [yield=<measure>{<|>}<limit>]` driver re-runs the deck once
  per seeded run — every tolerance-bearing model card re-draws per run (LOT shared across the
  model's instances, DEV per instance, uniform over ±tolerance, LOT+DEV additive; σ-dominant
  parameters scaled per family: D/Q/M/J/U/IS·KP·VTO·BETA·GAIN, passives the nominal value) as
  re-bound per-instance model clones, parametric `random()`/`gauss()` re-roll through the same
  re-parse route `.step` uses, and the runs land in the step-run shape. The CLI reports
  min/mean/σ/max per measurement, the histogram, and the yield fraction; `random()`-free decks may
  also pin the seed via `.mc seed=`. `LOT`/`DEV` percentages apply in both spellings (`3` and `3%`,
  the flag only preserves the card's spelling).)*
- [ ] **[P2] (PS AA)** Worst-case analysis from component/model tolerances with sensitivity ranking.
- [ ] **[P2] (PS AA)** Sensitivity analysis: ranked parameter sensitivities of a target measurement.
- [ ] **[P2] (PS AA)** Optimizer: curve-fitting parameter tuning against measurement goals
  (goal-function library incl. user-defined goal functions).
- [ ] **[P2] (PS AA)** Smoke test: device stress checks (max V/I/power/temperature from model cards)
  with derating tables.
- [ ] **[P2] (PS AA)** Performance analysis: measurement-vs-parameter curves across parametric runs
  (the CLI `--perf` curves landed with the Waveforms overlay item; this row tracks the GUI view).
- [ ] **[P2] (PS)** Digital primitives & mixed signal: gates, flip-flops, `PINDLY`/`CONSTRAINT`,
  digital stimulus (`DIGSTIM`, `DIGCLOCK`, `STIM*`, `FILESTIM`), A/D and D/A interface bridges, analog
  co-simulation during `.tran`.
- [ ] **[P2] (PS)** IBIS I/O model translation and simulation.

## 9. Model libraries & interop

- [x] **[P1] (both)** Bundled model libraries shipped with the app (generic analog, opamps, couplers,
  standard devices) and the include/lib search behavior to use them plus user directories — done:
  `libs/standard.lib`, `libs/opamps.lib`, `libs/nom.lib`; the CLI searches `-I`, `RSPICE_LIB`, the
  bundled tree, and the deck directory.
- [ ] **[P2] (PS)** Model Editor-style model authoring/import tooling (model cards from datasheet
  parameters); LTspice's symbol-authoring ecosystem (`*.asy`) equivalents live in the GUI section.
- [ ] **[P2] (both)** Encrypted/protected models: LTspice-encrypted subcircuits and PSpice-protected
  libraries. decryption is IP-encumbered and possibly infeasible — at minimum detect and fail loudly
  with actionable messages.
- [ ] **[P2] (PS)** Packaging: standalone/`PSpice for TI`-style distribution of a simulation profile
  workflow (`(PS)` ecosystem item).

## 10. Tooling, CLI & docs

- [x] **[P1] (spot)** Regression corpus: golden-result tests against analytic references (hydro RC
  step, resonant peaks, diode curves) and a manifest of `.cir` fixtures that runs in CI — done:
  `circuitsim/tests/regression.rs` plus the `examples/*.cir` manifest.
- [x] **[P1] (both)** CLI batch mode: run-to-completion flags with exit codes, selectable analysis
  output (all analyses, not just `.tran` CSV), waveform file output, per-figure error reporting —
  done: `-b/--batch`, `-a/--analysis`, `--output`, `--waves`, `-I`, warnings on stderr, exit 1 on error.
- [ ] **[P2] (both)** Manual: command reference, netlist format rules, worked examples; both reference
  tools ship professional-grade documentation.
- [ ] **[P2] (spot)** Trunk configuration committed for the published wasm build (`Trunk.toml`).

## 11. Beyond parity — advanced & fast (goals neither reference tool covers)

Neither LTspice nor PSpice does these today; RSpice targets them for its "advanced and fast" claim.

- [x] **[P1] (spot)** Benchmark harness: canonical circuit suite with throughput (node·iterations/s)
  and accuracy compared element-for-element against LTspice `.raw` results; "fast" must be measured,
  not asserted. *(Harness with canonical suite, throughput, and analytic accuracy at
  `circuitsim/examples/benchmark.rs`; the LTspice half is done as tooling: core `rawcompare.rs`
  (`compare_raw`) pairs plots by `Plotname:` (k-th occurrence with k-th, so stepped files line up)
  and traces by name, interpolates RSpice's trace onto each **reference** axis point (the tools pick
  different timesteps; exact when grids coincide, as in `.ac`/`.dc`), reads a reference time axis as
  `|t|` (LTspice flags stored points with negative stamps), compares single-point `.op` plots by
  name (LTspice's file has no axis variable), takes complex `.ac` traces by phasor distance, and
  reports `max|err|`, `rms` and `max|err|/peak` per trace — scale-free through zero crossings; a NaN
  or a missing reference plot never passes, reference-only traces and RSpice-only plots are listed not
  failed. Entry points: CLI `--compare-raw <file>` (+ `--compare-tol`, default 1e-3; table appended
  to the output, exit 1 past the tolerance) and the benchmark's `--write-decks DIR` (emit every case
  as `<case>.cir` to run in LTspice) → `--reference-dir DIR` (a `vs LTspice` column per case, exit 1
  past `--tol`). Verified against RSpice's own binary `.raw` round trip (f32 quantisation reads
  ~5e-8), a coarse analytic grid, injected errors and hand-built LTspice-shaped plots.
  **Not yet done:** no LTspice run happened in this environment, so there are no real LTspice
  numbers — generate `<case>.raw` with the author's Wine install and run the flow above.)*
- [x] **[P1] (spot)** Parallel parametric sweeps: distribute `.step`/Monte-Carlo/MC-style runs across
  cores — neither tool parallelizes over runs. *(done: `runs::run_plans` hands the independent run
  plans (`.step` combinations, `.temp` lists, `.mc` runs) to scoped worker threads — one atomic
  counter deals indices in order, results land in plan order, the calling thread works too, and a
  failure stops later claims so the error returned is always the lowest-indexed one, exactly the one
  a serial loop stops at. Results are bit-identical for any worker count (tests compare the full
  `Debug` dump of a stepped transient and a Monte Carlo deck at 1/2/3/8/64 workers; the first-error
  test uses two differently-failing plans so a "first to finish" driver would show). Knobs:
  `circuitsim::run_with_threads(netlist, n)`, `RSPICE_THREADS`, CLI `-j/--jobs N`; default = all
  cores. Measured: 32-run RC `.tran` sweep, 7.75 s → 4.31 s on 2 cores (1.8×), byte-identical
  output. Across-run parallelism only — threading inside one solve is §3's open item.)*
- [ ] **[P2] (spot)** GPU-accelerated linear solve (wgpu) as an optional backend for very large designs.
- [ ] **[P2] (spot)** Steady-state detection and automatic quality presets ("fast" / "accurate" solver
  profiles).
- [ ] **[P2] (spot)** Chunked/streaming waveform writer with memory-mapped reads for hour-scale runs.
- [ ] **[P2] (spot)** Result memoization keyed by netlist+model hash (skip identical re-runs).

---

### Footnote — claims to verify against shipped docs before implementing

These were compiled from built-in knowledge with no web research (per decision); the following are the
ones most likely to drift by version: exact LTspice MOSFET/BSIM level coverage and level-integer
mapping · PSpice MESFET spelling and letters · LTspice VBIC/Mextram type names · PSpice wattmeter and
special-library part availability · the current OrCAD X-era PSpice additions newer than the pinned
23.x baseline · decryption/packaging items — legal/feasibility review required.
