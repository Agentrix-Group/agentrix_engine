use agentrix_conformance_game::{conformance_game_key, ConformanceGame};
use agentrix_engine_host::{EngineHost, GameRegistry};
use agentrix_sim_core::{
    canonical_json_digest, ActionBatch, ActionStatus, DeterminismTier,
    SimulationSpec, SlotAction, StateCommitments, TickRate, RNG_ALGORITHM,
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
        game: conformance_game_key(),
        config_digest: canonical_json_digest("conformance-config", &config)?,
        config,
        tick_rate: TickRate::new(60, 1)?,
        seed: 11,
        slots: vec!["alpha".to_string(), "beta".to_string()],
        max_ticks: 10,
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
        game: starfighter_game_key(),
        config_digest: canonical_json_digest("starfighter-config", &config)?,
        config,
        tick_rate: TickRate::new(60, 1)?,
        seed: 19,
        slots: vec!["alpha".to_string(), "beta".to_string()],
        max_ticks: 8,
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
