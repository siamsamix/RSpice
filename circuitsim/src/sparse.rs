//! Sparse MNA solver (TODO §3): the stamp record → structural pattern →
//! symbolic pivot order → numeric LU pipeline.
//!
//! [`MnaSystem`](crate::mna::MnaSystem) records which matrix slots each build
//! wrote into through the `stamp_add`/`stamp_sub` seam ([`StampBits`]); the
//! record compiles to a compressed structure ([`CompressedPattern`]); the
//! structure compiles to a symbolic pivot order ([`markowitz_ordering`]);
//! and [`SparsePlan`] turns a compiled order back into a numeric LU factor
//! and solve against the system's live values. The dense `nalgebra` LU keeps
//! solving as the fallback and reference: a sparse plan that fails — no
//! admissible order, or a numerically zero pivot — is silently dropped and
//! the dense path answers, so the sparse layer can only ever change what a
//! solve *costs*, never what it *says*. (LU factorization reuse, the
//! measured-out dense item next to this TODO line, rides the same plan.)
//!
//! The record is deliberately a *superset* of the matrix's structural
//! non-zeros: a slot can be visited yet numerically zero (opposing stamps
//! that cancel, a switch conductance that lands exactly on the stub value).
//! "Visited ⊆ non-zero" would be false; "non-zero ⊆ visited" is the
//! invariant that holds, and it is the sound direction for a symbolic
//! pattern — the compiler may only ever see extra structure, never miss any.
//! The numeric factor carries the same direction through: visited-but-zero
//! entries are scattered (they may be *resurrected* by fill from an earlier
//! update arriving at that slot) but exact-zero factor entries are simply
//! not stored — a stored zero could only ever contribute zero.

use std::collections::{BTreeSet, HashMap};
use std::hash::Hasher;
use std::sync::Arc;

use nalgebra::{DMatrix, DVector};

use crate::mna::Field;

/// Which matrix slots one MNA build wrote into: a bitset over the `n²`
/// slots, [`visit`](Self::visit) is O(1) per stamp.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StampBits {
    /// Matrix side length; slot `(row, col)` is bit `row·n + col`.
    n: usize,
    words: Vec<u64>,
}

impl StampBits {
    /// An empty record for an `n × n` system.
    pub fn zeros(size: usize) -> Self {
        Self {
            n: size,
            words: vec![0; size * size / 64 + 1],
        }
    }

    /// Record that slot `(row, col)` was written into. Out-of-range slots
    /// panic the same way an `[index]` write into the matrix would.
    pub fn visit(&mut self, row: usize, col: usize) {
        debug_assert!(row < self.n && col < self.n, "stamp outside the system");
        let bit = row * self.n + col;
        self.words[bit / 64] |= 1 << (bit % 64);
    }

    /// Was slot `(row, col)` written into by any stamp of this build?
    pub fn is_set(&self, row: usize, col: usize) -> bool {
        debug_assert!(row < self.n && col < self.n, "probe outside the system");
        let bit = row * self.n + col;
        self.words[bit / 64] & (1 << (bit % 64)) != 0
    }

    /// Merge another record into this one (same-size systems only, checked
    /// in debug): e.g. the DC record unioned with the AC record to compile a
    /// pattern covering both arms.
    pub fn union(&mut self, other: &Self) {
        debug_assert_eq!(self.n, other.n, "stamp records of different sizes");
        for (word, other) in self.words.iter_mut().zip(&other.words) {
            *word |= *other;
        }
    }

    /// The record as compressed row storage: column indices sorted within
    /// each row, only the visited slots retained.
    pub fn compress(&self) -> CompressedPattern {
        let mut row_ptr = Vec::with_capacity(self.n + 1);
        let mut col_idx = Vec::new();
        for row in 0..self.n {
            row_ptr.push(col_idx.len());
            for col in 0..self.n {
                if self.is_set(row, col) {
                    col_idx.push(col);
                }
            }
        }
        row_ptr.push(col_idx.len());
        CompressedPattern {
            n: self.n,
            row_ptr,
            col_idx,
        }
    }
}

/// A structural pattern in compressed row storage: per row, the sorted
/// column indices that were visited.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CompressedPattern {
    n: usize,
    row_ptr: Vec<usize>,
    col_idx: Vec<usize>,
}

impl CompressedPattern {
    /// Side length.
    pub fn n(&self) -> usize {
        self.n
    }

    /// Stored entries total.
    pub fn nnz(&self) -> usize {
        self.col_idx.len()
    }

    /// The visited columns of one row, in ascending order.
    pub fn columns_of(&self, row: usize) -> &[usize] {
        &self.col_idx[self.row_ptr[row]..self.row_ptr[row + 1]]
    }

    /// Does the pattern contain slot `(row, col)`?
    pub fn contains(&self, row: usize, col: usize) -> bool {
        self.columns_of(row).binary_search(&col).is_ok()
    }
}

/// A symbolic pivot sequence for one structural pattern.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SymbolicOrder {
    /// `(row, col)` of every elimination, in pivot order. Each row index and
    /// each column index appears exactly once: MNA branch rows have no
    /// diagonal in DC, so the ordering is genuinely unsymmetric and the two
    /// permutations are distinct.
    pub order: Vec<(u32, u32)>,
    /// Slots the eliminations create beyond the input pattern.
    pub fill: usize,
}

/// Full Markowitz ordering of a structural pattern (unsymmetric, and MNA
/// needs it: branch rows have no diagonal in DC).
///
/// At every elimination the pivot is the active entry minimizing the full
/// Markowitz cost `(row_len−1)·(col_len−1)`; ties break on the lowest packed
/// index `row·n + col`, which makes the scan order (ascending rows, set
/// iteration ascending within a row) already yield that pivot. The scan is
/// the honest `O(n·nnz)` version — the dict-of-lists bookkeeping that keeps
/// Markowitz incremental is a follow-up behind the numeric solver, not
/// groundwork this slice needs to be correct.
///
/// One guard rides on top of the raw cost: a pivot is rejected when it
/// would strand *another* alive row or column — any other row holding the
/// pivot column as its only entry, or any other column holding the pivot
/// row as its only entry. Without it, greedy cost-minimal pivots happily
/// consume a shared column out from under a row that still needs it and
/// report `None` on patterns that admit an order (MNA is full of these:
/// one-element node pairs share their branch column). The guard is
/// still greedy — a transversal existing does not guarantee `Some`, only
/// makes it far more likely — documented as groundwork behavior.
///
/// Returns `None` when no admissible pivot remains among the active
/// entries: the surviving structure has nothing left to eliminate (no
/// single-entry solve can come out of that shape).
pub fn markowitz_ordering(pattern: &CompressedPattern) -> Option<SymbolicOrder> {
    let n = pattern.n();
    // Active bookkeeping: alive rows' visited columns and alive columns'
    // visited rows, always in sync — a dead row/column leaves both sides.
    let mut rows_alive = vec![true; n];
    let mut cols_alive = vec![true; n];
    let mut row_cols: Vec<BTreeSet<usize>> = (0..n)
        .map(|row| pattern.columns_of(row).iter().copied().collect())
        .collect();
    let mut col_rows: Vec<BTreeSet<usize>> = vec![BTreeSet::new(); n];
    for (row, cols) in row_cols.iter().enumerate() {
        for &col in cols {
            col_rows[col].insert(row);
        }
    }

    let mut order = Vec::with_capacity(n);
    let mut fill = 0usize;
    for _ in 0..n {
        // The cheapest active pivot; the strict `<` keeps the first minimum,
        // and candidates arrive in packed order, so ties break lowest.
        let mut best: Option<(usize, usize, usize)> = None;
        for row in 0..n {
            if !rows_alive[row] {
                continue;
            }
            let row_len = row_cols[row].len();
            for &col in &row_cols[row] {
                debug_assert!(cols_alive[col], "dead column revisited");
                // The stranding guard: skip pivots whose elimination empties
                // another alive row or column outright.
                let strands_a_row = col_rows[col]
                    .iter()
                    .any(|&other| other != row && row_cols[other].len() == 1);
                if strands_a_row {
                    continue;
                }
                let strands_a_col = row_cols[row]
                    .iter()
                    .any(|&other| other != col && col_rows[other].len() == 1);
                if strands_a_col {
                    continue;
                }
                let cost = (row_len - 1) * (col_rows[col].len() - 1);
                let better = match best {
                    None => true,
                    Some((best_cost, _, _)) => cost < best_cost,
                };
                if better {
                    best = Some((cost, row, col));
                }
            }
        }
        let (_, pivot_row, pivot_col) = best?;

        // Eliminating (pivot_row, pivot_col) crosses the pivot's column
        // neighbors with its row neighbors: every such pair becomes an
        // entry, and the ones not already present are fill.
        let cols_r: Vec<usize> = row_cols[pivot_row].iter().copied().collect();
        let rows_c: Vec<usize> = col_rows[pivot_col].iter().copied().collect();
        for &row in &rows_c {
            if row == pivot_row {
                continue;
            }
            for &col in &cols_r {
                if col == pivot_col {
                    continue;
                }
                if row_cols[row].insert(col) {
                    fill += 1;
                }
                col_rows[col].insert(row);
            }
        }
        // The pivot's row/column die; scrub them from every active set.
        for &col in &cols_r {
            col_rows[col].remove(&pivot_row);
        }
        for &row in &rows_c {
            row_cols[row].remove(&pivot_col);
        }
        rows_alive[pivot_row] = false;
        cols_alive[pivot_col] = false;
        row_cols[pivot_row].clear();
        col_rows[pivot_col].clear();
        order.push((pivot_row as u32, pivot_col as u32));
    }
    Some(SymbolicOrder { order, fill })
}

// ─── Numeric solver ─────────────────────────────────────────────────────────

/// A compiled pivot order ready to *factor and solve* against live system
/// values: the numeric consumer of the record → pattern →
/// [`markowitz_ordering`] pipeline.
///
/// Everything structural is precomputed once — the row/column permutations
/// into elimination index space and the column-compile the scatter needs —
/// so one solve after another pays only for values and arithmetic. Plans are
/// immutable and pooled per thread (see [`sparse_plan`]).
pub(crate) struct SparsePlan {
    /// The exact pattern this plan was compiled from: a memo hit is only
    /// used after a full pattern match, so a fingerprint collision can slow
    /// the lookup but never hand back a wrong plan.
    pattern: CompressedPattern,
    /// Row permutation: `row_perm[r]` is elimination index of original row
    /// `r` — the scatter direction.
    row_perm: Vec<usize>,
    /// `elim_rows[k]` / `elim_cols[k]`: the original row / column eliminated
    /// at elimination `k`.
    elim_rows: Vec<usize>,
    elim_cols: Vec<usize>,
    /// Columns-compile of `pattern`: `col_rows[col_ptr[c]..col_ptr[c+1]]`
    /// lists the original rows with an entry in original column `c`.
    col_ptr: Vec<usize>,
    col_rows: Vec<usize>,
}

impl SparsePlan {
    /// Compile a plan for `pattern`, or `None` when no Markowitz order
    /// exists (structurally stuck shapes keep solving densely).
    fn compile(pattern: &CompressedPattern) -> Option<Self> {
        let order = markowitz_ordering(pattern)?;
        let n = pattern.n();
        let mut row_perm = vec![0; n];
        let mut elim_rows = Vec::with_capacity(n);
        let mut elim_cols = Vec::with_capacity(n);
        for (k, (r, c)) in order.order.iter().enumerate() {
            let (r, c) = (*r as usize, *c as usize);
            row_perm[r] = k;
            elim_rows.push(r);
            elim_cols.push(c);
        }
        // Transposed compile: per original column, its original rows. The
        // count-then-place pass keeps columns internally row-ascending.
        let mut counts = vec![0; n];
        for &col in &pattern.col_idx {
            counts[col] += 1;
        }
        let mut col_ptr = Vec::with_capacity(n + 1);
        let mut offset = 0;
        for count in counts {
            col_ptr.push(offset);
            offset += count;
        }
        col_ptr.push(offset);
        let mut cursor = col_ptr[..n].to_vec();
        let mut col_rows = vec![0; pattern.nnz()];
        for row in 0..n {
            for &col in pattern.columns_of(row) {
                col_rows[cursor[col]] = row;
                cursor[col] += 1;
            }
        }
        Some(Self {
            pattern: pattern.clone(),
            row_perm,
            elim_rows,
            elim_cols,
            col_ptr,
            col_rows,
        })
    }

    /// Factor the live matrix values and solve A·x = rhs.
    ///
    /// The pivot sequence is the compiled Markowitz order, fixed at plan
    /// compile time — no magnitude reselection happens at factor time. A
    /// numerically zero pivot (the exact-zero test, the dense LU's own
    /// singularity notion) returns `None`, and the caller solves densely: a
    /// sparse failure never changes what a solve reports, only what it
    /// costs. NaNs propagate into the result like any other value, exactly
    /// as they would through the dense path.
    pub(crate) fn factor_solve<T: Field>(
        &self,
        a: &DMatrix<T>,
        rhs: &DVector<T>,
    ) -> Option<DVector<T>> {
        let n = self.pattern.n();
        // Permutation-space workspace for the column being factored. `x[i]`
        // is live when `stamp[i] == gen`, with `touched` naming every live
        // slot; the counter resets the workspace without clearing it.
        let mut x = vec![T::lift(0.0); n];
        let mut stamp = vec![0u32; n];
        let mut touched: Vec<usize> = Vec::with_capacity(n);
        let mut gen: u32 = 0;
        // Discovery frontier of the left-looking gather (Gilbert–Peierls
        // reach): rows joined by the L factors of already-gathered rows,
        // cleared per column.
        let mut frontier = BTreeSet::new();
        // The factors: U by rows (each push's column is strictly later in
        // its row's push history, so per-row stores stay sorted), L by
        // columns (unit diagonal implied), the diagonal separately.
        let mut urow: Vec<Vec<(usize, T)>> = Vec::new();
        let mut lcol: Vec<Vec<(usize, T)>> = Vec::new();
        urow.resize_with(n, Vec::new);
        lcol.resize_with(n, Vec::new);
        let mut diag = vec![T::lift(0.0); n];

        for pk in 0..n {
            gen += 1;
            touched.clear();
            frontier.clear();
            // Scatter the elimination column: original column `elim_cols[pk]`
            // spread over its pattern rows in permutation space. Every pivot
            // slot is structurally present — the order was picked from this
            // pattern — so the diagonal is seeded with the scatter.
            let col = self.elim_cols[pk];
            for &r in &self.col_rows[self.col_ptr[col]..self.col_ptr[col + 1]] {
                let i = self.row_perm[r];
                stamp[i] = gen;
                touched.push(i);
                frontier.insert(i);
                x[i] = a[(r, col)];
            }
            // Gather the column down its reach walking rows ascending. Every
            // legal feed into `x[j]` comes from a smaller row, so pop-first
            // order is topological for free: when `j` pops, every feeding
            // row already radiated into it. A gathered row's final value
            // is its U slot in this column; it feeds every row its L-column
            // touches. At the pivot the column stops radiating — rows beyond
            // it belong to this column's L, and their elimination belongs to
            // later columns.
            let mut d = T::lift(0.0);
            while let Some(j) = frontier.pop_first() {
                let jx = x[j];
                if j == pk {
                    d = jx;
                    break;
                }
                if jx == T::lift(0.0) {
                    continue;
                }
                urow[j].push((pk, jx));
                for &(i, l) in &lcol[j] {
                    if stamp[i] == gen {
                        x[i] -= l * jx;
                    } else {
                        stamp[i] = gen;
                        x[i] = T::lift(0.0) - l * jx;
                        touched.push(i);
                        frontier.insert(i);
                    }
                }
            }
            if d == T::lift(0.0) {
                return None;
            }
            diag[pk] = d;
            // The rows gathered beyond the pivot are this column's L entries.
            for &i in &touched {
                if i <= pk {
                    continue;
                }
                let v = x[i];
                if v != T::lift(0.0) {
                    lcol[pk].push((i, v / d));
                }
            }
        }

        // Solve the permuted system: y = P·rhs, the unit-L forward pass, the
        // U back pass over the same buffer (entries above each pivot are the
        // factor values already solved), then unpermute into the caller's
        // column order.
        let mut y: Vec<T> = vec![T::lift(0.0); n];
        for k in 0..n {
            y[k] = rhs[self.elim_rows[k]];
        }
        for j in 0..n {
            let yj = y[j];
            if yj == T::lift(0.0) {
                continue;
            }
            for &(i, l) in &lcol[j] {
                y[i] -= l * yj;
            }
        }
        for k in (0..n).rev() {
            let mut s = y[k];
            for &(c, u) in &urow[k] {
                s -= u * y[c];
            }
            y[k] = s / diag[k];
        }
        let mut out = vec![T::lift(0.0); n];
        for (k, &v) in y.iter().enumerate() {
            out[self.elim_cols[k]] = v;
        }
        Some(DVector::from_vec(out))
    }
}

/// Cheap plan fingerprint: size, entry count, and the sorted column list.
/// Collisions are expected-but-rare; a full pattern equality check inside
/// the pool keeps them honest ([`SparsePlan::pattern`]).
fn fingerprint(pattern: &CompressedPattern) -> u64 {
    use std::collections::hash_map::DefaultHasher;
    let mut hasher = DefaultHasher::new();
    hasher.write_usize(pattern.n());
    hasher.write_usize(pattern.nnz());
    for &col in &pattern.col_idx {
        hasher.write_usize(col);
    }
    hasher.finish()
}

/// The plan pool. Structural patterns recur — the same record is compressed
/// on every NR iterate and every timestep — and the Markowitz scan dominates
/// a plan's cost, so plans are pooled in a thread-local map keyed by the
/// fingerprint above. Pool entries never go stale: a candidate is used only
/// after matching the full pattern, and the scan is the honest O(n·nnz)
/// version, so the *first* build per pattern pays it once.
pub(crate) fn sparse_plan(pattern: &CompressedPattern) -> Option<Arc<SparsePlan>> {
    thread_local! {
        static PLANS: std::cell::RefCell<HashMap<u64, Vec<Arc<SparsePlan>>>> =
            std::cell::RefCell::new(HashMap::new());
    }

    let key = fingerprint(pattern);
    PLANS.with(|pool| {
        let mut pool = pool.borrow_mut();
        // Unbounded structural churn would be a leak; a proportional cap
        // trades the tail of the pool away. MNA per thread has a handful of
        // distinct patterns, so clearing at four digits is already generous.
        if pool.len() >= 1024 {
            pool.clear();
        }
        if let Some(bucket) = pool.get(&key) {
            if let Some(found) = bucket.iter().find(|plan| plan.pattern == *pattern) {
                return Some(Arc::clone(found));
            }
        }
        let plan = Arc::new(SparsePlan::compile(pattern)?);
        pool.entry(key).or_default().push(Arc::clone(&plan));
        Some(plan)
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Compress a record built with individual visits.
    #[test]
    fn compress_matches_visited_slots() {
        let mut bits = StampBits::zeros(4);
        bits.visit(1, 0);
        bits.visit(1, 3);
        bits.visit(2, 2);
        bits.visit(0, 0);
        let pat = bits.compress();
        assert_eq!(pat.n(), 4);
        assert_eq!(pat.nnz(), 4);
        assert_eq!(pat.columns_of(0), &[0]);
        assert_eq!(pat.columns_of(1), &[0, 3]);
        assert_eq!(pat.columns_of(2), &[2]);
        assert_eq!(pat.columns_of(3), &[] as &[usize]);
        assert!(pat.contains(1, 3));
        assert!(!pat.contains(3, 1));
    }

    /// Visits are idempotent through the set representation.
    #[test]
    fn repeated_visits_stay_single_entries() {
        let mut bits = StampBits::zeros(2);
        bits.visit(0, 1);
        bits.visit(0, 1);
        bits.visit(1, 0);
        assert_eq!(bits.compress().nnz(), 2);
    }

    /// A diagonal-only pattern pivots along the diagonal with no fill.
    #[test]
    fn diagonal_order_is_the_identity() {
        let mut bits = StampBits::zeros(3);
        for i in 0..3 {
            bits.visit(i, i);
        }
        let order = markowitz_ordering(&bits.compress()).unwrap();
        assert_eq!(order.order, vec![(0, 0), (1, 1), (2, 2)]);
        assert_eq!(order.fill, 0);
    }

    /// A full 2×2 has every candidate at cost 1; the tie must land on the
    /// low end of the packed order, the (0,0) pivot.
    #[test]
    fn full_tie_breaks_to_lowest_packed_pivot() {
        let mut bits = StampBits::zeros(2);
        for row in 0..2 {
            for col in 0..2 {
                bits.visit(row, col);
            }
        }
        let order = markowitz_ordering(&bits.compress()).unwrap();
        assert_eq!(order.order, vec![(0, 0), (1, 1)]);
        assert_eq!(order.fill, 0);
    }

    /// Tridiagonal: each pivot's cross product is already present.
    #[test]
    fn tridiagonal_needs_no_fill() {
        let mut bits = StampBits::zeros(3);
        for i in 0..3 {
            bits.visit(i, i);
        }
        bits.visit(0, 1);
        bits.visit(1, 0);
        bits.visit(1, 2);
        bits.visit(2, 1);
        let order = markowitz_ordering(&bits.compress()).unwrap();
        assert_eq!(order.fill, 0);
        assert_eq!(order.order.len(), 3);
    }

    /// A 3-cycle with no diagonals (the pure-MNA shape): every elimination
    /// is a leaf, so the pivots walk the cycle in order at zero fill.
    #[test]
    fn three_cycle_orders_along_the_ring() {
        let mut bits = StampBits::zeros(3);
        bits.visit(0, 1);
        bits.visit(1, 2);
        bits.visit(2, 0);
        let order = markowitz_ordering(&bits.compress()).unwrap();
        assert_eq!(order.order, vec![(0, 1), (1, 2), (2, 0)]);
        assert_eq!(order.fill, 0);
    }

    /// A pattern whose only pivots have both neighbor counts ≥ 2: the first
    /// elimination must synthesize the missing cross entry, fill = 1, and
    /// the rest then pivots down the synthesized diagonal.
    #[test]
    fn forced_cross_pivot_creates_one_fill() {
        let mut bits = StampBits::zeros(3);
        bits.visit(0, 0);
        bits.visit(0, 1);
        bits.visit(1, 0);
        bits.visit(1, 2);
        bits.visit(2, 1);
        bits.visit(2, 2);
        let order = markowitz_ordering(&bits.compress()).unwrap();
        assert_eq!(order.order, vec![(0, 0), (1, 1), (2, 2)]);
        assert_eq!(order.fill, 1);
    }

    /// Every ordering is a permutation on both axes — rows and columns each
    /// used exactly once (the unsymmetric-MNA guarantee).
    #[test]
    fn orderings_are_row_and_column_permutations() {
        let shapes: Vec<(usize, Vec<(usize, usize)>)> = vec![
            // 4×4 diagonal
            (4, vec![(0, 0), (1, 1), (2, 2), (3, 3)]),
            // 4×4 full
            (
                4,
                (0..4).flat_map(|r| (0..4).map(move |c| (r, c))).collect(),
            ),
            // 3×3 band with the forced cross pivot
            (3, vec![(0, 0), (0, 1), (1, 0), (1, 2), (2, 1), (2, 2)]),
        ];
        for (n, shape) in shapes {
            let mut bits = StampBits::zeros(n);
            for (r, c) in &shape {
                bits.visit(*r, *c);
            }
            let order = markowitz_ordering(&bits.compress()).unwrap();
            let mut rows: Vec<u32> = order.order.iter().map(|o| o.0).collect();
            let mut cols: Vec<u32> = order.order.iter().map(|o| o.1).collect();
            rows.sort_unstable();
            cols.sort_unstable();
            assert_eq!(rows, (0..n as u32).collect::<Vec<_>>());
            assert_eq!(cols, (0..n as u32).collect::<Vec<_>>());
        }
    }

    /// Rows alive with no active entry: no ordering exists.
    #[test]
    fn structurally_stuck_patterns_report_none() {
        let mut bits = StampBits::zeros(2);
        bits.visit(0, 0);
        let pat = bits.compress();
        assert!(markowitz_ordering(&pat).is_none());
    }

    // ─── Numeric solver ────────────────────────────────────────────────────

    /// The 3-cycle, the pure-MNA shape with no diagonals and different
    /// row/column permutations: the factor must respect both axes.
    #[test]
    fn sparse_lu_solves_the_three_cycle() {
        let mut bits = StampBits::zeros(3);
        bits.visit(0, 1);
        bits.visit(1, 2);
        bits.visit(2, 0);
        let plan = SparsePlan::compile(&bits.compress()).unwrap();
        // A = [[0, 2, 0], [0, 0, 3], [1, 0, 0]]: x = (b₂, b₀/2, b₁/3).
        let a = DMatrix::from_row_slice(3, 3, &[0.0, 2.0, 0.0, 0.0, 0.0, 3.0, 1.0, 0.0, 0.0]);
        let rhs = DVector::from_vec(vec![4.0, 3.0, 2.0]);
        let x = plan.factor_solve(&a, &rhs).unwrap();
        assert_eq!(x.as_slice(), &[2.0, 2.0, 1.0]);
    }

    /// A structurally absent diagonal the ordering promised as fill: the
    /// first elimination creates the (1,1) pivot value out of a visited-zero
    /// slot, and the elimination continues on the resurrected value.
    #[test]
    fn sparse_lu_carries_the_promised_fill() {
        let mut bits = StampBits::zeros(3);
        bits.visit(0, 0);
        bits.visit(0, 1);
        bits.visit(1, 0);
        bits.visit(1, 2);
        bits.visit(2, 1);
        bits.visit(2, 2);
        let plan = SparsePlan::compile(&bits.compress()).unwrap();
        // [[2, 1, 0], [2, 0, 3], [0, 1, 1]]: A·x with x = (1, 2, 3) is
        // (4, 11, 5); the (1,1) slot is visited-zero yet pivots because the
        // promised fill arrives there with value −L₁₀·U₀₁.
        let a = DMatrix::from_row_slice(3, 3, &[2.0, 1.0, 0.0, 2.0, 0.0, 3.0, 0.0, 1.0, 1.0]);
        let rhs = DVector::from_vec(vec![4.0, 11.0, 5.0]);
        let x = plan.factor_solve(&a, &rhs).unwrap();
        assert_eq!(x.as_slice(), &[1.0, 2.0, 3.0]);
    }

    /// A pivot that revisits exactly zero goes back to the dense solver:
    /// `None` is the caller's fallback signal, never a panic or a NaN flood.
    #[test]
    fn sparse_lu_reports_none_on_zero_pivot() {
        let mut bits = StampBits::zeros(2);
        for row in 0..2 {
            for col in 0..2 {
                bits.visit(row, col);
            }
        }
        let plan = SparsePlan::compile(&bits.compress()).unwrap();
        let a = DMatrix::from_row_slice(2, 2, &[1.0, 1.0, 0.0, 0.0]);
        let rhs = DVector::from_vec(vec![1.0, 0.0]);
        assert!(plan.factor_solve(&a, &rhs).is_none());
    }

    /// Visited-but-zero factor entries are simply not stored; the answer is
    /// unaffected because a stored zero could only contribute zero.
    #[test]
    fn zero_valued_entries_skip_storage_without_changing_the_answer() {
        let mut bits = StampBits::zeros(2);
        for row in 0..2 {
            for col in 0..2 {
                bits.visit(row, col);
            }
        }
        let plan = SparsePlan::compile(&bits.compress()).unwrap();
        // [[1, 1], [0, 2]]·x = [2, 4] → x = [0, 2]; the (1,0) slot is
        // visited but exactly zero, so L keeps no entry for it.
        let a = DMatrix::from_row_slice(2, 2, &[1.0, 1.0, 0.0, 2.0]);
        let rhs = DVector::from_vec(vec![2.0, 4.0]);
        let x = plan.factor_solve(&a, &rhs).unwrap();
        assert_eq!(x.as_slice(), &[0.0, 2.0]);
    }

    /// Identical patterns share one pooled plan; a pattern that differs
    /// gets its own. (`Arc::ptr_eq` is the observable.)
    #[test]
    fn sparse_plan_pool_reuses_identical_patterns() {
        let mut bits = StampBits::zeros(3);
        for i in 0..3 {
            bits.visit(i, i);
        }
        let diagonal = bits.compress();
        let first = sparse_plan(&diagonal).expect("diagonal pattern orders");
        let second = sparse_plan(&diagonal).expect("same pattern");
        assert!(Arc::ptr_eq(&first, &second));
        bits.visit(0, 1);
        let richer = sparse_plan(&bits.compress()).expect("the richer pattern orders too");
        assert!(!Arc::ptr_eq(&first, &richer));
    }
}
