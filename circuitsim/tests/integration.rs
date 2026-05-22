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
