//! Tests de aceptación de la Fase 5 (ATD-011): corren el binario real
//! (no in-process, no mockeado) y verifican el replay que efectivamente
//! escribe a disco -- determinismo del digest encadenado y
//! reconstrucción sin re-ejecutar agentes ni levantar el motor.

use std::path::{Path, PathBuf};
use std::process::Command;

fn fixture(name: &str) -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("tests/fixtures")
        .join(name)
}

fn unique_temp_path(label: &str) -> PathBuf {
    std::env::temp_dir().join(format!(
        "starfighter-replay-test-{label}-{}.json",
        std::process::id()
    ))
}

fn run_launcher_with_replay(match_id: &str, seed: u64, replay_path: &Path) -> std::process::Output {
    let bin = env!("CARGO_BIN_EXE_native-launcher");
    let bot = fixture("bot_cooperative.py");
    let scripts = format!(
        "{},{}",
        bot.to_str().expect("ruta UTF-8"),
        bot.to_str().expect("ruta UTF-8")
    );
    Command::new(bin)
        .arg("--scripts")
        .arg(scripts)
        .arg("--match-id")
        .arg(match_id)
        .arg("--seed")
        .arg(seed.to_string())
        .arg("--timeout-ms")
        .arg("400")
        .arg("--max-ticks")
        .arg("15")
        .arg("--output-replay")
        .arg(replay_path)
        .env("RUST_LOG", "warn")
        .output()
        .expect("el binario native-launcher debe poder ejecutarse")
}

/// Criterio de aceptación 1 (mismo seed, mismo digest): correr la misma
/// partida dos veces, mismos bots/seed/match_id, tiene que dar
/// exactamente el mismo digest final. Detectó un bug real durante el
/// desarrollo de esta fase: `actions` era un `HashMap` (orden de
/// iteración aleatorio por proceso en Rust), lo que hacía que el mismo
/// contenido lógico serializara distinto entre corridas -- se corrigió
/// a `BTreeMap` (orden determinístico por clave). Este test es la
/// prueba de regresión de ese bug.
#[test]
fn same_seed_produces_same_digest() {
    let path_a = unique_temp_path("a");
    let path_b = unique_temp_path("b");
    let _cleanup = scopeguard(&path_a, &path_b);

    let out_a = run_launcher_with_replay("determinism-test", 777, &path_a);
    assert!(
        out_a.status.success(),
        "primera corrida debe terminar bien. stderr:\n{}",
        String::from_utf8_lossy(&out_a.stderr)
    );
    let out_b = run_launcher_with_replay("determinism-test", 777, &path_b);
    assert!(
        out_b.status.success(),
        "segunda corrida debe terminar bien. stderr:\n{}",
        String::from_utf8_lossy(&out_b.stderr)
    );

    let replay_a: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(&path_a).expect("replay A debe existir"))
            .expect("replay A debe ser JSON válido");
    let replay_b: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(&path_b).expect("replay B debe existir"))
            .expect("replay B debe ser JSON válido");

    let digest_a = replay_a["digest"].as_str().expect("digest A debe ser string");
    let digest_b = replay_b["digest"].as_str().expect("digest B debe ser string");
    assert_eq!(
        digest_a, digest_b,
        "mismo seed/match_id/bots debe producir el mismo digest"
    );
    assert_eq!(digest_a.len(), 64, "SHA-256 en hex son 64 caracteres");
}

/// Criterio de aceptación 2 (reconstrucción sin re-ejecutar agentes):
/// el replay sellado en disco se lee e itera como JSON genérico -- sin
/// ningún tipo Rust de este crate, sin proceso de bot, sin Bevy -- para
/// probar que de verdad es autocontenido. Así es como lo consumiría
/// cualquier otra parte de Agentrix (el visor web, por ejemplo), que no
/// tiene ni tendría estos structs de Rust.
#[test]
fn replay_reconstructs_without_rerunning_agents_or_the_engine() {
    let path = unique_temp_path("reconstruct");
    let _cleanup = scopeguard(&path, &path);

    let out = run_launcher_with_replay("reconstruct-test", 42, &path);
    assert!(
        out.status.success(),
        "la corrida debe terminar bien. stderr:\n{}",
        String::from_utf8_lossy(&out.stderr)
    );

    let raw = std::fs::read_to_string(&path).expect("el replay debe existir en disco");
    let replay: serde_json::Value = serde_json::from_str(&raw).expect("debe ser JSON válido");

    for key in ["game_id", "match_id", "players", "frames", "scores", "digest"] {
        assert!(
            replay.get(key).is_some(),
            "el replay debe tener la clave requerida `{key}`"
        );
    }
    assert_eq!(replay["game_id"], "starfighter");
    assert_eq!(replay["seed"], 42);

    let frames = replay["frames"].as_array().expect("frames debe ser un array");
    assert!(!frames.is_empty(), "una partida de 15 ticks debe producir frames");
    assert_eq!(
        frames.len(),
        15,
        "max-ticks 15 sin resolución antes debe producir exactamente 15 frames"
    );

    // Reconstruir una trayectoria simple (posición de cada nave por
    // tick) puramente desde el JSON, sin re-simular nada -- es la
    // prueba real de "reconstruir sin re-ejecutar agentes".
    let mut previous_tick: Option<i64> = None;
    for frame in frames {
        let tick = frame["tick"].as_i64().expect("tick debe ser entero");
        if let Some(prev) = previous_tick {
            assert_eq!(tick, prev + 1, "los ticks deben venir en orden, sin huecos");
        }
        previous_tick = Some(tick);

        let fighters = frame["state"]["fighters"]
            .as_array()
            .expect("state.fighters debe ser un array");
        assert_eq!(fighters.len(), 2, "dos naves deben seguir en el state hasta que una muera");
        for fighter in fighters {
            assert!(fighter["position"]["x"].is_number());
            assert!(fighter["health"].is_number());
        }

        assert!(frame["actions"].is_object(), "actions debe ser un objeto");
        assert!(frame["events"].is_array(), "events debe ser un array");
    }
}

/// Limpia los archivos temporales al salir del test, incluso si un
/// assert entra en pánico a mitad de camino.
struct Cleanup<'a>(&'a Path, &'a Path);
impl Drop for Cleanup<'_> {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(self.0);
        let _ = std::fs::remove_file(self.1);
    }
}
fn scopeguard<'a>(a: &'a Path, b: &'a Path) -> Cleanup<'a> {
    Cleanup(a, b)
}
