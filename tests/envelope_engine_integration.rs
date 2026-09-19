//! Prueba real de aceptación del protocolo motor↔Go (`envelope_engine`):
//! levanta `starfighter-engine` como subproceso real (no in-process) y
//! le habla el protocolo `Envelope` exactamente como lo haría el
//! `subprocessClient` real de Go (`src/engine/subprocess.go`), incluida
//! la validación estricta de secuencia que ese cliente aplica.

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

    /// Lee y valida la próxima línea como haría el cliente real de Go:
    /// `protocolVersion` exacto y `sequence` estrictamente incremental.
    fn read_envelope(&mut self) -> Value {
        let mut line = String::new();
        self.stdout
            .read_line(&mut line)
            .expect("el motor debería mandar una línea");
        let env: Value =
            serde_json::from_str(&line).expect("línea debe ser JSON válido");
        assert_eq!(
            env["protocolVersion"], "agentrix-engine/1",
            "protocolVersion debe ser exactamente el que Go exige"
        );
        assert_eq!(
            env["sequence"], self.expected_recv_seq,
            "sequence de salida del motor debe incrementar de a 1 empezando en 1 (así lo valida subprocess.go real)"
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
            "protocolVersion": "agentrix-engine/1",
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

#[test]
fn full_protocol_sequence_against_real_subprocess() {
    let mut engine = EngineHandle::spawn();

    // 1. engine_ready, no pedido -- primer mensaje, sequence 1.
    let ready = engine.read_envelope();
    assert_eq!(ready["type"], "engine_ready");
    assert_eq!(
        ready["payload"]["supportedProtocols"][0],
        "agentrix-engine/1"
    );

    // 2. initialize_match con 2 jugadores.
    engine.write_envelope(
        "test-match-1",
        "initialize_match",
        json!({
            "matchId": "test-match-1",
            "gameId": "starfighter",
            "seed": 7,
            "fixedTimestepMs": 16,
            "maxTicks": 50,
            "players": ["p0", "p1"],
            "config": {"radar_range": "800"}
        }),
    );
    let initialized = engine.read_envelope();
    assert_eq!(initialized["type"], "match_initialized");
    let perceptions = &initialized["payload"]["perceptions"];
    assert!(
        perceptions.get("p0").is_some(),
        "debe haber percepción para p0"
    );
    assert!(
        perceptions.get("p1").is_some(),
        "debe haber percepción para p1"
    );
    let initial_hash = initialized["payload"]["stateHash"]
        .as_str()
        .unwrap()
        .to_string();
    assert_eq!(initial_hash.len(), 64, "stateHash debe ser SHA-256 en hex");
    assert_eq!(initialized["payload"]["publicSnapshot"]["tick"], 0);
    let public = &initialized["payload"]["publicSnapshot"];
    assert_eq!(public["fighters"].as_array().unwrap().len(), 2);
    assert!(public["bullets"].is_array());
    assert!(public["events"].is_array());
    assert_eq!(public["stateHash"], initialized["payload"]["stateHash"]);
    for fighter in public["fighters"].as_array().unwrap() {
        assert!(fighter["position"].is_object());
        assert!(fighter["velocity"].is_object());
        assert!(fighter["rotation"].is_number());
        assert!(fighter["health"].is_number());
        assert!(fighter["shieldActive"].is_boolean());
    }

    let initial_p0_pos = perceptions["p0"]["myself"]["position"].clone();

    // Un tick adelantado se rechaza; el motor conserva el tick esperado.
    engine.write_envelope(
        "test-match-1",
        "advance_tick",
        json!({"tick": 1, "actions": {}}),
    );
    let mismatch = engine.read_envelope();
    assert_eq!(mismatch["type"], "engine_error");

    // 3. Varios advance_tick con acciones reales -- p0 avanza a fondo,
    //    p1 no hace nada. Confirma que el estado realmente avanza.
    let mut last_hash = initial_hash;
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
        assert_eq!(result["payload"]["publicSnapshot"]["tick"], tick + 1);
        let new_hash =
            result["payload"]["stateHash"].as_str().unwrap().to_string();
        assert_ne!(
            new_hash, last_hash,
            "el stateHash debe cambiar tick a tick (el estado avanza)"
        );
        last_hash = new_hash;
    }

    // Una tanda más pidiendo la percepción para comparar posición.
    engine.write_envelope(
        "test-match-1",
        "advance_tick",
        json!({
            "tick": 5,
            "actions": {
                "p0": {"status": "valid", "payload": {"thrust": "FORWARD", "turn": "NONE", "shoot": false, "shield": false}},
                "p1": {"status": "disqualified", "errorDetails": "timeout"},
            }
        }),
    );
    let result = engine.read_envelope();
    assert_eq!(result["type"], "tick_completed");
    assert_eq!(result["payload"]["tick"], 6);
    assert_eq!(result["payload"]["isOver"], true);
    assert_eq!(result["payload"]["winner"], "p0");
    assert_eq!(result["payload"]["perceptions"]["p0"]["tick"], 6);
    assert_eq!(result["payload"]["perceptions"]["p1"]["tick"], 6);
    let p0_pos_now =
        &result["payload"]["publicSnapshot"]["fighters"][0]["position"];
    assert_ne!(
        p0_pos_now, &initial_p0_pos,
        "p0 empujó FORWARD 6 ticks, su posición debe haber cambiado"
    );

    engine.write_envelope(
        "test-match-1",
        "finish_match",
        json!({"reason": "timeout"}),
    );
    let completed = engine.read_envelope();
    assert_eq!(completed["type"], "match_completed");
    assert_eq!(completed["payload"]["reason"], "timeout");
    assert_eq!(completed["payload"]["finalTick"], 6);

    // 4. shutdown -> shutdown_ack, proceso termina limpio.
    engine.write_envelope(
        "test-match-1",
        "shutdown",
        json!({"reason": "test done"}),
    );
    let ack = engine.read_envelope();
    assert_eq!(ack["type"], "shutdown_ack");

    let status = engine
        .child
        .wait()
        .expect("el proceso debería terminar después de shutdown_ack");
    assert!(
        status.success(),
        "el motor debería salir con código 0 tras shutdown"
    );
}

#[test]
fn test_simultaneous_double_disqualification_results_in_no_winner() {
    let mut engine = EngineHandle::spawn();

    // 1. engine_ready
    let ready = engine.read_envelope();
    assert_eq!(ready["type"], "engine_ready");

    // 2. initialize_match
    engine.write_envelope(
        "test-match-double-dq",
        "initialize_match",
        json!({
            "matchId": "test-match-double-dq",
            "gameId": "starfighter",
            "players": ["p0", "p1"],
            "seed": 42,
            "maxTicks": 10,
            "config": {
                "radar_range": "800.0"
            },
            "fixedTimestepMs": 17,
        }),
    );
    let init_ack = engine.read_envelope();
    assert_eq!(init_ack["type"], "match_initialized");

    // 3. advance_tick with BOTH players disqualified simultaneously
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
    let result = engine.read_envelope();
    assert_eq!(result["type"], "tick_completed");
    assert_eq!(result["payload"]["tick"], 1);
    assert_eq!(result["payload"]["isOver"], true);
    assert!(
        result["payload"]["winner"].is_null(),
        "winner debe ser null ante doble descalificación simultánea"
    );

    // 4. finish_match
    engine.write_envelope(
        "test-match-double-dq",
        "finish_match",
        json!({"reason": "timeout"}),
    );
    let completed = engine.read_envelope();
    assert_eq!(completed["type"], "match_completed");
    assert!(
        completed["payload"]["winner"].is_null(),
        "winner debe ser null en match_completed"
    );

    let rankings = completed["payload"]["rankings"].as_array().unwrap();
    for rank_entry in rankings {
        assert_eq!(
            rank_entry["rank"], 1,
            "ambos jugadores deben estar empatados en rank 1"
        );
        assert_eq!(rank_entry["score"], 0, "puntaje debe ser 0");
    }

    // 5. shutdown
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

    // Engine expects sequence 1, but we send sequence 5.
    engine.write_raw_envelope(
        "agentrix-engine/1",
        "m-bad-seq",
        "initialize_match",
        5,
        json!({
            "matchId": "m-bad-seq",
            "gameId": "starfighter",
            "seed": 123,
            "maxTicks": 100,
            "players": ["p1", "p2"],
        }),
    );

    let err_env = engine.read_envelope();
    assert_eq!(err_env["type"], "engine_error");
    assert_eq!(err_env["payload"]["code"], "ERR_INVALID_SEQUENCE");
    assert_eq!(err_env["payload"]["fatal"], true);
}

#[test]
fn test_incompatible_protocol_version_rejected_with_error() {
    let mut engine = EngineHandle::spawn();
    let ready = engine.read_envelope();
    assert_eq!(ready["type"], "engine_ready");

    engine.write_raw_envelope(
        "agentrix-engine/99",
        "m-bad-ver",
        "initialize_match",
        1,
        json!({
            "matchId": "m-bad-ver",
            "gameId": "starfighter",
            "seed": 123,
            "maxTicks": 100,
            "players": ["p1", "p2"],
        }),
    );

    let err_env = engine.read_envelope();
    assert_eq!(err_env["type"], "engine_error");
    assert_eq!(err_env["payload"]["code"], "ERR_INCOMPATIBLE_VERSION");
    assert_eq!(err_env["payload"]["fatal"], true);
}

#[test]
fn test_advance_before_initialize_rejected_with_invalid_state() {
    let mut engine = EngineHandle::spawn();
    let ready = engine.read_envelope();
    assert_eq!(ready["type"], "engine_ready");

    // Sending advance_tick before initialize_match
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
    assert_eq!(err_env["payload"]["code"], "ERR_INVALID_STATE");
    assert_eq!(err_env["payload"]["fatal"], true);
}

#[test]
fn test_match_id_mismatch_rejected_with_error() {
    let mut engine = EngineHandle::spawn();
    let ready = engine.read_envelope();
    assert_eq!(ready["type"], "engine_ready");

    engine.write_envelope(
        "m-match-1",
        "initialize_match",
        json!({
            "matchId": "m-match-1",
            "gameId": "starfighter",
            "seed": 42,
            "tickHz": 60.0,
            "maxTicks": 100,
            "players": ["p1", "p2"],
        }),
    );

    let init_res = engine.read_envelope();
    assert_eq!(init_res["type"], "match_initialized");

    // Send advance_tick with a different matchId
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
    assert_eq!(err_env["payload"]["code"], "ERR_MATCH_ID_MISMATCH");
    assert_eq!(err_env["payload"]["fatal"], true);
}

#[test]
fn test_initialize_twice_rejected_with_invalid_state() {
    let mut engine = EngineHandle::spawn();
    let ready = engine.read_envelope();
    assert_eq!(ready["type"], "engine_ready");

    engine.write_envelope(
        "m-match-dup",
        "initialize_match",
        json!({
            "matchId": "m-match-dup",
            "gameId": "starfighter",
            "seed": 42,
            "maxTicks": 100,
            "players": ["p1", "p2"],
        }),
    );

    let init_res = engine.read_envelope();
    assert_eq!(init_res["type"], "match_initialized");

    // Try initializing again
    engine.write_envelope(
        "m-match-dup",
        "initialize_match",
        json!({
            "matchId": "m-match-dup",
            "gameId": "starfighter",
            "seed": 99,
            "maxTicks": 100,
            "players": ["p1", "p2"],
        }),
    );

    let err_env = engine.read_envelope();
    assert_eq!(err_env["type"], "engine_error");
    assert_eq!(err_env["payload"]["code"], "ERR_INVALID_STATE");
    assert_eq!(err_env["payload"]["fatal"], true);
}

#[test]
fn test_custom_starfighter_config_and_exact_60hz() {
    let mut engine = EngineHandle::spawn();
    let ready = engine.read_envelope();
    assert_eq!(ready["type"], "engine_ready");

    engine.write_envelope(
        "m-config-60hz",
        "initialize_match",
        json!({
            "matchId": "m-config-60hz",
            "gameId": "starfighter",
            "seed": 777,
            "tickHz": 60.0,
            "maxTicks": 50,
            "players": ["p1", "p2"],
            "config": {
                "tick_hz": 60.0,
                "arena_width": 2400.0,
                "arena_height": 1200.0,
                "ship_max_health": 150.0,
                "ship_max_energy": 120.0,
                "radar_range": 900.0
            }
        }),
    );

    let init_res = engine.read_envelope();
    assert_eq!(init_res["type"], "match_initialized");
    assert_eq!(init_res["matchId"], "m-config-60hz");
    assert_eq!(init_res["payload"]["initialTick"], 0);

    // Advance tick 0 -> 1
    engine.write_envelope(
        "m-config-60hz",
        "advance_tick",
        json!({
            "tick": 0,
            "actions": {
                "p1": {"status": "valid", "payload": {"thrust": "stop", "turn": "none", "shoot": "off", "shield": "off"}},
                "p2": {"status": "valid", "payload": {"thrust": "stop", "turn": "none", "shoot": "off", "shield": "off"}}
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
