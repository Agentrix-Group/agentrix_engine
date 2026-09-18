use bevy_starfighter::{build_app, init_tracing, Settings};
use clap::Parser;

#[derive(Parser, Debug)]
#[clap(author, version, about, long_about = None)]
struct Args {
    #[clap(long, value_parser, default_value = "0")]
    seed: u64,
    #[clap(long, value_parser, default_value = "2")]
    players: u32,
    #[clap(long, value_parser, default_value = "5")]
    asteroid_count: u32,
    /// Enable continuous collision detection
    #[clap(long)]
    ccd: bool,
    /// Run for a fixed number of ticks and exit, instead of running forever.
    #[clap(long, value_parser)]
    ticks: Option<u32>,
}

fn main() {
    init_tracing();
    let args = Args::parse();
    let settings = Settings {
        seed: args.seed,
        players: args.players,
        asteroid_count: args.asteroid_count,
        continuous_collision_detection: args.ccd,
        ..Settings::default()
    };
    let mut app = build_app(settings);
    tracing::info!("Starting native-launcher (headless)");

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
