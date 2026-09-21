//! Protocolo motor↔Go de Agentrix (`Envelope`, `src/engine/types.go` en
//! el repositorio Go). Es la única entrada ejecutable del motor.
//!
//! Acá el motor (este proceso) **nunca habla con bots directamente**.
//! Go es quien maneja los bots (por su cuenta, protocolo separado) y le
//! manda a este proceso las acciones *ya resueltas* de todos los
//! jugadores en cada `advance_tick`. Este proceso solo simula y devuelve
//! percepciones/eventos/estado -- es un servidor que Go maneja como
//! cliente, nunca inicia nada por sí mismo.
//!
//! ## Contrato exacto (confirmado leyendo `src/engine/{types,subprocess,client}.go`
//! del repo real de Agentrix, no de memoria)
//!
//! - JSON Lines sobre stdin (comandos que llegan) / stdout (respuestas).
//! - Todo mensaje lleva `protocolVersion` (constante `"agentrix-engine/1"`,
//!   Go rechaza cualquier otro valor), `type`, `matchId`, `sequence`.
//! - **`sequence` de salida (el motor hacia Go) debe empezar en 1 y
//!   incrementar de a 1 en cada mensaje que el motor emite** -- Go lo
//!   valida estrictamente (`subprocess.go:406-408`,
//!   `env.Sequence != c.expectedRecvSeq` → error). El primer mensaje
//!   (`engine_ready`, no pedido por Go) ya cuenta como sequence 1.
//! - Nombres de campo en **camelCase**, no snake_case como el resto de
//!   los contratos de Agentrix -- este protocolo específico usa esa
//!   convención (confirmado en `types.go`), se respeta tal cual acá.
//!
//! ## `stateHash`
//!
//! Cada snapshot público alimenta un hash SHA-256 encadenado,
//! `hash_n = SHA256(hash_{n-1} || JSON(snapshot_n))`, sembrado con el
//! `match_id`. Go guarda el snapshot y el hash sin recalcularlos.

use crate::protocol::WireAction;
use crate::{
    build_app, build_perception, collect_bullet_snapshots,
    collect_fighter_snapshots, Bullet, Fighter, FighterAction,
    FighterActionMessage, MatchResult, Settings, Shield, Shoot, Thrust,
    TickEvents, Turn,
};
use avian2d::prelude::LinearVelocity;
use bevy::ecs::system::RunSystemOnce;
use bevy::prelude::*;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use sha2::{Digest, Sha256};
use std::collections::BTreeMap;
use std::io::{BufRead, Write};

pub const PROTOCOL_VERSION: &str = "agentrix-engine/1";

const TYPE_INITIALIZE_MATCH: &str = "initialize_match";
const TYPE_ADVANCE_TICK: &str = "advance_tick";
const TYPE_FINISH_MATCH: &str = "finish_match";
const TYPE_SHUTDOWN: &str = "shutdown";
const TYPE_ENGINE_READY: &str = "engine_ready";
const TYPE_MATCH_INITIALIZED: &str = "match_initialized";
const TYPE_TICK_COMPLETED: &str = "tick_completed";
const TYPE_MATCH_COMPLETED: &str = "match_completed";
const TYPE_ENGINE_ERROR: &str = "engine_error";
const TYPE_SHUTDOWN_ACK: &str = "shutdown_ack";

#[derive(Debug, Clone, Serialize, Deserialize)]
struct Envelope {
    #[serde(rename = "protocolVersion")]
    protocol_version: String,
    #[serde(rename = "type")]
    msg_type: String,
    #[serde(rename = "matchId")]
    match_id: String,
    sequence: u64,
    payload: Value,
}

/// Payload canónico de `initialize_match`. Cualquier campo desconocido o
/// faltante es un error: el motor no rellena valores por defecto.
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct InitializeMatchRequest {
    #[serde(rename = "matchId")]
    match_id: String,
    #[serde(rename = "gameId")]
    game_id: String,
    #[serde(rename = "gameVersion")]
    game_version: String,
    #[serde(rename = "expectedEngineVersion")]
    expected_engine_version: String,
    seed: i64,
    #[serde(rename = "tickRate")]
    tick_rate: crate::TickRate,
    #[serde(rename = "maxTicks")]
    max_ticks: i64,
    players: Vec<String>,
    config: Value,
    #[serde(rename = "participantArtifactDigests")]
    participant_artifact_digests: BTreeMap<String, String>,
}

impl InitializeMatchRequest {
    fn validate(
        &self,
    ) -> Result<crate::StarfighterConfig, (&'static str, String)> {
        if self.game_id != "starfighter" {
            return Err((
                "ERR_UNSUPPORTED_GAME",
                format!(
                    "game {} is not simulated by this engine",
                    self.game_id
                ),
            ));
        }
        if self.game_version.is_empty() {
            return Err((
                "ERR_INVALID_PAYLOAD",
                "gameVersion is required".into(),
            ));
        }
        if self.expected_engine_version != env!("CARGO_PKG_VERSION") {
            return Err((
                "ERR_ENGINE_VERSION_MISMATCH",
                format!(
                    "expected engine {}, this engine is {}",
                    self.expected_engine_version,
                    env!("CARGO_PKG_VERSION")
                ),
            ));
        }
        self.tick_rate
            .validate()
            .map_err(|e| ("ERR_INVALID_TICK_RATE", e))?;
        if self.max_ticks <= 0 {
            return Err((
                "ERR_INVALID_PAYLOAD",
                "maxTicks must be positive".into(),
            ));
        }
        if self.players.is_empty() {
            return Err((
                "ERR_INVALID_PLAYERS",
                "players list cannot be empty".into(),
            ));
        }
        let mut unique = std::collections::BTreeSet::new();
        for player in &self.players {
            if player.is_empty() || !unique.insert(player) {
                return Err((
                    "ERR_INVALID_PLAYERS",
                    "players must be unique and non-empty".into(),
                ));
            }
            let digest = self
                .participant_artifact_digests
                .get(player)
                .map(String::as_str)
                .unwrap_or("");
            if digest.len() != 64
                || !digest
                    .bytes()
                    .all(|b| b.is_ascii_hexdigit() && !b.is_ascii_uppercase())
            {
                return Err((
                    "ERR_INVALID_DIGESTS",
                    format!("player {player} has no sha256 artifact digest"),
                ));
            }
        }
        if self.participant_artifact_digests.len() != self.players.len() {
            return Err((
                "ERR_INVALID_DIGESTS",
                "artifact digests must cover exactly the players".into(),
            ));
        }
        crate::StarfighterConfig::from_value(&self.config)
            .map_err(|e| ("ERR_INVALID_CONFIG", e))
    }
}

#[derive(Debug, Serialize)]
struct MatchInitializedResult {
    #[serde(rename = "matchId")]
    match_id: String,
    #[serde(rename = "initialTick")]
    initial_tick: i64,
    #[serde(rename = "stateHash")]
    state_hash: String,
    #[serde(rename = "publicSnapshot")]
    public_snapshot: PublicSnapshot,
    perceptions: BTreeMap<String, crate::Perception>,
    events: Vec<String>,
}

#[derive(Debug, Deserialize)]
struct PlayerActionInput {
    status: String,
    #[serde(default)]
    payload: Option<Value>,
}

fn default_action() -> FighterAction {
    FighterAction {
        thrust: Thrust::Off,
        turn: Turn::None,
        shoot: Shoot::Off,
        shield: Shield::Off,
    }
}

#[derive(Debug, Deserialize)]
struct AdvanceTickRequest {
    #[allow(dead_code)]
    tick: i64,
    actions: BTreeMap<String, PlayerActionInput>,
}

#[derive(Debug, Clone, Copy, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
enum FinishReason {
    Eliminated,
    Timeout,
    ScoreLimit,
}

#[derive(Debug, Deserialize)]
struct FinishMatchRequest {
    reason: FinishReason,
}

#[derive(Debug, Serialize)]
struct TickResult {
    tick: i64,
    events: Vec<String>,
    #[serde(rename = "stateHash")]
    state_hash: String,
    #[serde(rename = "isOver")]
    is_over: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    winner: Option<String>,
    #[serde(rename = "publicSnapshot")]
    public_snapshot: PublicSnapshot,
    #[serde(rename = "perceptions", skip_serializing_if = "Option::is_none")]
    perceptions: Option<BTreeMap<String, crate::Perception>>,
}

#[derive(Debug, Clone, Serialize)]
struct PublicFighter {
    #[serde(rename = "playerId")]
    player_id: String,
    position: crate::Vec2Data,
    velocity: crate::Vec2Data,
    rotation: f32,
    health: f32,
    #[serde(rename = "shieldActive")]
    shield_active: bool,
}

#[derive(Debug, Clone, Serialize)]
struct PublicBullet {
    #[serde(rename = "playerId")]
    player_id: String,
    position: crate::Vec2Data,
    velocity: crate::Vec2Data,
}

#[derive(Debug, Clone, Serialize)]
struct PublicSnapshot {
    tick: i64,
    fighters: Vec<PublicFighter>,
    bullets: Vec<PublicBullet>,
    events: Vec<String>,
    #[serde(rename = "stateHash")]
    state_hash: String,
}

#[derive(Debug, Serialize)]
struct PlayerRank {
    #[serde(rename = "playerId")]
    player_id: String,
    rank: i64,
    score: i64,
}

#[derive(Debug, Serialize)]
struct MatchResultPayload {
    #[serde(rename = "finalTick")]
    final_tick: i64,
    reason: FinishReason,
    #[serde(skip_serializing_if = "Option::is_none")]
    winner: Option<String>,
    scores: BTreeMap<String, i64>,
    rankings: Vec<PlayerRank>,
    #[serde(rename = "finalStateHash")]
    final_state_hash: String,
}

#[derive(Debug, Serialize)]
struct EngineReadyPayload {
    #[serde(rename = "engineVersion")]
    engine_version: String,
    #[serde(rename = "supportedProtocols")]
    supported_protocols: Vec<String>,
}

/// Digest encadenado del estado público que Agentrix guarda por tick.
struct ChainedHash {
    running: Vec<u8>,
}

impl ChainedHash {
    fn new(seed: &str) -> Self {
        ChainedHash {
            running: Sha256::digest(seed.as_bytes()).to_vec(),
        }
    }

    fn push(&mut self, data: &[u8]) -> String {
        let mut hasher = Sha256::new();
        hasher.update(&self.running);
        hasher.update(data);
        self.running = hasher.finalize().to_vec();
        self.running.iter().map(|b| format!("{b:02x}")).collect()
    }
}

/// Estado vivo de una partida en curso: la `App` de Bevy headless, el
/// orden jugador-string→player_id interno, y el hash encadenado.
struct MatchState {
    app: App,
    match_id: String,
    /// `players[i]` es el ID de Go para el jugador con `player_id`
    /// interno `i` -- mismo orden en el que llegaron en
    /// `InitializeMatchRequest.players`.
    players: Vec<String>,
    entities: Vec<Entity>,
    radar_range: f32,
    max_ticks: i64,
    hash: ChainedHash,
    current_tick: i64,
    final_state_hash: String,
    forced_winner: Option<String>,
    end_reason: Option<FinishReason>,
}

fn read_snapshots(
    app: &mut App,
) -> (Vec<crate::FighterSnapshot>, Vec<crate::BulletSnapshot>) {
    let fighters = app
        .world_mut()
        .run_system_once(|q: Query<(&Fighter, &Transform, &LinearVelocity)>| {
            collect_fighter_snapshots(&q)
        })
        .expect("run_system_once no debería fallar leyendo naves");
    let bullets = app
        .world_mut()
        .run_system_once(|q: Query<(&Bullet, &Transform, &LinearVelocity)>| {
            collect_bullet_snapshots(&q)
        })
        .expect("run_system_once no debería fallar leyendo balas");
    (fighters, bullets)
}

fn build_all_perceptions(
    state: &MatchState,
    tick: u64,
    fighters: &[crate::FighterSnapshot],
    bullets: &[crate::BulletSnapshot],
) -> BTreeMap<String, crate::Perception> {
    let mut out = BTreeMap::new();
    for (player_id, go_id) in state.players.iter().enumerate() {
        if let Some(p) = build_perception(
            tick,
            player_id,
            state.radar_range,
            fighters,
            bullets,
        ) {
            out.insert(go_id.clone(), p);
        }
    }
    out
}

fn public_entities(
    state: &MatchState,
    fighters: &[crate::FighterSnapshot],
    bullets: &[crate::BulletSnapshot],
) -> (Vec<PublicFighter>, Vec<PublicBullet>) {
    let public_fighters = fighters
        .iter()
        .filter_map(|fighter| {
            state
                .players
                .get(fighter.player_id)
                .map(|id| PublicFighter {
                    player_id: id.clone(),
                    position: fighter.position.into(),
                    velocity: fighter.velocity.into(),
                    rotation: fighter.facing.y.atan2(fighter.facing.x),
                    health: fighter.health,
                    shield_active: fighter.shield_active,
                })
        })
        .collect();
    let public_bullets = bullets
        .iter()
        .filter_map(|bullet| {
            state.players.get(bullet.player_id).map(|id| PublicBullet {
                player_id: id.clone(),
                position: bullet.position.into(),
                velocity: bullet.velocity.into(),
            })
        })
        .collect();
    (public_fighters, public_bullets)
}

fn snapshot_hash_input(
    tick: i64,
    fighters: &[PublicFighter],
    bullets: &[PublicBullet],
    events: &[String],
) -> Vec<u8> {
    serde_json::to_vec(&(tick, fighters, bullets, events)).unwrap_or_default()
}

/// Loop principal: lee `Envelope`s de stdin, despacha, escribe la
/// respuesta a stdout. Bloqueante y de un solo hilo a propósito -- Go
/// habla con un motor a la vez, secuencialmente (`AdvanceTick` espera la
/// respuesta antes de mandar el siguiente).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EngineLifecycleState {
    Ready,
    Initialized,
    Running,
    Finished,
    Shutdown,
}

pub fn run_stdio_server() {
    let stdin = std::io::stdin();
    let mut stdout = std::io::stdout();
    let mut out_seq: u64 = 1;
    let mut expected_in_seq: u64 = 1;
    let mut lifecycle = EngineLifecycleState::Ready;

    send(
        &mut stdout,
        &mut out_seq,
        "",
        TYPE_ENGINE_READY,
        &EngineReadyPayload {
            engine_version: env!("CARGO_PKG_VERSION").to_string(),
            supported_protocols: vec![PROTOCOL_VERSION.to_string()],
        },
    );

    let mut state: Option<MatchState> = None;

    for line in stdin.lock().lines() {
        let Ok(line) = line else { break };
        if line.trim().is_empty() {
            continue;
        }
        let env: Envelope = match serde_json::from_str(&line) {
            Ok(e) => e,
            Err(e) => {
                tracing::error!(target: "platform", "envelope inválido de Go: {e}");
                send_error_with_code(
                    &mut stdout,
                    &mut out_seq,
                    "",
                    "ERR_INVALID_JSON",
                    &format!("malformed JSON envelope: {e}"),
                    true,
                );
                continue;
            }
        };

        if env.protocol_version != PROTOCOL_VERSION {
            send_error_with_code(
                &mut stdout,
                &mut out_seq,
                &env.match_id,
                "ERR_INCOMPATIBLE_VERSION",
                &format!(
                    "incompatible protocol version: expected {PROTOCOL_VERSION}, got {}",
                    env.protocol_version
                ),
                true,
            );
            continue;
        }

        if env.sequence != expected_in_seq {
            send_error_with_code(
                &mut stdout,
                &mut out_seq,
                &env.match_id,
                "ERR_INVALID_SEQUENCE",
                &format!(
                    "invalid sequence: expected {expected_in_seq}, got {}",
                    env.sequence
                ),
                true,
            );
            continue;
        }
        expected_in_seq += 1;

        match env.msg_type.as_str() {
            TYPE_INITIALIZE_MATCH => {
                if lifecycle != EngineLifecycleState::Ready {
                    send_error_with_code(
                        &mut stdout,
                        &mut out_seq,
                        &env.match_id,
                        "ERR_INVALID_STATE",
                        &format!("cannot initialize match: engine is in state {lifecycle:?}"),
                        true,
                    );
                    continue;
                }
                if env.match_id.trim().is_empty() {
                    send_error_with_code(
                        &mut stdout,
                        &mut out_seq,
                        "",
                        "ERR_INVALID_MATCH_ID",
                        "matchId cannot be empty",
                        true,
                    );
                    continue;
                }
                let req: InitializeMatchRequest =
                    match serde_json::from_value(env.payload) {
                        Ok(r) => r,
                        Err(e) => {
                            send_error_with_code(
                                &mut stdout,
                                &mut out_seq,
                                &env.match_id,
                                "ERR_INVALID_PAYLOAD",
                                &e.to_string(),
                                true,
                            );
                            continue;
                        }
                    };
                if req.match_id != env.match_id {
                    send_error_with_code(
                        &mut stdout,
                        &mut out_seq,
                        &env.match_id,
                        "ERR_MATCH_ID_MISMATCH",
                        "matchId in payload does not match envelope",
                        true,
                    );
                    continue;
                }
                let starfighter_config = match req.validate() {
                    Ok(cfg) => cfg,
                    Err((code, message)) => {
                        send_error_with_code(
                            &mut stdout,
                            &mut out_seq,
                            &env.match_id,
                            code,
                            &message,
                            true,
                        );
                        continue;
                    }
                };
                let settings = Settings {
                    seed: req.seed as u64,
                    tick_hz: req.tick_rate.hz(),
                    players: req.players.len() as u32,
                    asteroid_count: starfighter_config.asteroid_count,
                    continuous_collision_detection: true,
                    radar_range: starfighter_config.radar_range,
                    config: starfighter_config.clone(),
                };
                let mut app = build_app(settings);
                app.finish();
                app.cleanup();
                app.insert_resource(
                    bevy::time::TimeUpdateStrategy::ManualDuration(
                        req.tick_rate.step(),
                    ),
                );
                app.update(); // Startup: spawnea las naves.

                let entities: Vec<Entity> = {
                    let mut query =
                        app.world_mut().query::<(Entity, &Fighter)>();
                    let mut pairs: Vec<(usize, Entity)> = query
                        .iter(app.world())
                        .map(|(e, f)| (f.player_id, e))
                        .collect();
                    pairs.sort_by_key(|(id, _)| *id);
                    pairs.into_iter().map(|(_, e)| e).collect()
                };

                let hash = ChainedHash::new(&req.match_id);
                let (fighters, bullets) = read_snapshots(&mut app);
                let mut match_state = MatchState {
                    app,
                    match_id: req.match_id.clone(),
                    players: req.players.clone(),
                    entities,
                    radar_range: starfighter_config.radar_range,
                    max_ticks: req.max_ticks,
                    hash,
                    current_tick: 0,
                    final_state_hash: String::new(),
                    forced_winner: None,
                    end_reason: None,
                };
                let perceptions =
                    build_all_perceptions(&match_state, 0, &fighters, &bullets);
                let (public_fighters, public_bullets) =
                    public_entities(&match_state, &fighters, &bullets);
                let state_hash = match_state.hash.push(&snapshot_hash_input(
                    0,
                    &public_fighters,
                    &public_bullets,
                    &[],
                ));
                match_state.final_state_hash = state_hash.clone();
                let public_snapshot = PublicSnapshot {
                    tick: 0,
                    fighters: public_fighters,
                    bullets: public_bullets,
                    events: vec![],
                    state_hash: state_hash.clone(),
                };

                send(
                    &mut stdout,
                    &mut out_seq,
                    &req.match_id,
                    TYPE_MATCH_INITIALIZED,
                    &MatchInitializedResult {
                        match_id: req.match_id.clone(),
                        initial_tick: 0,
                        state_hash,
                        public_snapshot,
                        perceptions,
                        events: vec![],
                    },
                );
                lifecycle = EngineLifecycleState::Initialized;
                state = Some(match_state);
            }

            TYPE_ADVANCE_TICK => {
                if lifecycle != EngineLifecycleState::Initialized
                    && lifecycle != EngineLifecycleState::Running
                {
                    send_error_with_code(
                        &mut stdout,
                        &mut out_seq,
                        &env.match_id,
                        "ERR_INVALID_STATE",
                        &format!(
                            "cannot advance tick: match is not active (state: {lifecycle:?})"
                        ),
                        true,
                    );
                    continue;
                }
                let Some(ref mut st) = state else {
                    continue;
                };
                if env.match_id != st.match_id {
                    send_error_with_code(
                        &mut stdout,
                        &mut out_seq,
                        &env.match_id,
                        "ERR_MATCH_ID_MISMATCH",
                        &format!(
                            "matchId mismatch: active match is {}, got {}",
                            st.match_id, env.match_id
                        ),
                        true,
                    );
                    continue;
                }
                let req: AdvanceTickRequest =
                    match serde_json::from_value(env.payload) {
                        Ok(r) => r,
                        Err(e) => {
                            send_error_with_code(
                                &mut stdout,
                                &mut out_seq,
                                &st.match_id,
                                "ERR_INVALID_PAYLOAD",
                                &e.to_string(),
                                false,
                            );
                            continue;
                        }
                    };
                if req.tick != st.current_tick {
                    send_error_with_code(
                        &mut stdout,
                        &mut out_seq,
                        &st.match_id,
                        "ERR_TICK_OUT_OF_SEQUENCE",
                        &format!(
                            "tick fuera de secuencia: recibido {}, esperado {}",
                            req.tick, st.current_tick
                        ),
                        false,
                    );
                    continue;
                }

                let mut actions_this_tick: Vec<(Entity, FighterAction)> =
                    Vec::new();
                let mut disqualified: Vec<String> = Vec::new();
                for go_id in &st.players {
                    if let Some(input) = req.actions.get(go_id) {
                        if input.status == "disqualified" {
                            disqualified.push(go_id.clone());
                        }
                    }
                }
                if disqualified.len() >= st.players.len()
                    && !st.players.is_empty()
                {
                    st.end_reason = Some(FinishReason::Timeout);
                    st.forced_winner = None;
                } else if let Some(dq_id) = disqualified.first() {
                    st.end_reason = Some(FinishReason::Timeout);
                    st.forced_winner = st
                        .players
                        .iter()
                        .find(|candidate| *candidate != dq_id)
                        .cloned();
                }

                for (player_id, go_id) in st.players.iter().enumerate() {
                    let action = req
                        .actions
                        .get(go_id)
                        .and_then(|input| {
                            if input.status != "valid" {
                                tracing::warn!(target: "agent", player_id, status = %input.status, "acción no válida, se usa acción por defecto");
                                return None;
                            }
                            let payload = input.payload.clone()?;
                            match serde_json::from_value::<WireAction>(payload) {
                                Ok(wire) => Some(FighterAction::from(wire)),
                                Err(e) => {
                                    tracing::warn!(target: "agent", player_id, "payload de acción malformado ({e}), se usa acción por defecto");
                                    None
                                }
                            }
                        })
                        .unwrap_or_else(default_action);
                    if let Some(&entity) = st.entities.get(player_id) {
                        actions_this_tick.push((entity, action));
                    }
                }

                let (events, match_result) = {
                    let mut writer = st
                        .app
                        .world_mut()
                        .resource_mut::<Messages<FighterActionMessage>>();
                    for (entity, action) in actions_this_tick {
                        writer.write(FighterActionMessage { action, entity });
                    }
                    st.app.world_mut().resource_mut::<TickEvents>().0.clear();
                    st.app.update();
                    let events =
                        st.app.world().resource::<TickEvents>().0.clone();
                    let result = *st.app.world().resource::<MatchResult>();
                    (events, result)
                };

                let resulting_tick = st.current_tick + 1;
                st.current_tick = resulting_tick;

                let (fighters, bullets) = read_snapshots(&mut st.app);
                let (public_fighters, public_bullets) =
                    public_entities(st, &fighters, &bullets);
                let state_hash = st.hash.push(&snapshot_hash_input(
                    resulting_tick,
                    &public_fighters,
                    &public_bullets,
                    &events,
                ));
                st.final_state_hash = state_hash.clone();
                let public_snapshot = PublicSnapshot {
                    tick: resulting_tick,
                    fighters: public_fighters,
                    bullets: public_bullets,
                    events: events.clone(),
                    state_hash: state_hash.clone(),
                };
                if st.end_reason.is_none() {
                    if match_result.finished {
                        st.end_reason = Some(FinishReason::Eliminated);
                    } else if resulting_tick >= st.max_ticks {
                        st.end_reason = Some(FinishReason::ScoreLimit);
                    }
                }
                let is_over = st.end_reason.is_some();
                if is_over {
                    lifecycle = EngineLifecycleState::Finished;
                } else {
                    lifecycle = EngineLifecycleState::Running;
                }
                let winner = st.forced_winner.clone().or_else(|| {
                    match_result.winner.and_then(|winner_id| {
                        st.players.get(winner_id).cloned()
                    })
                });
                // Every simulated state carries its private perceptions and its
                // public replay snapshot in the same tick_completed envelope.
                // A destroyed fighter is naturally absent because it can no
                // longer perceive; surviving slots still receive State[N].
                let perceptions = Some(build_all_perceptions(
                    st,
                    resulting_tick as u64,
                    &fighters,
                    &bullets,
                ));
                send(
                    &mut stdout,
                    &mut out_seq,
                    &st.match_id,
                    TYPE_TICK_COMPLETED,
                    &TickResult {
                        tick: resulting_tick,
                        events,
                        state_hash,
                        is_over,
                        winner,
                        public_snapshot,
                        perceptions,
                    },
                );
            }

            TYPE_FINISH_MATCH => {
                if lifecycle != EngineLifecycleState::Initialized
                    && lifecycle != EngineLifecycleState::Running
                    && lifecycle != EngineLifecycleState::Finished
                {
                    send_error_with_code(
                        &mut stdout,
                        &mut out_seq,
                        &env.match_id,
                        "ERR_INVALID_STATE",
                        "cannot finish match: match not initialized",
                        true,
                    );
                    continue;
                }
                let Some(ref st) = state else {
                    continue;
                };
                if env.match_id != st.match_id {
                    send_error_with_code(
                        &mut stdout,
                        &mut out_seq,
                        &env.match_id,
                        "ERR_MATCH_ID_MISMATCH",
                        &format!(
                            "matchId mismatch: active match is {}, got {}",
                            st.match_id, env.match_id
                        ),
                        true,
                    );
                    continue;
                }
                let req: FinishMatchRequest =
                    match serde_json::from_value(env.payload) {
                        Ok(request) => request,
                        Err(error) => {
                            send_error_with_code(
                                &mut stdout,
                                &mut out_seq,
                                &st.match_id,
                                "ERR_INVALID_PAYLOAD",
                                &error.to_string(),
                                false,
                            );
                            continue;
                        }
                    };
                let match_result = *st.app.world().resource::<MatchResult>();
                let winner = st.forced_winner.clone().or_else(|| {
                    match_result.winner.and_then(|winner_id| {
                        st.players.get(winner_id).cloned()
                    })
                });
                let scores: BTreeMap<String, i64> = st
                    .players
                    .iter()
                    .map(|id| {
                        (
                            id.clone(),
                            if Some(id) == winner.as_ref() { 1 } else { 0 },
                        )
                    })
                    .collect();
                let rankings = st
                    .players
                    .iter()
                    .map(|id| PlayerRank {
                        player_id: id.clone(),
                        rank: if Some(id) == winner.as_ref() {
                            1
                        } else if winner.is_some() {
                            2
                        } else {
                            1
                        },
                        score: if Some(id) == winner.as_ref() { 1 } else { 0 },
                    })
                    .collect();
                send(
                    &mut stdout,
                    &mut out_seq,
                    &st.match_id,
                    TYPE_MATCH_COMPLETED,
                    &MatchResultPayload {
                        final_tick: st.current_tick,
                        reason: st.end_reason.unwrap_or(req.reason),
                        winner,
                        scores,
                        rankings,
                        final_state_hash: st.final_state_hash.clone(),
                    },
                );
                lifecycle = EngineLifecycleState::Finished;
            }

            TYPE_SHUTDOWN => {
                send(
                    &mut stdout,
                    &mut out_seq,
                    &env.match_id,
                    TYPE_SHUTDOWN_ACK,
                    &serde_json::json!({}),
                );
                break;
            }

            other => {
                tracing::error!(target: "platform", "tipo de mensaje desconocido de Go: {other}");
                send_error_with_code(
                    &mut stdout,
                    &mut out_seq,
                    &env.match_id,
                    "ERR_UNKNOWN_MESSAGE_TYPE",
                    &format!("unknown message type: {other}"),
                    true,
                );
            }
        }
    }
}

fn send<T: Serialize>(
    stdout: &mut std::io::Stdout,
    seq: &mut u64,
    match_id: &str,
    msg_type: &str,
    payload: &T,
) {
    let env = Envelope {
        protocol_version: PROTOCOL_VERSION.to_string(),
        msg_type: msg_type.to_string(),
        match_id: match_id.to_string(),
        sequence: *seq,
        payload: serde_json::to_value(payload)
            .expect("payload siempre serializa"),
    };
    *seq += 1;
    let line = serde_json::to_string(&env).expect("Envelope siempre serializa");
    let _ = writeln!(stdout, "{line}");
    let _ = stdout.flush();
}

fn send_error_with_code(
    stdout: &mut std::io::Stdout,
    seq: &mut u64,
    match_id: &str,
    code: &str,
    message: &str,
    fatal: bool,
) {
    send(
        stdout,
        seq,
        match_id,
        TYPE_ENGINE_ERROR,
        &serde_json::json!({
            "code": code,
            "message": message,
            "fatal": fatal
        }),
    );
}
