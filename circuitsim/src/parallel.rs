//! Threading *inside* one run (TODO §3 "multithreaded assembly/factorization").
//!
//! Across-run parallelism (`.step` / `.temp` / `.mc`) lives in [`crate::runs`];
//! this module serves the regions inside one analysis that are embarrassingly
//! parallel and can be made **bit-identical to the serial order**:
//!
//! * the points of an `.ac` sweep (each frequency is an independent solve),
//! * the pure model evaluation of large device lists during stamping (the
//!   stamps themselves are then applied serially, in element order, so the
//!   matrix is summed in exactly the order a one-thread build uses).
//!
//! Time-stepping itself stays serial — step *n+1* needs step *n* — and so does
//! the numeric LU: the left-looking Gilbert–Peierls column order carries a data
//! dependency through every earlier column (see `docs/adr/0005`).
//!
//! # The thread budget
//!
//! A region may use [`thread_budget`] threads. It defaults to
//! [`crate::runs::default_threads`], but the run driver pins it: a deck run on
//! one `.step` worker gets a budget of 1 so nested regions never oversubscribe
//! the machine, and [`crate::run_with_threads`]`(netlist, 1)` is fully serial.
//! The budget only ever changes wall time, never results.

use std::cell::Cell;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Mutex;

thread_local! {
    static BUDGET: Cell<Option<usize>> = const { Cell::new(None) };
    /// Multi-worker regions started *by this thread* — the observable the
    /// tests use to prove a deck really took the parallel path.
    static REGIONS: Cell<u64> = const { Cell::new(0) };
}

/// Number of multi-worker regions this thread has started (see
/// [`map_ordered`], [`try_map_ordered`]). Test/benchmark instrumentation.
#[doc(hidden)]
pub fn parallel_regions() -> u64 {
    REGIONS.with(Cell::get)
}

/// Stack for helper threads: the stamp/eval closures are shallow, but the
/// default 2 MiB of a spawned thread is below the main thread's on some
/// platforms and the model code is not frugal.
const WORKER_STACK: usize = 8 * 1024 * 1024;

/// Threads an intra-run parallel region on this thread may use (≥ 1).
pub fn thread_budget() -> usize {
    BUDGET
        .with(Cell::get)
        .unwrap_or_else(crate::runs::default_threads)
        .max(1)
}

/// Pin this thread's budget for the guard's lifetime (restored on drop, also
/// on unwind). `threads` is clamped to at least 1.
pub fn with_budget(threads: usize) -> BudgetGuard {
    let previous = BUDGET.with(|b| b.replace(Some(threads.max(1))));
    BudgetGuard { previous }
}

/// Restores the previous thread budget on drop.
#[must_use = "the budget reverts when the guard is dropped"]
pub struct BudgetGuard {
    previous: Option<usize>,
}

impl Drop for BudgetGuard {
    fn drop(&mut self) {
        BUDGET.with(|b| b.set(self.previous));
    }
}

/// How many workers a region of `len` items should use when each worker should
/// own at least `min_per_worker` items: capped by the budget, 1 when the work
/// is too small to repay a thread spawn.
pub fn workers_for(len: usize, min_per_worker: usize) -> usize {
    if len == 0 {
        return 1;
    }
    let by_size = (len / min_per_worker.max(1)).max(1);
    thread_budget().min(by_size)
}

/// Evaluate `f(0..len)` and return the results **in index order**.
///
/// Items are dealt in contiguous blocks of `block` indices from one atomic
/// counter, so the calling thread and up to `workers - 1` helpers share the
/// load without a pool. `f` must be a pure function of its index (the result
/// then cannot depend on the worker count). A panic in a worker propagates out
/// of the scope exactly as a serial panic would; a failed `spawn` degrades to
/// fewer workers, never to an error.
pub fn map_ordered<T, F>(len: usize, workers: usize, block: usize, f: F) -> Vec<T>
where
    T: Send,
    F: Fn(usize) -> T + Sync,
{
    let workers = workers.max(1).min(len.max(1));
    if workers <= 1 {
        return (0..len).map(f).collect();
    }
    REGIONS.with(|r| r.set(r.get() + 1));
    let block = block.max(1);
    let blocks = len.div_ceil(block);
    let next = AtomicUsize::new(0);
    let slots: Vec<Mutex<Vec<T>>> = (0..blocks).map(|_| Mutex::new(Vec::new())).collect();
    let work = || loop {
        let b = next.fetch_add(1, Ordering::Relaxed);
        if b >= blocks {
            break;
        }
        let lo = b * block;
        let hi = (lo + block).min(len);
        let chunk: Vec<T> = (lo..hi).map(&f).collect();
        *slots[b].lock().expect("parallel block") = chunk;
    };
    std::thread::scope(|scope| {
        for _ in 1..workers {
            let _ = std::thread::Builder::new()
                .stack_size(WORKER_STACK)
                .spawn_scoped(scope, &work);
        }
        work();
    });
    let mut out = Vec::with_capacity(len);
    for slot in slots {
        out.extend(slot.into_inner().expect("parallel block"));
    }
    out
}

/// [`map_ordered`] for fallible items, with the serial loop's error contract:
/// the error returned is always the **lowest-indexed** one — the one a serial
/// `for` loop would have stopped at — whatever the worker count. Blocks past a
/// known failure are not started, so a failing sweep still stops early.
pub fn try_map_ordered<T, E, F>(
    len: usize,
    workers: usize,
    block: usize,
    f: F,
) -> Result<Vec<T>, E>
where
    T: Send,
    E: Send,
    F: Fn(usize) -> Result<T, E> + Sync,
{
    let workers = workers.max(1).min(len.max(1));
    if workers <= 1 {
        return (0..len).map(f).collect();
    }
    REGIONS.with(|r| r.set(r.get() + 1));
    let block = block.max(1);
    let blocks = len.div_ceil(block);
    let next = AtomicUsize::new(0);
    // Lowest failing *index* seen so far; blocks starting beyond it are skipped.
    let lowest_failure = AtomicUsize::new(usize::MAX);
    type Slot<T, E> = Mutex<Option<Result<Vec<T>, (usize, E)>>>;
    let slots: Vec<Slot<T, E>> = (0..blocks).map(|_| Mutex::new(None)).collect();
    let work = || loop {
        let b = next.fetch_add(1, Ordering::Relaxed);
        if b >= blocks {
            break;
        }
        let lo = b * block;
        if lo > lowest_failure.load(Ordering::Relaxed) {
            break;
        }
        let hi = (lo + block).min(len);
        let mut chunk = Vec::with_capacity(hi - lo);
        let mut outcome = None;
        for i in lo..hi {
            match f(i) {
                Ok(v) => chunk.push(v),
                Err(e) => {
                    lowest_failure.fetch_min(i, Ordering::Relaxed);
                    outcome = Some(Err((i, e)));
                    break;
                }
            }
        }
        *slots[b].lock().expect("parallel block") = Some(outcome.unwrap_or(Ok(chunk)));
    };
    std::thread::scope(|scope| {
        for _ in 1..workers {
            let _ = std::thread::Builder::new()
                .stack_size(WORKER_STACK)
                .spawn_scoped(scope, &work);
        }
        work();
    });
    let mut out = Vec::with_capacity(len);
    for slot in slots {
        match slot.into_inner().expect("parallel block") {
            Some(Ok(chunk)) => out.extend(chunk),
            Some(Err((_, e))) => return Err(e),
            // Skipped only because a lower block already failed — and that
            // block comes first in this scan, so it is returned before here.
            None => unreachable!("a block was skipped without a preceding failure"),
        }
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn map_is_in_index_order_for_any_worker_count() {
        let serial: Vec<u64> = (0..1000).map(|i| (i as u64).wrapping_mul(2654435761)).collect();
        for workers in [1, 2, 3, 8, 64] {
            for block in [1, 7, 128, 5000] {
                let got = map_ordered(1000, workers, block, |i| (i as u64).wrapping_mul(2654435761));
                assert_eq!(got, serial, "workers={workers} block={block}");
            }
        }
    }

    #[test]
    fn empty_and_tiny_inputs_work() {
        assert!(map_ordered(0, 8, 4, |i| i).is_empty());
        assert_eq!(map_ordered(1, 8, 4, |i| i + 10), vec![10]);
        assert_eq!(try_map_ordered::<usize, (), _>(0, 8, 4, Ok).unwrap(), Vec::<usize>::new());
    }

    #[test]
    fn try_map_returns_the_lowest_indexed_error() {
        // Two failures; a "first to finish" driver would sometimes report 90.
        let f = |i: usize| -> Result<usize, usize> {
            if i == 40 || i == 90 {
                if i == 40 {
                    std::thread::sleep(std::time::Duration::from_millis(20));
                }
                Err(i)
            } else {
                Ok(i)
            }
        };
        for workers in [1, 2, 4, 16] {
            for block in [1, 3, 25] {
                assert_eq!(try_map_ordered(100, workers, block, f), Err(40), "w={workers} b={block}");
            }
        }
        assert_eq!(try_map_ordered(30, 4, 5, f).unwrap().len(), 30);
    }

    #[test]
    fn budget_guard_nests_and_restores() {
        let base = thread_budget();
        {
            let _a = with_budget(3);
            assert_eq!(thread_budget(), 3);
            {
                let _b = with_budget(1);
                assert_eq!(thread_budget(), 1);
            }
            assert_eq!(thread_budget(), 3);
        }
        assert_eq!(thread_budget(), base);
        let _z = with_budget(0);
        assert_eq!(thread_budget(), 1, "zero clamps to one");
    }

    #[test]
    fn workers_for_gates_small_regions() {
        let _g = with_budget(8);
        assert_eq!(workers_for(0, 16), 1);
        assert_eq!(workers_for(10, 16), 1);
        assert_eq!(workers_for(64, 16), 4);
        assert_eq!(workers_for(10_000, 16), 8);
    }
}
