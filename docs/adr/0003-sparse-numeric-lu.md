# The numeric sparse LU is a fixed-order Gilbert–Peierls factor behind `solve`, guarded by the dense kernel

**Status:** accepted (2026-10-29)

TODO §3's slice 1 landed the record → pattern → Markowitz-order pipeline (`ADR 0002`)
with the dense LU untouched. This slice lands the numeric layer: `SparsePlan`
compiles the symbolic order once per structural pattern and factors/solves the
live values through it, while the dense `nalgebra` LU stays as the fallback and
the reference.

**Decision.** `MnaSystem::solve` consults the sparse path first, and the sparse
path can fail in only ways that fall back:

- **No admissible Markowitz order** (structurally stuck patterns — e.g. the
  zoo's mosfet bulk-as-source before its loads were added): `sparse_plan`
  returns `None`, the dense LU answers.
- **A numerically zero pivot** under the fixed order: `factor_solve` returns
  `None`, the dense LU answers — the exact-zero test matches the dense LU's
  own singularity notion, so the fallback is semantic, not heuristic.
- **A fresh pattern**: plans pool per thread, keyed by a cheap fingerprint
  and *verified against the full pattern* (`SparsePlan.pattern` equality), so
  a fingerprint collision costs a recompile, never a stale plan.

The invariant the design owns: **the sparse layer can only change what a solve
costs, never what it says.** On success it computes the answer (the factor is
exact algebra — unit-diagonal L by columns, U by rows, in a permutation space
where the Markowitz pivots sit on the diagonal); on any failure the byte-for-byte
previous dense path answers. NaNs propagate like values, as before.

**Factor algorithm.** Left-looking Gilbert–Peierls in elimination space: per
column, scatter the pattern entries of `elim_cols[k]`, then walk the discovery
frontier (`BTreeSet`, cleared per column) in ascending permutation-row order.
All feeds into `x[j]` come from smaller rows, so pop-first order is topological
for free; no DFS reach machinery is needed. A gathered row's final value is its
U slot; it radiates down its L column. Exact-zero factor entries are not stored
(stored zeros could only contribute zero), which keeps the factor pattern a
numeric *subset* of the symbolic fill while remaining correct.

**Kernel crossover.** Below `SMALL_SYSTEM` (16) the dense LU is simply the
better kernel and `solve` does not attempt the sparse path: measured A/B on
`examples/benchmark` showed the tiny decks (n ≤ 9) paying ~30% wall for the
plan/workspace setup while the 200-node ladder won ~4-5× (778→191 ms,
203 steps) and the 50-node ladder ~1.3×.

## Considered options

- **`sprs` (0.11) as an external backend.** Rejected: the crate is
  unmaintained since 2023, its splu imposes its own ordering (fighting the
  Markowitz contract `ADR 0002` established), and the in-house core is small
  enough to own — one left-looking loop plus substitution passes knit directly
  to the pattern types. Crates.io resolution was probed feasible (index
  reachable), so this was a design call, not a network constraint.
- **Magnitude-based numeric reordering inside the factor** (KLU-style partial
  pivoting restricted to the symbolic pattern). Rejected for this slice: MNA
  diagonals are gmin- and stub-anchored, the fallback already preserves
  dense-kind correctness on a zero pivot, and dynamic reselection would decay
  the "same order, same fill" story the plan pool is built on. Revisit with an
  ill-conditioned-deck motivating measurement.
- **Pooling the factor workspace inside the plan** (zero per-solve allocations).
  Deferred: `factor_solve` currently allocates its workspaces per call (~8
  allocations), which is the gate `SMALL_SYSTEM` papers over on tiny decks.
  Interior-mutability thread-local pools are the obvious next step, taken with
  the LU-reuse item (line 93) rather than smuggled into this slice.
- **Solving the transposed adjoint system off the same factors** (`.noise`).
  Deferred: `solve_transposed` stays dense; the adjoint rides the plan later —
  Uᵀ/Lᵀ requires the same substitution machinery mirrored, not new theory.

## Consequences

- Every existing solve flows through the plan pool after its first compile:
  the pattern is compressed per solve (an honest `n²` bitset scan) and matched,
  so per-iterate work is values + sparse arithmetic. A structural change to a
  pattern compiles a new plan automatically (no cache-invalidation API).
- The zoo comparator (`zoo_sparse_solve_matches_dense_reference_in_every_arm`)
  pins the sparse answer to the dense LU reference in all three arms (DC,
  transient, AC over `Complex64`); `sparse.rs`'s unit tests pin the factor's
  algebra on the no-diagonal cycle, the promised-fill case, the stored-zero
  skip, and the zero-pivot fallback.
- The zoo fixture itself gained anchors (`Rs1/Rs2` for the switch pair,
  `Rms/Rd` for the MOSFET source/drain, `Rta/Rtb` for the T-line hot pair):
  elements whose ports have no other path isolate at zero bias and make the
  DC arm singular in *both* solvers — the record tripwire never solved, so the
  fixture had never been asked to.
- The Markowitz scan's O(n·nnz) full sweep is paid once per pattern (pooled);
  making it incremental matters only with many distinct patterns per run.
- Benchmarks stay measured-never-asserted; the numbers above are A/B medians
  against commit `b8d92e3`'s binary, taken back-to-back on the same machine.
