//! Baseline de rendimiento del núcleo físico puro -- Fase 6.
//!
//! Mide ticks/seg de `build_app` + `app.update()` en loop, **sin**
//! procesos de bot externos: es el costo del motor (física de Avian2D +
//! los sistemas de `FixedUpdate` de este crate) aislado del overhead de
//! I/O de procesos que sí mide el protocolo stdio de Fase 4. Primera vez
//! que se mide formalmente -- no hay un número anterior contra el cual
//! comparar, pero la infraestructura queda lista para que las próximas
//! fases sí puedan detectar una regresión de rendimiento.

use bevy::prelude::*;
use bevy_starfighter::{build_app, spawn_fighter, Settings};
use criterion::{criterion_group, criterion_main, Criterion};
use std::time::Duration;

fn build_ready_app(players: u32) -> App {
    let settings = Settings {
        players: 0,
        asteroid_count: 5,
        ..Settings::default()
    };
    let mut app = build_app(settings);
    app.finish();
    app.cleanup();
    app.insert_resource(bevy::time::TimeUpdateStrategy::ManualDuration(
        Duration::from_secs_f64(1.0 / 60.0),
    ));
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

    group.finish();
}

criterion_group!(benches, bench_tick_throughput);
criterion_main!(benches);
