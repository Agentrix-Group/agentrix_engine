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
        let env: Value = serde_json::from_str(&line).expect("línea debe ser JSON válido");
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

    fn write_envelope(&mut self, match_id: &str, msg_type: &str, payload: Value) {
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
    assert_eq!(ready["payload"]["supportedProtocols"][0], "agentrix-engine/1");

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
    assert!(perceptions.get("p0").is_some(), "debe haber percepción para p0");
    assert!(perceptions.get("p1").is_some(), "debe haber percepción para p1");
    let initial_hash = initialized["payload"]["stateHash"]
        .as_str()
        .unwrap()
        .to_string();
    assert_eq!(initial_hash.len(), 64, "stateHash debe ser SHA-256 en hex");

    let initial_p0_pos = perceptions["p0"]["myself"]["position"].clone();

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
        assert!(
            result["type"] == "tick_completed" || result["type"] == "match_completed",
            "advance_tick debe responder tick_completed o match_completed, no {}",
            result["type"]
        );
        if result["type"] == "tick_completed" {
            let new_hash = result["payload"]["stateHash"].as_str().unwrap().to_string();
            assert_ne!(new_hash, last_hash, "el stateHash debe cambiar tick a tick (el estado avanza)");
            last_hash = new_hash;
        }
    }

    // Una tanda más pidiendo la percepción para comparar posición.
    engine.write_envelope(
        "test-match-1",
        "advance_tick",
        json!({
            "tick": 5,
            "actions": {
                "p0": {"status": "valid", "payload": {"thrust": "FORWARD", "turn": "NONE", "shoot": false, "shield": false}},
                "p1": {"status": "timeout"},
            }
        }),
    );
    let result = engine.read_envelope();
    if result["type"] == "tick_completed" {
        let p0_pos_now = &result["payload"]["perceptions"]["p0"]["myself"]["position"];
        assert_ne!(
            p0_pos_now, &initial_p0_pos,
            "p0 empujó FORWARD 6 ticks, su posición debe haber cambiado"
        );
    }

    // 4. shutdown -> shutdown_ack, proceso termina limpio.
    engine.write_envelope("test-match-1", "shutdown", json!({"reason": "test done"}));
    let ack = engine.read_envelope();
    assert_eq!(ack["type"], "shutdown_ack");

    let status = engine
        .child
        .wait()
        .expect("el proceso debería terminar después de shutdown_ack");
    assert!(status.success(), "el motor debería salir con código 0 tras shutdown");
}
