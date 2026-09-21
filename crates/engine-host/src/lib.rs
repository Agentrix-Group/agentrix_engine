//! Game-agnostic in-process host for authoritative simulations.
//!
//! Transport adapters are intentionally outside this crate. The host knows
//! game descriptors and opaque payloads, never concrete game components or
//! actions.

use agentrix_sim_core::{
    ActionBatch, CommitmentChain, GameKey, GameModule, SimError, Simulation,
    SimulationFrame, SimulationSpec, StateCommitments,
};
use serde::{Deserialize, Serialize};
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

    pub fn descriptors(&self) -> Vec<agentrix_sim_core::GameDescriptor> {
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
    Ready,
    Running,
    Terminal,
    Failed,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct HostFrame {
    pub frame: SimulationFrame,
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
            lifecycle: HostLifecycle::Ready,
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
        spec: SimulationSpec,
    ) -> Result<HostFrame, SimError> {
        if self.lifecycle != HostLifecycle::Ready {
            return Err(SimError::new(
                "invalid_lifecycle",
                "initialize is only valid in ready state",
            ));
        }
        spec.validate_common()?;
        let game = self.registry.resolve(&spec.game)?;
        validate_descriptor_limits(game.as_ref(), &spec)?;
        let commitments = CommitmentChain::new(&spec)?;
        let simulation = game.create(spec)?;
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
                if result.as_ref().is_ok_and(|frame| frame.frame.terminal) {
                    HostLifecycle::Terminal
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
        Ok(HostFrame { frame, commitments })
    }

    pub fn advance(
        &mut self,
        expected_tick: u64,
        actions: &ActionBatch,
    ) -> Result<HostFrame, SimError> {
        if self.lifecycle != HostLifecycle::Running {
            return Err(SimError::new(
                "invalid_lifecycle",
                "advance is only valid in running state",
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
        if expected_tick >= session.simulation.spec().max_ticks {
            return Err(SimError::new(
                "tick_out_of_range",
                format!("tick {expected_tick} exceeds max_ticks"),
            ));
        }
        validate_action_slots(session.simulation.spec(), actions)?;
        let frame = session.simulation.advance(actions)?;
        if frame.tick != expected_tick + 1 {
            self.lifecycle = HostLifecycle::Failed;
            return Err(SimError::new(
                "invalid_game_tick",
                format!(
                    "game returned tick {}, expected {}",
                    frame.tick,
                    expected_tick + 1
                ),
            ));
        }
        let state = session.simulation.canonical_authoritative_state()?;
        let commitments = session.commitments.commit(
            frame.tick,
            actions,
            &state,
            &frame.public_snapshot,
        )?;
        session.current_tick = frame.tick;
        if frame.terminal {
            self.lifecycle = HostLifecycle::Terminal;
        }
        Ok(HostFrame { frame, commitments })
    }
}

fn validate_descriptor_limits(
    game: &dyn GameModule,
    spec: &SimulationSpec,
) -> Result<(), SimError> {
    let descriptor = game.descriptor();
    let players = spec.slots.len() as u32;
    if players < descriptor.min_players || players > descriptor.max_players {
        return Err(SimError::new(
            "invalid_player_count",
            format!(
                "game accepts {}..={} players, got {players}",
                descriptor.min_players, descriptor.max_players
            ),
        ));
    }
    Ok(())
}

fn validate_action_slots(
    spec: &SimulationSpec,
    actions: &ActionBatch,
) -> Result<(), SimError> {
    if let Some(unknown) =
        actions.keys().find(|slot| !spec.slots.contains(slot))
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
        conformance_game_key, ConformanceGame, CONFORMANCE_GAME_DIGEST,
    };
    use agentrix_sim_core::{
        canonical_json_digest, ActionStatus, DeterminismTier, SlotAction,
        TickRate, RNG_ALGORITHM,
    };
    use serde_json::json;

    fn spec() -> SimulationSpec {
        let config = json!({"target": 3});
        SimulationSpec {
            game: conformance_game_key(),
            config_digest: canonical_json_digest("conformance-config", &config)
                .unwrap(),
            config,
            tick_rate: TickRate::new(60, 1).unwrap(),
            seed: 11,
            slots: vec!["alpha".to_string(), "beta".to_string()],
            max_ticks: 10,
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
        let initial = host.initialize(spec()).unwrap();
        assert_eq!(initial.frame.tick, 0);
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
            assert_eq!(frame.frame.tick, tick + 1);
        }
        assert_eq!(host.lifecycle(), HostLifecycle::Terminal);
    }

    #[test]
    fn exact_game_triple_is_required_before_state_creation() {
        let mut bad = spec();
        bad.game.game_digest = "wrong".to_string();
        let mut host = EngineHost::new(registry());
        let error = host.initialize(bad).unwrap_err();
        assert_eq!(error.code, "unknown_game");
        assert_eq!(host.lifecycle(), HostLifecycle::Ready);
        assert_ne!(CONFORMANCE_GAME_DIGEST, "wrong");
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
