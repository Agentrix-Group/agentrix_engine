//! Orquestación de una partida real contra procesos de bot externos
//! hablando el protocolo de agente (ATD-007) por stdin/stdout -- Fase 4.
//!
//! Principio de esta fase (RF-044, CA-005): un bot que no responde a
//! tiempo, manda JSON inválido, manda una forma inesperada, o su proceso
//! muere, nunca cuelga la partida ni tira el runner. Se le aplica
//! `default_action()` para ese tick y se loguea como advertencia de
//! `agent` -- nunca de `platform` ni `game` (vocabulario de ATD-016: el
//! origen del fallo se atribuye a quien corresponde).
//!
//! No usa async/tokio a propósito: un thread lector por proceso hijo
//! empujando líneas a un `mpsc::channel`, con `recv_timeout` en el loop
//! principal, alcanza para esto y no agrega una dependencia operativa
//! nueva (principio de "pocas dependencias operativas" del blueprint).

use crate::manifest::GameManifest;
use crate::protocol::{BotMessage, RunnerMessage, WireAction, PROTOCOL_VERSION};
use crate::replay::{BulletState, FighterState, FrameState, ReplayFrame, ReplaySealer};
use crate::{
    build_app, build_perception, collect_bullet_snapshots, collect_fighter_snapshots, Bullet,
    Fighter, FighterAction, FighterActionMessage, MatchResult, Settings, Shield, Shoot, Thrust,
    TickEvents, Turn,
};
use avian2d::prelude::LinearVelocity;
use bevy::ecs::system::RunSystemOnce;
use bevy::prelude::*;
use std::collections::BTreeMap;
use std::io::{BufRead, BufReader, Write};
use std::path::PathBuf;
use std::process::{Child, ChildStdin, Command, Stdio};
use std::sync::mpsc;
use std::thread;
use std::time::Duration;

/// Acción de emergencia cuando un bot no responde a tiempo, manda algo
/// inválido, o su proceso ya murió: todo apagado, nunca dispara ni gasta
/// energía. Es neutra a propósito -- ni castiga de más ni premia a quien
/// no pudo responder, simplemente esa nave no actúa ese tick.
///
/// `pub` porque `envelope_engine` (protocolo motor↔Go) la reutiliza para
/// el mismo caso (status != "valid" en un `PlayerActionInput") -- misma
/// semántica de "esa nave no actúa este tick", no se reimplementa.
pub fn default_action() -> FighterAction {
    FighterAction {
        thrust: Thrust::Off,
        turn: Turn::None,
        shoot: Shoot::Off,
        shield: Shield::Off,
    }
}

/// Conexión con el proceso de un bot. Una vez desconectada
/// (`disconnect`) queda así para el resto de la partida -- no se
/// reintenta relanzar un bot a mitad de partida.
struct BotHandle {
    player_id: usize,
    child: Option<Child>,
    stdin: Option<ChildStdin>,
    rx: Option<mpsc::Receiver<String>>,
}

impl BotHandle {
    fn spawn(player_id: usize, script: &std::path::Path) -> std::io::Result<Self> {
        let mut child = Command::new("python3")
            .arg(script)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()?;

        let stdin = child.stdin.take().expect("stdin fue pedido como piped");
        let stdout = child.stdout.take().expect("stdout fue pedido como piped");
        let stderr = child.stderr.take().expect("stderr fue pedido como piped");

        let (tx, rx) = mpsc::channel();
        thread::spawn(move || {
            for line in BufReader::new(stdout).lines() {
                match line {
                    Ok(l) => {
                        if tx.send(l).is_err() {
                            break;
                        }
                    }
                    Err(_) => break,
                }
            }
            // Al salir del loop (EOF o error de lectura) `tx` se dropea:
            // un `recv_timeout` posterior en el hilo principal devuelve
            // `Disconnected`, que se trata igual que un timeout.
        });

        thread::spawn(move || {
            for line in BufReader::new(stderr).lines().map_while(Result::ok) {
                tracing::debug!(target: "agent_stderr", player_id, "{line}");
            }
        });

        Ok(BotHandle {
            player_id,
            child: Some(child),
            stdin: Some(stdin),
            rx: Some(rx),
        })
    }

    fn disconnected(player_id: usize) -> Self {
        BotHandle {
            player_id,
            child: None,
            stdin: None,
            rx: None,
        }
    }

    fn is_connected(&self) -> bool {
        self.stdin.is_some()
    }

    /// Corta la conexión de forma permanente: mata el proceso si sigue
    /// vivo y deja de intentar hablarle. De acá en más siempre recibe
    /// `default_action()` para el resto de la partida.
    fn disconnect(&mut self, reason: &str) {
        if let Some(mut child) = self.child.take() {
            let _ = child.kill();
            let _ = child.wait();
        }
        self.stdin = None;
        self.rx = None;
        tracing::warn!(target: "agent", player_id = self.player_id, reason, "bot desconectado");
    }

    fn send(&mut self, msg: &RunnerMessage) {
        let Some(stdin) = self.stdin.as_mut() else {
            return;
        };
        let line = serde_json::to_string(msg).expect("RunnerMessage siempre serializa a JSON");
        if writeln!(stdin, "{line}").is_err() {
            self.disconnect("fallo al escribir en stdin (el proceso probablemente murió)");
        }
    }

    /// `None` si no llegó nada a tiempo, si ya está desconectado, o si
    /// el canal se cerró (el proceso murió) -- en los tres casos el
    /// llamador debe usar `default_action()`.
    fn recv_line(&mut self, timeout: Duration) -> Option<String> {
        let rx = self.rx.as_ref()?;
        match rx.recv_timeout(timeout) {
            Ok(line) => Some(line),
            Err(mpsc::RecvTimeoutError::Timeout) => {
                tracing::warn!(target: "agent", player_id = self.player_id, "timeout esperando respuesta");
                None
            }
            Err(mpsc::RecvTimeoutError::Disconnected) => {
                self.disconnect("canal de stdout cerrado (el proceso murió)");
                None
            }
        }
    }
}

pub struct MatchConfig {
    pub match_id: String,
    pub manifest_path: PathBuf,
    pub scripts: Vec<PathBuf>,
    pub seed: u64,
    pub timeout_ms: u64,
    pub ccd: bool,
    pub output_replay: Option<PathBuf>,
    /// Sobreescribe `manifest.max_ticks` cuando está presente. No forma
    /// parte del diseño original de la fase, pero es necesario para que
    /// los tests de aceptación de esta misma fase corran en segundos en
    /// vez de esperar 3600 ticks -- y de paso le sirve a un operador
    /// real para una partida de demostración más corta. `None` usa el
    /// valor real del manifest.
    pub max_ticks_override: Option<u32>,
}

#[derive(Debug, Clone)]
pub struct MatchOutcome {
    pub winner: Option<usize>,
    pub reason: String,
    pub ticks_run: u64,
}

pub fn run_match(config: MatchConfig) -> MatchOutcome {
    let manifest = match GameManifest::load(&config.manifest_path) {
        Ok(m) => m,
        Err(e) => {
            tracing::error!(target: "platform", "no se pudo cargar el manifest: {e}");
            return MatchOutcome {
                winner: None,
                reason: "manifest_load_failed".to_string(),
                ticks_run: 0,
            };
        }
    };

    let mut sealer = ReplaySealer::new(&config.match_id, config.seed);

    let players = config.scripts.len() as u32;
    let radar_range = manifest.setting_f32("radar_range").unwrap_or(800.0);
    let tick_hz = manifest
        .setting_f32("tick_hz")
        .map(|v| v as f64)
        .unwrap_or(60.0);
    let max_ticks = config.max_ticks_override.unwrap_or(manifest.max_ticks);

    let settings = Settings {
        seed: config.seed,
        tick_hz,
        players,
        // Una partida oficial con bots reales no necesita el ruido de
        // asteroides que sí usa el modo legado de demostración interna.
        // El motor lo sigue soportando (Settings::asteroid_count no se
        // eliminó), simplemente esta fase no lo activa por defecto.
        asteroid_count: 0,
        continuous_collision_detection: config.ccd,
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

    let timeout = Duration::from_millis(config.timeout_ms);
    let mut bots: Vec<BotHandle> = Vec::with_capacity(config.scripts.len());

    for (player_id, script) in config.scripts.iter().enumerate() {
        let mut handle = match BotHandle::spawn(player_id, script) {
            Ok(h) => h,
            Err(e) => {
                tracing::error!(target: "platform", player_id, "no se pudo lanzar el proceso del bot: {e}");
                bots.push(BotHandle::disconnected(player_id));
                continue;
            }
        };

        handle.send(&RunnerMessage::Handshake {
            protocol_version: PROTOCOL_VERSION.to_string(),
            match_id: config.match_id.clone(),
            player_id,
            seed: config.seed,
        });

        match handle.recv_line(timeout) {
            Some(line) => match serde_json::from_str::<BotMessage>(&line) {
                Ok(BotMessage::HandshakeAck { protocol_version })
                    if protocol_version == PROTOCOL_VERSION =>
                {
                    // Versión compatible, la conexión sigue en pie.
                }
                Ok(BotMessage::HandshakeAck { protocol_version }) => {
                    tracing::error!(
                        target: "agent",
                        player_id,
                        esperado = PROTOCOL_VERSION,
                        recibido = %protocol_version,
                        "versión de protocolo incompatible, se aborta la conexión"
                    );
                    handle.disconnect("versión de protocolo incompatible");
                }
                _ => {
                    tracing::error!(target: "agent", player_id, "handshake_ack inválido o inesperado, se aborta la conexión");
                    handle.disconnect("handshake_ack inválido o inesperado");
                }
            },
            None => {
                tracing::error!(target: "agent", player_id, "sin respuesta de handshake, se aborta la conexión");
                handle.disconnect("sin respuesta de handshake");
            }
        }

        if handle.is_connected() {
            handle.send(&RunnerMessage::Init {
                protocol_version: PROTOCOL_VERSION.to_string(),
                match_id: config.match_id.clone(),
                timeout_ms: config.timeout_ms,
                radar_range,
                max_ticks,
                tick_hz,
            });
        }

        bots.push(handle);
    }

    let mut tick: u64 = 0;
    let reason;
    loop {
        if app.world().resource::<MatchResult>().finished {
            reason = "match_result".to_string();
            break;
        }
        if tick >= max_ticks as u64 {
            reason = "max_ticks_reached".to_string();
            break;
        }

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

        let mut actions_this_tick: Vec<(Entity, FighterAction)> = Vec::new();
        let mut frame_actions: BTreeMap<String, WireAction> = BTreeMap::new();

        for (player_id, bot) in bots.iter_mut().enumerate() {
            let Some(perception) = build_perception(tick, player_id, radar_range, &fighters, &bullets)
            else {
                // Esta nave ya no existe (destruida) -- nada que
                // percibir ni actuar para este slot por el resto de la
                // partida.
                continue;
            };

            if bot.is_connected() {
                bot.send(&RunnerMessage::Perception {
                    protocol_version: PROTOCOL_VERSION.to_string(),
                    match_id: config.match_id.clone(),
                    perception,
                });
            }

            let action = if !bot.is_connected() {
                default_action()
            } else {
                match bot.recv_line(timeout) {
                    Some(line) => match serde_json::from_str::<BotMessage>(&line) {
                        Ok(BotMessage::Action {
                            tick: acked_tick,
                            action,
                        }) if acked_tick == tick => FighterAction::from(action),
                        Ok(_) => {
                            tracing::warn!(
                                target: "agent",
                                player_id,
                                tick,
                                "respuesta con tick equivocado o tipo inesperado, se usa acción por defecto"
                            );
                            default_action()
                        }
                        Err(e) => {
                            tracing::warn!(
                                target: "agent",
                                player_id,
                                tick,
                                "JSON inválido ({e}), se usa acción por defecto"
                            );
                            default_action()
                        }
                    },
                    // Timeout o desconexión: `recv_line` ya logueó el motivo.
                    None => default_action(),
                }
            };

            frame_actions.insert(player_id.to_string(), WireAction::from(action));
            actions_this_tick.push((entities[player_id], action));
        }

        for (entity, action) in actions_this_tick {
            app.world_mut()
                .write_message(FighterActionMessage { action, entity });
        }

        app.world_mut().resource_mut::<TickEvents>().0.clear();
        app.update();
        let tick_events = app.world().resource::<TickEvents>().0.clone();

        let post_fighters = app
            .world_mut()
            .run_system_once(|q: Query<(&Fighter, &Transform, &LinearVelocity)>| {
                collect_fighter_snapshots(&q)
            })
            .expect("run_system_once no debería fallar leyendo naves");
        let post_bullets = app
            .world_mut()
            .run_system_once(|q: Query<(&Bullet, &Transform, &LinearVelocity)>| {
                collect_bullet_snapshots(&q)
            })
            .expect("run_system_once no debería fallar leyendo balas");

        sealer.push_frame(ReplayFrame {
            tick,
            events: tick_events,
            state: FrameState {
                fighters: post_fighters.iter().map(FighterState::from).collect(),
                bullets: post_bullets.iter().map(BulletState::from).collect(),
            },
            actions: frame_actions,
        });

        tick += 1;
    }

    let final_winner = app.world().resource::<MatchResult>().winner;
    for bot in bots.iter_mut() {
        bot.send(&RunnerMessage::End {
            protocol_version: PROTOCOL_VERSION.to_string(),
            match_id: config.match_id.clone(),
            winner: final_winner,
            reason: reason.clone(),
        });
        bot.disconnect("fin de partida");
    }

    if let Some(path) = &config.output_replay {
        let player_labels: Vec<String> = (0..config.scripts.len()).map(|i| i.to_string()).collect();
        // Placeholder, no una fórmula real (ver doc de
        // `replay::MatchReplay::scores` -- `PE-005` sigue pendiente).
        let scores: BTreeMap<String, i64> = player_labels
            .iter()
            .map(|label| {
                let is_winner = final_winner.map(|w| w.to_string()) == Some(label.clone());
                (label.clone(), if is_winner { 1 } else { 0 })
            })
            .collect();

        let replay = sealer.seal(
            manifest.id.clone(),
            config.match_id.clone(),
            config.seed,
            player_labels,
            max_ticks,
            final_winner,
            scores,
        );

        match serde_json::to_string_pretty(&replay) {
            Ok(json) => match std::fs::write(path, json) {
                Ok(()) => {
                    tracing::info!(target: "platform", path = %path.display(), digest = %replay.digest, "replay sellado y escrito");
                }
                Err(e) => {
                    tracing::error!(target: "platform", path = %path.display(), "no se pudo escribir el replay: {e}");
                }
            },
            Err(e) => {
                tracing::error!(target: "platform", "no se pudo serializar el replay a JSON: {e}");
            }
        }
    }

    MatchOutcome {
        winner: final_winner,
        reason,
        ticks_run: tick,
    }
}
