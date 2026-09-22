//! Canonical stdio server for `agentrix-engine/2`.
//!
//! Provides the process-level boundary between Go orchestration and Rust simulation.
//! - Stdout receives strictly one valid JSON line per envelope.
//! - Stderr receives logs and diagnostics.
//! - Handshake begins with `engine_ready` (sequence 1).

use crate::game_module::StarfighterGame;
use agentrix_engine_host::{EngineHost, GameRegistry, HostLifecycle};
use agentrix_sim_core::{
    ActionBatch, ActionStatus, ExecutionSpec, SlotAction, PROTOCOL_VERSION,
};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use std::collections::BTreeMap;
use std::io::{BufRead, Write};
use std::sync::Arc;

pub const TYPE_ENGINE_READY: &str = "engine_ready";
pub const TYPE_INITIALIZE_MATCH: &str = "initialize_match";
pub const TYPE_MATCH_INITIALIZED: &str = "match_initialized";
pub const TYPE_ADVANCE_TICK: &str = "advance_tick";
pub const TYPE_TICK_COMPLETED: &str = "tick_completed";
pub const TYPE_FINISH_MATCH: &str = "finish_match";
pub const TYPE_MATCH_COMPLETED: &str = "match_completed";
pub const TYPE_SHUTDOWN: &str = "shutdown";
pub const TYPE_SHUTDOWN_ACK: &str = "shutdown_ack";
pub const TYPE_ENGINE_ERROR: &str = "engine_error";

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct V2Envelope {
    pub protocol_version: String,
    pub r#type: String,
    #[serde(default)]
    pub match_id: String,
    #[serde(default)]
    pub run_id: String,
    pub sequence: u64,
    pub payload: Value,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct AdvanceTickRequest {
    tick: u64,
    actions: BTreeMap<String, Value>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct FinishMatchRequest {
    #[serde(default)]
    reason: String,
}

pub fn compute_self_digest() -> Option<String> {
    if let Ok(env_digest) = std::env::var("AGENTRIX_ENGINE_DIGEST") {
        if env_digest.len() == 64 {
            return Some(env_digest.to_lowercase());
        }
    }
    if let Ok(exe_path) = std::env::current_exe() {
        if let Ok(bytes) = std::fs::read(exe_path) {
            let mut hasher = Sha256::new();
            hasher.update(&bytes);
            return Some(format!("{:x}", hasher.finalize()));
        }
    }
    None
}

pub struct StdioServer<R: BufRead, W: Write> {
    reader: R,
    writer: W,
    send_seq: u64,
    expected_recv_seq: u64,
    host: EngineHost,
    active_match_id: String,
    active_run_id: String,
}

impl<R: BufRead, W: Write> StdioServer<R, W> {
    pub fn new(reader: R, writer: W, registry: GameRegistry) -> Self {
        Self {
            reader,
            writer,
            send_seq: 1,
            expected_recv_seq: 1,
            host: EngineHost::new(registry),
            active_match_id: String::new(),
            active_run_id: String::new(),
        }
    }

    fn write_envelope(
        &mut self,
        msg_type: &str,
        payload: Value,
    ) -> Result<(), std::io::Error> {
        let env = V2Envelope {
            protocol_version: PROTOCOL_VERSION.to_string(),
            r#type: msg_type.to_string(),
            match_id: self.active_match_id.clone(),
            run_id: self.active_run_id.clone(),
            sequence: self.send_seq,
            payload,
        };
        self.send_seq += 1;
        let line =
            serde_json::to_string(&env).map_err(std::io::Error::other)?;
        writeln!(self.writer, "{line}")?;
        self.writer.flush()?;
        Ok(())
    }

    fn write_error(
        &mut self,
        code: &str,
        message: &str,
        fatal: bool,
    ) -> Result<(), std::io::Error> {
        self.write_envelope(
            TYPE_ENGINE_ERROR,
            json!({
                "code": code,
                "message": message,
                "fatal": fatal,
            }),
        )
    }

    pub fn run(&mut self) -> Result<(), std::io::Error> {
        // 1. Send engine_ready handshake
        let engine_digest =
            compute_self_digest().unwrap_or_else(|| "0".repeat(64));
        let descriptors = self.host.registry().descriptors();
        let ready_payload = json!({
            "engineVersion": "0.3.0",
            "supportedProtocols": [PROTOCOL_VERSION],
            "engineDigest": engine_digest,
            "buildIdentity": "starfighter-build",
            "target": "x86_64-unknown-linux-gnu",
            "games": descriptors,
            "capabilities": {
                "authoritative_commitment": true,
                "avian2d": true,
                "deterministic_core_d1": true,
            },
            "limits": {
                "maxTicks": 100_000,
                "maxPlayers": 64,
                "maxEntities": 100_000,
                "maxMessageBytes": 10 * 1024 * 1024,
            }
        });
        self.write_envelope(TYPE_ENGINE_READY, ready_payload)?;

        // 2. Process incoming line-delimited envelopes
        let mut line = String::new();
        loop {
            line.clear();
            let bytes_read = self.reader.read_line(&mut line)?;
            if bytes_read == 0 {
                // EOF from orchestrator
                break;
            }
            let trimmed = line.trim();
            if trimmed.is_empty() {
                continue;
            }

            let env: V2Envelope = match serde_json::from_str(trimmed) {
                Ok(e) => e,
                Err(err) => {
                    self.write_error(
                        "invalid_json",
                        &format!("failed to parse envelope: {err}"),
                        true,
                    )?;
                    return Err(std::io::Error::new(
                        std::io::ErrorKind::InvalidData,
                        err,
                    ));
                }
            };

            if env.protocol_version != PROTOCOL_VERSION {
                self.write_error(
                    "invalid_protocol_version",
                    &format!(
                        "expected protocol version {}, got {}",
                        PROTOCOL_VERSION, env.protocol_version
                    ),
                    true,
                )?;
                return Err(std::io::Error::new(
                    std::io::ErrorKind::InvalidData,
                    "incompatible protocol",
                ));
            }

            if env.sequence != self.expected_recv_seq {
                self.write_error(
                    "invalid_sequence",
                    &format!(
                        "expected sequence {}, got {}",
                        self.expected_recv_seq, env.sequence
                    ),
                    true,
                )?;
                return Err(std::io::Error::new(
                    std::io::ErrorKind::InvalidData,
                    "invalid sequence",
                ));
            }
            self.expected_recv_seq += 1;

            if !self.active_match_id.is_empty()
                && !env.match_id.is_empty()
                && env.match_id != self.active_match_id
            {
                self.write_error(
                    "match_id_mismatch",
                    &format!(
                        "envelope matchId {} does not match active match {}",
                        env.match_id, self.active_match_id
                    ),
                    true,
                )?;
                return Err(std::io::Error::new(
                    std::io::ErrorKind::InvalidData,
                    "match id mismatch",
                ));
            }

            match env.r#type.as_str() {
                TYPE_INITIALIZE_MATCH => {
                    if self.host.lifecycle() != HostLifecycle::Created {
                        self.write_error(
                            "invalid_lifecycle",
                            &format!(
                                "initialize_match is only valid in Created state, host is {:?}",
                                self.host.lifecycle()
                            ),
                            false,
                        )?;
                        continue;
                    }

                    let spec: ExecutionSpec =
                        if let Some(spec_val) = env.payload.get("spec") {
                            match serde_json::from_value(spec_val.clone()) {
                                Ok(s) => s,
                                Err(e) => {
                                    self.write_error(
                                        "invalid_execution_spec",
                                        &format!("failed to parse spec: {e}"),
                                        true,
                                    )?;
                                    return Err(std::io::Error::new(
                                        std::io::ErrorKind::InvalidData,
                                        e,
                                    ));
                                }
                            }
                        } else {
                            match serde_json::from_value(env.payload) {
                                Ok(s) => s,
                                Err(e) => {
                                    self.write_error(
                                        "invalid_execution_spec",
                                        &format!("failed to parse spec: {e}"),
                                        true,
                                    )?;
                                    return Err(std::io::Error::new(
                                        std::io::ErrorKind::InvalidData,
                                        e,
                                    ));
                                }
                            }
                        };

                    if !env.match_id.is_empty() && env.match_id != spec.match_id
                    {
                        self.write_error(
                            "match_id_mismatch",
                            &format!(
                                "envelope matchId {} does not match spec matchId {}",
                                env.match_id, spec.match_id
                            ),
                            true,
                        )?;
                        return Err(std::io::Error::new(
                            std::io::ErrorKind::InvalidData,
                            "match id mismatch",
                        ));
                    }

                    self.active_match_id = spec.match_id.clone();
                    self.active_run_id = spec.run_id.clone();

                    match self.host.initialize(spec) {
                        Ok(frame) => {
                            let frame_val = serde_json::to_value(frame)
                                .map_err(std::io::Error::other)?;
                            self.write_envelope(
                                TYPE_MATCH_INITIALIZED,
                                frame_val,
                            )?;
                        }
                        Err(e) => {
                            self.write_error(e.code, &e.message, true)?;
                            return Err(std::io::Error::other(e.message));
                        }
                    }
                }
                TYPE_ADVANCE_TICK => {
                    if self.host.lifecycle() != HostLifecycle::Running {
                        self.write_error(
                            "invalid_lifecycle",
                            &format!(
                                "advance_tick is only valid in Running state, host is {:?}",
                                self.host.lifecycle()
                            ),
                            true,
                        )?;
                        return Err(std::io::Error::new(
                            std::io::ErrorKind::InvalidData,
                            "invalid lifecycle",
                        ));
                    }

                    let req: AdvanceTickRequest =
                        match serde_json::from_value(env.payload) {
                            Ok(r) => r,
                            Err(e) => {
                                self.write_error(
                                    "invalid_advance_request",
                                    &format!(
                                        "failed to parse advance payload: {e}"
                                    ),
                                    true,
                                )?;
                                return Err(std::io::Error::new(
                                    std::io::ErrorKind::InvalidData,
                                    e,
                                ));
                            }
                        };

                    let mut batch = ActionBatch::new();
                    for (slot_id, action_val) in req.actions {
                        let slot_action: SlotAction = if action_val
                            .get("policyDecision")
                            .is_some()
                            || action_val.get("policy_decision").is_some()
                        {
                            match serde_json::from_value(action_val) {
                                Ok(sa) => sa,
                                Err(e) => {
                                    self.write_error(
                                        "invalid_slot_action",
                                        &format!("malformed slot action: {e}"),
                                        true,
                                    )?;
                                    return Err(std::io::Error::new(
                                        std::io::ErrorKind::InvalidData,
                                        e,
                                    ));
                                }
                            }
                        } else {
                            // Translate Go PlayerActionInput into SlotAction
                            let status_str = action_val
                                .get("status")
                                .and_then(Value::as_str)
                                .unwrap_or("valid");
                            let status = match status_str {
                                "valid" => ActionStatus::Valid,
                                "timeout" => ActionStatus::Timeout,
                                "invalid_output" => ActionStatus::InvalidOutput,
                                "crashed" => ActionStatus::Crashed,
                                "disqualified" => ActionStatus::Disqualified,
                                _ => ActionStatus::InvalidOutput,
                            };
                            let requested =
                                action_val.get("payload").cloned().or_else(
                                    || action_val.get("requested").cloned(),
                                );
                            let applied = if status == ActionStatus::Valid {
                                requested.clone()
                            } else {
                                None
                            };
                            let policy_decision =
                                if status == ActionStatus::Valid {
                                    "accepted".to_string()
                                } else {
                                    status_str.to_string()
                                };
                            let error_code = action_val
                                .get("errorDetails")
                                .or_else(|| action_val.get("error_code"))
                                .and_then(Value::as_str)
                                .map(|s| s.to_string());
                            SlotAction {
                                status,
                                requested,
                                applied,
                                policy_decision,
                                error_code,
                            }
                        };
                        batch.insert(slot_id, slot_action);
                    }

                    match self.host.advance(req.tick, &batch) {
                        Ok(frame) => {
                            let is_terminal = frame.terminal;
                            let frame_val = serde_json::to_value(frame)
                                .map_err(std::io::Error::other)?;
                            if is_terminal {
                                self.write_envelope(
                                    TYPE_MATCH_COMPLETED,
                                    frame_val,
                                )?;
                            } else {
                                self.write_envelope(
                                    TYPE_TICK_COMPLETED,
                                    frame_val,
                                )?;
                            }
                        }
                        Err(e) => {
                            self.write_error(e.code, &e.message, true)?;
                            return Err(std::io::Error::other(e.message));
                        }
                    }
                }
                TYPE_FINISH_MATCH => {
                    let req: FinishMatchRequest =
                        match serde_json::from_value(env.payload) {
                            Ok(r) => r,
                            Err(_) => FinishMatchRequest {
                                reason: "requested".to_string(),
                            },
                        };
                    let result_payload = json!({
                        "reason": req.reason,
                        "lifecycle": format!("{:?}", self.host.lifecycle()),
                    });
                    self.write_envelope(TYPE_MATCH_COMPLETED, result_payload)?;
                }
                TYPE_SHUTDOWN => {
                    self.write_envelope(TYPE_SHUTDOWN_ACK, json!({}))?;
                    return Ok(());
                }
                unknown => {
                    self.write_error(
                        "unknown_message_type",
                        &format!("unknown message type {unknown}"),
                        true,
                    )?;
                    return Err(std::io::Error::new(
                        std::io::ErrorKind::InvalidData,
                        format!("unknown message type {unknown}"),
                    ));
                }
            }
        }
        Ok(())
    }
}

pub fn run_stdio_server() {
    let mut registry = GameRegistry::new();
    registry
        .register(Arc::new(StarfighterGame::new()))
        .expect("failed to register StarfighterGame");
    let stdin = std::io::stdin();
    let stdout = std::io::stdout();
    let mut server = StdioServer::new(stdin.lock(), stdout.lock(), registry);
    if let Err(err) = server.run() {
        eprintln!("[engine-v2] stdio server exited with error: {err}");
        std::process::exit(1);
    }
}
