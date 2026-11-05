//! The run drivers (`.step`, `.mc`): how many decks, minted how, assembled
//! into what.
//!
//! `analysis.rs` holds the seven engines and
//! [`run_single`](crate::analysis::run_single) — what ONE deck produces.
//! This module holds the other question. The driver protocol — re-parse
//! with the row's overrides (or a straight clone), strip the deck's own
//! driver card, run once, land one [`StepRun`], then mirror the first run
//! into the top-level result — lives here exactly once ([`run_plans`]), so
//! the mirror invariant ("runs present ⇔ top-level result fields are the
//! first run's") is enforced by construction instead of spelled out per
//! driver and re-inferred downstream (the stepped `.raw` plot has to read
//! it).

use std::collections::HashMap;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Mutex;

use crate::analysis::{
    run_single, sweep_values, AcResult, DcResult, DcSweepPoint, DcSweepResult, FourResult,
    MeasureResult, NoiseResult, SimulationResult, TfResult, TranPoint, TranResult,
};
use crate::error::{Result, SimError};
use crate::mc;
use crate::netlist::{DcSweepValues, Netlist, SaveBiasCommand, StepSpec, StepValues};

// ─── Result shape ────────────────────────────────────────────────────────────

/// The result of one `.step` combination.
#[derive(Debug, Clone)]
pub struct StepRun {
    /// Swept value per step target, in `step_targets` order.
    pub values: Vec<f64>,
    pub dc: Option<DcResult>,
    pub dc_sweep: Option<DcSweepResult>,
    pub tran: Option<TranResult>,
    pub ac: Option<AcResult>,
    pub tf: Option<TfResult>,
    /// `.meas` results for this step run.
    pub measurements: Vec<MeasureResult>,
    /// `.four` result for this step run.
    pub four: Option<FourResult>,
    /// `.noise` result for this step run.
    pub noise: Option<NoiseResult>,
}

// ─── Drivers ─────────────────────────────────────────────────────────────────

/// Run every analysis once per `.step` combination, or a single pass when
/// the deck has no `.step` cards. A `.mc` deck instead runs once per seeded
/// Monte Carlo run ([`mc::apply_model_draws`] and the re-parse's draw seed do
/// the worrying — TODO §8).
///
/// The runs of a stepped / Monte Carlo deck are independent decks, so they
/// are distributed over [`default_threads`] worker threads (TODO §11 "parallel
/// parametric sweeps"); results land in plan order and are bit-identical to a
/// serial run. [`run_with_threads`] pins the worker count.
pub fn run(netlist: &Netlist) -> Result<SimulationResult> {
    run_with_threads(netlist, default_threads())
}

/// Worker threads used for the independent runs of one deck: the
/// `RSPICE_THREADS` environment variable when it holds a positive integer,
/// else the machine's available parallelism.
pub fn default_threads() -> usize {
    std::env::var("RSPICE_THREADS")
        .ok()
        .and_then(|v| v.trim().parse::<usize>().ok())
        .filter(|n| *n > 0)
        .unwrap_or_else(|| {
            std::thread::available_parallelism().map(|n| n.get()).unwrap_or(1)
        })
}

/// [`run`] with an explicit worker count (`1` = fully serial). The count only
/// changes wall time: runs are minted and solved independently and collected
/// in plan order, so the result — and which error surfaces first — never
/// depends on it.
pub fn run_with_threads(netlist: &Netlist, threads: usize) -> Result<SimulationResult> {
    // Intra-run regions (AC points, large device lists) share this count: a
    // single deck may use all of it; plan workers split it (see
    // `execute_plans`), so nesting never oversubscribes the machine.
    let _budget = crate::parallel::with_budget(threads);
    if netlist.analysis.mc.is_some() {
        return run_mc(netlist, threads);
    }
    let mut specs = netlist.analysis.steps.clone();
    if specs.is_empty() {
        // A single `.temp` value simply sets the run temperature; several
        // values are the same thing as `.step temp LIST …`.
        if netlist.analysis.temperatures.len() <= 1 {
            let result = run_single(netlist)?;
            write_savebias(netlist, &result)?;
            return Ok(result);
        }
        specs.push(StepSpec {
            target: "temp".to_string(),
            values: StepValues::List(netlist.analysis.temperatures.clone()),
        });
    } else if netlist.analysis.temperatures.len() > 1
        && !specs.iter().any(|s| s.target == "temp")
    {
        return Err(SimError::Analysis(
            "several .temp values and .step cards were both given —              use .step temp LIST … or a single .temp"
                .into(),
        ));
    }
    let targets: Vec<String> = specs.iter().map(|s| s.target.clone()).collect();
    let base_temperature = netlist.circuit.temperature;
    let plans = step_combinations(&specs)?
        .into_iter()
        .map(|combo| {
            let temperature = Some(
                targets
                    .iter()
                    .position(|t| t == "temp")
                    .map(|i| combo[i])
                    .unwrap_or(base_temperature),
            );
            let overrides: Vec<(String, f64)> = targets
                .iter()
                .cloned()
                .zip(combo.iter().copied())
                .filter(|(target, _)| target != "temp")
                .collect();
            RunPlan::Step {
                values: combo,
                temperature,
                overrides,
            }
        })
        .collect();
    run_plans(netlist, plans, targets, ".step produced no runs", threads)
}

/// The Monte Carlo driver: `.mc <runs> …` re-runs the deck `runs` times, once
/// per run index. Each run re-parses the source with the hidden
/// [`mc::RUN_KEY`] override (so `random()`/`gauss()` sorts of parameter draws
/// re-roll the way `.step` runs do), then [`mc::apply_model_draws`] re-binds
/// every tolerance-bearing model card to per-instance clones and the run is
/// solved as-is. Runs land in the same `runs` vector `.step` fills, tagged
/// with their run index (`step_targets: ["mc"]`), so the CLI statistics and
/// overlays read them like step runs.
fn run_mc(netlist: &Netlist, threads: usize) -> Result<SimulationResult> {
    if !netlist.analysis.steps.is_empty() || netlist.analysis.temperatures.len() > 1 {
        return Err(SimError::Analysis(
            "RSpice runs .mc on an unstepped deck; .step and .mc are separate run \
             drivers — comment out the card you are not using"
                .into(),
        ));
    }
    let mc = netlist
        .analysis
        .mc
        .as_ref()
        .ok_or_else(|| SimError::Analysis("run_mc without an .mc card".into()))?;
    if mc.runs > MAX_STEP_RUNS {
        return Err(SimError::Analysis(format!(
            ".mc would run {} simulations, over the {MAX_STEP_RUNS}-run ceiling",
            mc.runs
        )));
    }
    let base_seed = mc.seed.unwrap_or(netlist.mc_seed);
    let plans = (0..mc.runs)
        .map(|run| RunPlan::Mc {
            index: run,
            seed: mc::run_mix(base_seed, run),
        })
        .collect();
    run_plans(
        netlist,
        plans,
        vec!["mc".to_string()],
        ".mc produced no runs",
        threads,
    )
}

// ─── `.savebias` ─────────────────────────────────────────────────────────────

/// `.savebias`: once the run finished, write the solved DC snapshot it
/// named to disk in `.nodeset` text form — the form [`crate::netlist`]`'s `
/// `.loadbias` reads straight back. Selection mirrors LTspice: a transient
/// snapshot (`time=`/`repeat`) beats a `.dc` sweep point (`DC1=..DC3=`)
/// beats the operating point; nothing at all to save leaves any existing
/// file alone. Without a card this is a no-op, so every driver can call it
/// at its tail.
#[cfg(target_arch = "wasm32")]
fn write_savebias(netlist: &Netlist, _result: &SimulationResult) -> Result<()> {
    if netlist.analysis.savebias.is_some() {
        return Err(SimError::Analysis(
            ".savebias cannot write files on the web build".into(),
        ));
    }
    Ok(())
}

#[cfg(not(target_arch = "wasm32"))]
fn write_savebias(netlist: &Netlist, result: &SimulationResult) -> Result<()> {
    use std::path::{Path, PathBuf};

    let Some(cmd) = netlist.analysis.savebias.as_ref() else {
        return Ok(());
    };
    // Which run to save from: `step=<n>` names the 1-based step run of a
    // `.step`/`.mc` deck; otherwise the mirrored top-level view (run 0's
    // copy of a stepped deck, the single run otherwise) — the state
    // LTspice's most recently solved solution is.
    let (dc, dc_sweep, tran): (
        Option<&DcResult>,
        Option<&DcSweepResult>,
        Option<&TranResult>,
    ) = match cmd.step {
        Some(step) => {
            let run = result.runs.get(step - 1).ok_or_else(|| {
                SimError::Analysis(format!(
                    ".savebias step={step} names a run past the {} runs this deck made",
                    result.runs.len()
                ))
            })?;
            (run.dc.as_ref(), run.dc_sweep.as_ref(), run.tran.as_ref())
        }
        None => (
            result.dc.as_ref(),
            result.dc_sweep.as_ref(),
            result.tran.as_ref(),
        ),
    };

    // The snapshot, in order of analysis-kind precedence.
    let voltages: Option<&[f64]> = if let Some(tran) = tran {
        let want = tran_point(tran, cmd);
        want.map(|p| p.node_voltages.as_slice())
    } else if let Some(sweep) = dc_sweep {
        let want = sweep_point(sweep, cmd);
        want.map(|p| p.node_voltages.as_slice())
    } else {
        dc.map(|dc| dc.node_voltages.as_slice())
    };
    let Some(voltages) = voltages else {
        return Ok(()); // nothing solved worth writing
    };

    // One `.nodeset` entry per net, deck-spelled names, finite values — the
    // exact shape the loader folds back into the deck.
    let mut text = String::from("* .savebias snapshot (RSpice) — load it back with .loadbias\n");
    let mut kept = 0;
    for (i, &v) in voltages.iter().enumerate() {
        let name = netlist.circuit.node_name(i);
        if name.is_empty() || !v.is_finite() {
            continue;
        }
        text.push_str(&format!(".nodeset V({name})={v}\n"));
        kept += 1;
    }
    if kept == 0 {
        return Ok(());
    }

    // Path: absolute travels as-is; a file-backed deck saves next to
    // itself; otherwise the working directory spells it.
    let bare = cmd.file.trim().trim_matches(|c| c == '"' || c == '\'');
    let bare = bare.trim();
    if bare.is_empty() {
        return Err(SimError::Analysis(
            ".savebias needs a filename to write".into(),
        ));
    }
    let path: PathBuf = match netlist.options.deck_dir.as_deref() {
        Some(dir) if !Path::new(bare).is_absolute() => dir.join(bare),
        _ => PathBuf::from(bare),
    };
    std::fs::write(&path, text).map_err(|e| {
        SimError::Analysis(format!(".savebias could not write {}: {e}", path.display()))
    })
}

/// The transient snapshot the card names: `time=` without `repeat` saves at
/// the first sample that reaches the instant (LTspice writes when the run
/// crosses it); `repeat` (and no `time=` at all) reduces to the same thing
/// the end-of-run solution is — the file always holds only the most
/// recently solved point. A run that never reached `time=` falls back to
/// its own end-of-run solution the same way.
fn tran_point<'a>(tran: &'a TranResult, cmd: &SaveBiasCommand) -> Option<&'a TranPoint> {
    let at_want = cmd
        .time
        .and_then(|want| tran.points.iter().find(|p| p.time >= want));
    at_want.or_else(|| tran.points.last())
}

/// The `.dc` snapshot: `DC1=..DC3=` name the sweep values in spec order,
/// matched back to front (the most recently solved point of the sweep);
/// unqualified, that last swept point is what's saved.
fn sweep_point<'a>(sweep: &'a DcSweepResult, cmd: &SaveBiasCommand) -> Option<&'a DcSweepPoint> {
    let hit = sweep.points.iter().rev().find(|p| {
        cmd.dc
            .iter()
            .enumerate()
            .all(|(i, want)| match (want, p.values.get(i)) {
                (Some(want), Some(got)) => near_eq(*want, *got),
                (None, _) => true,
                (Some(_), None) => false,
            })
    });
    hit.or_else(|| sweep.points.last())
}

/// Two values close enough to name the same sweep point: swept arithmetic
/// is exact to a rounding, so a relative 1e-9 epsilon decides.
fn near_eq(want: f64, got: f64) -> bool {
    (want - got).abs() <= 1e-9 * want.abs().max(got.abs()).max(1.0)
}

// ─── The mint choreography ───────────────────────────────────────────────────

/// One run's minted facts. The whole per-run choreography — clone vs
/// re-parse, which card to strip, the temperature pin, the seed/draw
/// order — is keyed on the plan and applied in one place, so a driver never
/// re-learns it and a third driver arrives as one new arm here.
enum RunPlan {
    /// One `.step` combination: `values` in `step_targets` order,
    /// `overrides` naming every non-temp target, and the temperature to pin
    /// the deck to (`None` leaves the deck's own standing).
    Step {
        values: Vec<f64>,
        temperature: Option<f64>,
        overrides: Vec<(String, f64)>,
    },
    /// One seeded Monte Carlo run: the run index (the hidden `.mc` override
    /// and the recorded value) and its mixed seed.
    Mc { index: usize, seed: u64 },
}

impl RunPlan {
    /// The value row the `StepRun` records, in `step_targets` order.
    fn values(&self) -> Vec<f64> {
        match self {
            RunPlan::Step { values, .. } => values.clone(),
            RunPlan::Mc { index, .. } => vec![*index as f64],
        }
    }
}

/// Mint one run's inner deck: apply the plan's overrides through a re-parse
/// (or a straight clone when a `.step` row overrides nothing — the source
/// text produces exactly the deck we are holding), then strip the deck's
/// own driver card and pin the plan's per-run facts. The `Mc` arm's
/// statement order is load-bearing (the re-parse's parameter draws first,
/// the runtime seed second, the model-card draws third: see
/// `mc::apply_model_draws`), as is leaving the caller's deck untouched —
/// the next mint re-reads the original cards off it.
fn mint_inner(netlist: &Netlist, plan: &RunPlan) -> Result<Netlist> {
    let mut inner = match plan {
        RunPlan::Step { overrides, .. } => {
            if overrides.is_empty() {
                netlist.clone()
            } else {
                let overrides: HashMap<String, f64> = overrides.iter().cloned().collect();
                crate::netlist::parse_with_overrides(&netlist.source, &netlist.options, &overrides)?
            }
        }
        RunPlan::Mc { index, .. } => {
            let mut overrides: HashMap<String, f64> = HashMap::new();
            overrides.insert(mc::RUN_KEY.to_string(), *index as f64);
            crate::netlist::parse_with_overrides(&netlist.source, &netlist.options, &overrides)?
        }
    };
    match plan {
        // The inner parse sees the same `.step`/`.temp` cards; running them
        // again would nest the sweep inside itself.
        RunPlan::Step { temperature, .. } => {
            inner.analysis.steps.clear();
            inner.analysis.temperatures.clear();
            if let Some(t) = temperature {
                inner.circuit.temperature = *t;
            }
        }
        // The inner parse sees the same `.mc` card; running it again would
        // nest Monte Carlo inside itself. Re-rolled parameter draws ride the
        // re-parse; the runtime keyed draws follow `mc_seed`, and the model
        // cards follow the draws.
        RunPlan::Mc { seed, .. } => {
            inner.analysis.mc = None;
            inner.mc_seed = *seed;
            mc::apply_model_draws(&mut inner.circuit, *seed);
        }
    }
    Ok(inner)
}

/// The protocol, owned once: per plan, mint the inner deck, run it once,
/// land one `StepRun`; then mirror the first run into the top level.
///
/// Plans are independent by construction — each mints its own deck off the
/// caller's untouched one — so the mint/run pairs spread across `threads`
/// workers ([`execute_plans`]; a one-run sweep stays serial). Results land in
/// plan order, so the caller still sees the sequential loop's semantics: the
/// sweep's run row order, and the *first* failing plan in plan order aborting
/// the sweep.
fn run_plans(
    netlist: &Netlist,
    plans: Vec<RunPlan>,
    targets: Vec<String>,
    no_runs: &str,
    threads: usize,
) -> Result<SimulationResult> {
    let runs = execute_plans(netlist, &plans, threads)?;
    let first = runs
        .first()
        .cloned()
        .ok_or_else(|| SimError::Analysis(no_runs.to_string()))?;
    let result = SimulationResult {
        dc: first.dc,
        dc_sweep: first.dc_sweep,
        tran: first.tran,
        ac: first.ac,
        tf: first.tf,
        step_targets: targets,
        runs,
        measurements: first.measurements,
        four: first.four,
        noise: first.noise,
    };
    // The plan-level cleanup of a deck asking to save its solution; runs
    // land after so a `.savebias` error still reports against a finished
    // set of runs — matching LTspice writing at the end of the run.
    write_savebias(netlist, &result)?;
    Ok(result)
}

/// One plan: mint its deck, run it, land the `StepRun` — the per-plan body
/// [`execute_plans`] spreads, kept whole so a worker sees the same
/// choreography the sequential loop did.
fn execute_plan(netlist: &Netlist, plan: &RunPlan) -> Result<StepRun> {
    let inner = mint_inner(netlist, plan)?;
    let single = run_single(&inner)?;
    Ok(StepRun {
        values: plan.values(),
        dc: single.dc,
        dc_sweep: single.dc_sweep,
        tran: single.tran,
        ac: single.ac,
        tf: single.tf,
        measurements: single.measurements,
        four: single.four,
        noise: single.noise,
    })
}

/// Stack for a spawned worker: a run is a whole analysis (parser recursion,
/// expression trees), so give it the headroom a main thread has.
const WORKER_STACK: usize = 16 * 1024 * 1024;

/// Run every plan, in parallel over `threads` workers, returning the runs in
/// plan order.
///
/// Workers claim plan indices from one atomic counter, so indices are handed
/// out in increasing order. When a plan fails, workers stop claiming indices
/// past it; every lower index was claimed before it and still completes, so
/// the error returned is always the *lowest-indexed* failure — the same one a
/// serial loop stops at, whatever the thread count. The calling thread is a
/// worker too: a failed `spawn` degrades to fewer workers, never to an error.
/// A panic in a worker propagates out of the scope like a serial panic would.
fn execute_plans(netlist: &Netlist, plans: &[RunPlan], threads: usize) -> Result<Vec<StepRun>> {
    let workers = threads.max(1).min(plans.len());
    if workers <= 1 {
        return plans.iter().map(|plan| execute_plan(netlist, plan)).collect();
    }

    let next = AtomicUsize::new(0);
    let lowest_failure = AtomicUsize::new(usize::MAX);
    let slots: Vec<Mutex<Option<Result<StepRun>>>> =
        plans.iter().map(|_| Mutex::new(None)).collect();
    // Each plan worker gets its share of the thread budget for the regions
    // inside its run, so `-j N` stays N busy threads in total.
    let inner = (threads.max(1) / workers).max(1);
    let work = || {
        let _budget = crate::parallel::with_budget(inner);
        loop {
        let i = next.fetch_add(1, Ordering::Relaxed);
        if i >= plans.len() || i > lowest_failure.load(Ordering::Relaxed) {
            break;
        }
        let outcome = execute_plan(netlist, &plans[i]);
        if outcome.is_err() {
            lowest_failure.fetch_min(i, Ordering::Relaxed);
        }
        *slots[i].lock().expect("run slot") = Some(outcome);
        }
    };
    std::thread::scope(|scope| {
        for _ in 1..workers {
            let _ = std::thread::Builder::new()
                .stack_size(WORKER_STACK)
                .spawn_scoped(scope, &work);
        }
        work();
    });

    let mut runs = Vec::with_capacity(plans.len());
    for slot in slots {
        match slot.into_inner().expect("run slot") {
            Some(outcome) => runs.push(outcome?),
            None => {
                return Err(SimError::Analysis(
                    "internal error: a run was skipped without a preceding failure".into(),
                ))
            }
        }
    }
    Ok(runs)
}

// ─── Value lists ─────────────────────────────────────────────────────────────

/// Hard ceiling on the number of `.step` combinations (each is a full
/// analysis), so a fat-fingered sweep cannot run for days.
const MAX_STEP_RUNS: usize = 10_000;

/// The cartesian product of every `.step` value list, outer spec first.
fn step_combinations(specs: &[StepSpec]) -> Result<Vec<Vec<f64>>> {
    let mut lists: Vec<Vec<f64>> = Vec::with_capacity(specs.len());
    for spec in specs {
        lists.push(step_values(spec)?);
    }
    let total: usize = lists.iter().map(Vec::len).product();
    if total > MAX_STEP_RUNS {
        return Err(SimError::Analysis(format!(
            ".step would run {total} simulations, over the {MAX_STEP_RUNS}-run ceiling"
        )));
    }
    let mut out: Vec<Vec<f64>> = vec![Vec::with_capacity(specs.len())];
    for list in lists {
        let mut next = Vec::with_capacity(out.len() * list.len());
        for prefix in &out {
            for &v in &list {
                let mut combo = prefix.clone();
                combo.push(v);
                next.push(combo);
            }
        }
        out = next;
    }
    Ok(out)
}

fn step_values(spec: &StepSpec) -> Result<Vec<f64>> {
    let target = &spec.target;
    match &spec.values {
        StepValues::List(v) => Ok(v.clone()),
        StepValues::Linear { start, stop, step } => {
            sweep_values(
                &DcSweepValues::Linear { start: *start, stop: *stop, step: *step },
                target,
            )
        }
        StepValues::Decade { start, stop, points } => {
            let decades = (stop / start).log10();
            let total = (decades * *points as f64).round() as usize + 1;
            Ok((0..total)
                .map(|i| start * 10f64.powf(decades * i as f64 / (total - 1).max(1) as f64))
                .collect())
        }
        StepValues::Octave { start, stop, points } => {
            let octaves = (stop / start).log2();
            let total = (octaves * *points as f64).round() as usize + 1;
            Ok((0..total)
                .map(|i| start * 2f64.powf(octaves * i as f64 / (total - 1).max(1) as f64))
                .collect())
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{default_threads, mint_inner, run, run_plans, run_with_threads, RunPlan};
    use approx::assert_relative_eq;
    use crate::error::SimError;
    use crate::mc;
    use crate::parse;

    /// runs present ⇔ the top-level fields are the first run's — and the
    /// unstepped path stays unmirrored (runs empty, top level filled
    /// directly by `run_single`).
    #[test]
    fn mirror_makes_the_top_level_the_first_run() {
        let netlist = parse(
            "* .step mirror\n\
             .param r=1k\n\
             V1 in 0 DC 1\n\
             R1 in out {r}\n\
             R2 out 0 1k\n\
             .step param r list 1k 2k 3k\n\
             .op\n\
             .end\n",
        )
        .unwrap();
        let result = run(&netlist).unwrap();
        assert_eq!(result.step_targets, vec!["r".to_string()]);
        assert_eq!(result.runs.len(), 3);
        for (i, step) in result.runs.iter().enumerate() {
            assert_eq!(step.values, vec![1000.0 * (i as f64 + 1.0)]);
            assert!(step.dc.is_some());
        }
        // Bit-for-bit: the top level is the first run's clone.
        assert!(result
            .dc
            .as_ref()
            .unwrap()
            .node_voltages
            .iter()
            .zip(&result.runs[0].dc.as_ref().unwrap().node_voltages)
            .all(|(top, first)| top == first));
        assert!(result.noise.is_none());

        let plain = parse(
            "* unstepped: no plans, no mirror\n\
             V1 in 0 DC 1\n\
             R1 in out 1k\n\
             .op\n\
             .end\n",
        )
        .unwrap();
        let result = run(&plain).unwrap();
        assert!(result.runs.is_empty());
        assert!(result.step_targets.is_empty());
        assert!(result.dc.is_some());
    }

    /// The spread (thread workers joined in plan order) preserves the
    /// sequential loop's row order: each run's recorded value row and its
    /// own resolved deck. How many workers run is machine-dependent — the
    /// assertions hold on either the threaded path or the single-chunk one.
    #[test]
    fn spread_preserves_plan_order_and_each_runs_own_deck() {
        let netlist = parse(
            "* spread, then read out\n\
             .param r=1k\n\
             V1 in 0 DC 1\n\
             R1 in out {r}\n\
             R2 out 0 1k\n\
             .step param r list 1k 2k 3k 4k 5k 6k\n\
             .op\n\
             .end\n",
        )
        .unwrap();
        let result = run(&netlist).unwrap();
        assert_eq!(result.runs.len(), 6);
        for (i, step) in result.runs.iter().enumerate() {
            // The value row is the plan's, in plan order.
            assert_eq!(step.values, vec![(i + 1) as f64 * 1000.0]);
            // …and the deck behind the row is that run's: the out node rides
            // its own r (1 / (1 + r/1k) of the 1 V drive).
            let out = step.dc.as_ref().unwrap().node_voltages[2];
            assert_relative_eq!(out, 1.0 / (1.0 + (i + 1) as f64), epsilon = 1e-9);
        }
    }

    /// With workers spread across cores the sweep still aborts on the
    /// *first-in-plan-order* failure — a later run is not the one the
    /// caller hears about, and the error is the sequential loop's byte
    /// for byte.
    #[test]
    fn spread_sweeps_still_abort_on_the_first_failing_run() {
        let netlist = parse(
            "* zero resistance wins the error race\n\
             .param r=1k\n\
             V1 in 0 DC 1\n\
             R1 in out {r}\n\
             R2 out 0 1k\n\
             .step param r list 0 1k 2k\n\
             .op\n\
             .end\n",
        )
        .unwrap();
        let err = run(&netlist).unwrap_err();
        match err {
            SimError::Circuit(message) => {
                assert_eq!(message, "resistor R1 must have R > 0 (card or model)");
            }
            other => panic!("unexpected error shape: {other}"),
        }
    }

    /// The empty-plan error speaks in the deck's own words, byte for byte.
    #[test]
    fn no_runs_errors_are_byte_exact() {
        let netlist = parse("* empty: nothing to run\n.op\n.end\n").unwrap();
        let err = run_plans(&netlist, Vec::new(), vec![], ".step produced no runs", 1).unwrap_err();
        match err {
            SimError::Analysis(message) => assert_eq!(message, ".step produced no runs"),
            other => panic!("unexpected error shape: {other}"),
        }
        let err = run_plans(&netlist, Vec::new(), vec![], ".mc produced no runs", 1).unwrap_err();
        match err {
            SimError::Analysis(message) => assert_eq!(message, ".mc produced no runs"),
            other => panic!("unexpected error shape: {other}"),
        }
    }

    /// A `.step` mint strips exactly the deck's own cards and pins the
    /// temperature without touching the caller's deck.
    #[test]
    fn step_mint_strips_its_own_cards_and_pins_the_temperature() {
        let netlist = parse(
            "* stepped, then minted\n\
             V1 in 0 DC 1\n\
             R1 in out 1k\n\
             .step temp list 47 57\n\
             .op\n\
             .end\n",
        )
        .unwrap();
        let plan = RunPlan::Step {
            values: vec![47.0],
            temperature: Some(47.0),
            overrides: Vec::new(),
        };
        let inner = mint_inner(&netlist, &plan).unwrap();
        assert!(inner.analysis.steps.is_empty());
        assert!(inner.analysis.temperatures.is_empty());
        assert_eq!(inner.circuit.temperature, 47.0);
        assert!(!netlist.analysis.steps.is_empty());
        // `None` leaves the deck's own temperature standing.
        let plan = RunPlan::Step {
            values: vec![0.0],
            temperature: None,
            overrides: Vec::new(),
        };
        let inner = mint_inner(&netlist, &plan).unwrap();
        assert_eq!(inner.circuit.temperature, netlist.circuit.temperature);
    }

    /// A `.mc` mint inerts the card and carries the mixed seed; the caller's
    /// deck still owns the card it was minted from.
    #[test]
    fn mc_mint_inerts_the_mc_card_and_carries_the_seed() {
        let netlist = parse(
            "* minted montes\n\
             V1 in 0 DC 1\n\
             R1 in out 1k\n\
             .mc 25 seed=7\n\
             .op\n\
             .end\n",
        )
        .unwrap();
        let plan = RunPlan::Mc {
            index: 3,
            seed: mc::run_mix(7, 3),
        };
        let inner = mint_inner(&netlist, &plan).unwrap();
        assert!(inner.analysis.mc.is_none());
        assert_eq!(inner.mc_seed, mc::run_mix(7, 3));
        assert!(netlist.analysis.mc.is_some());
    }

    /// The worker count only changes wall time: a stepped transient deck and a
    /// Monte Carlo deck produce bit-identical results serial and parallel,
    /// down to every sample of every run.
    #[test]
    fn parallel_runs_are_bit_identical_to_serial() {
        let stepped = parse(
            "* stepped RC step\n\
             .param r=1k\n\
             V1 in 0 PULSE(0 5 0 1n 1n 1 2)\n\
             R1 in out {r}\n\
             C1 out 0 1u\n\
             .step param r list 100 220 470 1k 2.2k 4.7k 10k 22k\n\
             .tran 10u 2m\n\
             .meas tran vmax MAX V(out)\n\
             .end\n",
        )
        .unwrap();
        let serial = format!("{:?}", run_with_threads(&stepped, 1).unwrap());
        for threads in [2, 3, 8, 64] {
            let parallel = format!("{:?}", run_with_threads(&stepped, threads).unwrap());
            assert_eq!(serial, parallel, "{threads} threads changed a stepped result");
        }

        let monte = parse(
            "* monte carlo divider\n\
             .model rm R(R=1k DEV=5%)\n\
             V1 in 0 DC 1\n\
             R1 in out rm 1k\n\
             R2 out 0 rm 1k\n\
             .mc 24 seed=5\n\
             .op\n\
             .end\n",
        )
        .unwrap();
        let serial = format!("{:?}", run_with_threads(&monte, 1).unwrap());
        let parallel = format!("{:?}", run_with_threads(&monte, 6).unwrap());
        assert_eq!(serial, parallel, "threads changed a Monte Carlo result");
    }

    /// A failing run surfaces the same error a serial loop stops at: the
    /// lowest-indexed one, however many workers race past it. The two failing
    /// plans name different resistors, so a "first to finish" driver would
    /// show.
    #[test]
    fn parallel_failure_is_the_lowest_indexed_one() {
        // Plan order (outer r, inner s): index 2 = (1k, 0) breaks R2; index 4 =
        // (0, 1k) breaks R1, as do 5 and 6.
        let deck = "* failing sweep\n\
                    .param r=1k s=1k\n\
                    V1 in 0 DC 1\n\
                    R1 in out {r}\n\
                    R2 out 0 {s}\n\
                    .step param r list 1k 0\n\
                    .step param s list 1k 1k 0 1k\n\
                    .op\n\
                    .end\n";
        let netlist = parse(deck).expect("the deck parses; the failure is per run");
        let serial = run_with_threads(&netlist, 1).map(|_| ()).unwrap_err().to_string();
        assert!(serial.contains("R2"), "serial stops at plan 2: {serial}");
        for threads in [2, 3, 4, 16] {
            for _ in 0..20 {
                let parallel = run_with_threads(&netlist, threads)
                    .map(|_| ())
                    .unwrap_err()
                    .to_string();
                assert_eq!(serial, parallel, "{threads} threads");
            }
        }
    }

    #[test]
    fn default_threads_is_at_least_one() {
        assert!(default_threads() >= 1);
        // `run` (default workers) agrees with the pinned-serial run.
        let netlist = parse(
            "* tiny sweep\n.param r=1k\nV1 in 0 DC 1\nR1 in out {r}\nR2 out 0 1k\n\
             .step param r list 1k 2k\n.op\n.end\n",
        )
        .unwrap();
        assert_eq!(
            format!("{:?}", run(&netlist).unwrap()),
            format!("{:?}", run_with_threads(&netlist, 1).unwrap())
        );
    }
}
