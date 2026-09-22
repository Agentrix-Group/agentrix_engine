use agentrix_conformance_game::{conformance_game_key, ConformanceGame};
use agentrix_engine_host::{EngineHost, GameRegistry};
use agentrix_sim_core::{
    canonical_json_digest, ActionBatch, ActionStatus, DeterminismTier,
    DeterministicRng,
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
        "starfighter-long" | "starfighter-ffa-long" => {
            let players = if game == "starfighter-long" { 2 } else { 5 };
            let (ticks, last) = run_starfighter_long(players)?;
            println!(
                "{}",
                serde_json::to_string(&json!({"ticks": ticks, "last": last}))?
            );
            return Ok(());
        }
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

/// Slots de los vectores de Starfighter, en orden de `player_id`.
const STARFIGHTER_SLOTS: [&str; 5] =
    ["alpha", "beta", "gamma", "delta", "epsilon"];

fn starfighter_spec(
    players: usize,
    asteroid_count: u32,
    max_ticks: u64,
) -> Result<SimulationSpec, Box<dyn Error>> {
    let config = serde_json::to_value(StarfighterConfig {
        asteroid_count,
        ..StarfighterConfig::default()
    })?;
    Ok(SimulationSpec {
        protocol_version: PROTOCOL_VERSION.to_string(),
        run_id: "starfighter-run-1".to_string(),
        match_id: "starfighter-match-1".to_string(),
        engine_version: "0.4.0".to_string(),
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
        slots: STARFIGHTER_SLOTS[..players]
            .iter()
            .enumerate()
            .map(|(index, slot)| ExecutionSlotSpec {
                slot_id: slot.to_string(),
                // "aaaa…" para alpha, "bbbb…" para beta, etc.
                artifact_digest: std::iter::repeat_n(
                    char::from(b'a' + index as u8),
                    64,
                )
                .collect(),
            })
            .collect(),
        limits: ExecutionLimits {
            max_ticks,
            max_players: players as u32,
            max_entities: 10_000,
            max_message_bytes: 1024 * 1024,
        },
        failure_policy_version: "fail-closed/1".to_string(),
        determinism_tier: DeterminismTier::SameArtifactSameTarget,
        rng_algorithm: RNG_ALGORITHM.to_string(),
    })
}

fn run_starfighter() -> Result<Vec<StateCommitments>, Box<dyn Error>> {
    let spec = starfighter_spec(2, 1, 8)?;
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

/// Partida completa de hasta 3600 ticks (60 s a 60 Hz) con `players`
/// naves, 5 asteroides y acciones pseudoaleatorias sembradas. Devuelve los
/// ticks jugados y el último commitment: su `replayChainDigest` encadena
/// todos los anteriores, así que dos salidas iguales coinciden en cada tick.
fn run_starfighter_long(
    players: usize,
) -> Result<(u64, StateCommitments), Box<dyn Error>> {
    const MAX_TICKS: u64 = 3600;
    let spec = starfighter_spec(players, 5, MAX_TICKS)?;
    let mut registry = GameRegistry::new();
    registry.register(Arc::new(StarfighterGame::new()))?;
    let mut host = EngineHost::new(registry);
    let mut last = host.initialize(spec)?.commitments;
    let mut rng = DeterministicRng::from_seed(23);
    let mut ticks = 0;
    for tick in 0..MAX_TICKS {
        let mut actions = ActionBatch::new();
        for slot in &STARFIGHTER_SLOTS[..players] {
            let thrust = ["FORWARD", "OFF", "BRAKE"][rng.range_u32(0, 3)? as usize];
            let turn = ["LEFT", "RIGHT", "NONE"][rng.range_u32(0, 3)? as usize];
            actions.insert(
                slot.to_string(),
                outcome(json!({
                    "thrust": thrust,
                    "turn": turn,
                    "shoot": rng.probability(0.85)?,
                    "shield": rng.probability(0.2)?,
                })),
            );
        }
        let step = host.advance(tick, &actions)?;
        last = step.commitments;
        ticks = tick + 1;
        if step.terminal {
            break;
        }
    }
    Ok((ticks, last))
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
