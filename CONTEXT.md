# RSpice — Domain Glossary

Words used consistently across the three modules (`circuitsim`, `circuitsim-gui`, `circuitsim-cli`).
Use these terms in code names, tests, and reviews. Marked `(cut #N)` where a name was agreed for a
deepening that has not landed yet.

## Circuit language

- **Netlist** — SPICE-style text: element lines, directives (`.op` `.tran` `.ac` `.model`), ends with `.end`. Parsed into a `Circuit` + `AnalysisCommands` by `netlist.rs`. The same text is both the GUI editor's content and what `.cir` files carry.
- **Element** — one component line. The first letter names the kind: `R` resistor, `C` capacitor, `L` inductor, `V` voltage source, `D` diode, `M` MOSFET.
- **Node / ground** — a connection point, addressed as `NodeId`. Node `0` is ground and owns no matrix row.
- **Stamp** — writing one element's contribution into the MNA matrix/right-hand side at build time. Not a runtime mutation of results.
- **Model card** — a `.model` line defining device physics (`DiodeModel`, `MosfetModel` level-1 Shichman–Hodges). Elements reference cards by name; `validate()` rejects unknown names.
- **DeviceModel** — the module (`model.rs`, cut #1) owning device equations. Interface: `eval` takes f64 biases, returns f64 linearization (gm, gds, gbs, current equivalent); AC lifts values into Complex64 at the stamp, never inside the model.

## Analysis language

- **MNA** — modified nodal analysis. First rows are node voltages, then **branch rows**: voltage-source currents, inductor currents (in that order).
- **BranchLayout** — the branch-row layout as a named structure built with the system (cut #3); accessors replace raw `n_nodes + i` arithmetic. Not owned by `Circuit`.
- **MnaSystem(T)** — the solver system `a`, `z` generified over the field: `f64` for DC/transient, `Complex64` for AC (cut #2). `ac_mna.rs` folds into this.
- **Field** — the small trait (`from_real`, arithmetic ops) letting one stamping implementation serve both fields.
- **Mode** — `Dc`, `Tran { dt }`, `Ac { omega }`: three modes of one system build, replacing three builders.
- **Operating point (`.op` / `.dc`)** — DC solve; nonlinear elements linearize via Newton-Raphson.
- **Newton–Raphson (NR)** — the nonlinear convergence loop, tolerance 1e-6, 100 iterations. One loop shared by DC and transient (cut #4); `analysis` owns it, `mna` owns stamping.
- **Transient (`.tran`)** — backward-Euler companion models stepped in time; starts from the operating point, not zeros (fix lands with cut #4).
- **AC sweep (`.ac`)** — frequency-domain small-signal analysis about the DC bias; stimulus amplitude comes from the voltage source's `AC` field, not a separate command setting.
- **Analysis command** — parsed directive config (`.tran`/`.ac` parameters, `AcCommand`, `AcScale`). Lives with the netlist format, not with analyses (cut #3 resolves the current mutual import).

## GUI language

- **Schematic** — the grid-drawn circuit in the egui app; convertible to and from netlist text. Node numbering comes from a union-find over grid connectivity — one implementation, shared by the writer and the painter (cut #3).
- **Element kind / CompKind** — the GUI enum paralleling element letters; silent dropping of `D`/`M` on import is removed in cut #3.
- **Plot series / PlotState** — the tab plotting adapter (cut #6): typed bounds state replacing the stringly scale fields, one `apply_bounds`, one element→series interface used by all tabs. dB/phase math is GUI-internal, computed once, not re-derived per tab.
- **Tabs** — Overview, DC, Waveforms (Voltage / Current / Differential), Frequency (magnitude / phase / branch plots).
