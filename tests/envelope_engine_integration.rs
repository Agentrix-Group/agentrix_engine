//! Real acceptance tests for the `agentrix-engine/2` stdio protocol:
//! launches `starfighter-engine` as a real subprocess and communicates
//! strictly using `agentrix-engine/2` line-delimited envelopes.

use serde_json::{json, Value};
use std::io::{BufRead, BufReader, Write};
use std::process::{Child, ChildStdin, Command, Stdio};

struct EngineHandle {
    child: Child,
    stdin: ChildStdin,
    stdout: BufReader<std::process::ChildStdout>,
    send_seq: u64,
    expected_recv_seq: u64,
}

impl EngineHandle {
    fn spawn() -> Self {
        let bin = env!("CARGO_BIN_EXE_starfighter-engine");
        let mut child = Command::new(bin)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::inherit())
            .spawn()
            .expect("no se pudo lanzar starfighter-engine");
        let stdin = child.stdin.take().unwrap();
        let stdout = BufReader::new(child.stdout.take().unwrap());
        EngineHandle {
            child,
            stdin,
            stdout,
            send_seq: 1,
            expected_recv_seq: 1,
        }
    }

    /// Reads and validates the next line:
    /// `protocolVersion` exact and `sequence` strictly monotonically increasing from 1.
    fn read_envelope(&mut self) -> Value {
        let mut line = String::new();
        self.stdout
            .read_line(&mut line)
            .expect("el motor debería mandar una línea");
        let env: Value =
            serde_json::from_str(&line).expect("línea debe ser JSON válido");
        assert_eq!(
            env["protocolVersion"], "agentrix-engine/2",
            "protocolVersion debe ser exactamente agentrix-engine/2"
        );
        assert_eq!(
            env["sequence"], self.expected_recv_seq,
            "sequence de salida del motor debe incrementar de a 1 empezando en 1"
        );
        self.expected_recv_seq += 1;
        env
    }

    fn write_envelope(
        &mut self,
        match_id: &str,
        msg_type: &str,
        payload: Value,
    ) {
        let env = json!({
            "protocolVersion": "agentrix-engine/2",
            "type": msg_type,
            "matchId": match_id,
            "sequence": self.send_seq,
            "payload": payload,
        });
        self.send_seq += 1;
        writeln!(self.stdin, "{}", env).expect("escribir a stdin del motor");
    }

    fn write_raw_envelope(
        &mut self,
        version: &str,
        match_id: &str,
        msg_type: &str,
        seq: u64,
        payload: Value,
    ) {
        let env = json!({
            "protocolVersion": version,
            "type": msg_type,
            "matchId": match_id,
            "sequence": seq,
            "payload": payload,
        });
        writeln!(self.stdin, "{}", env).expect("escribir a stdin del motor");
    }
}

impl Drop for EngineHandle {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

fn sample_spec(match_id: &str, p0: &str, p1: &str, max_ticks: u64) -> Value {
    let config =
        serde_json::to_value(bevy_starfighter::StarfighterConfig::default())
            .unwrap();
    let config_digest =
        agentrix_sim_core::canonical_json_digest("starfighter-config", &config)
            .unwrap();
    json!({
        "protocolVersion": "agentrix-engine/2",
        "runId": format!("{match_id}-run-1"),
        "matchId": match_id,
        "engineVersion": "0.4.0",
        "engineDigest": "0".repeat(64),
        "buildIdentity": "starfighter-build",
        "target": "x86_64-unknown-linux-gnu",
        "game": {
            "gameId": "starfighter",
            "gameVersion": bevy_starfighter::game_module::STARFIGHTER_GAME_VERSION,
            "gameDigest": bevy_starfighter::game_module::derive_starfighter_game_digest(),
        },
        "schemaDigests": {
            "action": bevy_starfighter::game_module::ACTION_SCHEMA_DIGEST,
            "observation": bevy_starfighter::game_module::OBSERVATION_SCHEMA_DIGEST,
            "public": bevy_starfighter::game_module::PUBLIC_SCHEMA_DIGEST,
            "replay": "0".repeat(64),
        },
        "config": config,
        "configDigest": config_digest,
        "tickRate": {"numerator": 60, "denominator": 1},
        "seed": 7,
        "slots": [
            {"slotId": p0, "artifactDigest": "1".repeat(64)},
            {"slotId": p1, "artifactDigest": "2".repeat(64)},
        ],
        "limits": {
            "maxTicks": max_ticks,
            "maxPlayers": 2,
            "maxEntities": 1000,
            "maxMessageBytes": 65536,
        },
        "failurePolicyVersion": "fail-closed/1",
        "determinismTier": "same_artifact_same_target",
        "rngAlgorithm": "xoshiro256starstar/1"
    })
}

#[test]
fn full_protocol_sequence_against_real_subprocess() {
    let mut engine = EngineHandle::spawn();

    // 1. engine_ready
    let ready = engine.read_envelope();
    assert_eq!(ready["type"], "engine_ready");
    assert_eq!(
        ready["payload"]["supportedProtocols"][0],
        "agentrix-engine/2"
    );

    // 2. initialize_match
    let spec = sample_spec("test-match-1", "p0", "p1", 50);
    engine.write_envelope("test-match-1", "initialize_match", spec);
    let initialized = engine.read_envelope();
    assert_eq!(initialized["type"], "match_initialized");
    assert_eq!(initialized["payload"]["tick"], 0);

    let observations = &initialized["payload"]["observations"];
    assert!(observations.get("p0").is_some());
    assert!(observations.get("p1").is_some());

    let commitments = &initialized["payload"]["commitments"];
    assert_eq!(
        commitments["executionSpecDigest"].as_str().unwrap().len(),
        64
    );
    assert_eq!(
        commitments["authoritativeStateCommitment"]
            .as_str()
            .unwrap()
            .len(),
        64
    );
    assert_eq!(
        commitments["publicSnapshotHash"].as_str().unwrap().len(),
        64
    );
    assert_eq!(commitments["replayChainDigest"].as_str().unwrap().len(), 64);

    let initial_auth = commitments["authoritativeStateCommitment"]
        .as_str()
        .unwrap()
        .to_string();

    // 3. Advance ticks with actions
    let mut last_auth = initial_auth;
    for tick in 0..5u64 {
        engine.write_envelope(
            "test-match-1",
            "advance_tick",
            json!({
                "tick": tick,
                "actions": {
                    "p0": {"status": "valid", "payload": {"thrust": "FORWARD", "turn": "NONE", "shoot": false, "shield": false}},
                    "p1": {"status": "valid", "payload": {"thrust": "OFF", "turn": "NONE", "shoot": false, "shield": false}},
                }
            }),
        );
        let result = engine.read_envelope();
        assert_eq!(result["type"], "tick_completed");
        assert_eq!(result["payload"]["tick"], tick + 1);

        let new_auth = result["payload"]["commitments"]
            ["authoritativeStateCommitment"]
            .as_str()
            .unwrap()
            .to_string();
        assert_ne!(
            new_auth, last_auth,
            "authoritative state commitment must advance tick to tick"
        );
        last_auth = new_auth;
    }

    // 4. Finish match
    engine.write_envelope(
        "test-match-1",
        "finish_match",
        json!({"reason": "completed"}),
    );
    let completed = engine.read_envelope();
    assert_eq!(completed["type"], "match_completed");

    // 5. shutdown -> shutdown_ack
    engine.write_envelope(
        "test-match-1",
        "shutdown",
        json!({"reason": "test done"}),
    );
    let ack = engine.read_envelope();
    assert_eq!(ack["type"], "shutdown_ack");

    let status = engine.child.wait().expect("process exits cleanly");
    assert!(status.success());
}

#[test]
fn test_simultaneous_double_disqualification_results_in_no_winner() {
    let mut engine = EngineHandle::spawn();
    let ready = engine.read_envelope();
    assert_eq!(ready["type"], "engine_ready");

    let spec = sample_spec("test-match-double-dq", "p0", "p1", 10);
    engine.write_envelope("test-match-double-dq", "initialize_match", spec);
    let init_ack = engine.read_envelope();
    assert_eq!(init_ack["type"], "match_initialized");

    // advance_tick with BOTH players disqualified simultaneously
    engine.write_envelope(
        "test-match-double-dq",
        "advance_tick",
        json!({
            "tick": 0,
            "actions": {
                "p0": { "status": "disqualified" },
                "p1": { "status": "disqualified" },
            }
        }),
    );
    // ADR-0004 con la regla de ADR-0013: ambas naves caen en el mismo tick,
    // la partida termina ahí, sin ganador y con los dos slots empatados.
    let result = engine.read_envelope();
    assert_eq!(result["type"], "match_completed");
    assert_eq!(result["payload"]["tick"], 1);
    assert_eq!(result["payload"]["terminal"], true);
    let outcome = &result["payload"]["result"];
    assert!(outcome["winner"].is_null(), "result: {outcome}");
    assert_eq!(outcome["rankings"][0]["rank"], 1);
    assert_eq!(outcome["rankings"][1]["rank"], 1);

    // Call finish_match
    engine.write_envelope(
        "test-match-double-dq",
        "finish_match",
        json!({"reason": "double_disqualification"}),
    );
    let completed = engine.read_envelope();
    assert_eq!(completed["type"], "match_completed");

    // shutdown
    engine.write_envelope(
        "test-match-double-dq",
        "shutdown",
        json!({"reason": "test done"}),
    );
    let ack = engine.read_envelope();
    assert_eq!(ack["type"], "shutdown_ack");
    let status = engine.child.wait().unwrap();
    assert!(status.success());
}

#[test]
fn test_invalid_sequence_rejected_with_error() {
    let mut engine = EngineHandle::spawn();
    let ready = engine.read_envelope();
    assert_eq!(ready["type"], "engine_ready");

    // Send sequence 5 when 1 is expected
    let spec = sample_spec("m-bad-seq", "p1", "p2", 100);
    engine.write_raw_envelope(
        "agentrix-engine/2",
        "m-bad-seq",
        "initialize_match",
        5,
        spec,
    );

    let err_env = engine.read_envelope();
    assert_eq!(err_env["type"], "engine_error");
    assert_eq!(err_env["payload"]["code"], "invalid_sequence");
    assert_eq!(err_env["payload"]["fatal"], true);
}

#[test]
fn test_incompatible_protocol_version_rejected_with_error() {
    let mut engine = EngineHandle::spawn();
    let ready = engine.read_envelope();
    assert_eq!(ready["type"], "engine_ready");

    let spec = sample_spec("m-bad-ver", "p1", "p2", 100);
    engine.write_raw_envelope(
        "agentrix-engine/99",
        "m-bad-ver",
        "initialize_match",
        1,
        spec,
    );

    let err_env = engine.read_envelope();
    assert_eq!(err_env["type"], "engine_error");
    assert_eq!(err_env["payload"]["code"], "invalid_protocol_version");
    assert_eq!(err_env["payload"]["fatal"], true);
}

#[test]
fn test_advance_before_initialize_rejected_with_invalid_state() {
    let mut engine = EngineHandle::spawn();
    let ready = engine.read_envelope();
    assert_eq!(ready["type"], "engine_ready");

    engine.write_envelope(
        "m-uninit",
        "advance_tick",
        json!({
            "tick": 0,
            "actions": {},
        }),
    );

    let err_env = engine.read_envelope();
    assert_eq!(err_env["type"], "engine_error");
    assert_eq!(err_env["payload"]["code"], "invalid_lifecycle");
    assert_eq!(err_env["payload"]["fatal"], true);
}

#[test]
fn test_match_id_mismatch_rejected_with_error() {
    let mut engine = EngineHandle::spawn();
    let ready = engine.read_envelope();
    assert_eq!(ready["type"], "engine_ready");

    let spec = sample_spec("m-match-1", "p1", "p2", 100);
    engine.write_envelope("m-match-1", "initialize_match", spec);
    let init_res = engine.read_envelope();
    assert_eq!(init_res["type"], "match_initialized");

    // Send advance_tick with mismatched matchId
    engine.write_envelope(
        "m-match-2",
        "advance_tick",
        json!({
            "tick": 0,
            "actions": {
                "p1": {"status": "valid"},
                "p2": {"status": "valid"}
            },
        }),
    );

    let err_env = engine.read_envelope();
    assert_eq!(err_env["type"], "engine_error");
    assert_eq!(err_env["payload"]["code"], "match_id_mismatch");
    assert_eq!(err_env["payload"]["fatal"], true);
}

#[test]
fn test_initialize_twice_rejected_with_invalid_state() {
    let mut engine = EngineHandle::spawn();
    let ready = engine.read_envelope();
    assert_eq!(ready["type"], "engine_ready");

    let spec1 = sample_spec("m-match-dup", "p1", "p2", 100);
    engine.write_envelope("m-match-dup", "initialize_match", spec1);
    let init_res = engine.read_envelope();
    assert_eq!(init_res["type"], "match_initialized");

    // Try initializing again
    let spec2 = sample_spec("m-match-dup", "p1", "p2", 100);
    engine.write_envelope("m-match-dup", "initialize_match", spec2);

    let err_env = engine.read_envelope();
    assert_eq!(err_env["type"], "engine_error");
    assert_eq!(err_env["payload"]["code"], "invalid_lifecycle");
    assert_eq!(err_env["payload"]["fatal"], false);
}

#[test]
fn test_custom_starfighter_config_and_exact_60hz() {
    let mut engine = EngineHandle::spawn();
    let ready = engine.read_envelope();
    assert_eq!(ready["type"], "engine_ready");

    let config = json!({
        "arena_width": 2400.0,
        "arena_height": 1200.0,
        "ship_max_health": 150.0,
        "ship_max_energy": 120.0,
        "bullet_damage": 25.0,
        "asteroid_damage": 100.0,
        "shoot_energy_cost": 15.0,
        "shield_energy_cost_per_tick": 1.0,
        "shield_damage_reduction": 0.7,
        "energy_regen_per_tick": 0.5,
        "radar_range": 900.0,
        "asteroid_count": 0
    });
    let config_digest =
        agentrix_sim_core::canonical_json_digest("starfighter-config", &config)
            .unwrap();
    let spec = json!({
        "protocolVersion": "agentrix-engine/2",
        "runId": "m-config-60hz-run-1",
        "matchId": "m-config-60hz",
        "engineVersion": "0.4.0",
        "engineDigest": "0".repeat(64),
        "buildIdentity": "starfighter-build",
        "target": "x86_64-unknown-linux-gnu",
        "game": {
            "gameId": "starfighter",
            "gameVersion": bevy_starfighter::game_module::STARFIGHTER_GAME_VERSION,
            "gameDigest": bevy_starfighter::game_module::derive_starfighter_game_digest(),
        },
        "schemaDigests": {
            "action": bevy_starfighter::game_module::ACTION_SCHEMA_DIGEST,
            "observation": bevy_starfighter::game_module::OBSERVATION_SCHEMA_DIGEST,
            "public": bevy_starfighter::game_module::PUBLIC_SCHEMA_DIGEST,
            "replay": "0".repeat(64),
        },
        "config": config,
        "configDigest": config_digest,
        "tickRate": {"numerator": 60, "denominator": 1},
        "seed": 777,
        "slots": [
            {"slotId": "p1", "artifactDigest": "1".repeat(64)},
            {"slotId": "p2", "artifactDigest": "2".repeat(64)},
        ],
        "limits": {
            "maxTicks": 50,
            "maxPlayers": 2,
            "maxEntities": 1000,
            "maxMessageBytes": 65536,
        },
        "failurePolicyVersion": "fail-closed/1",
        "determinismTier": "same_artifact_same_target",
        "rngAlgorithm": "xoshiro256starstar/1"
    });

    engine.write_envelope("m-config-60hz", "initialize_match", spec);
    let init_res = engine.read_envelope();
    assert_eq!(init_res["type"], "match_initialized");
    assert_eq!(init_res["payload"]["tick"], 0);

    // Advance tick 0 -> 1
    engine.write_envelope(
        "m-config-60hz",
        "advance_tick",
        json!({
            "tick": 0,
            "actions": {
                "p1": {"status": "valid", "payload": {"thrust": "OFF", "turn": "NONE", "shoot": false, "shield": false}},
                "p2": {"status": "valid", "payload": {"thrust": "OFF", "turn": "NONE", "shoot": false, "shield": false}}
            }
        }),
    );

    let tick_res = engine.read_envelope();
    assert_eq!(tick_res["type"], "tick_completed");
    assert_eq!(tick_res["payload"]["tick"], 1);

    // Clean shutdown
    engine.write_envelope("m-config-60hz", "shutdown", json!({}));
    let ack = engine.read_envelope();
    assert_eq!(ack["type"], "shutdown_ack");
    let status = engine.child.wait().unwrap();
    assert!(status.success());
}
