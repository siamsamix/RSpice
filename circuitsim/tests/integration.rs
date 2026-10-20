use approx::assert_relative_eq;
use circuitsim::simulate;

#[test]
fn voltage_divider_dc() {
    let netlist = r#"
* Voltage divider
V1 1 0 DC 10
R1 1 2 1k
R2 2 0 1k
.op
.end
"#;
    let result = simulate(netlist).unwrap();
    let dc = result.dc.unwrap();
    assert_relative_eq!(dc.node_voltages[1], 10.0, epsilon = 1e-6);
    assert_relative_eq!(dc.node_voltages[2], 5.0, epsilon = 1e-6);
}

#[test]
fn rc_charging_transient() {
    let netlist = r#"
V1 1 0 DC 5
R1 1 2 1k
C1 2 0 1u
.tran 10u 5m
.end
"#;
    let result = simulate(netlist).unwrap();
    let tran = result.tran.unwrap();
    let final_v = tran.points.last().unwrap().node_voltages[2];
    // RC time constant 1ms; after 5ms capacitor is essentially fully charged.
    assert_relative_eq!(final_v, 5.0, epsilon = 0.05);
}

#[test]
fn rl_circuit_dc() {
    let netlist = r#"
V1 1 0 DC 12
R1 1 2 100
L1 2 0 1m
.op
.end
"#;
    let result = simulate(netlist).unwrap();
    let dc = result.dc.unwrap();
    // At DC the inductor is a short, so node 2 is at ground.
    assert_relative_eq!(dc.node_voltages[2], 0.0, epsilon = 1e-6);
    assert_relative_eq!(dc.source_currents[0].abs(), 12.0 / 100.0, epsilon = 1e-4);
}

#[test]
fn rc_lowpass_ac_response() {
    // Guards the AC fold into MnaSystem: small-signal stamps, the phasor
    // solve, and the complex branch extraction.
    let f0 = 1.0 / (2.0 * std::f64::consts::PI * 1_000.0 * 1_000.0e-6);
    let netlist = r#"
V1 1 0 DC 5 AC 1
R1 1 2 1k
C1 2 0 1u
.op
.ac LIN 2 1 159.1549431
.end
"#;
    let result = simulate(netlist).unwrap();
    let ac = result.ac.expect("AC result");

    // Last point sits at the pole: |H(f0)| = 1/√2, phase −45°.
    let last = ac.points.last().unwrap();
    let h = last.node_voltages[2] / last.node_voltages[1];
    assert_relative_eq!(h.norm(), std::f64::consts::FRAC_1_SQRT_2, epsilon = 1e-3);
    assert_relative_eq!(h.arg().to_degrees(), -45.0, epsilon = 0.5);

    // First point (1 Hz << f0) passes nearly everything.
    let first = ac.points.first().unwrap();
    let h0 = first.node_voltages[2] / first.node_voltages[1];
    assert_relative_eq!(h0.norm(), 0.99935, epsilon = 1e-3);
}
