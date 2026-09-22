//! Game-agnostic in-process host for authoritative simulations.
//!
//! Transport adapters are intentionally outside this crate. The host knows
//! game descriptors and opaque payloads, never concrete game components or
//! actions.

use agentrix_sim_core::{
    ActionBatch, CommitmentChain, ExecutionSpec, GameDescriptor, GameKey,
    GameModule, SimError, Simulation, StateCommitments,
};

use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::BTreeMap;
use std::sync::Arc;

#[derive(Default)]
pub struct GameRegistry {
    games: BTreeMap<(String, String, String), Arc<dyn GameModule>>,
}

impl GameRegistry {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn register(
        &mut self,
        game: Arc<dyn GameModule>,
    ) -> Result<(), SimError> {
        let descriptor = game.descriptor();
        let key = registry_key(&descriptor.key);
        if self.games.contains_key(&key) {
            return Err(SimError::new(
                "duplicate_game",
                format!(
                    "duplicate compiled game {}/{}/{}",
                    key.0, key.1, key.2
                ),
            ));
        }
        self.games.insert(key, game);
        Ok(())
    }

    pub fn resolve(
        &self,
        key: &GameKey,
    ) -> Result<Arc<dyn GameModule>, SimError> {
        self.games.get(&registry_key(key)).cloned().ok_or_else(|| {
            SimError::new(
                "unknown_game",
                format!(
                    "no compiled game matches ({}, {}, {})",
                    key.game_id, key.game_version, key.game_digest
                ),
            )
        })
    }

    pub fn descriptors(&self) -> Vec<GameDescriptor> {
        self.games
            .values()
            .map(|game| game.descriptor().clone())
            .collect()
    }
}

fn registry_key(key: &GameKey) -> (String, String, String) {
    (
        key.game_id.clone(),
        key.game_version.clone(),
        key.game_digest.clone(),
    )
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum HostLifecycle {
    Created,
    Initialized,
    Running,
    Completed,
    Failed,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct HostFrame {
    pub tick: u64,
    pub public_snapshot: Value,
    pub observations: BTreeMap<String, Value>,
    pub events: Vec<Value>,
    pub terminal: bool,
    pub result: Option<Value>,
    pub commitments: StateCommitments,
}

pub struct EngineHost {
    registry: GameRegistry,
    lifecycle: HostLifecycle,
    session: Option<Session>,
}

struct Session {
    simulation: Box<dyn Simulation>,
    commitments: CommitmentChain,
    current_tick: u64,
}

impl EngineHost {
    pub fn new(registry: GameRegistry) -> Self {
        Self {
            registry,
            lifecycle: HostLifecycle::Created,
            session: None,
        }
    }

    pub fn lifecycle(&self) -> HostLifecycle {
        self.lifecycle
    }

    pub fn registry(&self) -> &GameRegistry {
        &self.registry
    }

    pub fn initialize(
        &mut self,
        spec: ExecutionSpec,
    ) -> Result<HostFrame, SimError> {
        if self.lifecycle != HostLifecycle::Created {
            return Err(SimError::new(
                "invalid_lifecycle",
                format!(
                    "initialize is only valid in created state, host is {:?}",
                    self.lifecycle
                ),
            ));
        }
        spec.validate()?;
        let game = match self.registry.resolve(&spec.game) {
            Ok(g) => g,
            Err(e) => {
                // Unknown game remains in Created so host can be used or destroyed cleanly
                return Err(e);
            }
        };
        validate_descriptor_limits(game.as_ref(), &spec)?;
        let commitments = CommitmentChain::new(&spec)?;
        let simulation = match game.create(spec) {
            Ok(s) => s,
            Err(e) => {
                self.lifecycle = HostLifecycle::Failed;
                return Err(e);
            }
        };
        self.session = Some(Session {
            simulation,
            commitments,
            current_tick: 0,
        });

        let result = self.initial_frame();
        if result.is_err() {
            self.lifecycle = HostLifecycle::Failed;
            self.session = None;
        } else {
            self.lifecycle =
                if result.as_ref().is_ok_and(|frame| frame.terminal) {
                    HostLifecycle::Completed
                } else {
                    HostLifecycle::Running
                };
        }
        result
    }

    fn initial_frame(&mut self) -> Result<HostFrame, SimError> {
        let session = self.session.as_mut().ok_or_else(|| {
            SimError::new("missing_session", "host has no active simulation")
        })?;
        let frame = session.simulation.initial_frame()?;
        if frame.tick != 0 {
            return Err(SimError::new(
                "invalid_initial_tick",
                format!(
                    "game returned initial tick {}, expected 0",
                    frame.tick
                ),
            ));
        }
        let state = session.simulation.canonical_authoritative_state()?;
        let commitments = session.commitments.commit(
            frame.tick,
            &ActionBatch::new(),
            &state,
            &frame.public_snapshot,
        )?;
        Ok(HostFrame {
            tick: frame.tick,
            public_snapshot: frame.public_snapshot,
            observations: frame.observations,
            events: frame.events,
            terminal: frame.terminal,
            result: frame.result,
            commitments,
        })
    }

    pub fn advance(
        &mut self,
        expected_tick: u64,
        actions: &ActionBatch,
    ) -> Result<HostFrame, SimError> {
        if self.lifecycle != HostLifecycle::Running {
            return Err(SimError::new(
                "invalid_lifecycle",
                format!("advance is only valid in running state, current state is {:?}", self.lifecycle),
            ));
        }
        let session = self.session.as_mut().ok_or_else(|| {
            SimError::new("missing_session", "host has no active simulation")
        })?;
        if expected_tick != session.current_tick {
            return Err(SimError::new(
                "tick_mismatch",
                format!(
                    "expected action for tick {}, got {expected_tick}",
                    session.current_tick
                ),
            ));
        }
        if expected_tick >= session.simulation.spec().limits.max_ticks {
            return Err(SimError::new(
                "tick_out_of_range",
                format!("tick {expected_tick} exceeds max_ticks"),
            ));
        }

        // Validate batch slots before calling simulation to prevent partial state mutation
        validate_action_slots(session.simulation.spec(), actions)?;

        let frame = match session.simulation.advance(actions) {
            Ok(f) => f,
            Err(e) => {
                self.lifecycle = HostLifecycle::Failed;
                self.session = None;
                return Err(e);
            }
        };

        if frame.tick != expected_tick + 1 {
            self.lifecycle = HostLifecycle::Failed;
            self.session = None;
            return Err(SimError::new(
                "invalid_game_tick",
                format!(
                    "game returned tick {}, expected {}",
                    frame.tick,
                    expected_tick + 1
                ),
            ));
        }

        let state = match session.simulation.canonical_authoritative_state() {
            Ok(s) => s,
            Err(e) => {
                self.lifecycle = HostLifecycle::Failed;
                self.session = None;
                return Err(e);
            }
        };

        let commitments = match session.commitments.commit(
            frame.tick,
            actions,
            &state,
            &frame.public_snapshot,
        ) {
            Ok(c) => c,
            Err(e) => {
                self.lifecycle = HostLifecycle::Failed;
                self.session = None;
                return Err(e);
            }
        };

        session.current_tick = frame.tick;
        if frame.terminal {
            self.lifecycle = HostLifecycle::Completed;
        }

        Ok(HostFrame {
            tick: frame.tick,
            public_snapshot: frame.public_snapshot,
            observations: frame.observations,
            events: frame.events,
            terminal: frame.terminal,
            result: frame.result,
            commitments,
        })
    }
}

fn validate_descriptor_limits(
    game: &dyn GameModule,
    spec: &ExecutionSpec,
) -> Result<(), SimError> {
    let descriptor = game.descriptor();
    let players = spec.slots.len() as u32;
    if players < descriptor.players.minimum
        || players > descriptor.players.maximum
    {
        return Err(SimError::new(
            "invalid_player_count",
            format!(
                "game accepts {}..={} players, got {players}",
                descriptor.players.minimum, descriptor.players.maximum
            ),
        ));
    }
    Ok(())
}

fn validate_action_slots(
    spec: &ExecutionSpec,
    actions: &ActionBatch,
) -> Result<(), SimError> {
    let allowed_slots: Vec<String> = spec.slot_ids();
    if let Some(unknown) =
        actions.keys().find(|slot| !allowed_slots.contains(slot))
    {
        return Err(SimError::new(
            "unknown_action_slot",
            format!("action supplied for unknown slot {unknown}"),
        ));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use agentrix_conformance_game::{
        conformance_game_key, ConformanceGame, ACTION_SCHEMA_DIGEST,
        OBSERVATION_SCHEMA_DIGEST, PUBLIC_SCHEMA_DIGEST,
    };
    use agentrix_sim_core::{
        canonical_json_digest, ActionStatus, DeterminismTier, ExecutionLimits,
        ExecutionSlotSpec, SchemaDigests, SlotAction, TickRate,
        PROTOCOL_VERSION, RNG_ALGORITHM,
    };
    use serde_json::json;

    fn spec() -> ExecutionSpec {
        let config = json!({"target": 3});
        ExecutionSpec {
            protocol_version: PROTOCOL_VERSION.to_string(),
            run_id: "run-host-test".to_string(),
            match_id: "match-host-test".to_string(),
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
            seed: 11,
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

    fn registry() -> GameRegistry {
        let mut registry = GameRegistry::new();
        registry.register(Arc::new(ConformanceGame::new())).unwrap();
        registry
    }

    #[test]
    fn conformance_game_runs_to_completion_without_host_branches() {
        let mut host = EngineHost::new(registry());
        assert_eq!(host.lifecycle(), HostLifecycle::Created);
        let initial = host.initialize(spec()).unwrap();
        assert_eq!(initial.tick, 0);
        assert_eq!(host.lifecycle(), HostLifecycle::Running);

        let mut actions = ActionBatch::new();
        for tick in 0..3 {
            actions.insert(
                "alpha".to_string(),
                SlotAction {
                    status: ActionStatus::Valid,
                    requested: Some(json!({"increment": 1})),
                    applied: Some(json!({"increment": 1})),
                    policy_decision: "apply".to_string(),
                    error_code: None,
                },
            );
            let frame = host.advance(tick, &actions).unwrap();
            assert_eq!(frame.tick, tick + 1);
        }
        assert_eq!(host.lifecycle(), HostLifecycle::Completed);
    }

    #[test]
    fn exact_game_triple_is_required_before_state_creation() {
        let mut bad = spec();
        bad.game.game_digest = "f".repeat(64);
        let mut host = EngineHost::new(registry());
        let error = host.initialize(bad).unwrap_err();
        assert_eq!(error.code, "unknown_game");
        assert_eq!(host.lifecycle(), HostLifecycle::Created);
    }

    #[test]
    fn duplicate_registration_is_rejected_without_replacing_registry_entry() {
        let mut registry = registry();
        let error = registry
            .register(Arc::new(ConformanceGame::new()))
            .unwrap_err();
        assert_eq!(error.code, "duplicate_game");
        assert_eq!(registry.descriptors().len(), 1);
    }

    #[test]
    fn advance_with_unknown_slot_is_rejected_without_transition() {
        let mut host = EngineHost::new(registry());
        host.initialize(spec()).unwrap();
        assert_eq!(host.lifecycle(), HostLifecycle::Running);

        let mut bad_actions = ActionBatch::new();
        bad_actions.insert(
            "unknown_player".to_string(),
            SlotAction {
                status: ActionStatus::Valid,
                requested: Some(json!({"increment": 1})),
                applied: Some(json!({"increment": 1})),
                policy_decision: "apply".to_string(),
                error_code: None,
            },
        );
        let err = host.advance(0, &bad_actions).unwrap_err();
        assert_eq!(err.code, "unknown_action_slot");
        // State remains Running because advance was rejected before simulation execution
        assert_eq!(host.lifecycle(), HostLifecycle::Running);

        // Now valid action works fine
        let mut good_actions = ActionBatch::new();
        good_actions.insert(
            "alpha".to_string(),
            SlotAction {
                status: ActionStatus::Valid,
                requested: Some(json!({"increment": 1})),
                applied: Some(json!({"increment": 1})),
                policy_decision: "apply".to_string(),
                error_code: None,
            },
        );
        let frame = host.advance(0, &good_actions).unwrap();
        assert_eq!(frame.tick, 1);
    }

    #[test]
    fn fatal_failure_in_simulation_transitions_to_failed_and_rejects_retry() {
        let mut host = EngineHost::new(registry());
        host.initialize(spec()).unwrap();
        assert_eq!(host.lifecycle(), HostLifecycle::Running);

        // Action with out of bounds payload triggers error inside simulation.advance
        let mut fatal_actions = ActionBatch::new();
        fatal_actions.insert(
            "alpha".to_string(),
            SlotAction {
                status: ActionStatus::Valid,
                requested: Some(json!({"increment": 999})),
                applied: Some(json!({"increment": 999})),
                policy_decision: "apply".to_string(),
                error_code: None,
            },
        );
        let err = host.advance(0, &fatal_actions).unwrap_err();
        assert_eq!(err.code, "action_out_of_bounds");
        assert_eq!(host.lifecycle(), HostLifecycle::Failed);

        // Retry must fail with invalid_lifecycle
        let retry_err = host.advance(0, &ActionBatch::new()).unwrap_err();
        assert_eq!(retry_err.code, "invalid_lifecycle");
    }

    #[test]
    fn repeated_clean_hosts_produce_identical_commitment_chains() {
        fn run() -> Vec<StateCommitments> {
            let mut host = EngineHost::new(registry());
            let mut output = vec![host.initialize(spec()).unwrap().commitments];
            let mut actions = ActionBatch::new();
            for tick in 0..3 {
                actions.insert(
                    "alpha".to_string(),
                    SlotAction {
                        status: ActionStatus::Valid,
                        requested: Some(json!({"increment": 1})),
                        applied: Some(json!({"increment": 1})),
                        policy_decision: "apply".to_string(),
                        error_code: None,
                    },
                );
                output.push(host.advance(tick, &actions).unwrap().commitments);
            }
            output
        }

        let expected = run();
        for _ in 0..100 {
            assert_eq!(run(), expected);
        }
    }
}
