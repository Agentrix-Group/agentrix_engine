//! Baseline de rendimiento del núcleo físico puro -- Fase 6.
//!
//! Mide ticks/seg de `build_app` + `app.update()` en loop, **sin**
//! procesos de bot externos: es el costo del motor (backend físico +
//! los sistemas de reglas de este crate) aislado del overhead de
//! I/O de procesos que sí mide el protocolo stdio de Fase 4.
//!
//! Los escenarios `*_combat` (F0 del corte Rapier/FFA) usan solo la API
//! pública que sobrevive al cambio de backend físico (`build_app` +
//! `FighterActionMessage`), con acciones pseudoaleatorias sembradas: el
//! mismo código mide Avian2D antes de la migración y Rapier después.

use agentrix_sim_core::DeterministicRng;
use bevy::prelude::*;
use bevy_starfighter::{
    build_app, spawn_fighter, Fighter, FighterAction, FighterActionMessage,
    Settings, Shield, Shoot, Thrust, Turn,
};
use criterion::{criterion_group, criterion_main, Criterion};

const COMBAT_TICKS: u32 = 600;

fn build_ready_app(players: u32) -> App {
    let settings = Settings {
        players: 0,
        asteroid_count: 5,
        ..Settings::default()
    };
    let mut app = build_app(settings);
    app.finish();
    app.cleanup();
    app.update(); // Startup

    for i in 0..players {
        spawn_fighter(
            &mut app.world_mut().commands(),
            i as usize,
            Vec2::new(i as f32 * 100.0, 0.0),
        );
    }
    app.world_mut().flush();
    app
}

/// Partida con `players` naves spawneadas por el propio `setup` del
/// motor, lista para recibir acciones.
fn build_combat_app(players: u32) -> (App, Vec<Entity>) {
    let settings = Settings {
        seed: 7,
        players,
        ..Settings::default()
    };
    let mut app = build_app(settings);
    app.finish();
    app.cleanup();
    app.update(); // Startup
    let mut pairs: Vec<(usize, Entity)> = app
        .world_mut()
        .query::<(Entity, &Fighter)>()
        .iter(app.world())
        .map(|(entity, fighter)| (fighter.player_id, entity))
        .collect();
    pairs.sort_by_key(|(id, _)| *id);
    (app, pairs.into_iter().map(|(_, entity)| entity).collect())
}

fn random_action(rng: &mut DeterministicRng) -> FighterAction {
    FighterAction {
        thrust: match rng.range_u32(0, 3).expect("valid range") {
            0 => Thrust::On,
            1 => Thrust::Off,
            _ => Thrust::Stop,
        },
        turn: match rng.range_u32(0, 3).expect("valid range") {
            0 => Turn::Left,
            1 => Turn::Right,
            _ => Turn::None,
        },
        shoot: if rng.probability(0.85).expect("valid probability") {
            Shoot::On
        } else {
            Shoot::Off
        },
        shield: if rng.probability(0.2).expect("valid probability") {
            Shield::On
        } else {
            Shield::Off
        },
    }
}

fn run_combat(app: &mut App, entities: &[Entity]) {
    let mut rng = DeterministicRng::from_seed(11);
    for _ in 0..COMBAT_TICKS {
        for entity in entities {
            let action = random_action(&mut rng);
            app.world_mut().write_message(FighterActionMessage {
                action,
                entity: *entity,
            });
        }
        app.update();
    }
}

fn bench_tick_throughput(c: &mut Criterion) {
    let mut group = c.benchmark_group("tick_throughput");

    // 2 naves + 5 asteroides: la configuración por defecto de una
    // partida 1v1 real de Starfighter.
    group.bench_function("2_fighters_5_asteroids", |b| {
        b.iter_batched(
            || build_ready_app(2),
            |mut app| {
                for _ in 0..100 {
                    app.update();
                }
            },
            criterion::BatchSize::LargeInput,
        );
    });

    for players in [2u32, 5] {
        group.bench_function(
            format!("{players}_fighters_combat_{COMBAT_TICKS}_ticks"),
            |b| {
                b.iter_batched(
                    || build_combat_app(players),
                    |(mut app, entities)| run_combat(&mut app, &entities),
                    criterion::BatchSize::LargeInput,
                );
            },
        );
    }

    group.finish();
}

criterion_group!(benches, bench_tick_throughput);
criterion_main!(benches);
