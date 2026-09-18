//! Protocolo motor↔Go real de Agentrix (`Envelope`, `src/engine/types.go`
//! en el repo Go) -- distinto y separado del protocolo bot↔runner de
//! Fase 4 (`protocol.rs`/`runner.rs`), que sigue existiendo intacto como
//! herramienta de testing/demo standalone (`native-launcher --scripts`).
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
//! No se reutiliza `replay::ReplaySealer` (Fase 5) directamente: su API
//! está diseñada para consumir-y-sellar una sola vez al final de la
//! partida, no para exponer un digest intermedio en cada tick sin
//! consumirse. En vez de extender esa API (riesgo de tocar código ya
//! verificado de Fase 5 para un caso de uso distinto), acá se
//! reimplementa la misma idea de forma independiente y más simple: un
//! hash SHA-256 encadenado, `hash_n = SHA256(hash_{n-1} || bincode(estado_n))`,
//! sembrado del `match_id`. Mismo principio que ATD-011 ("checksum
//! acumulado"), sin acoplar este módulo a la implementación interna del
//! sellado de replay.

use crate::protocol::WireAction;
use crate::runner::default_action;
use crate::{
    build_app, build_perception, collect_bullet_snapshots, collect_fighter_snapshots, Bullet,
    Fighter, FighterAction, FighterActionMessage, MatchResult, Settings, TickEvents,
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
    perceptions: BTreeMap<String, crate::Perception>,
    events: Vec<String>,
}

#[derive(Debug, Deserialize)]
struct PlayerActionInput {
    status: String,
    #[serde(default)]
    payload: Option<Value>,
}

#[derive(Debug, Deserialize)]
struct AdvanceTickRequest {
    #[allow(dead_code)]
    tick: i64,
    actions: BTreeMap<String, PlayerActionInput>,
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
    #[serde(rename = "perceptions", skip_serializing_if = "Option::is_none")]
    perceptions: Option<BTreeMap<String, crate::Perception>>,
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
    reason: String,
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

/// Digest encadenado independiente de `replay::ReplaySealer` -- ver nota
/// de módulo sobre por qué no se reutiliza esa API acá.
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
                };
                let perceptions = build_all_perceptions(&match_state, 0, &fighters, &bullets);
                let state_hash = match_state
                    .hash
                    .push(&bincode::serialize(&fighters.len()).unwrap_or_default());

                send(
                    &mut stdout,
                    &mut out_seq,
                    &req.match_id,
                    TYPE_MATCH_INITIALIZED,
                    &MatchInitializedResult {
                        match_id: req.match_id.clone(),
                        initial_tick: 0,
                        state_hash,
                        perceptions,
                        events: vec![],
                    },
                );
                state = Some(match_state);
            }

            TYPE_ADVANCE_TICK => {
                let Some(ref mut st) = state else {
                    send_error(&mut stdout, &mut out_seq, &env.match_id, "match no inicializado");
                    continue;
                };
                let req: AdvanceTickRequest = match serde_json::from_value(env.payload) {
                    Ok(r) => r,
                    Err(e) => {
                        send_error(&mut stdout, &mut out_seq, &st.match_id, &e.to_string());
                        continue;
                    }
                };

                let mut actions_this_tick: Vec<(Entity, FighterAction)> = Vec::new();
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
                let tick_num = req.tick;
                let state_hash = st.hash.push(&bincode::serialize(&events).unwrap_or_default());
                let is_over = match_result.finished || (tick_num as i64) >= st.max_ticks;
                let winner = match_result.winner.and_then(|w| st.players.get(w).cloned());

                if is_over {
                    let scores: BTreeMap<String, i64> = st
                        .players
                        .iter()
                        .enumerate()
                        .map(|(i, id)| (id.clone(), if Some(i) == match_result.winner { 1 } else { 0 }))
                        .collect();
                    let rankings: Vec<PlayerRank> = st
                        .players
                        .iter()
                        .enumerate()
                        .map(|(i, id)| PlayerRank {
                            player_id: id.clone(),
                            rank: if Some(i) == match_result.winner { 1 } else { 2 },
                            score: if Some(i) == match_result.winner { 1 } else { 0 },
                        })
                        .collect();
                    send(
                        &mut stdout,
                        &mut out_seq,
                        &st.match_id,
                        TYPE_MATCH_COMPLETED,
                        &MatchResultPayload {
                            final_tick: tick_num,
                            reason: if match_result.finished {
                                "match_result".to_string()
                            } else {
                                "max_ticks_reached".to_string()
                            },
                            winner,
                            scores,
                            rankings,
                            final_state_hash: state_hash,
                        },
                    );
                } else {
                    let perceptions = build_all_perceptions(st, tick_num as u64 + 1, &fighters, &bullets);
                    send(
                        &mut stdout,
                        &mut out_seq,
                        &st.match_id,
                        TYPE_TICK_COMPLETED,
                        &TickResult {
                            tick: tick_num,
                            events,
                            state_hash,
                            is_over: false,
                            winner: None,
                            perceptions: Some(perceptions),
                        },
                    );
                }
            }

            TYPE_FINISH_MATCH => {
                let Some(ref st) = state else {
                    send_error(&mut stdout, &mut out_seq, &env.match_id, "match no inicializado");
                    continue;
                };
                let match_result = *st.app.world().resource::<MatchResult>();
                let winner = match_result.winner.and_then(|w| st.players.get(w).cloned());
                let scores: BTreeMap<String, i64> = st
                    .players
                    .iter()
                    .enumerate()
                    .map(|(i, id)| (id.clone(), if Some(i) == match_result.winner { 1 } else { 0 }))
                    .collect();
                send(
                    &mut stdout,
                    &mut out_seq,
                    &st.match_id,
                    TYPE_MATCH_COMPLETED,
                    &MatchResultPayload {
                        final_tick: 0,
                        reason: "finish_match_requested".to_string(),
                        winner,
                        scores,
                        rankings: vec![],
                        final_state_hash: String::new(),
                    },
                );
            }

            TYPE_SHUTDOWN => {
                send(&mut stdout, &mut out_seq, &env.match_id, TYPE_SHUTDOWN_ACK, &serde_json::json!({}));
                break;
            }

            other => {
                tracing::error!(target: "platform", "tipo de mensaje desconocido de Go: {other}");
                send_error(&mut stdout, &mut out_seq, &env.match_id, &format!("unknown type: {other}"));
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
