//! Imprime una instancia de ejemplo de `Perception` en JSON por stdout.
//!
//! Sirve como muestra real (no un ejemplo escrito a mano en un schema)
//! para validar `games/starfighter/contracts/perception.schema.json`:
//!
//! ```text
//! cargo run --example perception_sample | \
//!   uvx check-jsonschema --schemafile games/starfighter/contracts/perception.schema.json -
//! ```
use bevy::math::Vec2;
use bevy_starfighter::{build_perception, BulletSnapshot, FighterSnapshot};

fn main() {
    let me = FighterSnapshot {
        player_id: 0,
        position: Vec2::new(0.0, 0.0),
        velocity: Vec2::new(50.0, 0.0),
        facing: Vec2::new(0.0, 1.0),
        health: 100.0,
        energy: 80.0,
        shield_active: false,
        remaining_bullet_cooldown: 0,
    };
    let rival = FighterSnapshot {
        player_id: 1,
        position: Vec2::new(200.0, 100.0),
        velocity: Vec2::new(-30.0, 0.0),
        facing: Vec2::new(-1.0, 0.0),
        health: 60.0,
        energy: 999.0, // nunca debería aparecer en la salida
        shield_active: true,
        remaining_bullet_cooldown: 999, // idem
    };
    let bullet = BulletSnapshot {
        player_id: 1,
        position: Vec2::new(150.0, 90.0),
        velocity: Vec2::new(-1500.0, 0.0),
    };

    let perception = build_perception(42, 0, 800.0, &[me, rival], &[bullet])
        .expect("player 0 debe existir en la lista de fighters");

    println!(
        "{}",
        serde_json::to_string_pretty(&perception).expect("Perception debe serializar")
    );
}
