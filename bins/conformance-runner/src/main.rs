use agentrix_conformance_game::{conformance_game_key, ConformanceGame};
use agentrix_engine_host::{EngineHost, GameRegistry};
use agentrix_sim_core::{
    canonical_json_digest, ActionBatch, ActionStatus, DeterminismTier,
    ExecutionLimits, ExecutionSlotSpec, SchemaDigests, SimulationSpec,
    SlotAction, StateCommitments, TickRate, PROTOCOL_VERSION, RNG_ALGORITHM,
};
use bevy_starfighter::game_module::{starfighter_game_key, StarfighterGame};
use bevy_starfighter::StarfighterConfig;
use serde_json::json;
use std::error::Error;
use std::sync::Arc;

fn main() -> Result<(), Box<dyn Error>> {
    let game = std::env::args()
        .nth(1)
        .unwrap_or_else(|| "conformance".to_string());
    let commitments = match game.as_str() {
        "conformance" => run_conformance()?,
        "starfighter" => run_starfighter()?,
        other => return Err(format!("unknown vector {other}").into()),
    };
    println!("{}", serde_json::to_string(&commitments)?);
    Ok(())
}

fn run_conformance() -> Result<Vec<StateCommitments>, Box<dyn Error>> {
    let config = json!({"target": 3});
    let spec = SimulationSpec {
        protocol_version: PROTOCOL_VERSION.to_string(),
        run_id: "conformance-run-1".to_string(),
        match_id: "conformance-match-1".to_string(),
        engine_version: "0.1.0".to_string(),
        engine_digest: "0000000000000000000000000000000000000000000000000000000000000000".to_string(),
        build_identity: "conformance-build".to_string(),
        target: "x86_64-unknown-linux-gnu".to_string(),
        game: conformance_game_key(),
        schema_digests: SchemaDigests {
            action: agentrix_conformance_game::ACTION_SCHEMA_DIGEST.to_string(),
            observation: agentrix_conformance_game::OBSERVATION_SCHEMA_DIGEST.to_string(),
            public: agentrix_conformance_game::PUBLIC_SCHEMA_DIGEST.to_string(),
            replay: "0000000000000000000000000000000000000000000000000000000000000000".to_string(),
        },
        config_digest: canonical_json_digest("conformance-config", &config)?,
        config,
        tick_rate: TickRate::new(60, 1)?,
        seed: 11,
        slots: vec![
            ExecutionSlotSpec {
                slot_id: "alpha".to_string(),
                artifact_digest: "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa".to_string(),
            },
            ExecutionSlotSpec {
                slot_id: "beta".to_string(),
                artifact_digest: "bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb".to_string(),
            },
        ],
        limits: ExecutionLimits {
            max_ticks: 10,
            max_players: 2,
            max_entities: 100,
            max_message_bytes: 1024 * 1024,
        },
        failure_policy_version: "fail-closed/1".to_string(),
        determinism_tier: DeterminismTier::SameArtifactSameTarget,
        rng_algorithm: RNG_ALGORITHM.to_string(),
    };
    let mut registry = GameRegistry::new();
    registry.register(Arc::new(ConformanceGame::new()))?;
    let mut host = EngineHost::new(registry);
    let mut output = vec![host.initialize(spec)?.commitments];
    for tick in 0..3 {
        let mut actions = ActionBatch::new();
        actions.insert("alpha".to_string(), outcome(json!({"increment": 1})));
        output.push(host.advance(tick, &actions)?.commitments);
    }
    Ok(output)
}

fn run_starfighter() -> Result<Vec<StateCommitments>, Box<dyn Error>> {
    let config = serde_json::to_value(StarfighterConfig {
        asteroid_count: 1,
        ..StarfighterConfig::default()
    })?;
    let spec = SimulationSpec {
        protocol_version: PROTOCOL_VERSION.to_string(),
        run_id: "starfighter-run-1".to_string(),
        match_id: "starfighter-match-1".to_string(),
        engine_version: "0.3.0-core.1".to_string(),
        engine_digest: "0000000000000000000000000000000000000000000000000000000000000000".to_string(),
        build_identity: "starfighter-build".to_string(),
        target: "x86_64-unknown-linux-gnu".to_string(),
        game: starfighter_game_key(),
        schema_digests: SchemaDigests {
            action: bevy_starfighter::game_module::ACTION_SCHEMA_DIGEST.to_string(),
            observation: bevy_starfighter::game_module::OBSERVATION_SCHEMA_DIGEST.to_string(),
            public: bevy_starfighter::game_module::PUBLIC_SCHEMA_DIGEST.to_string(),
            replay: "0000000000000000000000000000000000000000000000000000000000000000".to_string(),
        },
        config_digest: canonical_json_digest("starfighter-config", &config)?,
        config,
        tick_rate: TickRate::new(60, 1)?,
        seed: 19,
        slots: vec![
            ExecutionSlotSpec {
                slot_id: "alpha".to_string(),
                artifact_digest: "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa".to_string(),
            },
            ExecutionSlotSpec {
                slot_id: "beta".to_string(),
                artifact_digest: "bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb".to_string(),
            },
        ],
        limits: ExecutionLimits {
            max_ticks: 8,
            max_players: 2,
            max_entities: 10_000,
            max_message_bytes: 1024 * 1024,
        },
        failure_policy_version: "fail-closed/1".to_string(),
        determinism_tier: DeterminismTier::SameArtifactSameTarget,
        rng_algorithm: RNG_ALGORITHM.to_string(),
    };
    let mut registry = GameRegistry::new();
    registry.register(Arc::new(StarfighterGame::new()))?;
    let mut host = EngineHost::new(registry);
    let mut output = vec![host.initialize(spec)?.commitments];
    for tick in 0..8 {
        let mut actions = ActionBatch::new();
        for slot in ["alpha", "beta"] {
            actions.insert(
                slot.to_string(),
                outcome(json!({
                    "thrust": if tick % 2 == 0 { "FORWARD" } else { "OFF" },
                    "turn": if slot == "alpha" { "LEFT" } else { "RIGHT" },
                    "shoot": tick % 3 == 0,
                    "shield": false
                })),
            );
        }
        output.push(host.advance(tick, &actions)?.commitments);
    }
    Ok(output)
}

fn outcome(action: serde_json::Value) -> SlotAction {
    SlotAction {
        status: ActionStatus::Valid,
        requested: Some(action.clone()),
        applied: Some(action),
        policy_decision: "apply".to_string(),
        error_code: None,
    }
}
