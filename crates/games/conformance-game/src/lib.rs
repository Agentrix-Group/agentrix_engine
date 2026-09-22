//! Minimal deterministic game used to prove that the host has no physics or
//! spatial-game assumptions. Each slot may increment its own counter; reaching
//! the configured target ends the match.

use agentrix_sim_core::{
    canonical_json_digest, ActionBatch, ActionStatus, DeterministicRng,
    ExecutionSpec, GameDescriptor, GameKey, GameModule, GamePlayerLimits,
    GameSchemaDigests, SimError, Simulation, SimulationFrame,
};

use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::collections::BTreeMap;

pub const CONFORMANCE_GAME_ID: &str = "conformance-counter";
pub const CONFORMANCE_GAME_VERSION: &str = "1.0.0";
pub const ACTION_SCHEMA_DIGEST: &str =
    "b16f6b1187bc4f6d7ed828ecb10c7d6f969cc916dd1864d0126bb40fb9a69778";
pub const OBSERVATION_SCHEMA_DIGEST: &str =
    "4da8895cea567124c9f4ee615a1ca4c269e36bc0217a9cc66259cc0c4a18b74f";
pub const PUBLIC_SCHEMA_DIGEST: &str =
    "7f00e46289b4ea445b34b6ba52652e934400f40e85c510816497e0004bb1f980";

pub fn derive_conformance_game_digest() -> String {
    let manifest = json!({
        "gameId": CONFORMANCE_GAME_ID,
        "gameVersion": CONFORMANCE_GAME_VERSION,
        "schemas": {
            "action": ACTION_SCHEMA_DIGEST,
            "observation": OBSERVATION_SCHEMA_DIGEST,
            "public": PUBLIC_SCHEMA_DIGEST
        }
    });
    canonical_json_digest("agentrix.game-manifest/1", &manifest)
        .expect("game manifest must digest")
}

pub fn conformance_game_key() -> GameKey {
    GameKey {
        game_id: CONFORMANCE_GAME_ID.to_string(),
        game_version: CONFORMANCE_GAME_VERSION.to_string(),
        game_digest: derive_conformance_game_digest(),
    }
}

pub struct ConformanceGame {
    descriptor: GameDescriptor,
}

impl ConformanceGame {
    pub fn new() -> Self {
        Self {
            descriptor: GameDescriptor {
                key: conformance_game_key(),
                players: GamePlayerLimits {
                    minimum: 1,
                    maximum: 8,
                },
                schemas: GameSchemaDigests {
                    action: ACTION_SCHEMA_DIGEST.to_string(),
                    observation: OBSERVATION_SCHEMA_DIGEST.to_string(),
                    public: PUBLIC_SCHEMA_DIGEST.to_string(),
                },
                capabilities: vec![
                    "authoritative_commitment".to_string(),
                    "deterministic_batch_core".to_string(),
                    "structured_events".to_string(),
                ],
            },
        }
    }
}

impl Default for ConformanceGame {
    fn default() -> Self {
        Self::new()
    }
}

impl GameModule for ConformanceGame {
    fn descriptor(&self) -> &GameDescriptor {
        &self.descriptor
    }

    fn create(
        &self,
        spec: ExecutionSpec,
    ) -> Result<Box<dyn Simulation>, SimError> {
        if spec.game != self.descriptor.key {
            return Err(SimError::new(
                "game_identity_mismatch",
                "simulation spec does not match the conformance game identity",
            ));
        }
        let expected_config_digest =
            canonical_json_digest("conformance-config", &spec.config)?;
        if expected_config_digest != spec.config_digest {
            return Err(SimError::new(
                "config_digest_mismatch",
                "config digest does not match canonical config",
            ));
        }
        let config: Config = serde_json::from_value(spec.config.clone())
            .map_err(|error| {
                SimError::new("invalid_config", error.to_string())
            })?;
        if !(1..=1_000).contains(&config.target) {
            return Err(SimError::new(
                "invalid_config",
                "target must be between 1 and 1000",
            ));
        }
        let counters = spec
            .slots
            .iter()
            .map(|slot| (slot.slot_id.clone(), 0))
            .collect();
        let rng = DeterministicRng::from_seed(spec.seed);
        Ok(Box::new(ConformanceSimulation {
            spec,
            config,
            counters,
            rng,
            tick: 0,
            terminal: false,
            initial_emitted: false,
        }))
    }
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Config {
    target: i64,
}

#[derive(Debug, Clone, Copy, Deserialize)]
#[serde(deny_unknown_fields)]
struct CounterAction {
    increment: i64,
}

struct ConformanceSimulation {
    spec: ExecutionSpec,
    config: Config,
    counters: BTreeMap<String, i64>,
    rng: DeterministicRng,
    tick: u64,
    terminal: bool,
    initial_emitted: bool,
}

impl ConformanceSimulation {
    fn frame(&self, events: Vec<Value>) -> SimulationFrame {
        let public_snapshot = json!({
            "schemaVersion": "conformance-public/1",
            "tick": self.tick,
            "counters": self.counters,
        });
        let observations = self
            .spec
            .slots
            .iter()
            .map(|slot| {
                (
                    slot.slot_id.clone(),
                    json!({
                        "schemaVersion": "conformance-observation/1",
                        "tick": self.tick,
                        "selfCounter": self.counters.get(&slot.slot_id).copied().unwrap_or(0),
                        "target": self.config.target,
                    }),
                )
            })
            .collect();
        let result = self.terminal.then(|| self.result());
        SimulationFrame {
            tick: self.tick,
            public_snapshot,
            observations,
            events,
            terminal: self.terminal,
            result,
        }
    }

    fn result(&self) -> Value {
        let mut sorted_entries: Vec<(String, i64)> = self
            .counters
            .iter()
            .map(|(slot, score)| (slot.clone(), *score))
            .collect();
        // Deterministic ordering: score descending, then slot_id ascending
        sorted_entries
            .sort_by(|a, b| b.1.cmp(&a.1).then_with(|| a.0.cmp(&b.0)));

        let highest = sorted_entries.first().map(|e| e.1).unwrap_or(0);
        let winners: Vec<String> = sorted_entries
            .iter()
            .filter(|e| e.1 == highest)
            .map(|e| e.0.clone())
            .collect();

        // Standard competition ranking (1, 2, 3...)
        let mut rankings = Vec::new();
        let mut current_rank = 1;
        for (i, (slot, score)) in sorted_entries.iter().enumerate() {
            if i > 0 && *score < sorted_entries[i - 1].1 {
                current_rank = (i + 1) as u32;
            }
            rankings.push(json!({
                "slot": slot,
                "score": score,
                "rank": current_rank,
            }));
        }

        json!({
            "schemaVersion": "agentrix-game-result/1",
            "terminalReason": if self.tick >= self.spec.limits.max_ticks { "tick_limit" } else { "score_limit" },
            "winner": if winners.len() == 1 { Some(&winners[0]) } else { None },
            "rankings": rankings,
        })
    }
}

impl Simulation for ConformanceSimulation {
    fn spec(&self) -> &ExecutionSpec {
        &self.spec
    }

    fn initial_frame(&mut self) -> Result<SimulationFrame, SimError> {
        if self.initial_emitted {
            return Err(SimError::new(
                "initial_frame_repeated",
                "initial_frame can only be called once",
            ));
        }
        self.initial_emitted = true;
        Ok(self.frame(Vec::new()))
    }

    fn advance(
        &mut self,
        actions: &ActionBatch,
    ) -> Result<SimulationFrame, SimError> {
        if !self.initial_emitted {
            return Err(SimError::new(
                "missing_initial_frame",
                "advance called before initial_frame",
            ));
        }
        if self.terminal {
            return Err(SimError::new(
                "terminal_simulation",
                "cannot advance completed simulation",
            ));
        }
        self.tick += 1;
        let mut events = Vec::new();
        for (slot, action) in actions {
            if action.status != ActionStatus::Valid {
                continue;
            }
            let Some(applied) = &action.applied else {
                continue;
            };
            let parsed: CounterAction = serde_json::from_value(applied.clone())
                .map_err(|error| {
                    SimError::new("invalid_action_payload", error.to_string())
                })?;
            if !(1..=10).contains(&parsed.increment) {
                return Err(SimError::new(
                    "action_out_of_bounds",
                    "increment must be between 1 and 10",
                ));
            }
            let counter = self.counters.get_mut(slot).ok_or_else(|| {
                SimError::new("unknown_slot", format!("slot {slot} not found"))
            })?;
            *counter += parsed.increment;
            events.push(json!({
                "schemaVersion": "conformance-event/1",
                "tick": self.tick,
                "slot": slot,
                "increment": parsed.increment,
                "current": *counter,
            }));
        }

        let target_reached = self
            .counters
            .values()
            .any(|counter| *counter >= self.config.target);
        if target_reached || self.tick >= self.spec.limits.max_ticks {
            self.terminal = true;
        }
        Ok(self.frame(events))
    }

    fn canonical_authoritative_state(&mut self) -> Result<Vec<u8>, SimError> {
        let mut encoder =
            agentrix_sim_core::CanonicalEncoder::new("conformance-state/1");
        encoder.u64(self.tick);
        encoder.bool(self.terminal);
        self.rng.encode_state(&mut encoder);
        encoder.u64(self.counters.len() as u64);
        for (slot, count) in &self.counters {
            encoder.string(slot);
            encoder.i64(*count);
        }
        Ok(encoder.into_bytes())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use agentrix_sim_core::{
        canonical_json_digest, DeterminismTier, ExecutionLimits,
        ExecutionSlotSpec, SchemaDigests, TickRate, PROTOCOL_VERSION,
        RNG_ALGORITHM,
    };
    use serde_json::json;
    use sha2::{Digest, Sha256};

    fn spec() -> ExecutionSpec {
        let config = json!({"target": 3});
        ExecutionSpec {
            protocol_version: PROTOCOL_VERSION.to_string(),
            run_id: "run-conf-1".to_string(),
            match_id: "match-conf-1".to_string(),
            engine_version: "0.3.0".to_string(),
            engine_digest: "a".repeat(64),
            build_identity: "test-build".to_string(),
            target: "x86_64-unknown-linux-gnu".to_string(),
            game: conformance_game_key(),
            schema_digests: SchemaDigests {
                action: ACTION_SCHEMA_DIGEST.to_string(),
                observation: OBSERVATION_SCHEMA_DIGEST.to_string(),
                public: PUBLIC_SCHEMA_DIGEST.to_string(),
                replay: "f".repeat(64),
            },
            config_digest: canonical_json_digest("conformance-config", &config)
                .unwrap(),
            config,
            tick_rate: TickRate::new(60, 1).unwrap(),
            seed: 5,
            slots: vec![
                ExecutionSlotSpec {
                    slot_id: "alpha".to_string(),
                    artifact_digest: "1".repeat(64),
                },
                ExecutionSlotSpec {
                    slot_id: "beta".to_string(),
                    artifact_digest: "2".repeat(64),
                },
            ],
            limits: ExecutionLimits {
                max_ticks: 10,
                max_players: 8,
                max_entities: 100,
                max_message_bytes: 65536,
            },
            failure_policy_version: "failure-policy/1".to_string(),
            determinism_tier: DeterminismTier::SameArtifactSameTarget,
            rng_algorithm: RNG_ALGORITHM.to_string(),
        }
    }

    #[test]
    fn config_and_actions_are_closed_and_strict() {
        let game = ConformanceGame::new();
        let mut bad_config = spec();
        bad_config.config = json!({"target": 2, "unknown": true});
        bad_config.config_digest =
            canonical_json_digest("conformance-config", &bad_config.config)
                .unwrap();
        assert!(game.create(bad_config).is_err());

        let mut simulation = game.create(spec()).unwrap();
        simulation.initial_frame().unwrap();
        let mut actions = ActionBatch::new();
        actions.insert(
            "alpha".to_string(),
            agentrix_sim_core::SlotAction {
                status: ActionStatus::Valid,
                requested: Some(json!({"increment": 1, "unknown": true})),
                applied: Some(json!({"increment": 1, "unknown": true})),
                policy_decision: "apply".to_string(),
                error_code: None,
            },
        );
        assert!(simulation.advance(&actions).is_err());
    }

    #[test]
    fn state_zero_does_not_advance_rng_or_tick() {
        let game = ConformanceGame::new();
        let mut first = game.create(spec()).unwrap();
        let before = first.canonical_authoritative_state().unwrap();
        let initial = first.initial_frame().unwrap();
        let after = first.canonical_authoritative_state().unwrap();
        assert_eq!(initial.tick, 0);
        assert_eq!(before, after);
    }

    #[test]
    fn generic_ranking_distinguishes_scores_and_breaks_ties() {
        let mut sim = ConformanceGame::new().create(spec()).unwrap();
        sim.initial_frame().unwrap();

        // Slot alpha increments by 3 (reaches target 3), beta increments by 1
        let mut actions = ActionBatch::new();
        actions.insert(
            "alpha".to_string(),
            agentrix_sim_core::SlotAction {
                status: ActionStatus::Valid,
                requested: Some(json!({"increment": 3})),
                applied: Some(json!({"increment": 3})),
                policy_decision: "apply".to_string(),
                error_code: None,
            },
        );
        actions.insert(
            "beta".to_string(),
            agentrix_sim_core::SlotAction {
                status: ActionStatus::Valid,
                requested: Some(json!({"increment": 1})),
                applied: Some(json!({"increment": 1})),
                policy_decision: "apply".to_string(),
                error_code: None,
            },
        );
        let frame = sim.advance(&actions).unwrap();
        assert!(frame.terminal);
        let res = frame.result.unwrap();
        assert_eq!(res["winner"], "alpha");
        let rankings = res["rankings"].as_array().unwrap();
        assert_eq!(rankings.len(), 2);
        assert_eq!(rankings[0]["slot"], "alpha");
        assert_eq!(rankings[0]["rank"], 1);
        assert_eq!(rankings[0]["score"], 3);
        assert_eq!(rankings[1]["slot"], "beta");
        assert_eq!(rankings[1]["rank"], 2);
        assert_eq!(rankings[1]["score"], 1);
    }

    #[test]
    fn descriptor_digests_match_versioned_schema_bytes() {
        fn digest(bytes: &[u8]) -> String {
            Sha256::digest(bytes)
                .iter()
                .map(|byte| format!("{byte:02x}"))
                .collect()
        }
        let descriptor = ConformanceGame::new().descriptor;
        assert_eq!(
            descriptor.schemas.action,
            digest(include_bytes!(
                "../../../../schemas/games/conformance-counter/1.0.0/action.schema.json"
            ))
        );
        assert_eq!(
            descriptor.schemas.observation,
            digest(include_bytes!(
                "../../../../schemas/games/conformance-counter/1.0.0/observation.schema.json"
            ))
        );
        assert_eq!(
            descriptor.schemas.public,
            digest(include_bytes!(
                "../../../../schemas/games/conformance-counter/1.0.0/public.schema.json"
            ))
        );
        assert_eq!(
            descriptor.key.game_digest,
            derive_conformance_game_digest()
        );
    }
}
