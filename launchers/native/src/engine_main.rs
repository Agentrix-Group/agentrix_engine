//! Binario motor↔Go real de Agentrix (protocolo `agentrix-engine/2`).
//! Go lo arranca como subproceso vía `manifest.BinaryPath` (o
//! `AGENTRIX_ENGINE_BIN`) y le habla por stdin/stdout.
//!
use bevy_starfighter::{init_tracing, stdio_server};

fn main() {
    init_tracing();
    stdio_server::run_stdio_server();
}
