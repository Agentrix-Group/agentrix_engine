use std::process::Command;

fn assert_clean_process_repetitions(vector: &str) {
    let binary = env!("CARGO_BIN_EXE_agentrix-conformance-vector");
    let first = Command::new(binary)
        .arg(vector)
        .output()
        .expect("conformance process should start");
    assert!(first.status.success(), "first process failed");
    assert!(
        !first.stdout.is_empty(),
        "process must emit canonical evidence"
    );
    let expected_bytes: &[u8] = match vector {
        "conformance" => include_bytes!(
            "../../../conformance/vectors/conformance-counter-1.json"
        ),
        "starfighter" => include_bytes!(
            "../../../conformance/vectors/starfighter-0.3.0-core.1.json"
        ),
        _ => panic!("unknown vector fixture"),
    };
    let actual: serde_json::Value = serde_json::from_slice(&first.stdout)
        .expect("runner output must be JSON");
    let expected: serde_json::Value =
        serde_json::from_slice(expected_bytes).expect("fixture must be JSON");
    assert_eq!(actual, expected, "conformance vector drifted");
    for repetition in 1..100 {
        let next = Command::new(binary)
            .arg(vector)
            .output()
            .expect("conformance process should start");
        assert!(
            next.status.success(),
            "process repetition {repetition} failed"
        );
        assert_eq!(
            next.stdout, first.stdout,
            "D2 divergence in {vector} process repetition {repetition}"
        );
    }
}

#[test]
fn conformance_game_is_bit_identical_across_100_clean_processes() {
    assert_clean_process_repetitions("conformance");
}

#[test]
fn starfighter_is_bit_identical_across_100_clean_processes() {
    assert_clean_process_repetitions("starfighter");
}
