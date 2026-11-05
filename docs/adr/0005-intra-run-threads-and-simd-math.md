# Threads inside one run, incremental Markowitz, and opt-in AVX2 math

**Status:** accepted (2026-11-05)

TODO §3 had two open rows: multithreading *inside* one solve (across-run
threading already existed, §11) and LTspice-style AVX2 transcendental math.

## Decision

**1. Parallelize only what stays bit-identical to the serial order.**
`parallel.rs` provides `map_ordered` / `try_map_ordered` (scoped threads, one
atomic block counter, results collected in index order, lowest-indexed error
wins) and a thread-local *budget*: `run_with_threads(n)` pins it to `n` for a
single deck, and the `.step`/`.mc` plan workers split it (`n / workers`), so
nesting never oversubscribes. Two regions use it:

* **`.ac` and `.noise` frequency points** — independent solves.
* **Device evaluation while stamping** (MOSFET, BJT, JFET): *evaluate in
  parallel, stamp serially*. Phase 1 (serial) resolves models and walks the
  shared junction-limiter state; phase 2 (parallel) runs the pure model
  evaluations; phase 3 (serial) applies the stamps in element order, so every
  matrix entry is summed in exactly the one-thread order.

Results are bit-for-bit independent of the thread count (tests compare full
`Debug` dumps at 1/2/3/4/5/8 threads and assert the parallel path actually ran).

**2. Not parallelized, on purpose.** Time stepping (step n+1 needs step n) and
the numeric LU: the left-looking Gilbert–Peierls column order carries a data
dependency through every earlier column, and the factor cache (ADR 0004) is a
per-thread `Rc` structure. Parallel factorization would need an elimination-tree
schedule and would change summation order — a separate decision.

**3. Incremental Markowitz.** Profiling a 400-stage device array showed the
time was not in device evaluation at all but in the `O(n·nnz)` ordering scan
(12.1 s for `.op`). The scan is replaced by a `(cost, row, col)` min-heap with
lazy invalidation: after each pivot only the entries of the rows/columns whose
length changed are re-priced. The pivot sequence is identical (a guard-failed
entry stays failed for as long as it exists — the argument is in the function's
docs), checked by a 1500-pattern randomized test against the retained scan
oracle. `.op` 12.1 s → 1.46 s; outputs byte-identical.

**4. AVX2 math is opt-in.** `fastmath.rs` has slice kernels `exp ln log10 sin cos
tan pow` on 4-lane AVX2+FMA (runtime detection; `RSPICE_NO_SIMD` forces
scalar), measured ≤ 4 ulp from libm (`pow` ≤ 4 + 2·|y·ln x|). The *scalar*
backend is libm exactly. Because the vector kernels are not libm, the engine
reaches them only through `.options fastmath=1` (default off, so every deck's
results stay byte-stable). Wired so far: the diode junction exponentials, one
batch per stamp pass. The pass also stopped recomputing the
model/temperature-only `is_at` (a `powf` + `exp`) per diode per iterate — the
same number, cached per model card, bit-identical.

## Measured (release, 2 cores)

| case | before | after |
|---|---|---|
| 400-stage BJT/JFET/MOSFET/diode array `.op` | 12.1 s | 1.46 s (ordering) |
| same deck, `.ac dec 10`, `-j1` → `-j2` | 15.2 s | 4.76 s → 3.36 s |
| kernel throughput exp / ln / log10 / sin / tan | — | 2.2× / 1.4× / 2.8× / 4.4× / 5.2× vs scalar |

Device-evaluation threading gave no measurable win on that deck: after the
ordering fix the remaining time is dense `O(n²)` work (`residual`, `compress`,
matrix zeroing), not model evaluation. It pays only for decks whose per-device
model cost dominates; the 128-devices-per-worker gate keeps it off otherwise.

## Still open

Sparse assembly/residual (the dense `n²` terms above), parallel factorization,
and wiring the AVX2 kernels beyond diodes (BJT/B-source evaluation is per-device
and not naturally batched).
