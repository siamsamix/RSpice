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
        assert_eq!(netlist.analysis.config.gmin, 1e-10);
        assert_eq!(netlist.analysis.config.gminsteps, 12);
        assert_eq!(netlist.analysis.config.srcsteps, 3);
        assert_eq!(netlist.analysis.config.itl1, 200);
        assert_eq!(netlist.analysis.config.itl4, 5);

        let text = emit(&netlist);
        for knob in ["gmin=", "gminsteps=", "srcsteps=", "itl1=", "itl4="] {
            assert!(text.contains(knob), "emit lost {knob}: {text}");
        }
        let reparsed = parse(&text).unwrap();
        assert_eq!(reparsed.analysis.config.gmin, netlist.analysis.config.gmin);
        assert_eq!(
            reparsed.analysis.config.gminsteps,
            netlist.analysis.config.gminsteps
        );
        assert_eq!(
            reparsed.analysis.config.srcsteps,
            netlist.analysis.config.srcsteps
        );
        assert_eq!(reparsed.analysis.config.itl1, netlist.analysis.config.itl1);
        assert_eq!(reparsed.analysis.config.itl4, netlist.analysis.config.itl4);

        // Default knobs stay silent in the emitted options line.
        let plain = parse("* plain deck\n.op\n.end\n").unwrap();
        let text = emit(&plain);
        for knob in ["gmin=", "gminsteps=", "srcsteps=", "itl1=", "itl4="] {
            assert!(!text.contains(knob), "default deck emitted {knob}: {text}");
        }
    }

    /// TODO §8: `.options seed=` raises the deck's base draw seed. It must
    /// reach the parameter environment *before* element lines expand (they
    /// draw at expansion), so a pre-scan lifts it — deck position does not
    /// matter — and `emit(parse)` round-trips it like the convergence knobs.
    #[test]
    fn options_seed_rolls_the_draws() {
        let deck = |seed: &str| {
            format!(
                "* seeded deck\n\
                 .op\n\
                 V1 1 0 {{rand(1)}}\n\
                 R1 1 0 1k\n\
                 .options seed={seed}\n\
                 .end\n"
            )
        };
        let seeded = parse(&deck("42")).unwrap();
        assert_eq!(seeded.analysis.config.mc_seed, 42, "the walk records the value");
        assert_ne!(seeded.mc_seed, 0, "the env seed left zero");
        let draw = seeded.circuit.voltage_sources[0].voltage;
        assert!((-1.0..=1.0).contains(&draw), "rand(1) inside ±1: {draw}");

        // Deck order does not matter: the seed card at the top is the same
        // env seed as at the bottom.
        let deck_top = "* deck\n.options seed=42\nV1 1 0 {rand(1)}\nR1 1 0 1k\n.end\n";
        let top = parse(deck_top).unwrap();
        assert_eq!(top.mc_seed, seeded.mc_seed);
        assert_eq!(top.circuit.voltage_sources[0].voltage, draw);

        // A different seed re-rolls the draw (statistically certain to
        // differ in bits; the hash is an avalanche).
        let other = parse(&deck("43")).unwrap();
        assert_ne!(
            other.circuit.voltage_sources[0].voltage.to_bits(),
            draw.to_bits()
        );

        // Round-trip: emit → re-parse keeps the seed and the draws.
        let text = emit(&seeded);
        assert!(text.contains("seed=42"), "emit lost the seed: {text}");
        let reparsed = parse(&text).unwrap();
        assert_eq!(reparsed.mc_seed, seeded.mc_seed);
        assert_eq!(reparsed.circuit.voltage_sources[0].voltage, draw);

        // A parameterized seed cannot reach the expansion-time draws — warn,
        // keep the param expansions honest, don't half-apply.
        let param_deck = "*. deck\n.param s = 42\n.op\nV1 1 0 {{rand(1)}}\n.options seed={{{s}}}\n.end\n";
        let parameterized = parse(param_deck).unwrap();
        assert!(parameterized
            .warnings
            .iter()
            .any(|w| w.contains("seed") && w.contains("parameterized")),
            "warnings: {:?}", parameterized.warnings);
    }

    /// The `.option` spelling (the reference tools accept both) must reach
    /// the same pre-scan. It used to fall through, firing a false
    /// "parameterized" warning and leaving the draw seed at zero.
    #[test]
    fn option_singular_spelling_seeds_the_draws_too() {
        let plural = parse(
            "* deck\n.op\nV1 1 0 {rand(1)}\nR1 1 0 1k\n.options seed=42\n.end\n",
        )
        .unwrap();
        let singular = parse(
            "* deck\n.op\nV1 1 0 {rand(1)}\nR1 1 0 1k\n.option seed=42\n.end\n",
        )
        .unwrap();
        assert_eq!(singular.analysis.config.mc_seed, 42, "the walk records the value");
        assert_eq!(singular.mc_seed, plural.mc_seed, "same env seed as the plural form");
        assert_eq!(
            singular.circuit.voltage_sources[0].voltage.to_bits(),
            plural.circuit.voltage_sources[0].voltage.to_bits(),
            "the draws match the plural spelling"
        );
        assert!(
            singular.warnings.is_empty(),
            "no false parameterized warning: {:?}",
            singular.warnings
        );
    }

    /// `.meas` cards round-trip: the emitter used to skip them entirely, so
    /// `parse(emit(parse))` silently dropped every measurement. Values are
    /// plain decimals so no parse path can drift an ulp between passes.
    #[test]
    fn meas_cards_round_trip() {
        let src = "*. meas deck\n\
                   V1 in 0 DC 0\n\
                   R1 in out 1k\n\
                   .tran 1u 1m\n\
                   .meas tran vavg AVG V(out) FROM=0.001 TO=0.009\n\
                   .meas tran tset FIND V(out) WHEN V(out)=0.99 RISE=1 FROM=0 TO=0.001\n\
                   .meas tran tcross CROSS V(out) VAL=1.5 FALL\n\
                   .meas tran tdelay TRIG V(in) VAL=0.5 RISE=2 TARG V(out) VAL=0.5 FALL=3 TO=0.001\n\
                   .end\n";
        let first = parse(src).expect("first parse");
        assert_eq!(first.analysis.measurements.len(), 4);
        let text = emit(&first);
        assert!(text.contains(".meas"), "emit dropped the measurements: {text}");
        let second = parse(&text).expect("re-parse of emitted text");
        assert_eq!(second.analysis.measurements.len(), 4);
        for (a, b) in first.analysis.measurements.iter().zip(&second.analysis.measurements) {
            assert_eq!(a.analysis, b.analysis, "analysis drift on {}", a.name);
            assert_eq!(a.name, b.name);
            assert_eq!(format!("{:?}", a.kind), format!("{:?}", b.kind), "kind drift on {}", a.name);
        }
        // Emitting the re-parsed deck again is a fixed point: the canonical
        // text is stable.
        assert_eq!(emit(&second), text);
    }

    /// TODO §4: `.op pon` / `.op all` record the operating-point report
    /// style. The variant survives `parse(emit(parse))`, and a bare `.op`
    /// stays bare in the emitted deck.
    #[test]
    fn op_modes_parse_and_round_trip() {
        let plain = parse("* deck\n.op\n.end\n").unwrap();
        assert_eq!(plain.analysis.op_mode, OpMode::Default);
        assert!(emit(&plain).lines().any(|l| l.trim() == ".op"), "bare .op: {}", emit(&plain));

        for (token, mode) in [("pon", OpMode::Pon), ("all", OpMode::All)] {
            let netlist = parse(&format!("* deck\n.OP {token}\n.end\n")).unwrap();
            assert_eq!(netlist.analysis.op_mode, mode, "token {token}");
            let text = emit(&netlist);
            assert!(
                text.lines().any(|l| l.trim() == format!(".op {token}")),
                "emit lost .op {token}: {text}"
            );
            let reparsed = parse(&text).unwrap();
            assert_eq!(reparsed.analysis.op_mode, mode);
        }

        // Unknown spellings warn and fall back to the default report.
        let unknown = parse("* deck\n.op sometimes\n.end\n").unwrap();
        assert_eq!(unknown.analysis.op_mode, OpMode::Default);
        assert!(
            unknown.warnings.iter().any(|w| w.contains("sometimes")),
            "warnings: {:?}",
            unknown.warnings
        );
    }

    /// TODO §5: the ABM forms (VALUE/EQUATION alias, TABLE, LAPLACE, FREQ,
    /// CHEBYSHEV across the E/G/F/H output letters) parse into behavioral
    /// entries and survive `parse(emit(parse))` — the emitter re-spells the
    /// canonical card shape each form was parsed from (documented-
    /// interpretation: dynamic forms respond in AC and stamp H(0) elsewhere;
    /// gains plain/phases degrees unless `MEASURE=` says dB/radians).
    #[test]
    fn abm_parse_and_round_trip() {
        let deck = "* abm forms\n\
                    .param gain=4\n\
                    Vin in 0 DC 0\n\
                    Vs b 0 DC 0\n\
                    E1 eout 0 VALUE={V(in)*gain}\n\
                    E2 qout 0 EQUATION {V(in)*2}\n\
                    E3 tout 0 TABLE {V(in)} = (2, 5) (0, 1) (-2, -1)\n\
                    G1 gout 0 LAPLACE {V(in)/4} = {1, 2} {1, 0.5}\n\
                    G2 fout 0 FREQ {V(in)} = <100, 5> <1, 0.5> MEASURE=DB\n\
                    F1 cout 0 CHEBYSHEV {I(vs)} 0.5 0.25\n\
                    H1 hout 0 VALUE = {I(vs)*1k}\n\
                    .end\n";
        let first = parse(deck).unwrap();

        // Exactly three warnings: one dynamic-form note per LAPLACE/FREQ/
        // CHEBYSHEV card. VALUE/TABLE/EQUATION are instant transfers.
        let dynamic = "respond in AC";
        assert_eq!(first.warnings.len(), 3, "{:?}", first.warnings);
        assert!(first.warnings.iter().all(|w| w.contains(dynamic)));

        let b = |name: &str| {
            first
                .circuit
                .behavioral
                .iter()
                .find(|b| b.name.eq_ignore_ascii_case(name))
        };
        use crate::circuit::{AbmForm, BehavioralKind};

        // E/H are voltage flavours, G/F current flavours (PSpice grammar).
        assert_eq!(b("E1").unwrap().kind, BehavioralKind::Voltage);
        assert_eq!(b("H1").unwrap().kind, BehavioralKind::Voltage);
        assert_eq!(b("G1").unwrap().kind, BehavioralKind::Current);
        assert_eq!(b("F1").unwrap().kind, BehavioralKind::Current);

        // VALUE: text rides through verbatim; the parameter folds into the
        // AST, so only the V(in) probe remains.
        let e1 = b("E1").unwrap();
        assert!(matches!(e1.abm, Some(AbmForm::Value)));
        assert_eq!(e1.expr, "V(in)*gain");
        assert_eq!(e1.behavior.probes.len(), 1);

        // EQUATION is the alias of VALUE (same entry kind).
        assert!(matches!(b("E2").unwrap().abm, Some(AbmForm::Value)));

        // TABLE builds the AST table node: pairs travel sorted ascending
        // (the deck lists them descending), zero output outside bounds.
        let e3 = b("E3").unwrap();
        assert!(matches!(e3.abm, Some(AbmForm::Table)));
        match &e3.behavior.ast {
            crate::behavior::BExpr::Table { points, .. } => {
                assert_eq!(points.as_slice(), &[(-2.0, -1.0), (0.0, 1.0), (2.0, 5.0)]);
            }
            other => panic!("E3 AST: {other:?}"),
        }

        // LAPLACE lists travel descending (first = highest power of s), so
        // `{1, 2}` is `s + 2` over `s + 0.5`; H(0) = 2/0.5 = 4.
        let g1 = b("G1").unwrap();
        match &g1.abm {
            Some(AbmForm::Laplace { num, den }) => {
                assert_eq!(num.as_slice(), &[1.0, 2.0]);
                assert_eq!(den.as_slice(), &[1.0, 0.5]);
                assert!((num.last().unwrap() / den.last().unwrap() - 4.0).abs() < 1e-12);
            }
            other => panic!("G1 form: {other:?}"),
        }

        // FREQ rows travel ascending with their MEASURE unit recorded.
        let g2 = b("G2").unwrap();
        match &g2.abm {
            Some(AbmForm::Freq { rows, measure }) => {
                assert_eq!(
                    rows.as_slice(),
                    &[(1.0, 0.5, 0.0), (100.0, 5.0, 0.0)]
                );
                assert_eq!(*measure, Some(crate::circuit::AbmMeasure::Db));
            }
            other => panic!("G2 form: {other:?}"),
        }

        // CHEBYSHEV keeps the second-order coefficient first.
        match &b("F1").unwrap().abm {
            Some(AbmForm::Chebyshev { coeffs }) => {
                assert_eq!(coeffs.as_slice(), &[0.5, 0.25]);
            }
            other => panic!("F1 form: {other:?}"),
        }

        // Round trip: emitted text re-spells each exact form, sorted both
        // ways, with the EQUATION alias canonicalized to VALUE=.
        let text = emit(&first);
        for fragment in [
            "E1 eout 0 VALUE={V(in)*gain}",
            "E2 qout 0 VALUE={V(in)*2}",
            "E3 tout 0 TABLE {V(in)} = (-2, -1) (0, 1) (2, 5)",
            "G1 gout 0 LAPLACE {V(in)/4} = {1, 2} {1, 0.5}",
            "G2 fout 0 FREQ {V(in)} = <1, 0.5> <100, 5> MEASURE=DB",
            "F1 cout 0 CHEBYSHEV {I(vs)} 0.5 0.25",
            "H1 hout 0 VALUE={I(vs)*1k}",
        ] {
            assert!(
                text.lines().any(|l| l.trim() == fragment),
                "emit missed '{fragment}':\n{text}"
            );
        }
        let second = parse(&text).unwrap();
        assert_eq!(second.circuit.behavioral.len(), first.circuit.behavioral.len());
        assert_eq!(second.warnings.len(), 3, "{:?}", second.warnings);
        let dbg = format!("{:?}", first.circuit.behavioral);
        assert_eq!(
            format!("{:?}", second.circuit.behavioral),
            dbg,
            "re-parse changed the behavioral entries"
        );
    }

    /// TODO §5: the ideal op-amp `U` (documented interpretation — the
    /// reference tools reserve `U` for nothing) parses from both spellings —
    /// a bare `U` (default GAIN 1e6, RIN ∞, ROUT 0, VOS 0) and a
    /// `.model <name> U(…)` reference — and survives `parse(emit(parse))`
    /// byte-stable on the circuit.
    #[test]
    fn opamp_parse_and_round_trip() {
        let deck = "* op-amp cards\n\
                    .model u1 U(GAIN=1e5 RIN=100k ROUT=50 VOS=1m)\n\
                    U1 o1 0 b 0\n\
                    U2 o2 0 a 0 u1\n\
                    .end\n";
        let first = parse(deck).unwrap();
        assert!(first.warnings.is_empty(), "{:?}", first.warnings);

        assert_eq!(first.circuit.opamps.len(), 2);
        let bare = &first.circuit.opamps[0];
        assert_eq!(bare.name, "U1");
        assert!(bare.model.is_empty());
        let fit = &first.circuit.opamps[1];
        assert_eq!(fit.model, "u1");

        // The card parameters land in the model table; the bare `U`
        // resolves to the documented defaults.
        let m = first.circuit.opamp_params_at("u1");
        assert_eq!(m.gain, 1e5);
        assert_eq!(m.rin, 1e5);
        assert_eq!(m.rout, 50.0);
        assert_eq!(m.vos, 1e-3);
        let defaults = first.circuit.opamp_params_at("");
        assert_eq!(defaults.gain, 1e6);
        assert_eq!(defaults.rin, f64::INFINITY);
        assert_eq!(defaults.rout, 0.0);
        assert_eq!(defaults.vos, 0.0);

        // Round trip: the emitted deck re-spells both cards, and a second
        // parse reproduces the first circuit byte-for-byte.
        let text = emit(&first);
        for fragment in [
            ".model u1 U(GAIN=100000 RIN=100000 ROUT=50 VOS=0.001)",
            "U1 o1 0 b 0",
            "U2 o2 0 a 0 u1",
        ] {
            assert!(
                text.lines().any(|l| l.trim() == fragment),
                "emit missed '{fragment}':\n{text}"
            );
        }
        let second = parse(&text).unwrap();
        assert!(second.warnings.is_empty(), "{:?}", second.warnings);
        assert_eq!(
            format!("{:?}", second.circuit),
            format!("{:?}", first.circuit),
            "re-parse changed the circuit"
        );
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

    /// Tokens after a waveform are carried as warnings, not dropped — every
    /// waveform arm used to return early and swallow the card's tail.
    #[test]
    fn source_trailing_tokens_warn() {
        let n = parse(
            "V1 in 0 PULSE(0 5 0 1n 1n 1u 2u) extra\n\
             R1 in 0 1k\n\
             .op\n\
             .end\n",
        )
        .unwrap();
        assert!(
            n.warnings.iter().any(|w| w.contains("source") && w.contains("extra")),
            "warnings: {:?}",
            n.warnings
        );
        // The pulse itself still parsed, and the extra token did not shift it.
        let Some(Waveform::Pulse(pulse)) = n.circuit.voltage_sources[0].waveform.as_ref() else {
            panic!("expected a PULSE waveform");
        };
        assert_eq!(pulse.v2, 5.0);
        assert_eq!(pulse.per, crate::units::parse_value("2u").unwrap());
    }

    /// A non-ASCII tail must surface as a parse error, not panic on a
    /// mid-character byte slice (`&text[..4]` used to do exactly that).
    #[test]
    fn pwl_non_ascii_tail_is_an_error_not_a_panic() {
        let result = parse("V1 in 0 pwlfilÿx\nR1 in 0 1k\n.op\n.end\n");
        assert!(result.is_err(), "a broken PWL card must be a loud error");
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

#[test]
fn mc_card_round_trips_and_bins_without_hist_warns() {
    let netlist =
        parse(".mc 40 seed=7 hist=vout bins=8 yield=vout<2.5\n.tran 10u 1m\n.end\n").unwrap();
    let mc = netlist.analysis.mc.as_ref().unwrap();
    assert_eq!(mc.runs, 40);
    assert_eq!(mc.seed, Some(7));
    assert_eq!(mc.hist.as_deref(), Some("vout"));
    assert_eq!(mc.bins, 8);
    assert_eq!(mc.criterion, Some(("vout".into(), McOp::Below, 2.5)));

    // The card survives the canonical emission, clauses in card order.
    let text = emit(&netlist);
    assert!(text.contains(".mc 40 seed=7 hist=vout bins=8 yield=vout<2.5"));

    // The other inequality spelling and a clause-free card.
    let hi = parse(".mc 10 yield=volt>4.0\n.end\n").unwrap();
    let mc = hi.analysis.mc.as_ref().unwrap();
    assert_eq!(mc.runs, 10);
    assert_eq!(mc.seed, None);
    assert_eq!(mc.criterion, Some(("volt".into(), McOp::Above, 4.0)));
    assert!(emit(&hi).contains(".mc 10 yield=volt>4"));

    // `bins=` without `hist=` warns and leaves the default bin count.
    let stray = parse(".mc 5 bins=10\n.end\n").unwrap();
    let mc = stray.analysis.mc.as_ref().unwrap();
    assert_eq!(mc.runs, 5);
    assert!(mc.hist.is_none());
    assert_eq!(mc.bins, MC_DEFAULT_BINS);
    assert_eq!(mc.criterion, None);
    assert!(
        stray.warnings.iter().any(|w| w.contains("bins=10")),
        "{:?}",
        stray.warnings
    );
}
