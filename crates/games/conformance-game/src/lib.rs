//! Minimal deterministic game used to prove that the host has no physics or
//! spatial-game assumptions. Each slot may increment its own counter; reaching
//! the configured target ends the match.

use agentrix_sim_core::{
    canonical_json_digest, ActionBatch, ActionStatus, CanonicalEncoder,
    DeterministicRng, GameDescriptor, GameKey, GameModule, SimError,
    Simulation, SimulationFrame, SimulationSpec,
};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::collections::BTreeMap;

pub const CONFORMANCE_GAME_ID: &str = "conformance-counter";
pub const CONFORMANCE_GAME_VERSION: &str = "1.0.0";
pub const CONFORMANCE_GAME_DIGEST: &str =
    "a258b3b53683aea9b6c18f74d777321d93734e14cd6634d12a86c149b2527aeb";
const ACTION_SCHEMA_DIGEST: &str =
    "b16f6b1187bc4f6d7ed828ecb10c7d6f969cc916dd1864d0126bb40fb9a69778";
const OBSERVATION_SCHEMA_DIGEST: &str =
    "4da8895cea567124c9f4ee615a1ca4c269e36bc0217a9cc66259cc0c4a18b74f";
const PUBLIC_SCHEMA_DIGEST: &str =
    "7f00e46289b4ea445b34b6ba52652e934400f40e85c510816497e0004bb1f980";

pub fn conformance_game_key() -> GameKey {
    GameKey {
        game_id: CONFORMANCE_GAME_ID.to_string(),
        game_version: CONFORMANCE_GAME_VERSION.to_string(),
        game_digest: CONFORMANCE_GAME_DIGEST.to_string(),
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
                min_players: 1,
                max_players: 8,
                action_schema_digest: ACTION_SCHEMA_DIGEST.to_string(),
                observation_schema_digest: OBSERVATION_SCHEMA_DIGEST
                    .to_string(),
                public_schema_digest: PUBLIC_SCHEMA_DIGEST.to_string(),
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
        spec: SimulationSpec,
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
        let counters =
            spec.slots.iter().map(|slot| (slot.clone(), 0)).collect();
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
    spec: SimulationSpec,
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
                    slot.clone(),
                    json!({
                        "schemaVersion": "conformance-observation/1",
                        "tick": self.tick,
                        "selfCounter": self.counters.get(slot).copied().unwrap_or(0),
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
        let highest = self.counters.values().copied().max().unwrap_or(0);
        let winners: Vec<_> = self
            .counters
            .iter()
            .filter_map(|(slot, score)| {
                (*score == highest).then_some(slot.clone())
            })
            .collect();
        let rankings: Vec<_> = self
            .counters
            .iter()
            .map(|(slot, score)| {
                json!({
                    "slot": slot,
                    "score": score,
                    "rank": if *score == highest { 1 } else { 2 },
                })
            })
            .collect();
        json!({
            "schemaVersion": "agentrix-game-result/1",
            "terminalReason": if self.tick >= self.spec.max_ticks { "tick_limit" } else { "score_limit" },
            "winner": if winners.len() == 1 { Some(&winners[0]) } else { None },
            "rankings": rankings,
        })
    }
}

impl Simulation for ConformanceSimulation {
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
        Ok(self.frame(Vec::new()))
    }

    fn advance(
        &mut self,
        actions: &ActionBatch,
    ) -> Result<SimulationFrame, SimError> {
        if !self.initial_emitted || self.terminal {
            return Err(SimError::new(
                "invalid_game_lifecycle",
                "game is not in a running state",
            ));
        }
        let mut events = Vec::new();
        for (slot, outcome) in actions {
            if outcome.status != ActionStatus::Valid {
                continue;
            }
            let applied = outcome.applied.clone().ok_or_else(|| {
                SimError::new(
                    "missing_applied_action",
                    format!("valid action for {slot} has no applied payload"),
                )
            })?;
            let action: CounterAction = serde_json::from_value(applied)
                .map_err(|error| {
                    SimError::new(
                        "invalid_action",
                        format!("slot {slot}: {error}"),
                    )
                })?;
            if !(-1..=1).contains(&action.increment) {
                return Err(SimError::new(
                    "invalid_action",
                    format!("slot {slot}: increment must be -1, 0, or 1"),
                ));
            }
            let counter = self.counters.get_mut(slot).ok_or_else(|| {
                SimError::new(
                    "unknown_action_slot",
                    format!("unknown slot {slot}"),
                )
            })?;
            *counter = counter.saturating_add(action.increment).max(0);
            events.push(json!({
                "code": "counter_changed",
                "actor": slot,
                "values": {"increment": action.increment, "counter": *counter},
            }));
        }
        // Advance a domain-separated hidden stream so RNG state is part of the
        // future even though the public counter game is intentionally simple.
        self.rng.next_u64();
        self.tick += 1;
        self.terminal = self.tick >= self.spec.max_ticks
            || self
                .counters
                .values()
                .any(|counter| *counter >= self.config.target);
        events.sort_by_key(|event| {
            event["actor"].as_str().unwrap_or("").to_string()
        });
        Ok(self.frame(events))
    }

    fn canonical_authoritative_state(&mut self) -> Result<Vec<u8>, SimError> {
        let mut encoder =
            CanonicalEncoder::new("conformance-authoritative-state/1");
        encoder.u64(self.tick);
        encoder.bool(self.terminal);
        encoder.i64(self.config.target);
        encoder.u64(self.counters.len() as u64);
        for (slot, counter) in &self.counters {
            encoder.string(slot);
            encoder.i64(*counter);
        }
        self.rng.encode_state(&mut encoder);
        Ok(encoder.into_bytes())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use agentrix_sim_core::{DeterminismTier, TickRate, RNG_ALGORITHM};
    use sha2::{Digest, Sha256};

    fn spec() -> SimulationSpec {
        let config = json!({"target": 2});
        SimulationSpec {
            game: conformance_game_key(),
            config_digest: canonical_json_digest("conformance-config", &config)
                .unwrap(),
            config,
            tick_rate: TickRate::new(60, 1).unwrap(),
            seed: 5,
            slots: vec!["alpha".to_string(), "beta".to_string()],
            max_ticks: 10,
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
    fn descriptor_digests_match_versioned_schema_bytes() {
        fn digest(bytes: &[u8]) -> String {
            Sha256::digest(bytes)
                .iter()
                .map(|byte| format!("{byte:02x}"))
                .collect()
        }
        let descriptor = ConformanceGame::new().descriptor;
        assert_eq!(
            descriptor.action_schema_digest,
            digest(include_bytes!(
                "../../../../schemas/games/conformance-counter/1.0.0/action.schema.json"
            ))
        );
        assert_eq!(
            descriptor.observation_schema_digest,
            digest(include_bytes!(
                "../../../../schemas/games/conformance-counter/1.0.0/observation.schema.json"
            ))
        );
        assert_eq!(
            descriptor.public_schema_digest,
            digest(include_bytes!(
                "../../../../schemas/games/conformance-counter/1.0.0/public.schema.json"
            ))
        );
    }
}
