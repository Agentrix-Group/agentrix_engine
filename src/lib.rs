//! Núcleo físico mínimo de starfighter sobre Avian2D + Bevy headless.
//!
//! Estado de la migración a Agentrix:
//! - Fase 0: motor sobre Avian2D, sin `entity-gym-rs`/`pyo3`, CCD real.
//! - Fase 1: modelo de nave único (HP/energía/escudo), condición de fin
//!   de partida simétrica.
//! - Fase 2: contratos (`games/starfighter/manifest.yaml`,
//!   `contracts/action.schema.json`).
//! - Fase 3: percepción aislada por slot (`Perception`, `RivalContact`
//!   nunca expone energía/cooldown de un rival).
//! - Fase 4 (`protocol`/`manifest`/`runner`): runner CLI real hablando
//!   el protocolo de agente (ATD-007) por stdin/stdout con procesos de
//!   bot externos.
//!
//! Valores de balance (HP, daño, costos de energía) son **placeholders**
//! documentados en su lugar de definición: la afinación real es trabajo
//! de un concurso real, no de esta fase. Lo que sí es un requisito desde
//! la Fase 1 es que sean **iguales para todos los slots** — no hay
//! ninguna rama por `player_id` en todo este archivo.

use avian2d::prelude::*;
use bevy::prelude::*;
use rand::rngs::SmallRng;
use rand::{Rng, SeedableRng};
use serde::Serialize;
use std::ops::{Deref, DerefMut};

pub mod manifest;
pub mod protocol;
pub mod replay;
pub mod runner;

/// Inicializa `tracing` para escribir a **stderr**, nunca a stdout.
/// stdout queda reservado para el protocolo de agente (ATD-007, Fase 4).
pub fn init_tracing() {
    let _ = tracing_subscriber::fmt()
        .with_writer(std::io::stderr)
        .with_env_filter(tracing_subscriber::EnvFilter::from_default_env())
        .try_init();
}

#[derive(Clone, Resource)]
pub struct Settings {
    pub seed: u64,
    pub tick_hz: f64,
    pub players: u32,
    pub asteroid_count: u32,
    pub continuous_collision_detection: bool,
    /// Distancia máxima a la que un slot puede ver naves y balas
    /// rivales (Decisión 2.3). Hoy está sincronizado a mano con
    /// `games/starfighter/manifest.yaml` (`settings.radar_range`) --
    /// recién en Fase 4 el runner va a leer el manifest real y construir
    /// `Settings` desde ahí, no antes.
    pub radar_range: f32,
}

impl Default for Settings {
    fn default() -> Self {
        Settings {
            seed: 0,
            tick_hz: 60.0,
            players: 2,
            asteroid_count: 5,
            continuous_collision_detection: true,
            radar_range: 800.0,
        }
    }
}

const ARENA_HALF_WIDTH: f32 = 1000.0;
const ARENA_HALF_HEIGHT: f32 = 500.0;

/// HP inicial/máximo de cualquier nave. Placeholder de balance (Fase 1);
/// el valor concreto lo fija un concurso real, no este archivo.
const MAX_HEALTH: f32 = 100.0;
/// Energía inicial/máxima de cualquier nave.
const MAX_ENERGY: f32 = 100.0;
/// Daño de una bala nave-a-nave antes de reducción por escudo.
const BULLET_DAMAGE: f32 = 25.0;
/// Daño letal en un solo golpe contra un asteroide: esta fase se enfoca
/// en la simetría nave-vs-nave, no en el balance del asteroide, así que
/// se mantiene el comportamiento simple (un golpe con un asteroide
/// destruye la nave) heredado de la Fase 0.
const ASTEROID_DAMAGE: f32 = MAX_HEALTH;
/// Costo de energía por disparo. Si la nave no tiene suficiente, no
/// dispara aunque el cooldown de la bala ya esté listo.
const SHOOT_ENERGY_COST: f32 = 15.0;
/// Costo de energía por tick mientras el escudo está levantado. A 60
/// ticks/s, mantenerlo levantado sin pausa agota el pool completo en
/// ~1.7s — el escudo cuesta sostenerlo, no es gratis como en el diseño
/// original (que lo daba gratis y solo al jugador 0).
const SHIELD_ENERGY_COST_PER_TICK: f32 = 1.0;
/// Fracción de daño que absorbe un escudo activo (no bloqueo total: el
/// diseño original ya reducía en vez de anular el daño con escudo).
const SHIELD_DAMAGE_REDUCTION: f32 = 0.7;
/// Regeneración pasiva de energía por tick cuando no se está gastando en
/// disparo o escudo ese mismo tick.
const ENERGY_REGEN_PER_TICK: f32 = 0.5;

#[derive(Component)]
pub struct Fighter {
    pub health: f32,
    pub max_health: f32,
    pub energy: f32,
    pub max_energy: f32,
    pub shield_active: bool,
    pub max_velocity: f32,
    pub acceleration: f32,
    pub deceleration: f32,
    pub drag_exp: f32,
    pub drag_coef: f32,
    pub turn_acceleration: f32,
    pub max_turn_speed: f32,
    pub bullet_speed: f32,
    pub bullet_lifetime: u32,
    pub bullet_cooldown: u32,
    pub remaining_bullet_cooldown: i32,
    pub is_turning: bool,
    pub player_id: usize,
}

impl Default for Fighter {
    /// Un único set de estadísticas para todas las naves. No hay
    /// ninguna rama `player_id == 0`: el balance final es trabajo de un
    /// concurso real, esto es solo un placeholder simétrico.
    fn default() -> Self {
        Fighter {
            health: MAX_HEALTH,
            max_health: MAX_HEALTH,
            energy: MAX_ENERGY,
            max_energy: MAX_ENERGY,
            shield_active: false,
            max_velocity: 500.0,
            acceleration: 800_000.0,
            deceleration: 800_000.0,
            drag_exp: 1.5,
            drag_coef: 0.02,
            turn_acceleration: 0.5,
            max_turn_speed: 4.0,
            bullet_speed: 1500.0,
            bullet_lifetime: 90,
            bullet_cooldown: 40,
            remaining_bullet_cooldown: 0,
            is_turning: false,
            player_id: 0,
        }
    }
}

#[derive(Component)]
pub struct Bullet {
    pub remaining_lifetime: i32,
    pub player_id: usize,
}

#[derive(Component)]
pub struct Asteroid {
    pub health: f32,
    pub radius: f32,
}

#[derive(Component, Debug, Clone, Copy, PartialEq, Eq)]
pub enum CollisionType {
    Fighter,
    Bullet,
    Asteroid,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Thrust {
    On,
    Off,
    Stop,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Turn {
    Left,
    Right,
    None,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Shoot {
    On,
    Off,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Shield {
    On,
    Off,
}

#[derive(Clone, Copy, Debug)]
pub struct FighterAction {
    pub thrust: Thrust,
    pub turn: Turn,
    pub shoot: Shoot,
    pub shield: Shield,
}

#[derive(Message, Clone, Copy)]
pub struct FighterActionMessage {
    pub action: FighterAction,
    pub entity: Entity,
}

/// Evento público: una nave fue destruida. Consumido por
/// `check_match_end` para fijar `MatchResult`, y disponible como hook de
/// puntuación para fases posteriores.
#[derive(Message, Clone, Copy)]
pub struct FighterDestroyed {
    pub entity: Entity,
    pub player_id: usize,
}

/// Condición de fin de partida: simétrica, no distingue slots. Se fija
/// una sola vez, la primera vez que queda una nave viva o cero (empate
/// por destrucción mutua). Un límite externo de ticks sin que esto se
/// fije también cuenta como empate, pero eso lo decide quien corre la
/// partida (el motor no fuerza un límite de ticks acá).
#[derive(Resource, Default, Clone, Copy, Debug)]
pub struct MatchResult {
    pub finished: bool,
    /// `player_id` del único sobreviviente. `None` si terminó por
    /// destrucción mutua (cero sobrevivientes) o si no terminó todavía.
    pub winner: Option<usize>,
}

/// Narración de lo que pasó en el tick actual -- Fase 5 (ATD-011), para
/// el campo `events` de cada frame del replay. Quien orquesta la partida
/// (`runner::run_match`) es responsable de vaciarlo antes de cada tick y
/// de leerlo después; el motor solo empuja strings acá, nunca lo limpia
/// solo. No es un evento por cada micro-cambio de física -- solo lo que
/// un espectador humano querría ver narrado (disparo, impacto,
/// destrucción, fin de partida), mismo criterio que "eventos
/// destacados" de `06_ux_y_lenguaje_visual.md`.
#[derive(Resource, Default, Clone, Debug)]
pub struct TickEvents(pub Vec<String>);

#[derive(Resource)]
pub struct RngState(pub SmallRng);

impl Deref for RngState {
    type Target = SmallRng;
    fn deref(&self) -> &SmallRng {
        &self.0
    }
}

impl DerefMut for RngState {
    fn deref_mut(&mut self) -> &mut SmallRng {
        &mut self.0
    }
}

/// Construye la app headless. Sin `DefaultPlugins`, sin ventana, sin
/// assets: el renderer de Agentrix reconstruye la vista desde el replay,
/// no desde este proceso (ATD-011).
pub fn build_app(settings: Settings) -> App {
    let mut app = App::new();
    app.add_plugins(MinimalPlugins)
        .add_plugins(
            PhysicsPlugins::default().with_length_unit(50.0),
        )
        .insert_resource(Time::<Fixed>::from_hz(settings.tick_hz))
        .insert_resource(Gravity(Vec2::ZERO))
        .insert_resource(RngState(SmallRng::seed_from_u64(settings.seed)))
        .init_resource::<MatchResult>()
        .init_resource::<TickEvents>()
        .add_message::<FighterActionMessage>()
        .add_message::<FighterDestroyed>()
        .insert_resource(settings)
        .add_systems(Startup, setup)
        .add_systems(
            FixedUpdate,
            (
                check_boundary_collision,
                spawn_asteroids,
                fighter_actions,
                cooldowns,
                expire_bullets,
                detect_collisions,
                check_match_end,
            )
                .chain(),
        );
    app
}

fn setup(settings: Res<Settings>, mut cmd: Commands, mut rng: ResMut<RngState>) {
    for i in 0..settings.players as usize {
        let angle = i as f32 / settings.players.max(1) as f32
            * std::f32::consts::PI
            * 2.0;
        let position =
            Vec2::new(angle.cos(), angle.sin()) * (ARENA_HALF_WIDTH * 0.6);
        spawn_fighter(&mut cmd, i, position);
    }
    let _ = &mut rng; // reservado para spawns futuros no deterministas por posición fija
}

pub fn spawn_fighter(cmd: &mut Commands, player_id: usize, position: Vec2) -> Entity {
    cmd.spawn((
        Fighter {
            player_id,
            ..default()
        },
        RigidBody::Dynamic,
        Collider::convex_hull(vec![
            Vec2::new(0.0, 25.0),
            Vec2::new(-15.0, -15.0),
            Vec2::new(15.0, -15.0),
        ])
        .expect("triangle is a valid convex hull"),
        CollisionEventsEnabled,
        Transform::from_translation(position.extend(0.0)),
        LinearVelocity::ZERO,
        AngularVelocity::ZERO,
        ConstantForce::default(),
        CollisionType::Fighter,
    ))
    .id()
}

#[allow(clippy::too_many_arguments)]
pub fn spawn_bullet(
    cmd: &mut Commands,
    settings: &Settings,
    position: Vec2,
    velocity: Vec2,
    lifetime: u32,
    player_id: usize,
) {
    let mut entity = cmd.spawn((
        Bullet {
            remaining_lifetime: lifetime as i32,
            player_id,
        },
        RigidBody::Dynamic,
        Collider::circle(3.0),
        LockedAxes::ROTATION_LOCKED,
        CollisionType::Bullet,
        CollisionEventsEnabled,
        Transform::from_translation(position.extend(0.0)),
        LinearVelocity(velocity),
        AngularVelocity::ZERO,
    ));
    if settings.continuous_collision_detection {
        entity.insert(SweptCcd::default());
    }
}

fn spawn_asteroids(
    settings: Res<Settings>,
    mut cmd: Commands,
    asteroids: Query<(Entity, &Transform), With<Asteroid>>,
    mut rng: ResMut<RngState>,
) {
    let mut count = 0;
    for (entity, transform) in &asteroids {
        let p = transform.translation;
        if p.x.abs() > ARENA_HALF_WIDTH * 1.5 || p.y.abs() > ARENA_HALF_HEIGHT * 1.5
        {
            cmd.entity(entity).despawn();
        } else {
            count += 1;
        }
    }
    while count < settings.asteroid_count {
        let speed = rng.gen_range(50.0..300.0);
        let direction = rng.gen_range(0.0..std::f32::consts::PI * 2.0);
        let spawn_angle = rng.gen_range(0.0..std::f32::consts::PI * 2.0);
        let radius = (rng.gen_range(20.0..60.0_f32) * rng.gen_range(20.0..60.0_f32))
            .sqrt();
        cmd.spawn((
            Asteroid {
                health: 2.0,
                radius,
            },
            RigidBody::Dynamic,
            LockedAxes::ROTATION_LOCKED,
            Collider::circle(radius),
            CollisionEventsEnabled,
            Transform::from_translation(Vec3::new(
                ARENA_HALF_WIDTH * 1.5 * spawn_angle.cos(),
                ARENA_HALF_HEIGHT * 1.5 * spawn_angle.sin(),
                0.0,
            )),
            LinearVelocity(speed * Vec2::new(direction.cos(), direction.sin())),
            CollisionType::Asteroid,
        ));
        count += 1;
    }
}

fn check_boundary_collision(
    mut fighters: Query<(&mut LinearVelocity, &mut AngularVelocity, &Transform, &Fighter)>,
) {
    for (mut velocity, mut angular, transform, fighter) in &mut fighters {
        let x = transform.translation.x;
        let y = transform.translation.y;
        if x > ARENA_HALF_WIDTH {
            velocity.x = -velocity.x.abs();
        } else if x < -ARENA_HALF_WIDTH {
            velocity.x = velocity.x.abs();
        }
        if y > ARENA_HALF_HEIGHT {
            velocity.y = -velocity.y.abs();
        } else if y < -ARENA_HALF_HEIGHT {
            velocity.y = velocity.y.abs();
        }
        if !fighter.is_turning && angular.0 != 0.0 {
            angular.0 -= angular.0.signum() * fighter.turn_acceleration;
        }
        let speed = velocity.0.length();
        if speed > fighter.max_velocity {
            velocity.0 = velocity.0.normalize() * fighter.max_velocity;
        }
    }
}

pub fn fighter_actions(
    mut actions: MessageReader<FighterActionMessage>,
    mut fighters: Query<(
        &mut Fighter,
        &Transform,
        &mut LinearVelocity,
        &mut AngularVelocity,
        &mut ConstantForce,
    )>,
    mut cmd: Commands,
    settings: Res<Settings>,
    mut events: ResMut<TickEvents>,
) {
    for FighterActionMessage { action, entity } in actions.read() {
        let Ok((mut fighter, transform, mut vel, mut angular, mut force)) =
            fighters.get_mut(*entity)
        else {
            continue;
        };
        force.0 = Vec2::ZERO;
        fighter.is_turning = action.turn != Turn::None;

        let facing = facing_direction(transform);

        angular.0 += match action.turn {
            Turn::Left => fighter.turn_acceleration,
            Turn::Right => -fighter.turn_acceleration,
            Turn::None => 0.0,
        };
        angular.0 = angular.0.clamp(-fighter.max_turn_speed, fighter.max_turn_speed);

        let speed = vel.0.length();
        match action.thrust {
            Thrust::On => {
                force.0 = facing * fighter.acceleration;
            }
            Thrust::Off => {
                vel.0 *= 1.0
                    - fighter.drag_coef * (speed / fighter.max_velocity).powf(fighter.drag_exp);
            }
            Thrust::Stop => {
                if speed < 1.0 {
                    vel.0 = Vec2::ZERO;
                } else {
                    force.0 = -vel.0.normalize() * fighter.deceleration;
                }
            }
        }

        // El escudo es un estado persistente (se mantiene entre ticks
        // hasta que una acción diga lo contrario), no un impulso — el
        // costo por tick de mantenerlo se cobra en `cooldowns`, que
        // corre para todas las naves sin depender de recibir un mensaje
        // de acción ese tick.
        fighter.shield_active = matches!(action.shield, Shield::On);

        if let Shoot::On = action.shoot {
            if fighter.remaining_bullet_cooldown <= 0 && fighter.energy >= SHOOT_ENERGY_COST {
                spawn_bullet(
                    &mut cmd,
                    &settings,
                    transform.translation.truncate() + facing * 24.0,
                    vel.0 + facing * fighter.bullet_speed,
                    fighter.bullet_lifetime,
                    fighter.player_id,
                );
                fighter.remaining_bullet_cooldown = fighter.bullet_cooldown as i32;
                fighter.energy -= SHOOT_ENERGY_COST;
                events.0.push(format!("P{} fired a bullet", fighter.player_id));
            }
        }
    }
}

fn facing_direction(transform: &Transform) -> Vec2 {
    let (_, angle) = transform.rotation.to_axis_angle();
    let angle = if transform.rotation.z < 0.0 { -angle } else { angle }
        + std::f32::consts::PI / 2.0;
    Vec2::new(angle.cos(), angle.sin())
}

/// Corre para todas las naves cada tick, tengan o no un mensaje de
/// acción ese tick: cooldown de disparo, costo de sostener el escudo, y
/// regeneración pasiva de energía cuando no se está gastando.
fn cooldowns(mut fighters: Query<&mut Fighter>) {
    for mut fighter in &mut fighters {
        fighter.remaining_bullet_cooldown -= 1;

        if fighter.shield_active {
            if fighter.energy >= SHIELD_ENERGY_COST_PER_TICK {
                fighter.energy -= SHIELD_ENERGY_COST_PER_TICK;
            } else {
                // Sin energía para sostenerlo: se apaga solo, simétrico
                // para cualquier slot.
                fighter.energy = 0.0;
                fighter.shield_active = false;
            }
        } else {
            fighter.energy = (fighter.energy + ENERGY_REGEN_PER_TICK).min(fighter.max_energy);
        }
    }
}

fn expire_bullets(mut cmd: Commands, mut bullets: Query<(Entity, &mut Bullet)>) {
    for (entity, mut bullet) in &mut bullets {
        bullet.remaining_lifetime -= 1;
        if bullet.remaining_lifetime <= 0 {
            cmd.entity(entity).despawn();
        }
    }
}

#[allow(clippy::too_many_arguments)]
fn detect_collisions(
    mut cmd: Commands,
    mut collisions: MessageReader<CollisionStart>,
    collision_type: Query<&CollisionType>,
    mut fighters: Query<&mut Fighter>,
    bullets: Query<&Bullet>,
    mut asteroids: Query<&mut Asteroid>,
    mut destroyed: MessageWriter<FighterDestroyed>,
    mut events: ResMut<TickEvents>,
) {
    for event in collisions.read() {
        let (a, b) = (event.collider1, event.collider2);
        let (Ok(ta), Ok(tb)) = (collision_type.get(a), collision_type.get(b)) else {
            continue;
        };
        match (ta, tb) {
            (CollisionType::Fighter, CollisionType::Asteroid)
            | (CollisionType::Asteroid, CollisionType::Fighter) => {
                let (fighter_entity, asteroid_entity) = if *ta == CollisionType::Fighter {
                    (a, b)
                } else {
                    (b, a)
                };
                take_hit(
                    &mut cmd,
                    &mut fighters,
                    fighter_entity,
                    ASTEROID_DAMAGE,
                    "an asteroid",
                    &mut destroyed,
                    &mut events,
                );
                cmd.entity(asteroid_entity).despawn();
            }
            (CollisionType::Bullet, CollisionType::Asteroid)
            | (CollisionType::Asteroid, CollisionType::Bullet) => {
                let (bullet_entity, asteroid_entity) = if *ta == CollisionType::Bullet {
                    (a, b)
                } else {
                    (b, a)
                };
                cmd.entity(bullet_entity).despawn();
                if let Ok(mut asteroid) = asteroids.get_mut(asteroid_entity) {
                    asteroid.health -= 1.0;
                    if asteroid.health <= 0.0 {
                        cmd.entity(asteroid_entity).despawn();
                    }
                }
            }
            (CollisionType::Fighter, CollisionType::Bullet)
            | (CollisionType::Bullet, CollisionType::Fighter) => {
                let (fighter_entity, bullet_entity) = if *ta == CollisionType::Fighter {
                    (a, b)
                } else {
                    (b, a)
                };
                let shooter = bullets.get(bullet_entity).ok().map(|b| b.player_id);
                let same_owner = fighters
                    .get(fighter_entity)
                    .ok()
                    .zip(shooter)
                    .map(|(fighter, shooter_id)| fighter.player_id == shooter_id)
                    .unwrap_or(true);
                if !same_owner {
                    let source = shooter
                        .map(|id| format!("P{id}'s bullet"))
                        .unwrap_or_else(|| "a bullet".to_string());
                    take_hit(
                        &mut cmd,
                        &mut fighters,
                        fighter_entity,
                        BULLET_DAMAGE,
                        &source,
                        &mut destroyed,
                        &mut events,
                    );
                    cmd.entity(bullet_entity).despawn();
                }
            }
            (CollisionType::Bullet, CollisionType::Bullet) => {
                cmd.entity(a).despawn();
                cmd.entity(b).despawn();
            }
            _ => {}
        }
    }
}

/// Resta `damage` a la nave (reducido si tiene el escudo activo) y la
/// destruye si su HP llega a cero. Mismo camino para cualquier
/// `player_id` — no hay ninguna rama especial por slot acá.
#[allow(clippy::too_many_arguments)]
fn take_hit(
    cmd: &mut Commands,
    fighters: &mut Query<&mut Fighter>,
    entity: Entity,
    damage: f32,
    source: &str,
    destroyed: &mut MessageWriter<FighterDestroyed>,
    events: &mut TickEvents,
) {
    let Ok(mut fighter) = fighters.get_mut(entity) else {
        return;
    };
    let effective_damage = if fighter.shield_active {
        damage * (1.0 - SHIELD_DAMAGE_REDUCTION)
    } else {
        damage
    };
    fighter.health -= effective_damage;
    let shielded_note = if fighter.shield_active { " (shielded)" } else { "" };
    events.0.push(format!(
        "P{} hit by {source} for {effective_damage:.1} dmg{shielded_note}",
        fighter.player_id
    ));
    if fighter.health <= 0.0 {
        events.0.push(format!("P{} destroyed", fighter.player_id));
        destroyed.write(FighterDestroyed {
            entity,
            player_id: fighter.player_id,
        });
        cmd.entity(entity).despawn();
    }
}

/// Fija `MatchResult` la primera vez que queda una nave viva (gana ese
/// slot) o cero (empate por destrucción mutua). No decide nada por
/// límite de ticks — eso es responsabilidad de quien corre la partida.
fn check_match_end(
    mut result: ResMut<MatchResult>,
    fighters: Query<&Fighter>,
    mut events: ResMut<TickEvents>,
) {
    if result.finished {
        return;
    }
    let alive: Vec<usize> = fighters.iter().map(|f| f.player_id).collect();
    if alive.len() <= 1 {
        result.finished = true;
        result.winner = alive.first().copied();
        match result.winner {
            Some(pid) => events.0.push(format!("Match ended: P{pid} wins")),
            None => events.0.push("Match ended in a draw (mutual destruction)".to_string()),
        }
    }
}

// ---------------------------------------------------------------------
// Percepción aislada por slot (Fase 3).
//
// No existe ningún sistema de percepción heredado de fases anteriores:
// el `ai()`/`Obs` original de `entity-gym-rs` se borró completo en la
// Fase 0 junto con toda la maquinaria de RL. Esto se construye desde
// cero, con la garantía real de RF-042/CA-013: un agente nunca puede
// leer energía ni cooldown de un rival, porque `RivalContact` no tiene
// esos campos -- no es que se serialicen ocultos o en null, el tipo
// directamente no los declara. Serialization con `serde` porque el
// contrato de percepción es JSON (mismo criterio que
// `games/starfighter/contracts/action.schema.json` de la Fase 2).
//
// Decisión de diseño: las posiciones/velocidades de rivales y balas son
// **relativas a la nave propia** (`relative_position`/`relative_velocity`
// = objetivo - propio), no absolutas en el mundo. Es la semántica natural
// de un contacto de radar (rumbo y distancia desde uno mismo, no
// coordenadas del mapa), evita que un agente tenga que restar su propia
// posición en cada tick para algo tan básico como "¿hacia dónde está el
// rival", y es lo que ya hacía el featurizer original de
// `entity-gym-rs` (`reldx`/`reldy`) antes de que se borrara en Fase 0 --
// no es una idea nueva, es preservar la única parte de ese diseño que
// tenía sentido, ahora sin filtrar información interna del rival.
// ---------------------------------------------------------------------

/// Vector 2D serializable para el contrato de percepción. `bevy::Vec2`
/// (reexport de `glam::Vec2`) no tiene `Serialize` habilitado en este
/// crate (compilamos bevy con `default-features = false` y no activamos
/// el feature `serde` de glam) -- este tipo local, explícito en el JSON
/// de salida, es más claro para un contrato externo que depender de la
/// representación interna de una librería de matemáticas de Rust.
#[derive(Debug, Clone, Copy, Serialize)]
pub struct Vec2Data {
    pub x: f32,
    pub y: f32,
}

impl From<Vec2> for Vec2Data {
    fn from(v: Vec2) -> Self {
        Vec2Data { x: v.x, y: v.y }
    }
}

/// Estado propio **completo** -- todo lo que la propia nave puede ver de
/// sí misma, incluida información interna (energía, cooldown) que nunca
/// se expone de un rival.
#[derive(Debug, Clone, Serialize)]
pub struct SelfState {
    pub position: Vec2Data,
    pub velocity: Vec2Data,
    pub facing: Vec2Data,
    pub health: f32,
    pub energy: f32,
    pub shield_active: bool,
    pub remaining_bullet_cooldown: i32,
}

/// Lo que un slot puede ver de una nave rival. **No tiene** `energy` ni
/// `remaining_bullet_cooldown` -- a diferencia de `SelfState`, no es que
/// esos campos vengan en `null`, el tipo no los declara en absoluto.
#[derive(Debug, Clone, Serialize)]
pub struct RivalContact {
    pub player_id: usize,
    pub relative_position: Vec2Data,
    pub relative_velocity: Vec2Data,
    pub facing: Vec2Data,
    pub health: f32,
    pub shield_active: bool,
}

#[derive(Debug, Clone, Serialize)]
pub struct BulletContact {
    pub player_id: usize,
    pub relative_position: Vec2Data,
    pub relative_velocity: Vec2Data,
}

#[derive(Debug, Clone, Serialize)]
pub struct Perception {
    pub tick: u64,
    pub player_id: usize,
    pub myself: SelfState,
    pub rivals: Vec<RivalContact>,
    pub bullets: Vec<BulletContact>,
}

/// Datos crudos de una nave leídos del ECS, antes de decidir qué es
/// visible para quién. Separar esto de `build_perception` deja la lógica
/// de "qué ve un slot" pura y testeable sin levantar una `App` de Bevy.
#[derive(Debug, Clone, Copy)]
pub struct FighterSnapshot {
    pub player_id: usize,
    pub position: Vec2,
    pub velocity: Vec2,
    pub facing: Vec2,
    pub health: f32,
    pub energy: f32,
    pub shield_active: bool,
    pub remaining_bullet_cooldown: i32,
}

#[derive(Debug, Clone, Copy)]
pub struct BulletSnapshot {
    pub player_id: usize,
    pub position: Vec2,
    pub velocity: Vec2,
}

pub fn collect_fighter_snapshots(
    fighters: &Query<(&Fighter, &Transform, &LinearVelocity)>,
) -> Vec<FighterSnapshot> {
    fighters
        .iter()
        .map(|(fighter, transform, velocity)| FighterSnapshot {
            player_id: fighter.player_id,
            position: transform.translation.truncate(),
            velocity: velocity.0,
            facing: facing_direction(transform),
            health: fighter.health,
            energy: fighter.energy,
            shield_active: fighter.shield_active,
            remaining_bullet_cooldown: fighter.remaining_bullet_cooldown,
        })
        .collect()
}

pub fn collect_bullet_snapshots(
    bullets: &Query<(&Bullet, &Transform, &LinearVelocity)>,
) -> Vec<BulletSnapshot> {
    bullets
        .iter()
        .map(|(bullet, transform, velocity)| BulletSnapshot {
            player_id: bullet.player_id,
            position: transform.translation.truncate(),
            velocity: velocity.0,
        })
        .collect()
}

/// Arma la percepción de un slot a partir de snapshots ya leídos del
/// ECS: su propio estado completo, más naves y balas rivales dentro de
/// `radar_range`. Fuera de rango un contacto directamente no aparece en
/// el vector -- no se serializa vacío ni en null. `None` si `player_id`
/// no corresponde a ninguna nave viva (una nave destruida no percibe
/// nada; quien corre la partida decide qué hacer con eso).
pub fn build_perception(
    tick: u64,
    player_id: usize,
    radar_range: f32,
    fighters: &[FighterSnapshot],
    bullets: &[BulletSnapshot],
) -> Option<Perception> {
    let me = fighters.iter().find(|f| f.player_id == player_id)?;

    let myself = SelfState {
        position: me.position.into(),
        velocity: me.velocity.into(),
        facing: me.facing.into(),
        health: me.health,
        energy: me.energy,
        shield_active: me.shield_active,
        remaining_bullet_cooldown: me.remaining_bullet_cooldown,
    };

    let rivals = fighters
        .iter()
        .filter(|f| f.player_id != player_id)
        .filter(|f| f.position.distance(me.position) <= radar_range)
        .map(|f| RivalContact {
            player_id: f.player_id,
            relative_position: (f.position - me.position).into(),
            relative_velocity: (f.velocity - me.velocity).into(),
            facing: f.facing.into(),
            health: f.health,
            shield_active: f.shield_active,
        })
        .collect();

    let bullets = bullets
        .iter()
        .filter(|b| b.position.distance(me.position) <= radar_range)
        .map(|b| BulletContact {
            player_id: b.player_id,
            relative_position: (b.position - me.position).into(),
            relative_velocity: (b.velocity - me.velocity).into(),
        })
        .collect();

    Some(Perception {
        tick,
        player_id,
        myself,
        rivals,
        bullets,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use bevy::ecs::system::RunSystemOnce;
    use std::time::Duration;

    /// Criterio de aceptación de la Fase 0: una bala a una velocidad
    /// realista de juego (3000 u/s, por encima del `bullet_speed`
    /// original de 1500-2500) no atraviesa una nave sin generar colisión.
    ///
    /// Nota de hallazgo: a velocidades extremas (~80 000 u/s, ~40x el
    /// ancho de la nave) tanto la detección discreta/especulativa de
    /// Avian2D por defecto como `SweptCcd` fallan en este setup — no es
    /// el rango realista de este juego (el original usaba 1500-2500),
    /// así que se documenta como hallazgo de la Fase 0 y no se persigue
    /// más en esta fase (RD-009). A la velocidad de este test, la
    /// detección especulativa por defecto de Avian2D ya es suficiente
    /// por sí sola; `SweptCcd` queda cableado vía
    /// `continuous_collision_detection` para cuando haga falta.
    #[test]
    fn fast_bullet_does_not_tunnel_through_fighter() {
        let settings = Settings {
            asteroid_count: 0,
            players: 0,
            continuous_collision_detection: true,
            ..default()
        };
        let mut app = build_app(settings);
        // Sin esto, `app.update()` en un loop apretado no acumula tiempo
        // real y `FixedUpdate` (donde corre la física) nunca se dispara.
        app.insert_resource(bevy::time::TimeUpdateStrategy::ManualDuration(
            Duration::from_secs_f64(1.0 / 60.0),
        ));
        // `App::run()` llama a esto antes de entrar al loop; como acá
        // llamamos `update()` a mano (sin `run()`), hay que hacerlo
        // explícito para que los `finish()`/`cleanup()` de los plugins
        // corran (p.ej. el registro de diagnósticos internos de avian2d).
        app.finish();
        app.cleanup();
        app.update(); // Startup

        let target = spawn_fighter(&mut app.world_mut().commands(), 0, Vec2::new(500.0, 0.0));
        app.world_mut().flush();

        // Bala a 3000 u/s (el bullet_speed de referencia del original
        // era 1500-2500): a 60 Hz avanza 50 u/tick, más que el radio del
        // collider de bala (3 u) y comparable al ancho de la nave (~30 u).
        let bullet_settings = app.world().resource::<Settings>().clone();
        spawn_bullet(
            &mut app.world_mut().commands(),
            &bullet_settings,
            Vec2::new(0.0, 2.0),
            Vec2::new(3000.0, 0.0),
            600,
            1,
        );
        app.world_mut().flush();

        let mut collided = false;
        for _ in 0..30 {
            app.update();
            if app
                .world()
                .get_resource::<Messages<CollisionStart>>()
                .map(|m| !m.is_empty())
                .unwrap_or(false)
            {
                collided = true;
                break;
            }
            // Si el objetivo ya fue despawneado por el impacto, también cuenta.
            if app.world().get_entity(target).is_err() {
                collided = true;
                break;
            }
        }
        assert!(
            collided,
            "la bala debería haber colisionado con la nave (CCD activo)"
        );
    }

    /// Criterio de aceptación de la Fase 1: en 1v1 con acciones
    /// aleatorias por tick (sin ninguna estrategia), la tasa de victoria
    /// del slot 0 sobre las partidas resueltas cae dentro de un margen
    /// simétrico alrededor de 50%. Si el motor todavía favoreciera un
    /// slot (como el original, que le daba a P0 5x aceleración, escudo
    /// gratis y muerte súbita para el resto), esto se vería como una
    /// tasa de victoria muy alejada de 50%.
    ///
    /// El margen es un intervalo de 3 desvíos estándar bajo la hipótesis
    /// nula de una moneda justa (p=0.5): `3 * sqrt(0.25 / n_resueltas)`.
    /// Con el tamaño de muestra real de esta corrida (impreso en el
    /// test) eso da un margen de un dígito de puntos porcentuales —
    /// suficientemente ajustado para detectar un sesgo estructural como
    /// el del diseño original, pero sin ser tan estricto que el ruido de
    /// una sola corrida lo haga fallar por casualidad.
    ///
    /// Nota de calibración: con `shoot` al 50% de probabilidad por tick,
    /// una corrida de 300 partidas midió 262 empates y solo 38 decididas
    /// (12.7%) — apuntar al azar en una arena de 2000x1000 rara vez
    /// conecta un disparo. Se sube la probabilidad de disparo al 85% en
    /// esta política aleatoria (solo afecta a este test, no al balance
    /// del motor) para que suficientes partidas se resuelvan sin
    /// necesitar un `MAX_TICKS` desproporcionado, y se sube `N_MATCHES`
    /// para compensar el `MIN_DECIDED` real.
    ///
    /// **Marcado `#[ignore]`**: 1500 partidas de hasta 3600 ticks tardan
    /// ~3 minutos en `--release` (medido) y sobre 16 minutos sin
    /// terminar en modo debug (medido, se abortó) — demasiado lento para
    /// que corra por defecto en cada `cargo test`. Correrlo explícito
    /// con `cargo test --release --lib symmetric_1v1 -- --ignored
    /// --nocapture`. Resultado de referencia de esta corrida: p0=0.5024,
    /// margen ±0.1033 sobre 211/1500 partidas resueltas — sin sesgo
    /// estructural detectable por slot.
    #[test]
    #[ignore = "estadístico, ~3 min en release — correr explícito, ver comentario arriba"]
    fn symmetric_1v1_random_actions_no_slot_bias() {
        const N_MATCHES: u64 = 1500;
        const MAX_TICKS: u32 = 3600; // 60s a 60Hz; sin resolver = empate
        const MIN_DECIDED: u32 = 150; // debajo de esto el test no es significativo

        let mut wins = [0u32; 2];
        let mut draws = 0u32;

        for seed in 0..N_MATCHES {
            let settings = Settings {
                seed,
                players: 2,
                asteroid_count: 0,
                ..default()
            };
            let mut app = build_app(settings);
            app.insert_resource(bevy::time::TimeUpdateStrategy::ManualDuration(
                Duration::from_secs_f64(1.0 / 60.0),
            ));
            app.finish();
            app.cleanup();
            app.update(); // Startup: spawnea las 2 naves

            let entities: Vec<Entity> = {
                let mut query = app.world_mut().query::<(Entity, &Fighter)>();
                let mut pairs: Vec<(usize, Entity)> = query
                    .iter(app.world())
                    .map(|(e, f)| (f.player_id, e))
                    .collect();
                pairs.sort_by_key(|(id, _)| *id);
                pairs.into_iter().map(|(_, e)| e).collect()
            };
            assert_eq!(entities.len(), 2, "setup debe spawnear exactamente 2 naves");

            for _ in 0..MAX_TICKS {
                let actions: Vec<FighterAction> = {
                    let mut rng = app.world_mut().resource_mut::<RngState>();
                    entities
                        .iter()
                        .map(|_| FighterAction {
                            thrust: match rng.gen_range(0..3) {
                                0 => Thrust::On,
                                1 => Thrust::Off,
                                _ => Thrust::Stop,
                            },
                            turn: match rng.gen_range(0..3) {
                                0 => Turn::Left,
                                1 => Turn::Right,
                                _ => Turn::None,
                            },
                            shoot: if rng.gen_bool(0.85) {
                                Shoot::On
                            } else {
                                Shoot::Off
                            },
                            shield: if rng.gen_bool(0.2) {
                                Shield::On
                            } else {
                                Shield::Off
                            },
                        })
                        .collect()
                };
                for (entity, action) in entities.iter().zip(actions) {
                    app.world_mut()
                        .write_message(FighterActionMessage { action, entity: *entity });
                }

                app.update();

                if app.world().resource::<MatchResult>().finished {
                    break;
                }
            }

            let result = *app.world().resource::<MatchResult>();
            match result.winner {
                Some(pid) if pid < 2 => wins[pid] += 1,
                _ => draws += 1, // sin resolver, o destrucción mutua
            }
        }

        let decided = wins[0] + wins[1];
        println!(
            "symmetric_1v1: wins0={} wins1={} draws={} decided={}/{}",
            wins[0], wins[1], draws, decided, N_MATCHES
        );
        assert!(
            decided >= MIN_DECIDED,
            "muy pocas partidas se resolvieron ({decided}/{N_MATCHES}) para que el test sea \
             estadísticamente significativo — subir MAX_TICKS o revisar el balance de daño/HP"
        );

        let p0 = wins[0] as f64 / decided as f64;
        let se = (0.25 / decided as f64).sqrt();
        let margin = 3.0 * se;
        println!(
            "symmetric_1v1: p0={:.4} margen=±{:.4} (3 sigma sobre {} partidas resueltas)",
            p0, margin, decided
        );
        assert!(
            (p0 - 0.5).abs() <= margin,
            "tasa de victoria del slot 0 ({:.1}%) fuera del margen simétrico esperado \
             (50% ± {:.1} puntos porcentuales sobre {} partidas resueltas) — posible sesgo \
             estructural por slot",
            p0 * 100.0,
            margin * 100.0,
            decided
        );
    }

    fn snapshot(player_id: usize, position: Vec2) -> FighterSnapshot {
        FighterSnapshot {
            player_id,
            position,
            velocity: Vec2::ZERO,
            facing: Vec2::Y,
            health: 100.0,
            energy: 100.0,
            shield_active: false,
            remaining_bullet_cooldown: 0,
        }
    }

    /// Criterio de aceptación 1 de la Fase 3: un rival fuera de
    /// `radar_range` no aparece en absoluto en `rivals` (no se serializa
    /// vacío/nulo, directamente no está en el vector); dentro de rango,
    /// aparece exactamente un contacto.
    #[test]
    fn perception_range_isolation() {
        let radar_range = 800.0;
        let me = snapshot(0, Vec2::ZERO);

        let far = snapshot(1, Vec2::new(radar_range + 1.0, 0.0));
        let perception_far =
            build_perception(0, 0, radar_range, &[me, far], &[]).expect("player 0 vive");
        assert!(
            perception_far.rivals.is_empty(),
            "un rival a más de radar_range no debería aparecer en absoluto"
        );

        let near = snapshot(1, Vec2::new(radar_range - 1.0, 0.0));
        let perception_near =
            build_perception(0, 0, radar_range, &[me, near], &[]).expect("player 0 vive");
        assert_eq!(
            perception_near.rivals.len(),
            1,
            "un rival dentro de radar_range debe aparecer como exactamente un contacto"
        );
        assert_eq!(perception_near.rivals[0].player_id, 1);
    }

    /// Criterio de aceptación 2 de la Fase 3 -- el más importante, es la
    /// garantía real de RF-042/CA-013. El tipo `RivalContact` ya impide
    /// esto en tiempo de compilación (no tiene esos campos), pero lo que
    /// de verdad importa es confirmar el JSON serializado real: si
    /// alguien agregara esos campos a `RivalContact` en el futuro sin
    /// darse cuenta del problema, este test los atraparía igual, a nivel
    /// de dato serializado, no de tipo.
    #[test]
    fn perception_never_leaks_rival_internal_fields() {
        let me = snapshot(0, Vec2::ZERO);
        let mut rival = snapshot(1, Vec2::new(100.0, 0.0));
        rival.energy = 42.0; // valor centinela: si esto se filtra, lo vemos en el JSON
        rival.remaining_bullet_cooldown = 7;

        let perception =
            build_perception(0, 0, 800.0, &[me, rival], &[]).expect("player 0 vive");
        assert_eq!(perception.rivals.len(), 1, "el rival debe estar dentro de rango");

        let value = serde_json::to_value(&perception).expect("Perception debe serializar a JSON");
        let rivals_json = value
            .get("rivals")
            .expect("la percepción debe tener el campo rivals")
            .to_string();

        assert!(
            !rivals_json.contains("energy"),
            "el bloque de rivales serializado no debe contener la clave `energy` en ningún lado: {rivals_json}"
        );
        assert!(
            !rivals_json.contains("remaining_bullet_cooldown"),
            "el bloque de rivales serializado no debe contener `remaining_bullet_cooldown`: {rivals_json}"
        );
        assert!(
            !rivals_json.contains("42"),
            "el valor centinela de energía del rival (42) no debe aparecer en el bloque de rivales: {rivals_json}"
        );

        // Control positivo: `myself` sí debe tener esos campos -- si este
        // assert fallara, sería la prueba de que el test de arriba no
        // está probando nada real (falso negativo por accidente de
        // nombres de campo).
        let myself_json = value
            .get("myself")
            .expect("la percepción debe tener el campo myself")
            .to_string();
        assert!(
            myself_json.contains("energy") && myself_json.contains("remaining_bullet_cooldown"),
            "myself sí debe exponer energía y cooldown propios: {myself_json}"
        );
    }

    /// Confirma que la extracción real desde el ECS (`collect_fighter_snapshots`)
    /// produce datos consistentes con lo que se spawneó -- no solo que la
    /// lógica pura de `build_perception` sea correcta en aislamiento.
    #[test]
    fn perception_ecs_integration() {
        let settings = Settings {
            players: 0,
            asteroid_count: 0,
            ..default()
        };
        let mut app = build_app(settings);
        app.finish();
        app.cleanup();
        app.update();

        spawn_fighter(&mut app.world_mut().commands(), 0, Vec2::new(0.0, 0.0));
        spawn_fighter(&mut app.world_mut().commands(), 1, Vec2::new(100.0, 0.0));
        app.world_mut().flush();
        app.update();

        let fighters = app
            .world_mut()
            .run_system_once(
                |q: Query<(&Fighter, &Transform, &LinearVelocity)>| collect_fighter_snapshots(&q),
            )
            .expect("run_system_once no debería fallar");
        assert_eq!(fighters.len(), 2, "deben leerse las 2 naves spawneadas");

        let perception =
            build_perception(0, 0, 800.0, &fighters, &[]).expect("player 0 debe existir");
        assert_eq!(perception.rivals.len(), 1);
        assert_eq!(perception.rivals[0].player_id, 1);
        // Distancia real ~100 (puede moverse levemente por un tick de física).
        let dist = (perception.rivals[0].relative_position.x.powi(2)
            + perception.rivals[0].relative_position.y.powi(2))
        .sqrt();
        assert!(
            (dist - 100.0).abs() < 5.0,
            "la distancia relativa leída del ECS debería ser ~100, fue {dist}"
        );
    }
}
