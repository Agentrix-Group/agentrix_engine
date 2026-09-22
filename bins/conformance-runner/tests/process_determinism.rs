use std::process::Command;

/// Corre el runner `repetitions` veces en procesos limpios, exige que todas
/// las salidas sean idénticas byte a byte (D2) y que coincidan con el vector
/// dorado versionado (D1).
fn assert_clean_process_repetitions(
    vector: &str,
    expected_bytes: &[u8],
    repetitions: usize,
) {
    let binary = env!("CARGO_BIN_EXE_agentrix-conformance-vector");
    let run = || {
        let output = Command::new(binary)
            .arg(vector)
            .output()
            .expect("conformance process should start");
        assert!(output.status.success(), "{vector} process failed");
        assert!(
            !output.stdout.is_empty(),
            "process must emit canonical evidence"
        );
        output.stdout
    };
    let first = run();
    let actual: serde_json::Value =
        serde_json::from_slice(&first).expect("runner output must be JSON");
    let expected: serde_json::Value =
        serde_json::from_slice(expected_bytes).expect("fixture must be JSON");
    assert_eq!(actual, expected, "conformance vector {vector} drifted");
    for repetition in 1..repetitions {
        assert_eq!(
            run(),
            first,
            "D2 divergence in {vector} process repetition {repetition}"
        );
    }
}

#[test]
fn conformance_game_is_bit_identical_across_100_clean_processes() {
    assert_clean_process_repetitions(
        "conformance",
        include_bytes!("../../../conformance/vectors/conformance-counter-1.json"),
        100,
    );
}

/// `starfighter-0.3.0-core.1.json` se conserva como evidencia histórica del
/// juego sobre Avian2D; ningún binario actual lo reproduce (ADR-0013).
#[test]
fn starfighter_is_bit_identical_across_100_clean_processes() {
    assert_clean_process_repetitions(
        "starfighter",
        include_bytes!("../../../conformance/vectors/starfighter-0.4.0.json"),
        100,
    );
}

/// Criterio de F3/F5 (ADR-0013): una partida completa 1 contra 1, hasta
/// 3600 ticks, reproduce el vector congelado en procesos limpios.
#[test]
fn starfighter_full_1v1_match_matches_frozen_vector() {
    assert_clean_process_repetitions(
        "starfighter-long",
        include_bytes!(
            "../../../conformance/vectors/starfighter-0.4.0-long-2p.json"
        ),
        5,
    );
}

/// Criterio de F5 (ADR-0013): una partida completa de 5 naves todos contra
/// todos reproduce el vector congelado en procesos limpios.
#[test]
fn starfighter_full_ffa_match_matches_frozen_vector() {
    assert_clean_process_repetitions(
        "starfighter-ffa-long",
        include_bytes!(
            "../../../conformance/vectors/starfighter-0.4.0-ffa-long-5p.json"
        ),
        5,
    );
}
