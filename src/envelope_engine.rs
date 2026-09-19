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
    build_app, build_perception, collect_bullet_snapshots, collect_fighter_snapshots, Bullet,
    Fighter, FighterAction, FighterActionMessage, MatchResult, Settings, Shield, Shoot, Thrust,
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
use std::time::Duration;

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

#[derive(Debug, Deserialize)]
struct InitializeMatchRequest {
    #[serde(rename = "matchId")]
    match_id: String,
    #[serde(rename = "gameId", default)]
    #[allow(dead_code)]
    game_id: String,
    seed: i64,
    #[serde(rename = "fixedTimestepMs")]
    fixed_timestep_ms: i64,
    #[serde(rename = "maxTicks")]
    max_ticks: i64,
    players: Vec<String>,
    #[serde(default)]
    config: Option<Value>,
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

fn radar_range_from_config(config: &Option<Value>) -> f32 {
    let Some(Value::Object(map)) = config else {
        return 800.0;
    };
    match map.get("radar_range") {
        Some(Value::String(s)) => s.parse().unwrap_or(800.0),
        Some(Value::Number(n)) => n.as_f64().unwrap_or(800.0) as f32,
        _ => 800.0,
    }
}

fn read_snapshots(app: &mut App) -> (Vec<crate::FighterSnapshot>, Vec<crate::BulletSnapshot>) {
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
        if let Some(p) = build_perception(tick, player_id, state.radar_range, fighters, bullets) {
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
pub fn run_stdio_server() {
    let stdin = std::io::stdin();
    let mut stdout = std::io::stdout();
    let mut out_seq: u64 = 1;

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
                continue;
            }
        };

        match env.msg_type.as_str() {
            TYPE_INITIALIZE_MATCH => {
                let req: InitializeMatchRequest = match serde_json::from_value(env.payload) {
                    Ok(r) => r,
                    Err(e) => {
                        send_error(&mut stdout, &mut out_seq, &env.match_id, &e.to_string());
                        continue;
                    }
                };
                let radar_range = radar_range_from_config(&req.config);
                let tick_hz = if req.fixed_timestep_ms > 0 {
                    1000.0 / req.fixed_timestep_ms as f64
                } else {
                    60.0
                };
                let settings = Settings {
                    seed: req.seed as u64,
                    tick_hz,
                    players: req.players.len() as u32,
                    asteroid_count: 0,
                    continuous_collision_detection: true,
                    radar_range,
                };
                let mut app = build_app(settings);
                app.finish();
                app.cleanup();
                app.insert_resource(bevy::time::TimeUpdateStrategy::ManualDuration(
                    Duration::from_secs_f64(1.0 / tick_hz),
                ));
                app.update(); // Startup: spawnea las naves.

                let entities: Vec<Entity> = {
                    let mut query = app.world_mut().query::<(Entity, &Fighter)>();
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
                    radar_range,
                    max_ticks: req.max_ticks,
                    hash,
                    current_tick: 0,
                    final_state_hash: String::new(),
                    forced_winner: None,
                    end_reason: None,
                };
                let perceptions = build_all_perceptions(&match_state, 0, &fighters, &bullets);
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
                state = Some(match_state);
            }

            TYPE_ADVANCE_TICK => {
                let Some(ref mut st) = state else {
                    send_error(
                        &mut stdout,
                        &mut out_seq,
                        &env.match_id,
                        "match no inicializado",
                    );
                    continue;
                };
                let req: AdvanceTickRequest = match serde_json::from_value(env.payload) {
                    Ok(r) => r,
                    Err(e) => {
                        send_error(&mut stdout, &mut out_seq, &st.match_id, &e.to_string());
                        continue;
                    }
                };
                if req.tick != st.current_tick {
                    send_error(
                        &mut stdout,
                        &mut out_seq,
                        &st.match_id,
                        &format!(
                            "tick fuera de secuencia: recibido {}, esperado {}",
                            req.tick, st.current_tick
                        ),
                    );
                    continue;
                }

                let mut actions_this_tick: Vec<(Entity, FighterAction)> = Vec::new();
                for (player_id, go_id) in st.players.iter().enumerate() {
                    if let Some(input) = req.actions.get(go_id) {
                        if input.status == "disqualified" {
                            st.end_reason = Some(FinishReason::Timeout);
                            st.forced_winner = st
                                .players
                                .iter()
                                .find(|candidate| *candidate != go_id)
                                .cloned();
                        }
                    }
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
                    actions_this_tick.push((st.entities[player_id], action));
                }

                for (entity, action) in actions_this_tick {
                    st.app
                        .world_mut()
                        .write_message(FighterActionMessage { action, entity });
                }

                st.app.world_mut().resource_mut::<TickEvents>().0.clear();
                st.app.update();
                let events = st.app.world().resource::<TickEvents>().0.clone();
                let match_result = *st.app.world().resource::<MatchResult>();

                let (fighters, bullets) = read_snapshots(&mut st.app);
                let resulting_tick = req.tick + 1;
                st.current_tick = resulting_tick;
                let (public_fighters, public_bullets) = public_entities(st, &fighters, &bullets);
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
                let winner = st.forced_winner.clone().or_else(|| {
                    match_result
                        .winner
                        .and_then(|winner_id| st.players.get(winner_id).cloned())
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
                let Some(ref st) = state else {
                    send_error(
                        &mut stdout,
                        &mut out_seq,
                        &env.match_id,
                        "match no inicializado",
                    );
                    continue;
                };
                let req: FinishMatchRequest = match serde_json::from_value(env.payload) {
                    Ok(request) => request,
                    Err(error) => {
                        send_error(&mut stdout, &mut out_seq, &st.match_id, &error.to_string());
                        continue;
                    }
                };
                let match_result = *st.app.world().resource::<MatchResult>();
                let winner = st.forced_winner.clone().or_else(|| {
                    match_result
                        .winner
                        .and_then(|winner_id| st.players.get(winner_id).cloned())
                });
                let scores: BTreeMap<String, i64> = st
                    .players
                    .iter()
                    .map(|id| (id.clone(), if Some(id) == winner.as_ref() { 1 } else { 0 }))
                    .collect();
                let rankings = st
                    .players
                    .iter()
                    .map(|id| PlayerRank {
                        player_id: id.clone(),
                        rank: if Some(id) == winner.as_ref() { 1 } else { 2 },
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
                        reason: st.end_reason.clone().unwrap_or(req.reason),
                        winner,
                        scores,
                        rankings,
                        final_state_hash: st.final_state_hash.clone(),
                    },
                );
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
                send_error(
                    &mut stdout,
                    &mut out_seq,
                    &env.match_id,
                    &format!("unknown type: {other}"),
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
        payload: serde_json::to_value(payload).expect("payload siempre serializa"),
    };
    *seq += 1;
    let line = serde_json::to_string(&env).expect("Envelope siempre serializa");
    let _ = writeln!(stdout, "{line}");
    let _ = stdout.flush();
}

fn send_error(stdout: &mut std::io::Stdout, seq: &mut u64, match_id: &str, message: &str) {
    send(
        stdout,
        seq,
        match_id,
        TYPE_ENGINE_ERROR,
        &serde_json::json!({"code": "internal_error", "message": message, "fatal": false}),
    );
}
