//! Binario motor↔Go real de Agentrix (protocolo `Envelope`,
//! `agentrix-engine/1`). Go lo arranca como subproceso vía
//! `manifest.BinaryPath` (o `AGENTRIX_ENGINE_BIN`) y le habla por
//! stdin/stdout -- este proceso nunca inicia nada por sí mismo ni habla
//! con bots directamente (eso lo maneja Go). Ver
//! `bevy_starfighter::envelope_engine` para el detalle del protocolo.
//!
//! Distinto de `main.rs` (`native-launcher`, Fases 4-6), que sigue
//! existiendo intacto como herramienta de testing/demo standalone
//! hablando directo con bots por stdio.

use bevy_starfighter::{envelope_engine, init_tracing};

fn main() {
    init_tracing();
    envelope_engine::run_stdio_server();
}
