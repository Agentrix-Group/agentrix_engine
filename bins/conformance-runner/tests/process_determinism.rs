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
    // `starfighter-0.3.0-core.1.json` fue producido con Avian2D. Desde F3
    // (ADR-0013) Starfighter corre sobre Rapier y ese vector ya no aplica;
    // el vector de `starfighter 0.4.0` se congela en F5. Hasta entonces
    // Starfighter solo se verifica por identidad entre procesos limpios.
    let expected_bytes: Option<&[u8]> = match vector {
        "conformance" => Some(include_bytes!(
            "../../../conformance/vectors/conformance-counter-1.json"
        )),
        "starfighter" => None,
        _ => panic!("unknown vector fixture"),
    };
    let actual: serde_json::Value = serde_json::from_slice(&first.stdout)
        .expect("runner output must be JSON");
    if let Some(expected_bytes) = expected_bytes {
        let expected: serde_json::Value = serde_json::from_slice(expected_bytes)
            .expect("fixture must be JSON");
        assert_eq!(actual, expected, "conformance vector drifted");
    }
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

/// Criterio de F3 (ADR-0013): una partida completa de hasta 3600 ticks
/// produce la misma cadena de commitments en procesos limpios distintos.
#[test]
fn starfighter_full_match_is_bit_identical_across_clean_processes() {
    let binary = env!("CARGO_BIN_EXE_agentrix-conformance-vector");
    let run = || {
        let output = Command::new(binary)
            .arg("starfighter-long")
            .output()
            .expect("conformance process should start");
        assert!(output.status.success(), "starfighter-long failed");
        output.stdout
    };
    let first = run();
    let summary: serde_json::Value =
        serde_json::from_slice(&first).expect("runner output must be JSON");
    assert!(
        summary["ticks"].as_u64().unwrap_or(0) > 0,
        "the match must advance: {summary}"
    );
    for repetition in 1..5 {
        assert_eq!(
            run(),
            first,
            "D2 divergence in full Starfighter match, repetition {repetition}"
        );
    }
}
