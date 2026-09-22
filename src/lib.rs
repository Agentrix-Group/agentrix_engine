//! Núcleo físico mínimo de starfighter sobre Rapier2D + Bevy headless.
//!
//! Estado de la migración a Agentrix:
//! - Fase 0: motor sin `entity-gym-rs`/`pyo3`; desde ADR-0013 la física es
//!   `rapier2d` directo (ver `physics`).
//! - Fase 1: modelo de nave único (HP/energía/escudo), condición de fin
//!   de partida simétrica.
//! - contratos autoritativos mantenidos por el repositorio Agentrix;
//! - Fase 3: percepción aislada por slot (`Perception`, `RivalContact`
//!   nunca expone energía/cooldown de un rival).
//! - integración Agentrix: el binario `starfighter-engine` recibe acciones
//!   opacas desde Go y emite percepciones privadas y snapshots públicos.
//!
//! Valores de balance (HP, daño, costos de energía) son **placeholders**
//! documentados en su lugar de definición: la afinación real es trabajo
//! de un concurso real, no de esta fase. Lo que sí es un requisito desde
//! la Fase 1 es que sean **iguales para todos los slots** — no hay
//! ninguna rama por `player_id` en todo este archivo.

use agentrix_sim_core::{DeterministicRng, StableEntityId};
use bevy::prelude::*;
pub use physics::{
    AngularVelocity, Contact, LinearVelocity, Physics, PhysicsBody,
    PhysicsHandle, PhysicsShape, ThrustForce, TickContacts, BULLET_RADIUS,
};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::ops::{Deref, DerefMut};

pub mod game_module;
pub mod physics;
pub mod protocol;
pub mod stdio_server;

/// Inicializa `tracing` para escribir a **stderr**, nunca a stdout.
/// stdout queda reservado para el protocolo `agentrix-engine/2`.
pub fn init_tracing() {
    let _ = tracing_subscriber::fmt()
        .with_writer(std::io::stderr)
        .with_env_filter(tracing_subscriber::EnvFilter::from_default_env())
        .try_init();
}

/// Configuración autoritativa del juego Starfighter. Define todos los
/// parámetros ajustables de simulación, combate y dimensiones de arena.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct StarfighterConfig {
    pub arena_width: f32,
    pub arena_height: f32,
    pub ship_max_health: f32,
    pub ship_max_energy: f32,
    pub bullet_damage: f32,
    pub asteroid_damage: f32,
    pub shoot_energy_cost: f32,
    pub shield_energy_cost_per_tick: f32,
    pub shield_damage_reduction: f32,
    pub energy_regen_per_tick: f32,
    pub radar_range: f32,
    pub asteroid_count: u32,
}

impl Default for StarfighterConfig {
    fn default() -> Self {
        StarfighterConfig {
            arena_width: 2000.0,
            arena_height: 1000.0,
            ship_max_health: 100.0,
            ship_max_energy: 100.0,
            bullet_damage: 25.0,
            asteroid_damage: 100.0,
            shoot_energy_cost: 15.0,
            shield_energy_cost_per_tick: 1.0,
            shield_damage_reduction: 0.7,
            energy_regen_per_tick: 0.5,
            radar_range: 800.0,
            asteroid_count: 5,
        }
    }
}

impl StarfighterConfig {
    pub fn from_value(
        val: &serde_json::Value,
    ) -> Result<Self, agentrix_sim_core::SimError> {
        let cfg: StarfighterConfig = serde_json::from_value(val.clone())
            .map_err(|e| {
                agentrix_sim_core::SimError::new(
                    "invalid_config",
                    format!("invalid starfighter config: {e}"),
                )
            })?;
        cfg.validate()?;
        Ok(cfg)
    }

    pub fn validate(&self) -> Result<(), agentrix_sim_core::SimError> {
        let positive = [
            ("arena_width", self.arena_width),
            ("arena_height", self.arena_height),
            ("ship_max_health", self.ship_max_health),
            ("ship_max_energy", self.ship_max_energy),
            ("radar_range", self.radar_range),
        ];
        for (name, value) in positive {
            if !(value.is_finite() && value > 0.0) {
                return Err(agentrix_sim_core::SimError::new(
                    "invalid_config",
                    format!("{name} must be a positive finite number"),
                ));
            }
        }
        let non_negative = [
            ("bullet_damage", self.bullet_damage),
            ("asteroid_damage", self.asteroid_damage),
            ("shoot_energy_cost", self.shoot_energy_cost),
            (
                "shield_energy_cost_per_tick",
                self.shield_energy_cost_per_tick,
            ),
            ("energy_regen_per_tick", self.energy_regen_per_tick),
        ];
        for (name, value) in non_negative {
            if !(value.is_finite() && value >= 0.0) {
                return Err(agentrix_sim_core::SimError::new(
                    "invalid_config",
                    format!("{name} must be a non-negative finite number"),
                ));
            }
        }
        if !(0.0..=1.0).contains(&self.shield_damage_reduction)
            || !self.shield_damage_reduction.is_finite()
        {
            return Err(agentrix_sim_core::SimError::new(
                "invalid_config",
                "shield_damage_reduction must be within [0, 1]",
            ));
        }
        Ok(())
    }
}

#[derive(Clone, Resource)]
pub struct Settings {
    pub seed: u64,
    pub tick_rate: agentrix_sim_core::TickRate,
    pub players: u32,
    pub asteroid_count: u32,
    pub config: StarfighterConfig,
    pub max_entities: u64,
}

impl Default for Settings {
    fn default() -> Self {
        let config = StarfighterConfig::default();
        Settings {
            seed: 0,
            tick_rate: agentrix_sim_core::TickRate::SIXTY_HZ,
            players: 2,
            asteroid_count: config.asteroid_count,
            config,
            max_entities: 10_000,
        }
    }
}

impl Settings {
    #[inline]
    pub fn arena_half_width(&self) -> f32 {
        self.config.arena_width * 0.5
    }

    #[inline]
    pub fn arena_half_height(&self) -> f32 {
        self.config.arena_height * 0.5
    }

    #[inline]
    pub fn radar_range(&self) -> f32 {
        self.config.radar_range
    }

    #[inline]
    pub fn ship_max_health(&self) -> f32 {
        self.config.ship_max_health
    }

    #[inline]
    pub fn ship_max_energy(&self) -> f32 {
        self.config.ship_max_energy
    }

    #[inline]
    pub fn bullet_damage(&self) -> f32 {
        self.config.bullet_damage
    }

    #[inline]
    pub fn asteroid_damage(&self) -> f32 {
        self.config.asteroid_damage
    }

    #[inline]
    pub fn shoot_energy_cost(&self) -> f32 {
        self.config.shoot_energy_cost
    }

    #[inline]
    pub fn shield_energy_cost_per_tick(&self) -> f32 {
        self.config.shield_energy_cost_per_tick
    }

    #[inline]
    pub fn shield_damage_reduction(&self) -> f32 {
        self.config.shield_damage_reduction
    }

    #[inline]
    pub fn energy_regen_per_tick(&self) -> f32 {
        self.config.energy_regen_per_tick
    }
}

pub const DEFAULT_ARENA_HALF_WIDTH: f32 = 1000.0;
pub const DEFAULT_ARENA_HALF_HEIGHT: f32 = 500.0;

/// Constantes conservadas como defaults históricos y placeholders documentales.
#[allow(dead_code)]
const ARENA_HALF_WIDTH: f32 = DEFAULT_ARENA_HALF_WIDTH;
#[allow(dead_code)]
const ARENA_HALF_HEIGHT: f32 = DEFAULT_ARENA_HALF_HEIGHT;
#[allow(dead_code)]
const MAX_HEALTH: f32 = 100.0;
#[allow(dead_code)]
const MAX_ENERGY: f32 = 100.0;
#[allow(dead_code)]
const BULLET_DAMAGE: f32 = 25.0;
#[allow(dead_code)]
const ASTEROID_DAMAGE: f32 = MAX_HEALTH;
#[allow(dead_code)]
const SHOOT_ENERGY_COST: f32 = 15.0;
#[allow(dead_code)]
const SHIELD_ENERGY_COST_PER_TICK: f32 = 1.0;
#[allow(dead_code)]
const SHIELD_DAMAGE_REDUCTION: f32 = 0.7;
#[allow(dead_code)]
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

/// Logical identity used by canonical state and future public snapshots.
/// Bevy's generational `Entity` is deliberately never persisted or hashed.
#[derive(Component, Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub struct EntityId(pub StableEntityId);

#[derive(Resource, Debug)]
pub struct EntityIdAllocator {
    next: u64,
    active_count: u64,
    max_entities: u64,
}

impl Default for EntityIdAllocator {
    fn default() -> Self {
        Self {
            next: 1_000_000,
            active_count: 0,
            max_entities: 10_000,
        }
    }
}

impl EntityIdAllocator {
    pub fn new(max_entities: u64) -> Self {
        Self {
            next: 1_000_000,
            active_count: 0,
            max_entities,
        }
    }

    pub fn allocate(
        &mut self,
    ) -> Result<StableEntityId, agentrix_sim_core::SimError> {
        if self.active_count >= self.max_entities {
            return Err(agentrix_sim_core::SimError::new(
                "max_entities_exceeded",
                format!("entity limit {} exceeded", self.max_entities),
            ));
        }
        let id = StableEntityId(self.next);
        self.next = self.next.checked_add(1).ok_or_else(|| {
            agentrix_sim_core::SimError::new(
                "entity_id_overflow",
                "entity id allocation overflow",
            )
        })?;
        self.active_count += 1;
        Ok(id)
    }

    pub fn deallocate(&mut self) {
        self.active_count = self.active_count.saturating_sub(1);
    }

    pub fn next_id(&self) -> u64 {
        self.next
    }

    pub fn active_count(&self) -> u64 {
        self.active_count
    }
}

/// Marca las entidades cuyo `EntityId` salió de `EntityIdAllocator`. Al
/// despawnearse, `release_allocated_id` devuelve su lugar al cupo de
/// `max_entities`; sin esto el cupo solo crece y, al agotarse, las naves
/// dejan de poder disparar en plena partida.
#[derive(Component)]
pub struct AllocatedId;

fn release_allocated_id(
    _: On<Remove, AllocatedId>,
    mut ids: ResMut<EntityIdAllocator>,
) {
    ids.deallocate();
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

/// Pedido de eliminar una nave cuyo agente fue descalificado (timeout,
/// crash) en este tick (ADR-0013): la nave sale de la partida sin atribuir
/// la baja a nadie y los demás siguen jugando. En 1 contra 1 reproduce la
/// política de ADR-0004: con una sola descalificación gana el rival y con
/// dos simultáneas ambas naves caen en el mismo tick y empatan.
#[derive(Message, Clone, Copy)]
pub struct DisqualifyFighter {
    pub entity: Entity,
}

/// Condición de fin de partida: simétrica, no distingue slots. Se fija
/// una sola vez, la primera vez que queda una nave viva o cero (empate
/// por destrucción mutua). El límite de ticks lo aplica quien corre la
/// partida; la clasificación completa sale de `placements`.
#[derive(Resource, Default, Clone, Copy, Debug)]
pub struct MatchResult {
    pub finished: bool,
    /// `player_id` del único sobreviviente. `None` si terminó por
    /// destrucción mutua (cero sobrevivientes) o si no terminó todavía.
    pub winner: Option<usize>,
}

/// Registro competitivo de la partida, parte del estado autoritativo.
#[derive(Resource, Default, Clone, Debug, PartialEq)]
pub struct Scoreboard {
    /// Tick del estado en que cada slot fue eliminado; ausente si vive.
    pub eliminated_at: BTreeMap<usize, u64>,
    /// Bajas atribuidas a cada slot (solo impactos de sus balas).
    pub kills: BTreeMap<usize, u32>,
}

/// Puesto final de un slot en la clasificación.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Placement {
    pub player_id: usize,
    /// Clasificación de competencia: 1 + cantidad de slots estrictamente
    /// mejores. Los empatados comparten puesto (1, 2, 2, 4).
    pub rank: u32,
    pub kills: u32,
}

/// Clasificación todos contra todos (ADR-0013):
/// - los sobrevivientes van antes que los eliminados, ordenados por salud
///   restante (al terminar por límite de ticks puede haber varios);
/// - los eliminados se ordenan por tick de eliminación, el más tardío
///   primero; los eliminados en el mismo tick empatan.
///
/// `alive` trae `(player_id, health)` de cada nave viva. El resultado sale
/// ordenado por puesto y, a igual puesto, por `player_id`.
pub fn placements(
    players: usize,
    alive: &[(usize, f32)],
    scoreboard: &Scoreboard,
) -> Vec<Placement> {
    #[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
    enum Standing {
        // Mayor es mejor en ambos casos.
        Eliminated { tick: u64 },
        Alive { health_bits: u32 },
    }
    let health_key = |health: f32| {
        // Salud no negativa: el orden de los bits IEEE coincide con el
        // orden numérico, y dos naves empatan solo si su salud es idéntica.
        health.max(0.0).to_bits()
    };
    let standings: Vec<(usize, Standing)> = (0..players)
        .map(|player_id| {
            let standing = match alive.iter().find(|(id, _)| *id == player_id)
            {
                Some((_, health)) => Standing::Alive {
                    health_bits: health_key(*health),
                },
                None => Standing::Eliminated {
                    tick: scoreboard
                        .eliminated_at
                        .get(&player_id)
                        .copied()
                        .unwrap_or(0),
                },
            };
            (player_id, standing)
        })
        .collect();
    let mut result: Vec<Placement> = standings
        .iter()
        .map(|&(player_id, standing)| Placement {
            player_id,
            rank: 1 + standings
                .iter()
                .filter(|(_, other)| *other > standing)
                .count() as u32,
            kills: scoreboard.kills.get(&player_id).copied().unwrap_or(0),
        })
        .collect();
    result.sort_by_key(|placement| (placement.rank, placement.player_id));
    result
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum StarfighterEvent {
    Fired {
        player_id: usize,
    },
    Hit {
        player_id: usize,
        source: String,
        damage: f32,
        shielded: bool,
    },
    Destroyed {
        player_id: usize,
        source: String,
        /// Slot que hizo la baja; `None` si fue un asteroide.
        killer: Option<usize>,
    },
    MatchEnded {
        winner: Option<usize>,
    },
}

impl Eq for StarfighterEvent {}

impl PartialOrd for StarfighterEvent {
    fn partial_cmp(&self, other: &Self) -> Option<std::cmp::Ordering> {
        Some(self.cmp(other))
    }
}

impl Ord for StarfighterEvent {
    fn cmp(&self, other: &Self) -> std::cmp::Ordering {
        match (self, other) {
            (Self::Fired { player_id: a }, Self::Fired { player_id: b }) => {
                a.cmp(b)
            }
            (Self::Fired { .. }, _) => std::cmp::Ordering::Less,
            (_, Self::Fired { .. }) => std::cmp::Ordering::Greater,

            (
                Self::Hit {
                    player_id: p1,
                    source: s1,
                    damage: d1,
                    shielded: sh1,
                },
                Self::Hit {
                    player_id: p2,
                    source: s2,
                    damage: d2,
                    shielded: sh2,
                },
            ) => (p1, s1, sh1)
                .cmp(&(p2, s2, sh2))
                .then_with(|| d1.total_cmp(d2)),
            (Self::Hit { .. }, _) => std::cmp::Ordering::Less,
            (_, Self::Hit { .. }) => std::cmp::Ordering::Greater,

            (
                Self::Destroyed {
                    player_id: p1,
                    source: s1,
                    killer: k1,
                },
                Self::Destroyed {
                    player_id: p2,
                    source: s2,
                    killer: k2,
                },
            ) => (p1, s1, k1).cmp(&(p2, s2, k2)),
            (Self::Destroyed { .. }, _) => std::cmp::Ordering::Less,
            (_, Self::Destroyed { .. }) => std::cmp::Ordering::Greater,

            (
                Self::MatchEnded { winner: w1 },
                Self::MatchEnded { winner: w2 },
            ) => w1.cmp(w2),
        }
    }
}

#[derive(Resource, Default, Clone, Debug)]
pub struct TickEvents(pub Vec<StarfighterEvent>);

#[derive(Resource)]
pub struct RngState(pub DeterministicRng);

impl Deref for RngState {
    type Target = DeterministicRng;
    fn deref(&self) -> &DeterministicRng {
        &self.0
    }
}

impl DerefMut for RngState {
    fn deref_mut(&mut self) -> &mut DeterministicRng {
        &mut self.0
    }
}

/// `false` durante el `update()` de Startup, `true` desde el siguiente:
/// así el primer `update()` produce State[0] y cada uno de los demás
/// avanza exactamente un tick, sin depender del reloj de Bevy.
#[derive(Resource, Default)]
pub struct SimulationClock {
    armed: bool,
    tick: u64,
}

impl SimulationClock {
    /// Tick del estado que se está produciendo (0 en Startup).
    pub fn tick(&self) -> u64 {
        self.tick
    }
}

fn arm_simulation_clock(mut clock: ResMut<SimulationClock>) {
    clock.armed = true;
}

fn advance_simulation_clock(mut clock: ResMut<SimulationClock>) {
    clock.tick += 1;
}

fn simulation_armed(clock: Res<SimulationClock>) -> bool {
    clock.armed
}

/// Construye la app headless. Sin `DefaultPlugins`, sin ventana, sin
/// assets: el renderer de Agentrix reconstruye la vista desde el replay,
/// no desde este proceso (ATD-011).
///
/// El primer `app.update()` corre `Startup` (State[0]); cada `update()`
/// posterior es exactamente un tick: reglas, un paso físico de
/// `1 / tick_rate` segundos y resolución de contactos de ese mismo paso.
pub fn build_app(settings: Settings) -> App {
    let mut app = App::new();
    app.add_plugins(MinimalPlugins);
    physics::add_physics(&mut app, &settings);
    app.insert_resource(RngState(DeterministicRng::from_seed(settings.seed)))
        .insert_resource(EntityIdAllocator::new(settings.max_entities))
        .init_resource::<MatchResult>()
        .init_resource::<TickEvents>()
        .init_resource::<SimulationClock>()
        .init_resource::<Scoreboard>()
        .add_message::<FighterActionMessage>()
        .add_message::<FighterDestroyed>()
        .add_message::<DisqualifyFighter>()
        .insert_resource(settings)
        .add_observer(release_allocated_id)
        .add_systems(Startup, setup)
        .add_systems(
            Update,
            (
                advance_simulation_clock,
                apply_disqualifications,
                check_boundary_collision,
                spawn_asteroids,
                fighter_actions,
                cooldowns,
                expire_bullets,
                physics::physics_insert,
                physics::physics_step,
                detect_collisions,
                check_match_end,
            )
                .chain()
                .run_if(simulation_armed),
        )
        .add_systems(PostUpdate, arm_simulation_clock);
    app
}

/// Puntos de spawn: `players` puntos equiespaciados en ángulo sobre una
/// elipse al 60 % de la arena, siempre dentro de ella. Con dos jugadores son
/// (±0.6·half_width, 0), igual que el spawn circular anterior.
pub fn spawn_points(settings: &Settings) -> Vec<Vec2> {
    let players = settings.players.max(1);
    (0..players)
        .map(|i| {
            let angle = i as f32 / players as f32 * std::f32::consts::TAU;
            Vec2::new(
                libm::cosf(angle) * settings.arena_half_width() * 0.6,
                libm::sinf(angle) * settings.arena_half_height() * 0.6,
            )
        })
        .collect()
}

/// Dirección desde `position` hacia el centro de la arena.
fn facing_center(position: Vec2) -> Vec2 {
    let toward_center = -position;
    if toward_center.length_squared() > 0.0 {
        toward_center.normalize()
    } else {
        Vec2::Y
    }
}

/// Reparte los puntos de spawn con una permutación derivada de la semilla
/// (Fisher-Yates sobre `RngState`): con una arena rectangular los puntos no
/// son equivalentes, así que ningún slot conserva el mismo lugar en todas
/// las partidas. Cada nave nace mirando al centro.
fn setup(
    settings: Res<Settings>,
    mut cmd: Commands,
    mut rng: ResMut<RngState>,
) {
    let points = spawn_points(&settings);
    let mut order: Vec<usize> = (0..settings.players as usize).collect();
    for i in (1..order.len()).rev() {
        let j = rng
            .range_u32(0, i as u32 + 1)
            .expect("non-empty spawn permutation range") as usize;
        order.swap(i, j);
    }
    for (player_id, point) in order.into_iter().enumerate() {
        let position = points[point];
        spawn_fighter_facing(
            &mut cmd,
            player_id,
            position,
            facing_center(position),
            &settings,
        );
    }
}

pub fn spawn_fighter(
    cmd: &mut Commands,
    player_id: usize,
    position: Vec2,
) -> Entity {
    spawn_fighter_with_settings(cmd, player_id, position, &Settings::default())
}

pub fn spawn_fighter_with_settings(
    cmd: &mut Commands,
    player_id: usize,
    position: Vec2,
    settings: &Settings,
) -> Entity {
    spawn_fighter_facing(cmd, player_id, position, Vec2::Y, settings)
}

/// Spawnea una nave con la nariz apuntando a `facing` (vector unitario).
pub fn spawn_fighter_facing(
    cmd: &mut Commands,
    player_id: usize,
    position: Vec2,
    facing: Vec2,
    settings: &Settings,
) -> Entity {
    // facing = (-sin θ, cos θ)  =>  cos θ = facing.y, sin θ = -facing.x
    let rotation = physics::cos_sin_to_quat(facing.y, -facing.x);
    cmd.spawn((
        Fighter {
            player_id,
            health: settings.ship_max_health(),
            max_health: settings.ship_max_health(),
            energy: settings.ship_max_energy(),
            max_energy: settings.ship_max_energy(),
            ..default()
        },
        EntityId(StableEntityId(player_id as u64 + 1)),
        PhysicsBody {
            shape: PhysicsShape::ConvexHull(vec![
                Vec2::new(0.0, 25.0),
                Vec2::new(-15.0, -15.0),
                Vec2::new(15.0, -15.0),
            ]),
            lock_rotation: false,
        },
        Transform::from_translation(position.extend(0.0))
            .with_rotation(rotation),
        LinearVelocity::ZERO,
        AngularVelocity::ZERO,
        ThrustForce::default(),
        CollisionType::Fighter,
    ))
    .id()
}

/// Las balas no son cuerpos rígidos: `physics_step` las mueve y barre su
/// trayectoria de cada tick contra naves, asteroides y otras balas.
pub fn spawn_bullet(
    cmd: &mut Commands,
    position: Vec2,
    velocity: Vec2,
    lifetime: u32,
    player_id: usize,
    stable_id: StableEntityId,
) -> Entity {
    cmd.spawn((
        Bullet {
            remaining_lifetime: lifetime as i32,
            player_id,
        },
        EntityId(stable_id),
        CollisionType::Bullet,
        Transform::from_translation(position.extend(0.0)),
        LinearVelocity(velocity),
    ))
    .id()
}

fn spawn_asteroids(
    settings: Res<Settings>,
    mut cmd: Commands,
    asteroids: Query<(Entity, &Transform), With<Asteroid>>,
    mut rng: ResMut<RngState>,
    mut ids: ResMut<EntityIdAllocator>,
) {
    let mut count = 0;
    let margin_x = settings.arena_half_width() * 1.5;
    let margin_y = settings.arena_half_height() * 1.5;
    for (entity, transform) in &asteroids {
        let p = transform.translation;
        if p.x.abs() > margin_x || p.y.abs() > margin_y {
            cmd.entity(entity).despawn();
        } else {
            count += 1;
        }
    }
    while count < settings.asteroid_count {
        let speed = 50.0 + 250.0 * rng.unit_f32();
        let direction = std::f32::consts::TAU * rng.unit_f32();
        let spawn_angle = std::f32::consts::TAU * rng.unit_f32();
        let radius =
            ((20.0 + 40.0 * rng.unit_f32()) * (20.0 + 40.0 * rng.unit_f32()))
                .sqrt();
        if let Ok(id) = ids.allocate() {
            cmd.spawn((
                Asteroid {
                    health: 2.0,
                    radius,
                },
                EntityId(id),
                AllocatedId,
                PhysicsBody {
                    shape: PhysicsShape::Ball(radius),
                    lock_rotation: true,
                },
                Transform::from_translation(Vec3::new(
                    margin_x * libm::cosf(spawn_angle),
                    margin_y * libm::sinf(spawn_angle),
                    0.0,
                )),
                LinearVelocity(
                    speed
                        * Vec2::new(
                            libm::cosf(direction),
                            libm::sinf(direction),
                        ),
                ),
                CollisionType::Asteroid,
            ));
            count += 1;
        } else {
            break;
        }
    }
}

fn check_boundary_collision(
    mut fighters: Query<(
        &mut LinearVelocity,
        &mut AngularVelocity,
        &Transform,
        &Fighter,
    )>,
    settings: Res<Settings>,
) {
    let half_w = settings.arena_half_width();
    let half_h = settings.arena_half_height();
    for (mut velocity, mut angular, transform, fighter) in &mut fighters {
        let x = transform.translation.x;
        let y = transform.translation.y;
        if x > half_w {
            velocity.x = -velocity.x.abs();
        } else if x < -half_w {
            velocity.x = velocity.x.abs();
        }
        if y > half_h {
            velocity.y = -velocity.y.abs();
        } else if y < -half_h {
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
        &mut ThrustForce,
    )>,
    mut cmd: Commands,
    settings: Res<Settings>,
    mut events: ResMut<TickEvents>,
    mut ids: ResMut<EntityIdAllocator>,
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
        angular.0 = angular
            .0
            .clamp(-fighter.max_turn_speed, fighter.max_turn_speed);

        let speed = vel.0.length();
        match action.thrust {
            Thrust::On => {
                force.0 = facing * fighter.acceleration;
            }
            Thrust::Off => {
                vel.0 *= 1.0
                    - fighter.drag_coef
                        * libm::powf(
                            speed / fighter.max_velocity,
                            fighter.drag_exp,
                        );
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
            let shoot_cost = settings.shoot_energy_cost();
            if fighter.remaining_bullet_cooldown <= 0
                && fighter.energy >= shoot_cost
            {
                if let Ok(bullet_id) = ids.allocate() {
                    let bullet = spawn_bullet(
                        &mut cmd,
                        transform.translation.truncate() + facing * 24.0,
                        vel.0 + facing * fighter.bullet_speed,
                        fighter.bullet_lifetime,
                        fighter.player_id,
                        bullet_id,
                    );
                    cmd.entity(bullet).insert(AllocatedId);
                    fighter.remaining_bullet_cooldown =
                        fighter.bullet_cooldown as i32;
                    fighter.energy -= shoot_cost;
                    events.0.push(StarfighterEvent::Fired {
                        player_id: fighter.player_id,
                    });
                }
            }
        }
    }
}

/// Dirección de la nariz de la nave: el eje +Y local rotado, es decir
/// (-sin θ, cos θ). Se calcula del cuaternión sin trigonometría.
fn facing_direction(transform: &Transform) -> Vec2 {
    let (cos, sin) = physics::quat_to_cos_sin(transform.rotation);
    Vec2::new(-sin, cos)
}

/// Corre para todas las naves cada tick, tengan o no un mensaje de
/// acción ese tick: cooldown de disparo, costo de sostener el escudo, y
/// regeneración pasiva de energía cuando no se está gastando.
fn cooldowns(mut fighters: Query<&mut Fighter>, settings: Res<Settings>) {
    let shield_cost = settings.shield_energy_cost_per_tick();
    let regen = settings.energy_regen_per_tick();
    for mut fighter in &mut fighters {
        fighter.remaining_bullet_cooldown -= 1;

        if fighter.shield_active {
            if fighter.energy >= shield_cost {
                fighter.energy -= shield_cost;
            } else {
                // Sin energía para sostenerlo: se apaga solo, simétrico
                // para cualquier slot.
                fighter.energy = 0.0;
                fighter.shield_active = false;
            }
        } else {
            fighter.energy = (fighter.energy + regen).min(fighter.max_energy);
        }
    }
}

fn expire_bullets(
    mut cmd: Commands,
    mut bullets: Query<(Entity, &mut Bullet)>,
) {
    for (entity, mut bullet) in &mut bullets {
        bullet.remaining_lifetime -= 1;
        if bullet.remaining_lifetime <= 0 {
            cmd.entity(entity).despawn();
        }
    }
}

/// Clave canónica de un par en colisión: los `EntityId` estables del par,
/// menor primero. Las entidades sin `EntityId` van al final.
fn collision_pair_key(
    ids: &Query<&EntityId>,
    a: Entity,
    b: Entity,
) -> (u64, u64) {
    let id = |entity| ids.get(entity).map(|id| id.0 .0).unwrap_or(u64::MAX);
    let (ia, ib) = (id(a), id(b));
    (ia.min(ib), ia.max(ib))
}

/// Aplica las reglas de contacto del tick.
///
/// Los pares se procesan por instante de impacto dentro del tick y, a
/// igual instante, en orden canónico por `EntityId` (nunca en el orden en
/// que el backend físico los reporta). Cada entidad consumida en el
/// tick (bala que impactó, asteroide destruido, nave destruida) deja de
/// producir efectos: el despawn por `Commands` es diferido, así que sin este
/// registro una nave podía recibir daño y emitir `Destroyed` dos veces, o
/// una bala golpear dos objetivos.
///
/// Excepción deliberada: un asteroide que toca varias naves en el mismo tick
/// daña a todas. Si solo dañara a la primera, el orden por id favorecería a
/// los slots de id mayor.
#[allow(clippy::too_many_arguments)]
fn detect_collisions(
    mut cmd: Commands,
    contacts: Res<TickContacts>,
    collision_type: Query<&CollisionType>,
    ids: Query<&EntityId>,
    mut fighters: Query<&mut Fighter>,
    bullets: Query<&Bullet>,
    mut asteroids: Query<&mut Asteroid>,
    mut destroyed: MessageWriter<FighterDestroyed>,
    mut events: ResMut<TickEvents>,
    mut scoreboard: ResMut<Scoreboard>,
    clock: Res<SimulationClock>,
    settings: Res<Settings>,
) {
    let mut ledger = HitLedger {
        tick: clock.tick(),
        events: &mut events,
        scoreboard: &mut scoreboard,
        destroyed: &mut destroyed,
    };
    let mut contacts: Vec<&Contact> = contacts.0.iter().collect();
    contacts.sort_by(|x, y| {
        x.time_of_impact.total_cmp(&y.time_of_impact).then_with(|| {
            collision_pair_key(&ids, x.a, x.b)
                .cmp(&collision_pair_key(&ids, y.a, y.b))
        })
    });
    let mut pairs: Vec<(Entity, Entity)> = Vec::new();
    for contact in contacts {
        let key = collision_pair_key(&ids, contact.a, contact.b);
        if !pairs.iter().any(|&(a, b)| collision_pair_key(&ids, a, b) == key)
        {
            pairs.push((contact.a, contact.b));
        }
    }

    let mut consumed: Vec<Entity> = Vec::new();
    for (a, b) in pairs {
        let (Ok(ta), Ok(tb)) = (collision_type.get(a), collision_type.get(b))
        else {
            continue;
        };
        match (ta, tb) {
            (CollisionType::Fighter, CollisionType::Asteroid)
            | (CollisionType::Asteroid, CollisionType::Fighter) => {
                let (fighter_entity, asteroid_entity) =
                    if *ta == CollisionType::Fighter {
                        (a, b)
                    } else {
                        (b, a)
                    };
                if consumed.contains(&fighter_entity) {
                    continue;
                }
                if take_hit(
                    &mut cmd,
                    &mut fighters,
                    fighter_entity,
                    HitSource {
                        damage: settings.asteroid_damage(),
                        shield_reduction: settings.shield_damage_reduction(),
                        description: "an asteroid",
                        killer: None,
                    },
                    &mut ledger,
                ) {
                    consumed.push(fighter_entity);
                }
                if !consumed.contains(&asteroid_entity) {
                    consumed.push(asteroid_entity);
                    cmd.entity(asteroid_entity).despawn();
                }
            }
            (CollisionType::Bullet, CollisionType::Asteroid)
            | (CollisionType::Asteroid, CollisionType::Bullet) => {
                let (bullet_entity, asteroid_entity) =
                    if *ta == CollisionType::Bullet {
                        (a, b)
                    } else {
                        (b, a)
                    };
                if consumed.contains(&bullet_entity)
                    || consumed.contains(&asteroid_entity)
                {
                    continue;
                }
                consumed.push(bullet_entity);
                cmd.entity(bullet_entity).despawn();
                if let Ok(mut asteroid) = asteroids.get_mut(asteroid_entity) {
                    asteroid.health -= 1.0;
                    if asteroid.health <= 0.0 {
                        consumed.push(asteroid_entity);
                        cmd.entity(asteroid_entity).despawn();
                    }
                }
            }
            (CollisionType::Fighter, CollisionType::Bullet)
            | (CollisionType::Bullet, CollisionType::Fighter) => {
                let (fighter_entity, bullet_entity) =
                    if *ta == CollisionType::Fighter {
                        (a, b)
                    } else {
                        (b, a)
                    };
                if consumed.contains(&fighter_entity)
                    || consumed.contains(&bullet_entity)
                {
                    continue;
                }
                let shooter =
                    bullets.get(bullet_entity).ok().map(|b| b.player_id);
                let same_owner = fighters
                    .get(fighter_entity)
                    .ok()
                    .zip(shooter)
                    .map(|(fighter, shooter_id)| {
                        fighter.player_id == shooter_id
                    })
                    .unwrap_or(true);
                if !same_owner {
                    let source = shooter
                        .map(|id| format!("P{id}'s bullet"))
                        .unwrap_or_else(|| "a bullet".to_string());
                    if take_hit(
                        &mut cmd,
                        &mut fighters,
                        fighter_entity,
                        HitSource {
                            damage: settings.bullet_damage(),
                            shield_reduction: settings
                                .shield_damage_reduction(),
                            description: &source,
                            killer: shooter,
                        },
                        &mut ledger,
                    ) {
                        consumed.push(fighter_entity);
                    }
                    consumed.push(bullet_entity);
                    cmd.entity(bullet_entity).despawn();
                }
            }
            (CollisionType::Bullet, CollisionType::Bullet) => {
                if consumed.contains(&a) || consumed.contains(&b) {
                    continue;
                }
                consumed.extend([a, b]);
                cmd.entity(a).despawn();
                cmd.entity(b).despawn();
            }
            _ => {}
        }
    }
}

/// Dónde queda registrado lo que produce un impacto durante el tick.
struct HitLedger<'a, 'w> {
    tick: u64,
    events: &'a mut TickEvents,
    scoreboard: &'a mut Scoreboard,
    destroyed: &'a mut MessageWriter<'w, FighterDestroyed>,
}

/// Qué golpea a la nave: daño base, descripción y slot autor (si lo hay).
struct HitSource<'a> {
    damage: f32,
    shield_reduction: f32,
    description: &'a str,
    killer: Option<usize>,
}

/// Resta el daño a la nave (reducido si tiene el escudo activo) y la
/// destruye si su HP llega a cero, registrando tick de eliminación y baja
/// del autor. Mismo camino para cualquier `player_id` — no hay ninguna
/// rama especial por slot acá. Devuelve `true` si la nave quedó destruida.
fn take_hit(
    cmd: &mut Commands,
    fighters: &mut Query<&mut Fighter>,
    entity: Entity,
    hit: HitSource,
    ledger: &mut HitLedger,
) -> bool {
    let HitSource {
        damage,
        shield_reduction,
        description: source,
        killer,
    } = hit;
    let Ok(mut fighter) = fighters.get_mut(entity) else {
        return false;
    };
    let effective_damage = if fighter.shield_active {
        damage * (1.0 - shield_reduction)
    } else {
        damage
    };
    fighter.health -= effective_damage;
    ledger.events.0.push(StarfighterEvent::Hit {
        player_id: fighter.player_id,
        source: source.to_string(),
        damage: effective_damage,
        shielded: fighter.shield_active,
    });
    if fighter.health <= 0.0 {
        fighter.health = 0.0;
        ledger.events.0.push(StarfighterEvent::Destroyed {
            player_id: fighter.player_id,
            source: source.to_string(),
            killer,
        });
        ledger
            .scoreboard
            .eliminated_at
            .insert(fighter.player_id, ledger.tick);
        if let Some(killer) = killer {
            *ledger.scoreboard.kills.entry(killer).or_insert(0) += 1;
        }
        ledger.destroyed.write(FighterDestroyed {
            entity,
            player_id: fighter.player_id,
        });
        cmd.entity(entity).despawn();
        return true;
    }
    false
}

/// Elimina las naves descalificadas en este tick, antes de cualquier otra
/// regla: su acción del tick ya no se aplica y no pueden recibir ni causar
/// daño.
fn apply_disqualifications(
    mut cmd: Commands,
    mut requests: MessageReader<DisqualifyFighter>,
    fighters: Query<&Fighter>,
    mut destroyed: MessageWriter<FighterDestroyed>,
    mut events: ResMut<TickEvents>,
    mut scoreboard: ResMut<Scoreboard>,
    clock: Res<SimulationClock>,
) {
    let mut removed: Vec<Entity> = Vec::new();
    for DisqualifyFighter { entity } in requests.read() {
        if removed.contains(entity) {
            continue;
        }
        let Ok(fighter) = fighters.get(*entity) else {
            continue;
        };
        removed.push(*entity);
        events.0.push(StarfighterEvent::Destroyed {
            player_id: fighter.player_id,
            source: "disqualified".to_string(),
            killer: None,
        });
        scoreboard
            .eliminated_at
            .insert(fighter.player_id, clock.tick());
        destroyed.write(FighterDestroyed {
            entity: *entity,
            player_id: fighter.player_id,
        });
        cmd.entity(*entity).despawn();
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
        events.0.push(StarfighterEvent::MatchEnded {
            winner: result.winner,
        });
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
// el contrato Starfighter del repositorio Agentrix).
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

    #[test]
    fn starfighter_config_strict_validation() {
        let valid_json = serde_json::json!({
            "arena_width": 2000.0,
            "arena_height": 1000.0,
            "asteroid_count": 5,
            "radar_range": 1500.0,
            "ship_max_health": 100.0,
            "ship_max_energy": 100.0,
            "bullet_damage": 25.0,
            "asteroid_damage": 100.0,
            "shoot_energy_cost": 15.0,
            "shield_energy_cost_per_tick": 1.0,
            "shield_damage_reduction": 0.7,
            "energy_regen_per_tick": 0.5
        });
        let config = StarfighterConfig::from_value(&valid_json)
            .expect("valid config must pass");
        assert_eq!(config.asteroid_count, 5);

        // Unknown field must be rejected
        let with_unknown = serde_json::json!({
            "arena_width": 2000.0,
            "unknown_field": 123
        });
        assert!(StarfighterConfig::from_value(&with_unknown).is_err());

        // Negative dimension must be rejected
        let negative_dim = serde_json::json!({
            "arena_width": -100.0
        });
        assert!(StarfighterConfig::from_value(&negative_dim).is_err());
    }

    #[test]
    fn entity_id_allocator_enforces_limits_and_overflow() {
        let mut allocator = EntityIdAllocator::new(2);
        assert!(allocator.allocate().is_ok());
        assert!(allocator.allocate().is_ok());
        assert!(allocator.allocate().is_err()); // Exceeded limit

        let mut overflow_allocator = EntityIdAllocator {
            next: u64::MAX,
            active_count: 0,
            max_entities: 10,
        };
        assert!(overflow_allocator.allocate().is_err());
    }

    /// Dispara una bala del slot 1 hacia una nave del slot 0 ubicada a 500
    /// u y devuelve cuántos impactos recibió la nave en los ticks que la
    /// bala tarda en cruzarla (más un margen).
    fn hits_from_bullet_at_speed(speed: f32) -> usize {
        let settings = Settings {
            asteroid_count: 0,
            players: 0,
            ..default()
        };
        let mut app = build_app(settings);
        app.finish();
        app.cleanup();
        app.update(); // Startup

        spawn_fighter(
            &mut app.world_mut().commands(),
            0,
            Vec2::new(500.0, 0.0),
        );
        spawn_bullet(
            &mut app.world_mut().commands(),
            Vec2::new(0.0, 2.0),
            Vec2::new(speed, 0.0),
            600,
            1,
            StableEntityId(1_000_000),
        );
        app.world_mut().flush();

        let ticks_to_cross = (600.0 / (speed / 60.0)).ceil() as u32 + 2;
        for _ in 0..ticks_to_cross {
            app.update();
        }
        app.world()
            .resource::<TickEvents>()
            .0
            .iter()
            .filter(|event| {
                matches!(event, StarfighterEvent::Hit { player_id: 0, .. })
            })
            .count()
    }

    /// Criterio de aceptación de la Fase 0 y de F3 (ADR-0013): una bala no
    /// atraviesa una nave sin impactarla, y la impacta exactamente una vez.
    ///
    /// Con Avian2D, a ~80 000 u/s (~1300 u por tick, unas 40 veces el ancho
    /// de la nave) tanto la detección especulativa como `SweptCcd` fallaban
    /// (hallazgo de la Fase 0, RD-009). Con el barrido de trayectoria de F3
    /// la velocidad deja de importar: se prueba en el rango real del juego
    /// (3000 u/s, 50 u/tick), a 10 000 u/s y en ese caso extremo.
    #[test]
    fn fast_bullet_does_not_tunnel_through_fighter() {
        for speed in [3_000.0, 10_000.0, 80_000.0] {
            assert_eq!(
                hits_from_bullet_at_speed(speed),
                1,
                "una bala a {speed} u/s debe impactar la nave exactamente una vez"
            );
        }
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
            assert_eq!(
                entities.len(),
                2,
                "setup debe spawnear exactamente 2 naves"
            );

            for _ in 0..MAX_TICKS {
                let actions: Vec<FighterAction> = {
                    let mut rng = app.world_mut().resource_mut::<RngState>();
                    entities
                        .iter()
                        .map(|_| FighterAction {
                            thrust: match rng
                                .range_u32(0, 3)
                                .expect("valid test range")
                            {
                                0 => Thrust::On,
                                1 => Thrust::Off,
                                _ => Thrust::Stop,
                            },
                            turn: match rng
                                .range_u32(0, 3)
                                .expect("valid test range")
                            {
                                0 => Turn::Left,
                                1 => Turn::Right,
                                _ => Turn::None,
                            },
                            shoot: if rng
                                .probability(0.85)
                                .expect("valid probability")
                            {
                                Shoot::On
                            } else {
                                Shoot::Off
                            },
                            shield: if rng
                                .probability(0.2)
                                .expect("valid probability")
                            {
                                Shield::On
                            } else {
                                Shield::Off
                            },
                        })
                        .collect()
                };
                for (entity, action) in entities.iter().zip(actions) {
                    app.world_mut().write_message(FighterActionMessage {
                        action,
                        entity: *entity,
                    });
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
            build_perception(0, 0, radar_range, &[me, far], &[])
                .expect("player 0 vive");
        assert!(
            perception_far.rivals.is_empty(),
            "un rival a más de radar_range no debería aparecer en absoluto"
        );

        let near = snapshot(1, Vec2::new(radar_range - 1.0, 0.0));
        let perception_near =
            build_perception(0, 0, radar_range, &[me, near], &[])
                .expect("player 0 vive");
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

        let perception = build_perception(0, 0, 800.0, &[me, rival], &[])
            .expect("player 0 vive");
        assert_eq!(
            perception.rivals.len(),
            1,
            "el rival debe estar dentro de rango"
        );

        let value = serde_json::to_value(&perception)
            .expect("Perception debe serializar a JSON");
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
            myself_json.contains("energy")
                && myself_json.contains("remaining_bullet_cooldown"),
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
        spawn_fighter(
            &mut app.world_mut().commands(),
            1,
            Vec2::new(100.0, 0.0),
        );
        app.world_mut().flush();
        app.update();

        let fighters = app
            .world_mut()
            .run_system_once(
                |q: Query<(&Fighter, &Transform, &LinearVelocity)>| {
                    collect_fighter_snapshots(&q)
                },
            )
            .expect("run_system_once no debería fallar");
        assert_eq!(fighters.len(), 2, "deben leerse las 2 naves spawneadas");

        let perception = build_perception(0, 0, 800.0, &fighters, &[])
            .expect("player 0 debe existir");
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

    /// F1: una partida larga disparando sin parar no agota `max_entities`.
    /// Antes de liberar ids al despawnear, el cupo solo crecía y, al
    /// llenarse, `fighter_actions` dejaba de crear balas sin ningún error.
    /// El cupo (64) es muy inferior a las balas y asteroides creados en
    /// 10 000 ticks, así que el test solo pasa si los ids se liberan.
    #[test]
    fn long_match_shooting_never_exhausts_entity_budget() {
        const TICKS: u32 = 10_000;
        let settings = Settings {
            players: 0,
            max_entities: 64,
            config: StarfighterConfig {
                // Los asteroides chocan y se reciclan sin matar a la nave.
                asteroid_damage: 0.0,
                ..StarfighterConfig::default()
            },
            ..default()
        };
        let mut app = build_app(settings);
        app.finish();
        app.cleanup();
        app.update();
        let entity =
            spawn_fighter(&mut app.world_mut().commands(), 0, Vec2::ZERO);
        app.world_mut().flush();

        let fired = |app: &App| {
            app.world()
                .resource::<TickEvents>()
                .0
                .iter()
                .filter(|event| matches!(event, StarfighterEvent::Fired { .. }))
                .count()
        };
        let action = FighterAction {
            thrust: Thrust::Off,
            turn: Turn::Left,
            shoot: Shoot::On,
            shield: Shield::Off,
        };
        let mut fired_before_last_1000 = 0;
        for tick in 0..TICKS {
            if tick == TICKS - 1000 {
                fired_before_last_1000 = fired(&app);
            }
            app.world_mut()
                .write_message(FighterActionMessage { action, entity });
            app.update();
        }
        let total_fired = fired(&app);
        assert!(
            total_fired > 64,
            "el test debe disparar más balas que el cupo: {total_fired}"
        );
        assert!(
            total_fired > fired_before_last_1000,
            "la nave dejó de disparar en los últimos 1000 ticks ({total_fired} disparos en total)"
        );

        let live_allocated = app
            .world_mut()
            .query_filtered::<(), With<AllocatedId>>()
            .iter(app.world())
            .count() as u64;
        assert_eq!(
            app.world().resource::<EntityIdAllocator>().active_count(),
            live_allocated,
            "el cupo ocupado debe ser igual a las entidades vivas con id asignado"
        );
    }

    /// F1: una nave destruida por un asteroide no puede recibir además una
    /// bala en el mismo tick. El despawn por `Commands` es diferido, así que
    /// antes `take_hit` la encontraba de nuevo y emitía dos `Destroyed`.
    #[test]
    fn fighter_destroyed_once_when_hit_twice_in_same_tick() {
        let settings = Settings {
            players: 0,
            asteroid_count: 0,
            ..default()
        };
        let mut app = build_app(settings);
        app.finish();
        app.cleanup();
        app.update();

        let fighter =
            spawn_fighter(&mut app.world_mut().commands(), 0, Vec2::ZERO);
        let asteroid = app
            .world_mut()
            .spawn((
                Asteroid {
                    health: 2.0,
                    radius: 10.0,
                },
                EntityId(StableEntityId(1_000_000)),
                CollisionType::Asteroid,
            ))
            .id();
        let bullet = app
            .world_mut()
            .spawn((
                Bullet {
                    remaining_lifetime: 10,
                    player_id: 1,
                },
                EntityId(StableEntityId(1_000_001)),
                CollisionType::Bullet,
            ))
            .id();
        app.world_mut().flush();
        // Ambos impactos son letales por sí solos.
        app.world_mut().get_mut::<Fighter>(fighter).unwrap().health = 10.0;

        // El orden de reporte no importa: a igual instante de impacto se
        // procesan por EntityId.
        app.world_mut().resource_mut::<TickContacts>().0 = [bullet, asteroid]
            .into_iter()
            .map(|other| Contact {
                a: other,
                b: fighter,
                time_of_impact: 1.0,
            })
            .collect();
        app.world_mut()
            .run_system_once(detect_collisions)
            .expect("run_system_once no debería fallar");
        app.world_mut().flush();

        let events = &app.world().resource::<TickEvents>().0;
        let destroyed = events
            .iter()
            .filter(|event| matches!(event, StarfighterEvent::Destroyed { .. }))
            .count();
        assert_eq!(destroyed, 1, "eventos: {events:?}");
        assert!(
            events.iter().any(|event| matches!(
                event,
                StarfighterEvent::Destroyed { source, .. } if source == "an asteroid"
            )),
            "el asteroide (id menor) debe resolverse primero: {events:?}"
        );
        assert!(app.world().get_entity(fighter).is_err());
        assert!(
            app.world().get_entity(bullet).is_ok(),
            "la bala no impactó a nadie y debe seguir viva"
        );
    }

    /// Naves vivas de una app ya iniciada, ordenadas por `player_id`.
    fn fighters_by_slot(app: &mut App) -> Vec<(usize, Entity)> {
        let mut pairs: Vec<(usize, Entity)> = app
            .world_mut()
            .query::<(Entity, &Fighter)>()
            .iter(app.world())
            .map(|(entity, fighter)| (fighter.player_id, entity))
            .collect();
        pairs.sort_by_key(|(id, _)| *id);
        pairs
    }

    /// Criterio de F4 (ADR-0013): con 2 a 5 jugadores ninguna nave nace
    /// fuera de la arena, todas miran al centro y no hay dos en el mismo
    /// punto. El spawn circular anterior dejaba naves en y ≈ ±570 con
    /// `half_height` = 500 a partir de 3 jugadores.
    #[test]
    fn spawn_is_inside_arena_facing_center_for_2_to_5_players() {
        for players in 2..=5u32 {
            for seed in 0..8 {
                let settings = Settings {
                    seed,
                    players,
                    asteroid_count: 0,
                    ..default()
                };
                let (half_w, half_h) =
                    (settings.arena_half_width(), settings.arena_half_height());
                let mut app = build_app(settings);
                app.finish();
                app.cleanup();
                app.update();

                let fighters = fighters_by_slot(&mut app);
                assert_eq!(fighters.len(), players as usize);
                let mut positions = Vec::new();
                for (player_id, entity) in fighters {
                    let transform =
                        app.world().get::<Transform>(entity).unwrap();
                    let position = transform.translation.truncate();
                    assert!(
                        position.x.abs() <= half_w && position.y.abs() <= half_h,
                        "{players} jugadores, semilla {seed}: P{player_id} nace fuera de la arena en {position:?}"
                    );
                    let to_center = (-position).normalize();
                    let facing = facing_direction(transform);
                    assert!(
                        facing.dot(to_center) > 0.999,
                        "{players} jugadores, semilla {seed}: P{player_id} mira {facing:?}, el centro está en {to_center:?}"
                    );
                    assert!(
                        positions
                            .iter()
                            .all(|other: &Vec2| other.distance(position) > 100.0),
                        "{players} jugadores, semilla {seed}: dos naves nacen juntas"
                    );
                    positions.push(position);
                }
            }
        }
    }

    /// La asignación de puntos de spawn depende de la semilla: en 5
    /// jugadores, el slot 0 no nace siempre en el mismo lugar.
    #[test]
    fn spawn_assignment_is_permuted_by_seed() {
        let mut first_slot_positions = Vec::new();
        for seed in 0..16 {
            let mut app = build_app(Settings {
                seed,
                players: 5,
                asteroid_count: 0,
                ..default()
            });
            app.finish();
            app.cleanup();
            app.update();
            let (_, entity) = fighters_by_slot(&mut app)[0];
            let position =
                app.world().get::<Transform>(entity).unwrap().translation;
            if !first_slot_positions.contains(&position) {
                first_slot_positions.push(position);
            }
        }
        assert!(
            first_slot_positions.len() >= 3,
            "el slot 0 ocupó solo {} puntos distintos en 16 semillas",
            first_slot_positions.len()
        );
    }

    #[test]
    fn placements_follow_elimination_order_and_health() {
        let scoreboard = Scoreboard {
            eliminated_at: BTreeMap::from([(0, 50), (2, 80), (3, 80)]),
            kills: BTreeMap::from([(1, 2), (4, 1)]),
        };
        // Termina por límite de ticks con P1 y P4 vivos.
        let result = placements(5, &[(1, 40.0), (4, 75.0)], &scoreboard);
        let ranks: Vec<(usize, u32, u32)> = result
            .iter()
            .map(|p| (p.player_id, p.rank, p.kills))
            .collect();
        assert_eq!(
            ranks,
            vec![(4, 1, 1), (1, 2, 2), (2, 3, 0), (3, 3, 0), (0, 5, 0)],
            "vivos por salud, luego eliminados del más tardío al más temprano, empatando el mismo tick"
        );

        // Destrucción mutua en el último tick: los dos últimos empatan
        // primeros y no hay ganador único.
        let scoreboard = Scoreboard {
            eliminated_at: BTreeMap::from([(0, 10), (1, 90), (2, 90)]),
            kills: BTreeMap::new(),
        };
        let result = placements(3, &[], &scoreboard);
        assert_eq!(result[0].rank, 1);
        assert_eq!(result[1].rank, 1);
        assert_eq!(result[2].rank, 3);

        // Sobrevivientes con salud idéntica empatan.
        let result =
            placements(2, &[(0, 100.0), (1, 100.0)], &Scoreboard::default());
        assert!(result.iter().all(|p| p.rank == 1));
    }

    /// Una baja por bala queda atribuida al slot que disparó, con el tick
    /// de eliminación del estado en curso; un asteroide no suma bajas.
    #[test]
    fn bullet_kill_is_attributed_to_shooter() {
        let mut app = build_app(Settings {
            players: 0,
            asteroid_count: 0,
            ..default()
        });
        app.finish();
        app.cleanup();
        app.update();
        let fighter =
            spawn_fighter(&mut app.world_mut().commands(), 0, Vec2::ZERO);
        let bullet = app
            .world_mut()
            .spawn((
                Bullet {
                    remaining_lifetime: 10,
                    player_id: 3,
                },
                EntityId(StableEntityId(1_000_000)),
                CollisionType::Bullet,
            ))
            .id();
        app.world_mut().flush();
        app.world_mut().get_mut::<Fighter>(fighter).unwrap().health = 10.0;
        app.world_mut().resource_mut::<TickContacts>().0 = vec![Contact {
            a: bullet,
            b: fighter,
            time_of_impact: 0.5,
        }];
        app.world_mut()
            .run_system_once(detect_collisions)
            .expect("run_system_once no debería fallar");

        let events = &app.world().resource::<TickEvents>().0;
        assert!(
            events.contains(&StarfighterEvent::Destroyed {
                player_id: 0,
                source: "P3's bullet".to_string(),
                killer: Some(3),
            }),
            "eventos: {events:?}"
        );
        let scoreboard = app.world().resource::<Scoreboard>();
        assert_eq!(scoreboard.kills.get(&3), Some(&1));
        assert_eq!(scoreboard.eliminated_at.get(&0), Some(&0));
    }

    /// Criterio de F4 (ADR-0013): en partidas de 5 naves con acciones
    /// aleatorias, la tasa de victoria de cada slot cae dentro de 3σ de 1/5
    /// sobre las partidas con ganador único (último sobreviviente o, al
    /// agotar los ticks, única nave con más salud). Sin la permutación de
    /// spawn por semilla, un slot con un lugar sistemáticamente mejor se
    /// vería acá.
    ///
    /// **Marcado `#[ignore]`** por costo, igual que el test 1v1. Correrlo con
    /// `cargo test --release --lib symmetric_ffa -- --ignored --nocapture`.
    #[test]
    #[ignore = "estadístico, correr explícito en release"]
    fn symmetric_ffa_5_players_random_actions_no_slot_bias() {
        const PLAYERS: usize = 5;
        const N_MATCHES: u64 = 1000;
        const MAX_TICKS: u32 = 3600;
        const MIN_DECIDED: u32 = 300;

        let mut wins = [0u32; PLAYERS];
        let mut undecided = 0u32;
        for seed in 0..N_MATCHES {
            let mut app = build_app(Settings {
                seed,
                players: PLAYERS as u32,
                ..default()
            });
            app.finish();
            app.cleanup();
            app.update();
            let entities: Vec<Entity> = fighters_by_slot(&mut app)
                .into_iter()
                .map(|(_, entity)| entity)
                .collect();
            let mut rng = DeterministicRng::from_seed(seed ^ 0x5eed);
            for _ in 0..MAX_TICKS {
                for entity in &entities {
                    let action = FighterAction {
                        thrust: [Thrust::On, Thrust::Off, Thrust::Stop]
                            [rng.range_u32(0, 3).unwrap() as usize],
                        turn: [Turn::Left, Turn::Right, Turn::None]
                            [rng.range_u32(0, 3).unwrap() as usize],
                        shoot: if rng.probability(0.85).unwrap() {
                            Shoot::On
                        } else {
                            Shoot::Off
                        },
                        shield: if rng.probability(0.2).unwrap() {
                            Shield::On
                        } else {
                            Shield::Off
                        },
                    };
                    app.world_mut().write_message(FighterActionMessage {
                        action,
                        entity: *entity,
                    });
                }
                app.update();
                if app.world().resource::<MatchResult>().finished {
                    break;
                }
            }
            let alive: Vec<(usize, f32)> = app
                .world_mut()
                .query::<&Fighter>()
                .iter(app.world())
                .map(|fighter| (fighter.player_id, fighter.health))
                .collect();
            let result = placements(
                PLAYERS,
                &alive,
                app.world().resource::<Scoreboard>(),
            );
            match result.iter().filter(|p| p.rank == 1).collect::<Vec<_>>()[..]
            {
                [winner] => wins[winner.player_id] += 1,
                _ => undecided += 1,
            }
        }

        let decided: u32 = wins.iter().sum();
        println!(
            "symmetric_ffa: wins={wins:?} undecided={undecided} decided={decided}/{N_MATCHES}"
        );
        assert!(
            decided >= MIN_DECIDED,
            "muy pocas partidas con ganador único ({decided}/{N_MATCHES})"
        );
        let expected = 1.0 / PLAYERS as f64;
        let margin = 3.0 * (expected * (1.0 - expected) / decided as f64).sqrt();
        for (slot, slot_wins) in wins.iter().enumerate() {
            let rate = *slot_wins as f64 / decided as f64;
            println!(
                "symmetric_ffa: P{slot} tasa={rate:.4} esperado={expected:.2}±{margin:.4}"
            );
            assert!(
                (rate - expected).abs() <= margin,
                "P{slot} gana {:.1}% de las partidas decididas, fuera de {:.1}% ± {:.1} puntos",
                rate * 100.0,
                expected * 100.0,
                margin * 100.0
            );
        }
    }

    /// Fase 6: invariantes de física y reglas sobre entradas generadas al
    /// azar dentro de rangos que el propio motor puede producir -- no
    /// valores absurdos fuera de lo que un agente real podría causar.
    ///
    /// Nota sobre dos invariantes que el plan original sugería tal cual y
    /// que **no** son ciertas literalmente en esta implementación, así
    /// que se ajustaron (documentado acá, no en silencio):
    ///
    /// - "Una nave nunca queda con `health` negativa" -- `take_hit` resta
    ///   el daño sin clamp; una nave puede quedar transitoriamente en
    ///   negativo *antes* de despawnearse en el mismo llamado. Lo que sí
    ///   es cierto, y lo que se prueba acá, es que una nave que **sobrevive**
    ///   (sigue existiendo después de `take_hit`) nunca tiene
    ///   `health <= 0.0` -- si el daño era letal, fue despawneada.
    /// - "Una nave nunca termina fuera de los límites del arena" --
    ///   `check_boundary_collision` no teletransporta la posición, solo
    ///   invierte la componente de velocidad que apunta hacia afuera; la
    ///   posición puede estar momentáneamente más allá del borde en el
    ///   mismo tick en que se detecta. La propiedad real y verificable es
    ///   que no *diverge*: tras varios ticks de física real con velocidad
    ///   inicial hacia afuera, la nave se mantiene dentro de un margen
    ///   razonable del arena, no se escapa indefinidamente.
    mod proptests {
        use super::*;
        use proptest::prelude::*;

        fn spawn_one_fighter_app() -> (App, Entity) {
            let settings = Settings {
                players: 0,
                asteroid_count: 0,
                ..default()
            };
            let mut app = build_app(settings);
            app.finish();
            app.cleanup();
            app.update(); // Startup, sin naves automáticas (players: 0)
            let entity =
                spawn_fighter(&mut app.world_mut().commands(), 0, Vec2::ZERO);
            app.world_mut().flush();
            (app, entity)
        }

        fn fighter_action_strategy() -> impl Strategy<Value = FighterAction> {
            (
                prop_oneof![
                    Just(Thrust::On),
                    Just(Thrust::Off),
                    Just(Thrust::Stop)
                ],
                prop_oneof![
                    Just(Turn::Left),
                    Just(Turn::Right),
                    Just(Turn::None)
                ],
                any::<bool>(),
                any::<bool>(),
            )
                .prop_map(|(thrust, turn, shoot, shield)| {
                    FighterAction {
                        thrust,
                        turn,
                        shoot: if shoot { Shoot::On } else { Shoot::Off },
                        shield: if shield { Shield::On } else { Shield::Off },
                    }
                })
        }

        proptest! {
            #![proptest_config(ProptestConfig::with_cases(128))]

            /// Una nave que sobrevive a `take_hit` nunca queda con
            /// `health <= 0.0` -- si el daño era letal, se despawneó (ver
            /// nota de módulo sobre por qué esta es la forma correcta de
            /// la propiedad, no "health nunca negativa" a secas).
            #[test]
            fn take_hit_never_leaves_a_survivor_with_nonpositive_health(
                starting_health in 1.0f32..=200.0,
                damage in 0.0f32..=250.0,
            ) {
                let (mut app, entity) = spawn_one_fighter_app();
                if let Some(mut fighter) = app.world_mut().get_mut::<Fighter>(entity) {
                    fighter.health = starting_health;
                }
                app.world_mut()
                    .run_system_once(
                        move |mut cmd: Commands,
                              mut fighters: Query<&mut Fighter>,
                              mut destroyed: MessageWriter<FighterDestroyed>,
                              mut events: ResMut<TickEvents>,
                              mut scoreboard: ResMut<Scoreboard>| {
                            let mut ledger = HitLedger {
                                tick: 1,
                                events: &mut events,
                                scoreboard: &mut scoreboard,
                                destroyed: &mut destroyed,
                            };
                            take_hit(
                                &mut cmd,
                                &mut fighters,
                                entity,
                                HitSource {
                                    damage,
                                    shield_reduction: 0.7,
                                    description: "proptest",
                                    killer: None,
                                },
                                &mut ledger,
                            );
                        },
                    )
                    .expect("run_system_once no debería fallar");
                app.world_mut().flush();

                if let Some(fighter) = app.world().get::<Fighter>(entity) {
                    prop_assert!(
                        fighter.health > 0.0,
                        "nave sobrevivió con health <= 0: {}",
                        fighter.health
                    );
                }
                // `None` (despawneada): el daño fue letal, comportamiento correcto.
            }

            /// Ninguna secuencia de acciones reales (procesadas por los
            /// sistemas reales `fighter_actions`+`cooldowns`, no
            /// reimplementados acá) deja `energy` negativa -- el disparo
            /// exige `energy >= SHOOT_ENERGY_COST` antes de cobrarlo, y el
            /// escudo se apaga solo cuando no alcanza para sostenerlo un
            /// tick más (ver `cooldowns` en este mismo archivo).
            #[test]
            fn energy_never_goes_negative_across_random_action_sequences(
                actions in prop::collection::vec(fighter_action_strategy(), 1..=30)
            ) {
                let (mut app, entity) = spawn_one_fighter_app();
                for action in actions {
                    app.world_mut()
                        .write_message(FighterActionMessage { action, entity });
                    app.update();
                    let Some(fighter) = app.world().get::<Fighter>(entity) else {
                        // Naves que no reciben daño en este test no deberían
                        // despawnear nunca; si pasa, es una falla real.
                        prop_assert!(false, "la nave desapareció sin haber recibido daño");
                        break;
                    };
                    prop_assert!(
                        fighter.energy >= 0.0 && fighter.energy <= fighter.max_energy,
                        "energy fuera de [0, max_energy]: {}",
                        fighter.energy
                    );
                }
            }

            /// Una nave con velocidad inicial hacia afuera del arena no
            /// diverge tras varios ticks de física real -- la reflexión de
            /// `check_boundary_collision` la mantiene dentro de un margen
            /// razonable, no exacto (ver nota de módulo).
            #[test]
            fn boundary_reflection_prevents_runaway_divergence(
                start_x in -50.0f32..=50.0,
                start_y in -50.0f32..=50.0,
                outward_speed in 100.0f32..=1500.0,
                angle in 0.0f32..(std::f32::consts::PI * 2.0),
            ) {
                let (mut app, entity) = spawn_one_fighter_app();
                {
                    let mut transform = app.world_mut().get_mut::<Transform>(entity).unwrap();
                    transform.translation.x = start_x;
                    transform.translation.y = start_y;
                }
                {
                    let mut velocity = app.world_mut().get_mut::<LinearVelocity>(entity).unwrap();
                    velocity.0 = outward_speed * Vec2::new(angle.cos(), angle.sin());
                }
                for _ in 0..120 {
                    app.update();
                }
                let transform = app.world().get::<Transform>(entity).unwrap();
                let margin_x = ARENA_HALF_WIDTH * 1.5;
                let margin_y = ARENA_HALF_HEIGHT * 1.5;
                prop_assert!(
                    transform.translation.x.abs() <= margin_x
                        && transform.translation.y.abs() <= margin_y,
                    "la nave divergió del arena: ({}, {})",
                    transform.translation.x,
                    transform.translation.y
                );
            }
        }

        // Generalización con propiedades del test puntual de tunneling de
        // Fase 0 (`fast_bullet_does_not_tunnel_through_fighter`): en vez de
        // un único valor fijo (3000 u/s), cualquier velocidad dentro del
        // rango real del juego (1500-3000, el `bullet_speed` original era
        // 1500-2500) que efectivamente cruza la nave produce colisión.
        // Separado del bloque `proptest!` de arriba porque necesita menos
        // casos (cada uno corre física de verdad, no es gratis) -- un
        // `ProptestConfig` propio en vez de heredar el de 128 casos.
        proptest! {
            #![proptest_config(ProptestConfig::with_cases(20))]

            #[test]
            fn bullet_within_real_speed_range_never_tunnels(
                bullet_speed in 1500.0f32..=3000.0,
            ) {
                prop_assert_eq!(
                    hits_from_bullet_at_speed(bullet_speed),
                    1,
                    "una bala a {} u/s (rango real del juego) no impactó la nave exactamente una vez",
                    bullet_speed
                );
            }
        }
    }
}
