//! Starfighter adapter for the generic `agentrix-sim-core` boundary.
//!
//! The legacy JSON-lines adapter remains active until the coordinated protocol
//! v2 cut. This module is the authoritative in-process path exercised by the
//! phase 1/2 conformance and determinism tests.

use crate::protocol::WireAction;
use crate::{
    build_app, build_perception, collect_bullet_snapshots,
    collect_fighter_snapshots, Asteroid, Bullet, CollisionType, EntityId,
    EntityIdAllocator, Fighter, FighterAction, FighterActionMessage,
    MatchResult, RngState, Settings, Shield, Shoot, StarfighterConfig,
    StarfighterEvent, Thrust, TickEvents, Turn,
};
use agentrix_sim_core::{
    canonical_json_digest, ActionBatch, ActionStatus, CanonicalEncoder,
    GameDescriptor, GameKey, GameModule, GamePlayerLimits, GameSchemaDigests,
    SimError, Simulation, SimulationFrame, SimulationSpec,
};
use avian2d::prelude::{
    AngularVelocity, Collisions, ConstantForce, LinearVelocity,
};
use bevy::ecs::system::RunSystemOnce;
use bevy::prelude::*;
use serde_json::{json, Value};
use std::collections::BTreeMap;
use std::time::Duration;

pub const STARFIGHTER_GAME_ID: &str = "starfighter";
pub const STARFIGHTER_GAME_VERSION: &str = "0.3.0-core.1";
pub const ACTION_SCHEMA_DIGEST: &str =
    "6a01f31f81fcc4ee40aed89bee030a5250d841edbf5a9c93548d7f8ae78356d4";
pub const OBSERVATION_SCHEMA_DIGEST: &str =
    "09264082275be4df7a7da2d51f447f8dbb1c08929a0a4120a9f9202a950426ea";
pub const PUBLIC_SCHEMA_DIGEST: &str =
    "6a422a9da18f35530ea63ac49959e77ac08a08c30e2a728b51d6c6144ead293c";

pub fn derive_starfighter_game_digest() -> String {
    let manifest = json!({
        "gameId": STARFIGHTER_GAME_ID,
        "gameVersion": STARFIGHTER_GAME_VERSION,
        "schemas": {
            "action": ACTION_SCHEMA_DIGEST,
            "observation": OBSERVATION_SCHEMA_DIGEST,
            "public": PUBLIC_SCHEMA_DIGEST
        }
    });
    canonical_json_digest("agentrix.game-manifest/1", &manifest)
        .expect("game manifest must digest")
}

pub fn starfighter_game_key() -> GameKey {
    GameKey {
        game_id: STARFIGHTER_GAME_ID.to_string(),
        game_version: STARFIGHTER_GAME_VERSION.to_string(),
        game_digest: derive_starfighter_game_digest(),
    }
}

pub struct StarfighterGame {
    descriptor: GameDescriptor,
}

impl StarfighterGame {
    pub fn new() -> Self {
        Self {
            descriptor: GameDescriptor {
                key: starfighter_game_key(),
                players: GamePlayerLimits {
                    minimum: 2,
                    maximum: 2,
                },
                schemas: GameSchemaDigests {
                    action: ACTION_SCHEMA_DIGEST.to_string(),
                    observation: OBSERVATION_SCHEMA_DIGEST.to_string(),
                    public: PUBLIC_SCHEMA_DIGEST.to_string(),
                },
                capabilities: vec![
                    "authoritative_commitment".to_string(),
                    "avian2d".to_string(),
                    "deterministic_core_d1".to_string(),
                ],
            },
        }
    }
}

impl Default for StarfighterGame {
    fn default() -> Self {
        Self::new()
    }
}

impl GameModule for StarfighterGame {
    fn descriptor(&self) -> &GameDescriptor {
        &self.descriptor
    }

    fn create(
        &self,
        spec: SimulationSpec,
    ) -> Result<Box<dyn Simulation>, SimError> {
        spec.validate()?;
        if spec.game != self.descriptor.key {
            return Err(SimError::new(
                "game_identity_mismatch",
                "simulation spec does not match Starfighter identity",
            ));
        }
        let expected_digest =
            canonical_json_digest("starfighter-config", &spec.config)?;
        if expected_digest != spec.config_digest {
            return Err(SimError::new(
                "config_digest_mismatch",
                "config digest does not match canonical Starfighter config",
            ));
        }
        let config = StarfighterConfig::from_value(&spec.config)?;
        let settings = Settings {
            seed: spec.seed,
            tick_rate: spec.tick_rate,
            players: spec.slots.len() as u32,
            asteroid_count: config.asteroid_count,
            radar_range: config.radar_range,
            max_entities: u64::from(spec.limits.max_entities),
            config,
            ..Settings::default()
        };
        let mut app = build_app(settings);
        app.finish();
        app.cleanup();

        // Startup/reset must produce State[0], never State[1]. Bevy receives
        // zero elapsed time for the startup update, then the exact rational
        // cadence is installed for subsequent simulation updates.
        app.insert_resource(bevy::time::TimeUpdateStrategy::ManualDuration(
            Duration::ZERO,
        ));
        app.update();
        app.insert_resource(bevy::time::TimeUpdateStrategy::ManualDuration(
            Duration::from_secs_f64(spec.tick_rate.seconds_per_tick()),
        ));

        let entities = fighter_entities(&mut app)?;
        if entities.len() != spec.slots.len() {
            return Err(SimError::new(
                "starfighter_reset_failed",
                "reset did not create exactly one fighter per slot",
            ));
        }
        Ok(Box::new(StarfighterSimulation {
            app,
            spec,
            entities,
            tick: 0,
            initial_emitted: false,
        }))
    }
}

struct StarfighterSimulation {
    app: App,
    spec: SimulationSpec,
    entities: Vec<Entity>,
    tick: u64,
    initial_emitted: bool,
}

#[derive(Clone)]
struct PhysicsContactState {
    collider1: Option<agentrix_sim_core::StableEntityId>,
    collider2: Option<agentrix_sim_core::StableEntityId>,
    body1: Option<agentrix_sim_core::StableEntityId>,
    body2: Option<agentrix_sim_core::StableEntityId>,
    flags: u16,
    manifolds: Vec<PhysicsManifoldState>,
}

#[derive(Clone)]
struct PhysicsManifoldState {
    normal: Vec2,
    friction: f32,
    restitution: f32,
    tangent_speed: f32,
    points: Vec<PhysicsContactPointState>,
}

#[derive(Clone)]
struct PhysicsContactPointState {
    anchor1: Vec2,
    anchor2: Vec2,
    point: Vec2,
    penetration: f32,
    normal_impulse: f32,
    normal_speed: f32,
    warm_start_normal_impulse: f32,
    warm_start_tangent_impulse: f32,
    feature1: u32,
    feature2: u32,
}

fn read_snapshots(
    app: &mut App,
) -> Result<(Vec<crate::FighterSnapshot>, Vec<crate::BulletSnapshot>), SimError>
{
    let fighters = app
        .world_mut()
        .run_system_once(|q: Query<(&Fighter, &Transform, &LinearVelocity)>| {
            collect_fighter_snapshots(&q)
        })
        .map_err(|error| {
            SimError::new("fighter_snapshot_query_failed", format!("{error:?}"))
        })?;
    let bullets = app
        .world_mut()
        .run_system_once(|q: Query<(&Bullet, &Transform, &LinearVelocity)>| {
            collect_bullet_snapshots(&q)
        })
        .map_err(|error| {
            SimError::new("bullet_snapshot_query_failed", format!("{error:?}"))
        })?;
    Ok((fighters, bullets))
}

fn neutral_action() -> FighterAction {
    FighterAction {
        thrust: Thrust::Off,
        turn: Turn::None,
        shoot: Shoot::Off,
        shield: Shield::Off,
    }
}

impl StarfighterSimulation {
    fn frame(
        &mut self,
        events: Vec<StarfighterEvent>,
    ) -> Result<SimulationFrame, SimError> {
        let (fighters, bullets) = read_snapshots(&mut self.app)?;
        let slot_ids = self.spec.slot_ids();
        let observations = self
            .spec
            .slots
            .iter()
            .enumerate()
            .filter_map(|(player_id, slot)| {
                build_perception(
                    self.tick,
                    player_id,
                    self.app.world().resource::<Settings>().radar_range,
                    &fighters,
                    &bullets,
                )
                .map(|perception| {
                    serde_json::to_value(perception)
                        .map(|value| (slot.slot_id.clone(), value))
                        .map_err(|error| {
                            SimError::new(
                                "observation_serialization_failed",
                                error.to_string(),
                            )
                        })
                })
            })
            .collect::<Result<BTreeMap<_, _>, _>>()?;
        let public_snapshot =
            public_snapshot(&mut self.app, self.tick, &slot_ids, &events)?;
        let match_result = *self.app.world().resource::<MatchResult>();
        let terminal =
            match_result.finished || self.tick >= self.spec.limits.max_ticks;
        let result = terminal.then(|| {
            game_result(
                match_result,
                self.tick,
                &slot_ids,
                self.spec.limits.max_ticks,
            )
        });
        let events_value: Vec<Value> = events
            .into_iter()
            .map(|e| serde_json::to_value(e).unwrap_or(Value::Null))
            .collect();
        Ok(SimulationFrame {
            tick: self.tick,
            public_snapshot,
            observations,
            events: events_value,
            terminal,
            result,
        })
    }
}

impl Simulation for StarfighterSimulation {
    fn spec(&self) -> &SimulationSpec {
        &self.spec
    }

    fn initial_frame(&mut self) -> Result<SimulationFrame, SimError> {
        if self.initial_emitted {
            return Err(SimError::new(
                "initial_frame_repeated",
                "initial frame may only be emitted once",
            ));
        }
        self.initial_emitted = true;
        self.frame(Vec::new())
    }

    fn advance(
        &mut self,
        actions: &ActionBatch,
    ) -> Result<SimulationFrame, SimError> {
        if !self.initial_emitted {
            return Err(SimError::new(
                "invalid_game_lifecycle",
                "initial frame must be emitted before advance",
            ));
        }
        if self.tick >= self.spec.limits.max_ticks
            || self.app.world().resource::<MatchResult>().finished
        {
            return Err(SimError::new(
                "invalid_game_lifecycle",
                "Starfighter simulation is already terminal",
            ));
        }
        self.app.world_mut().resource_mut::<TickEvents>().0.clear();
        for (player_id, slot_spec) in self.spec.slots.iter().enumerate() {
            let slot = &slot_spec.slot_id;
            let action = match actions.get(slot) {
                Some(outcome) if outcome.status == ActionStatus::Valid => {
                    let payload = outcome.applied.clone().ok_or_else(|| {
                        SimError::new(
                            "missing_applied_action",
                            format!("valid action for {slot} has no applied payload"),
                        )
                    })?;
                    let wire: WireAction = serde_json::from_value(payload)
                        .map_err(|error| {
                            SimError::new(
                                "invalid_action",
                                format!("slot {slot}: {error}"),
                            )
                        })?;
                    FighterAction::from(wire)
                }
                _ => neutral_action(),
            };
            if let Some(entity) = self.entities.get(player_id) {
                self.app.world_mut().write_message(FighterActionMessage {
                    action,
                    entity: *entity,
                });
            }
        }
        self.app.update();
        self.tick += 1;
        let mut events = self.app.world().resource::<TickEvents>().0.clone();
        events.sort();
        self.frame(events)
    }

    fn canonical_authoritative_state(&mut self) -> Result<Vec<u8>, SimError> {
        encode_authoritative_state(&mut self.app, self.tick, &self.spec)
    }
}

fn fighter_entities(app: &mut App) -> Result<Vec<Entity>, SimError> {
    app.world_mut()
        .run_system_once(|query: Query<(Entity, &Fighter)>| {
            let mut entities: Vec<_> = query
                .iter()
                .map(|(entity, fighter)| (fighter.player_id, entity))
                .collect();
            entities.sort_by_key(|(player_id, _)| *player_id);
            entities
                .into_iter()
                .map(|(_, entity)| entity)
                .collect::<Vec<_>>()
        })
        .map_err(|error| {
            SimError::new("fighter_query_failed", error.to_string())
        })
}

fn public_snapshot(
    app: &mut App,
    tick: u64,
    slots: &[String],
    events: &[StarfighterEvent],
) -> Result<Value, SimError> {
    let mut fighter_state = app
        .world_mut()
        .run_system_once(
            |query: Query<(
                &EntityId,
                &Fighter,
                &Transform,
                &LinearVelocity,
                &AngularVelocity,
            )>| {
                query
                    .iter()
                    .map(|(id, fighter, transform, velocity, angular)| {
                        (
                            id.0,
                            fighter.player_id,
                            transform.translation,
                            velocity.0,
                            transform.rotation.to_euler(EulerRot::XYZ).2,
                            angular.0,
                            fighter.health,
                            fighter.shield_active,
                        )
                    })
                    .collect::<Vec<_>>()
            },
        )
        .map_err(|error| {
            SimError::new("public_fighter_query_failed", error.to_string())
        })?;
    fighter_state.sort_by_key(|fighter| fighter.0);
    let fighters: Vec<_> = fighter_state
        .into_iter()
        .map(|fighter| {
            json!({
                "entityId": fighter.0.0,
                "slot": slots.get(fighter.1),
                "position": {"x": fighter.2.x, "y": fighter.2.y},
                "velocity": {"x": fighter.3.x, "y": fighter.3.y},
                "rotation": fighter.4,
                "angularVelocity": fighter.5,
                "health": fighter.6,
                "shieldActive": fighter.7,
            })
        })
        .collect();
    let mut bullet_state = app
        .world_mut()
        .run_system_once(
            |query: Query<(
                &EntityId,
                &Bullet,
                &Transform,
                &LinearVelocity,
            )>| {
                query
                    .iter()
                    .map(|(id, bullet, transform, velocity)| {
                        (
                            id.0,
                            bullet.player_id,
                            transform.translation,
                            velocity.0,
                        )
                    })
                    .collect::<Vec<_>>()
            },
        )
        .map_err(|error| {
            SimError::new("public_bullet_query_failed", error.to_string())
        })?;
    bullet_state.sort_by_key(|bullet| bullet.0);
    let bullets: Vec<_> = bullet_state
        .into_iter()
        .map(|bullet| {
            json!({
                "entityId": bullet.0.0,
                "slot": slots.get(bullet.1),
                "position": {"x": bullet.2.x, "y": bullet.2.y},
                "velocity": {"x": bullet.3.x, "y": bullet.3.y},
            })
        })
        .collect();
    Ok(json!({
        "schemaVersion": "starfighter-public/1",
        "tick": tick,
        "fighters": fighters,
        "bullets": bullets,
        "events": events,
    }))
}

fn game_result(
    result: MatchResult,
    tick: u64,
    slots: &[String],
    _max_ticks: u64,
) -> Value {
    let winner = result.winner.and_then(|player_id| slots.get(player_id));
    let mut rankings = Vec::new();
    for (i, slot) in slots.iter().enumerate() {
        let score = if Some(i) == result.winner { 1 } else { 0 };
        let rank = if Some(i) == result.winner {
            1
        } else if result.winner.is_some() {
            2
        } else {
            1
        };
        rankings.push(json!({
            "slot": slot,
            "score": score,
            "rank": rank,
        }));
    }
    rankings.sort_by(|a: &Value, b: &Value| {
        let rank_a = a["rank"].as_u64().unwrap_or(0);
        let rank_b = b["rank"].as_u64().unwrap_or(0);
        let slot_a = a["slot"].as_str().unwrap_or("");
        let slot_b = b["slot"].as_str().unwrap_or("");
        rank_a.cmp(&rank_b).then_with(|| slot_a.cmp(slot_b))
    });
    json!({
        "schemaVersion": "agentrix-game-result/1",
        "terminalReason": if result.finished { "eliminated" } else { "tick_limit" },
        "winner": winner,
        "rankings": rankings,
        "tick": tick,
    })
}

fn encode_authoritative_state(
    app: &mut App,
    tick: u64,
    spec: &SimulationSpec,
) -> Result<Vec<u8>, SimError> {
    let mut encoder =
        CanonicalEncoder::new("starfighter-authoritative-state/1");
    encoder.u64(tick);
    encoder.string(&spec.game.game_digest);
    encoder.string(&spec.config_digest);
    encoder.u32(spec.tick_rate.numerator);
    encoder.u32(spec.tick_rate.denominator);

    let settings_value =
        serde_json::to_value(&app.world().resource::<Settings>().config)
            .map_err(|error| {
                SimError::new(
                    "settings_serialization_failed",
                    error.to_string(),
                )
            })?;
    encoder.json(&settings_value)?;
    encoder.u64(app.world().resource::<EntityIdAllocator>().next_id());
    app.world()
        .resource::<RngState>()
        .0
        .encode_state(&mut encoder);
    let result = app.world().resource::<MatchResult>();
    encoder.bool(result.finished);
    match result.winner {
        Some(winner) => {
            encoder.bool(true);
            encoder.u64(winner as u64);
        }
        None => encoder.bool(false),
    }

    let mut fighters = app
        .world_mut()
        .run_system_once(
            |query: Query<(
                &EntityId,
                &Fighter,
                &Transform,
                &LinearVelocity,
                &AngularVelocity,
                &ConstantForce,
                &CollisionType,
            )>| {
                query
                    .iter()
                    .map(
                        |(
                            id,
                            fighter,
                            transform,
                            linear,
                            angular,
                            force,
                            collision,
                        )| {
                            (
                                id.0,
                                fighter.player_id,
                                fighter.health,
                                fighter.max_health,
                                fighter.energy,
                                fighter.max_energy,
                                fighter.shield_active,
                                fighter.remaining_bullet_cooldown,
                                fighter.is_turning,
                                transform.translation,
                                transform.rotation,
                                linear.0,
                                angular.0,
                                force.0,
                                *collision,
                            )
                        },
                    )
                    .collect::<Vec<_>>()
            },
        )
        .map_err(|error| {
            SimError::new("fighter_state_query_failed", error.to_string())
        })?;
    fighters.sort_by_key(|fighter| fighter.0);
    encoder.u64(fighters.len() as u64);
    for fighter in fighters {
        encoder.u64(fighter.0 .0);
        encoder.u64(fighter.1 as u64);
        encoder.f32(fighter.2)?;
        encoder.f32(fighter.3)?;
        encoder.f32(fighter.4)?;
        encoder.f32(fighter.5)?;
        encoder.bool(fighter.6);
        encoder.i64(i64::from(fighter.7));
        encoder.bool(fighter.8);
        encode_vec3(&mut encoder, fighter.9)?;
        encode_quat(&mut encoder, fighter.10)?;
        encode_vec2(&mut encoder, fighter.11)?;
        encoder.f32(fighter.12)?;
        encode_vec2(&mut encoder, fighter.13)?;
        encode_collision_type(&mut encoder, fighter.14);
    }

    let mut bullets = app
        .world_mut()
        .run_system_once(
            |query: Query<(
                &EntityId,
                &Bullet,
                &Transform,
                &LinearVelocity,
            )>| {
                query
                    .iter()
                    .map(|(id, bullet, transform, velocity)| {
                        (
                            id.0,
                            bullet.player_id,
                            bullet.remaining_lifetime,
                            transform.translation,
                            transform.rotation,
                            velocity.0,
                        )
                    })
                    .collect::<Vec<_>>()
            },
        )
        .map_err(|error| {
            SimError::new("bullet_state_query_failed", error.to_string())
        })?;
    bullets.sort_by_key(|bullet| bullet.0);
    encoder.u64(bullets.len() as u64);
    for bullet in bullets {
        encoder.u64(bullet.0 .0);
        encoder.u64(bullet.1 as u64);
        encoder.i64(i64::from(bullet.2));
        encode_vec3(&mut encoder, bullet.3)?;
        encode_quat(&mut encoder, bullet.4)?;
        encode_vec2(&mut encoder, bullet.5)?;
    }

    let mut asteroids = app
        .world_mut()
        .run_system_once(
            |query: Query<(
                &EntityId,
                &Asteroid,
                &Transform,
                &LinearVelocity,
            )>| {
                query
                    .iter()
                    .map(|(id, asteroid, transform, velocity)| {
                        (
                            id.0,
                            asteroid.health,
                            asteroid.radius,
                            transform.translation,
                            transform.rotation,
                            velocity.0,
                        )
                    })
                    .collect::<Vec<_>>()
            },
        )
        .map_err(|error| {
            SimError::new("asteroid_state_query_failed", error.to_string())
        })?;
    asteroids.sort_by_key(|asteroid| asteroid.0);
    encoder.u64(asteroids.len() as u64);
    for asteroid in asteroids {
        encoder.u64(asteroid.0 .0);
        encoder.f32(asteroid.1)?;
        encoder.f32(asteroid.2)?;
        encode_vec3(&mut encoder, asteroid.3)?;
        encode_quat(&mut encoder, asteroid.4)?;
        encode_vec2(&mut encoder, asteroid.5)?;
    }

    encode_physics_contacts(app, &mut encoder)?;

    let mut events = app.world().resource::<TickEvents>().0.clone();
    events.sort();
    encoder.u64(events.len() as u64);
    for event in &events {
        match event {
            StarfighterEvent::Fired { player_id } => {
                encoder.u8(1);
                encoder.u64(*player_id as u64);
            }
            StarfighterEvent::Hit {
                player_id,
                source,
                damage,
                shielded,
            } => {
                encoder.u8(2);
                encoder.u64(*player_id as u64);
                encoder.string(source);
                encoder.f32(*damage)?;
                encoder.bool(*shielded);
            }
            StarfighterEvent::Destroyed { player_id, source } => {
                encoder.u8(3);
                encoder.u64(*player_id as u64);
                encoder.string(source);
            }
            StarfighterEvent::MatchEnded { winner } => {
                encoder.u8(4);
                match winner {
                    Some(pid) => {
                        encoder.bool(true);
                        encoder.u64(*pid as u64);
                    }
                    None => {
                        encoder.bool(false);
                    }
                }
            }
        }
    }
    Ok(encoder.into_bytes())
}

fn encode_physics_contacts(
    app: &mut App,
    encoder: &mut CanonicalEncoder,
) -> Result<(), SimError> {
    let mut contacts = app
        .world_mut()
        .run_system_once(|collisions: Collisions, ids: Query<&EntityId>| {
            collisions
                .graph()
                .iter_active()
                .chain(collisions.graph().iter_sleeping())
                .map(|pair| {
                    let mut manifolds: Vec<_> = pair
                        .manifolds
                        .iter()
                        .map(|manifold| {
                            let mut points: Vec<_> = manifold
                                .points
                                .iter()
                                .map(|point| PhysicsContactPointState {
                                    anchor1: point.anchor1,
                                    anchor2: point.anchor2,
                                    point: point.point,
                                    penetration: point.penetration,
                                    normal_impulse: point.normal_impulse,
                                    normal_speed: point.normal_speed,
                                    warm_start_normal_impulse: point
                                        .warm_start_normal_impulse,
                                    warm_start_tangent_impulse: point
                                        .warm_start_tangent_impulse,
                                    feature1: point.feature_id1.0,
                                    feature2: point.feature_id2.0,
                                })
                                .collect();
                            points.sort_by_key(|point| {
                                (
                                    point.feature1,
                                    point.feature2,
                                    point.point.x.to_bits(),
                                    point.point.y.to_bits(),
                                )
                            });
                            PhysicsManifoldState {
                                normal: manifold.normal,
                                friction: manifold.friction,
                                restitution: manifold.restitution,
                                tangent_speed: manifold.tangent_speed,
                                points,
                            }
                        })
                        .collect();
                    manifolds.sort_by_key(|manifold| {
                        let feature_key = manifold
                            .points
                            .first()
                            .map(|point| (point.feature1, point.feature2))
                            .unwrap_or((u32::MAX, u32::MAX));
                        (
                            manifold.normal.x.to_bits(),
                            manifold.normal.y.to_bits(),
                            feature_key,
                        )
                    });
                    PhysicsContactState {
                        collider1: ids.get(pair.collider1).ok().map(|id| id.0),
                        collider2: ids.get(pair.collider2).ok().map(|id| id.0),
                        body1: pair.body1.and_then(|entity| {
                            ids.get(entity).ok().map(|id| id.0)
                        }),
                        body2: pair.body2.and_then(|entity| {
                            ids.get(entity).ok().map(|id| id.0)
                        }),
                        flags: pair.flags.bits(),
                        manifolds,
                    }
                })
                .collect::<Vec<_>>()
        })
        .map_err(|error| {
            SimError::new("physics_contact_query_failed", error.to_string())
        })?;
    contacts.sort_by_key(|contact| {
        (
            contact.collider1.map(|id| id.0).unwrap_or(u64::MAX),
            contact.collider2.map(|id| id.0).unwrap_or(u64::MAX),
        )
    });
    encoder.u64(contacts.len() as u64);
    for contact in contacts {
        let collider1 = contact.collider1.ok_or_else(|| {
            SimError::new(
                "missing_stable_entity_id",
                "physics contact collider1 has no logical ID",
            )
        })?;
        let collider2 = contact.collider2.ok_or_else(|| {
            SimError::new(
                "missing_stable_entity_id",
                "physics contact collider2 has no logical ID",
            )
        })?;
        encoder.u64(collider1.0);
        encoder.u64(collider2.0);
        encode_optional_entity_id(encoder, contact.body1);
        encode_optional_entity_id(encoder, contact.body2);
        encoder.u32(u32::from(contact.flags));
        encoder.u64(contact.manifolds.len() as u64);
        for manifold in contact.manifolds {
            encode_vec2(encoder, manifold.normal)?;
            encoder.f32(manifold.friction)?;
            encoder.f32(manifold.restitution)?;
            encoder.f32(manifold.tangent_speed)?;
            encoder.u64(manifold.points.len() as u64);
            for point in manifold.points {
                encode_vec2(encoder, point.anchor1)?;
                encode_vec2(encoder, point.anchor2)?;
                encode_vec2(encoder, point.point)?;
                encoder.f32(point.penetration)?;
                encoder.f32(point.normal_impulse)?;
                encoder.f32(point.normal_speed)?;
                encoder.f32(point.warm_start_normal_impulse)?;
                encoder.f32(point.warm_start_tangent_impulse)?;
                encoder.u32(point.feature1);
                encoder.u32(point.feature2);
            }
        }
    }
    Ok(())
}

fn encode_optional_entity_id(
    encoder: &mut CanonicalEncoder,
    value: Option<agentrix_sim_core::StableEntityId>,
) {
    match value {
        Some(value) => {
            encoder.bool(true);
            encoder.u64(value.0);
        }
        None => encoder.bool(false),
    }
}

fn encode_vec2(
    encoder: &mut CanonicalEncoder,
    value: Vec2,
) -> Result<(), SimError> {
    encoder.f32(value.x)?;
    encoder.f32(value.y)
}

fn encode_vec3(
    encoder: &mut CanonicalEncoder,
    value: Vec3,
) -> Result<(), SimError> {
    encoder.f32(value.x)?;
    encoder.f32(value.y)?;
    encoder.f32(value.z)
}

fn encode_quat(
    encoder: &mut CanonicalEncoder,
    value: Quat,
) -> Result<(), SimError> {
    encoder.f32(value.x)?;
    encoder.f32(value.y)?;
    encoder.f32(value.z)?;
    encoder.f32(value.w)
}

fn encode_collision_type(encoder: &mut CanonicalEncoder, value: CollisionType) {
    encoder.u8(match value {
        CollisionType::Fighter => 0,
        CollisionType::Bullet => 1,
        CollisionType::Asteroid => 2,
    });
}

#[cfg(test)]
mod tests {
    use super::*;
    use agentrix_engine_host::{EngineHost, GameRegistry};
    use agentrix_sim_core::{
        ActionStatus, DeterminismTier, ExecutionLimits, ExecutionSlotSpec,
        SchemaDigests, SlotAction, TickRate, PROTOCOL_VERSION, RNG_ALGORITHM,
    };
    use sha2::{Digest, Sha256};
    use std::sync::Arc;

    fn spec() -> SimulationSpec {
        let config = serde_json::to_value(StarfighterConfig {
            asteroid_count: 1,
            ..StarfighterConfig::default()
        })
        .unwrap();
        SimulationSpec {
            protocol_version: PROTOCOL_VERSION.to_string(),
            run_id: "run-test".to_string(),
            match_id: "match-test".to_string(),
            engine_version: "0.3.0-core.1".to_string(),
            engine_digest: "0000000000000000000000000000000000000000000000000000000000000000".to_string(),
            build_identity: "test-build".to_string(),
            target: "x86_64-unknown-linux-gnu".to_string(),
            game: starfighter_game_key(),
            schema_digests: SchemaDigests {
                action: ACTION_SCHEMA_DIGEST.to_string(),
                observation: OBSERVATION_SCHEMA_DIGEST.to_string(),
                public: PUBLIC_SCHEMA_DIGEST.to_string(),
                replay: "0000000000000000000000000000000000000000000000000000000000000000".to_string(),
            },
            config_digest: canonical_json_digest("starfighter-config", &config)
                .unwrap(),
            config,
            tick_rate: TickRate::new(60, 1).unwrap(),
            seed: 19,
            slots: vec![
                ExecutionSlotSpec {
                    slot_id: "alpha".to_string(),
                    artifact_digest: "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa".to_string(),
                },
                ExecutionSlotSpec {
                    slot_id: "beta".to_string(),
                    artifact_digest: "bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb".to_string(),
                },
            ],
            limits: ExecutionLimits {
                max_ticks: 8,
                max_players: 2,
                max_entities: 10_000,
                max_message_bytes: 1_048_576,
            },
            failure_policy_version: "fail-closed/1".to_string(),
            determinism_tier: DeterminismTier::SameArtifactSameTarget,
            rng_algorithm: RNG_ALGORITHM.to_string(),
        }
    }

    fn run() -> Vec<agentrix_sim_core::StateCommitments> {
        let mut registry = GameRegistry::new();
        registry.register(Arc::new(StarfighterGame::new())).unwrap();
        let mut host = EngineHost::new(registry);
        let mut output = vec![host.initialize(spec()).unwrap().commitments];
        for tick in 0..8 {
            let mut actions = ActionBatch::new();
            for slot in ["alpha", "beta"] {
                let action = json!({
                    "thrust": if tick % 2 == 0 { "FORWARD" } else { "OFF" },
                    "turn": if slot == "alpha" { "LEFT" } else { "RIGHT" },
                    "shoot": tick % 3 == 0,
                    "shield": false,
                });
                actions.insert(
                    slot.to_string(),
                    SlotAction {
                        status: ActionStatus::Valid,
                        requested: Some(action.clone()),
                        applied: Some(action),
                        policy_decision: "apply".to_string(),
                        error_code: None,
                    },
                );
            }
            output.push(host.advance(tick, &actions).unwrap().commitments);
        }
        output
    }

    #[test]
    fn state_zero_is_reset_only_and_each_advance_is_exactly_one_tick() {
        let mut registry = GameRegistry::new();
        registry.register(Arc::new(StarfighterGame::new())).unwrap();
        let mut host = EngineHost::new(registry);
        let initial = host.initialize(spec()).unwrap();
        assert_eq!(initial.tick, 0);
        assert!(initial.public_snapshot["bullets"]
            .as_array()
            .unwrap()
            .is_empty());
        let first = host.advance(0, &ActionBatch::new()).unwrap();
        assert_eq!(first.tick, 1);
    }

    #[test]
    fn fresh_process_equivalent_runs_have_identical_d1_chains() {
        let expected = run();
        for _ in 0..100 {
            assert_eq!(run(), expected);
        }
    }

    #[test]
    fn descriptor_digests_match_versioned_schema_bytes() {
        fn digest(bytes: &[u8]) -> String {
            Sha256::digest(bytes)
                .iter()
                .map(|byte| format!("{byte:02x}"))
                .collect()
        }
        let descriptor = StarfighterGame::new().descriptor;
        assert_eq!(
            descriptor.schemas.action,
            digest(include_bytes!(
                "../schemas/games/starfighter/0.3.0-core.1/action.schema.json"
            ))
        );
        assert_eq!(
            descriptor.schemas.observation,
            digest(include_bytes!(
                "../schemas/games/starfighter/0.3.0-core.1/observation.schema.json"
            ))
        );
        assert_eq!(
            descriptor.schemas.public,
            digest(include_bytes!(
                "../schemas/games/starfighter/0.3.0-core.1/public.schema.json"
            ))
        );
    }

    #[test]
    fn hidden_rule_and_rng_state_change_authoritative_encoding() {
        let simulation_spec = spec();
        let config: StarfighterConfig =
            serde_json::from_value(simulation_spec.config.clone()).unwrap();
        let settings = Settings {
            seed: simulation_spec.seed,
            tick_rate: simulation_spec.tick_rate,
            players: 2,
            asteroid_count: config.asteroid_count,
            radar_range: config.radar_range,
            max_entities: u64::from(simulation_spec.limits.max_entities),
            config,
            ..Settings::default()
        };
        let mut app = build_app(settings);
        app.finish();
        app.cleanup();
        app.insert_resource(bevy::time::TimeUpdateStrategy::ManualDuration(
            Duration::ZERO,
        ));
        app.update();

        let baseline =
            encode_authoritative_state(&mut app, 0, &simulation_spec).unwrap();
        app.world_mut()
            .run_system_once(|mut fighters: Query<&mut Fighter>| {
                let mut fighter = fighters.iter_mut().next().unwrap();
                fighter.energy -= 1.0;
            })
            .unwrap();
        let changed_hidden_rule =
            encode_authoritative_state(&mut app, 0, &simulation_spec).unwrap();
        assert_ne!(baseline, changed_hidden_rule);

        app.world_mut().resource_mut::<RngState>().next_u64();
        let changed_rng =
            encode_authoritative_state(&mut app, 0, &simulation_spec).unwrap();
        assert_ne!(changed_hidden_rule, changed_rng);
    }
}
