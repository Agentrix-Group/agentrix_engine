use bevy_starfighter::runner::{run_match, MatchConfig};
use bevy_starfighter::{build_app, init_tracing, Settings};
use clap::Parser;
use std::path::PathBuf;

#[derive(Parser, Debug)]
#[clap(author, version, about, long_about = None)]
struct Args {
    #[clap(long, value_parser, default_value = "0")]
    seed: u64,
    /// Solo se usa en modo legado (sin `--scripts`): cantidad de naves
    /// con IA aleatoria/manual interna. En modo `--scripts` la cantidad
    /// de jugadores la determina la cantidad de scripts, este flag se
    /// ignora.
    #[clap(long, value_parser, default_value = "2")]
    players: u32,
    #[clap(long, value_parser, default_value = "5")]
    asteroid_count: u32,
    /// Enable continuous collision detection
    #[clap(long)]
    ccd: bool,
    /// Modo legado: correr por un número fijo de ticks y salir, en vez
    /// de correr para siempre (`app.run()`).
    #[clap(long, value_parser)]
    ticks: Option<u32>,

    /// Fase 4: correr una partida real contra procesos de bot externos
    /// hablando el protocolo de agente (ATD-007) por stdin/stdout, uno
    /// por script, en el orden dado (slot 0, slot 1, ...). Activa el
    /// modo de partida real en vez del modo legado de arriba.
    #[clap(long, value_delimiter = ',')]
    scripts: Option<Vec<PathBuf>>,

    #[clap(long, default_value = "local-match")]
    match_id: String,

    #[clap(long, default_value = "games/starfighter/manifest.yaml")]
    manifest: PathBuf,

    /// Presupuesto de tiempo por decisión de un bot, en milisegundos.
    /// Placeholder de balance (mismo criterio que HP/energía en Fase 1):
    /// PE-007 del blueprint todavía tiene que fijar el valor real de
    /// producción antes de validar envíos oficiales.
    #[clap(long, default_value_t = 50)]
    timeout_ms: u64,

    /// Sobreescribe `manifest.max_ticks` para esta corrida. Pensado para
    /// pruebas y demostraciones cortas -- una partida oficial debería
    /// dejarlo sin usar y confiar en el límite real del manifest.
    #[clap(long)]
    max_ticks: Option<u32>,

    /// Aceptado por forma; la grabación real es Fase 5 (ATD-011,
    /// replay autoritativo por eventos sellados). Ver
    /// `bevy_starfighter::runner::run_match` para el detalle de por qué
    /// no se escribe ningún archivo todavía en esta fase.
    #[clap(long, value_parser)]
    output_replay: Option<PathBuf>,
}

fn main() {
    init_tracing();
    let args = Args::parse();

    if let Some(scripts) = args.scripts {
        let outcome = run_match(MatchConfig {
            match_id: args.match_id,
            manifest_path: args.manifest,
            scripts,
            seed: args.seed,
            timeout_ms: args.timeout_ms,
            ccd: args.ccd,
            output_replay: args.output_replay,
            max_ticks_override: args.max_ticks,
        });
        tracing::info!(
            "Partida terminada: winner={:?} reason={} ticks={}",
            outcome.winner,
            outcome.reason,
            outcome.ticks_run
        );
        return;
    }

    // Modo legado (Fases 0-3): IA aleatoria/manual interna dentro del
    // mismo proceso, sin bots externos ni protocolo de agente.
    let settings = Settings {
        seed: args.seed,
        players: args.players,
        asteroid_count: args.asteroid_count,
        continuous_collision_detection: args.ccd,
        ..Settings::default()
    };
    let mut app = build_app(settings);
    tracing::info!("Starting native-launcher (headless, legacy mode)");

    match args.ticks {
        Some(ticks) => {
            app.finish();
            app.cleanup();
            app.insert_resource(bevy::time::TimeUpdateStrategy::ManualDuration(
                std::time::Duration::from_secs_f64(1.0 / 60.0),
            ));
            for _ in 0..ticks {
                app.update();
            }
        }
        None => {
            app.run();
        }
    }
}
