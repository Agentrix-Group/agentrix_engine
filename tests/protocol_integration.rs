//! Tests de aceptación de la Fase 4: corren el binario real
//! (`native-launcher`) contra procesos Python reales (no mockeados, no
//! in-process) hablando el protocolo de agente (ATD-007) por
//! stdin/stdout. Cada test verifica un criterio de aceptación distinto
//! de la fase.
//!
//! `--scripts` acepta una ruta por slot, sin argumentos extra -- así
//! sería en producción (un script, punto). El modo de cada bot de
//! prueba (cooperative/hang/garbage/bad_version) lo fija qué wrapper de
//! `tests/fixtures/bot_*.py` se pasa, no un argumento de línea de
//! comandos.

use std::path::PathBuf;
use std::process::Command;

fn fixture(name: &str) -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("tests/fixtures")
        .join(name)
}

fn run_launcher(scripts_csv: &str) -> std::process::Output {
    let bin = env!("CARGO_BIN_EXE_native-launcher");
    Command::new(bin)
        .arg("--scripts")
        .arg(scripts_csv)
        .arg("--timeout-ms")
        .arg("400")
        .arg("--max-ticks")
        .arg("15")
        // "info" incluye "warn"/"error" también (umbral por severidad):
        // necesitamos ver tanto el resultado final (`info!`) como las
        // advertencias de agente (`warn!`) en los mismos tests.
        .env("RUST_LOG", "info")
        .output()
        .expect("el binario native-launcher debe poder ejecutarse")
}

fn scripts_csv(paths: &[PathBuf]) -> String {
    paths
        .iter()
        .map(|p| p.to_str().expect("ruta UTF-8").to_string())
        .collect::<Vec<_>>()
        .join(",")
}

/// Criterio 1: una partida completa real entre dos procesos Python de
/// verdad corre hasta terminar sin colgarse y sin que el runner
/// crashee.
#[test]
fn full_match_between_two_cooperative_bots() {
    let bot = fixture("bot_cooperative.py");
    let output = run_launcher(&scripts_csv(&[bot.clone(), bot]));

    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        output.status.success(),
        "el runner debe salir con éxito. stderr:\n{stderr}"
    );
    // El resultado de la partida se loguea con `tracing::info!`, que
    // `init_tracing()` manda a stderr a propósito (stdout queda
    // reservado para el protocolo de agente, ver protocol.rs) -- no a
    // stdout.
    assert!(
        stderr.contains("Partida terminada"),
        "debe loguear el resultado de la partida. stderr:\n{stderr}"
    );
    assert!(
        !stderr.contains("timeout esperando respuesta"),
        "dos bots cooperativos no deberían generar ningún timeout. stderr:\n{stderr}"
    );
    assert!(
        !stderr.contains("JSON inválido"),
        "dos bots cooperativos no deberían generar ningún error de JSON. stderr:\n{stderr}"
    );
}

/// Criterio 2: un bot que no responde a tiempo no cuelga la partida --
/// se le aplica la acción por defecto y queda logueado como advertencia
/// de `agent`.
#[test]
fn slow_bot_times_out_without_hanging_the_match() {
    let output = run_launcher(&scripts_csv(&[
        fixture("bot_cooperative.py"),
        fixture("bot_hang.py"),
    ]));

    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        output.status.success(),
        "el runner no debe crashear por un bot lento. stderr:\n{stderr}"
    );
    assert!(
        stderr.contains("timeout esperando respuesta"),
        "debe quedar logueado el timeout del slot lento. stderr:\n{stderr}"
    );
}

/// Criterio 3: un bot que manda basura en vez de JSON no tira el
/// runner -- mismo tratamiento que un timeout.
#[test]
fn garbage_output_does_not_crash_the_runner() {
    let output = run_launcher(&scripts_csv(&[
        fixture("bot_cooperative.py"),
        fixture("bot_garbage.py"),
    ]));

    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        output.status.success(),
        "el runner no debe crashear por JSON inválido de un bot. stderr:\n{stderr}"
    );
    assert!(
        stderr.contains("JSON inválido"),
        "debe quedar logueado el JSON inválido del slot que manda basura. stderr:\n{stderr}"
    );
}

/// Criterio 4: un bot que confirma una versión de protocolo distinta a
/// la real hace que el runner aborte esa conexión de forma explícita,
/// sin seguir adelante como si nada ni colgar la partida.
#[test]
fn incompatible_protocol_version_aborts_that_connection() {
    let output = run_launcher(&scripts_csv(&[
        fixture("bot_cooperative.py"),
        fixture("bot_bad_version.py"),
    ]));

    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        output.status.success(),
        "una versión incompatible debe abortar esa conexión, no crashear el runner. stderr:\n{stderr}"
    );
    assert!(
        stderr.contains("versión de protocolo incompatible"),
        "debe quedar logueado el rechazo explícito de versión. stderr:\n{stderr}"
    );
}
