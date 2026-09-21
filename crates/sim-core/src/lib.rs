//! Deterministic, game-agnostic simulation contracts.
//!
//! This crate deliberately has no Bevy, Avian, transport, filesystem, or
//! renderer dependency. Games validate their own payloads and expose only
//! canonical state plus opaque JSON views at this boundary.

use serde::{Deserialize, Serialize};
use serde_json::Value;
use sha2::{Digest, Sha256};
use std::collections::BTreeMap;
use std::fmt::{Display, Formatter};

pub const CANONICAL_ENCODING_VERSION: &str = "agentrix-canonical/1";
pub const RNG_ALGORITHM: &str = "xoshiro256starstar/1";

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SimError {
    pub code: &'static str,
    pub message: String,
}

impl SimError {
    pub fn new(code: &'static str, message: impl Into<String>) -> Self {
        Self {
            code,
            message: message.into(),
        }
    }
}

impl Display for SimError {
    fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}: {}", self.code, self.message)
    }
}

impl std::error::Error for SimError {}

#[derive(
    Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize,
)]
#[serde(transparent)]
pub struct StableEntityId(pub u64);

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TickRate {
    pub numerator: u32,
    pub denominator: u32,
}

impl TickRate {
    pub fn new(numerator: u32, denominator: u32) -> Result<Self, SimError> {
        if numerator == 0 || denominator == 0 {
            return Err(SimError::new(
                "invalid_tick_rate",
                "tick-rate numerator and denominator must be positive",
            ));
        }
        let gcd = gcd(numerator, denominator);
        Ok(Self {
            numerator: numerator / gcd,
            denominator: denominator / gcd,
        })
    }

    pub fn seconds_per_tick(self) -> f64 {
        f64::from(self.denominator) / f64::from(self.numerator)
    }
}

fn gcd(mut left: u32, mut right: u32) -> u32 {
    while right != 0 {
        let remainder = left % right;
        left = right;
        right = remainder;
    }
    left
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct GameKey {
    pub game_id: String,
    pub game_version: String,
    pub game_digest: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DeterminismTier {
    SameArtifactSameTarget,
    CertifiedTargetMatrix,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SimulationSpec {
    pub game: GameKey,
    pub config: Value,
    pub config_digest: String,
    pub tick_rate: TickRate,
    pub seed: u64,
    pub slots: Vec<String>,
    pub max_ticks: u64,
    pub determinism_tier: DeterminismTier,
    pub rng_algorithm: String,
}

impl SimulationSpec {
    pub fn validate_common(&self) -> Result<(), SimError> {
        if self.slots.is_empty() {
            return Err(SimError::new(
                "invalid_slots",
                "at least one slot is required",
            ));
        }
        if self.max_ticks == 0 {
            return Err(SimError::new(
                "invalid_max_ticks",
                "max_ticks must be positive",
            ));
        }
        if self.rng_algorithm != RNG_ALGORITHM {
            return Err(SimError::new(
                "unsupported_rng",
                format!(
                    "expected RNG algorithm {RNG_ALGORITHM}, got {}",
                    self.rng_algorithm
                ),
            ));
        }
        let mut sorted = self.slots.clone();
        sorted.sort();
        sorted.dedup();
        if sorted.len() != self.slots.len()
            || self.slots.iter().any(|slot| slot.trim().is_empty())
        {
            return Err(SimError::new(
                "invalid_slots",
                "slot identifiers must be non-empty and unique",
            ));
        }
        Ok(())
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ActionStatus {
    Valid,
    Timeout,
    InvalidOutput,
    Crashed,
    Disqualified,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SlotAction {
    pub status: ActionStatus,
    pub requested: Option<Value>,
    pub applied: Option<Value>,
    pub policy_decision: String,
    pub error_code: Option<String>,
}

pub type ActionBatch = BTreeMap<String, SlotAction>;

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SimulationFrame {
    pub tick: u64,
    pub public_snapshot: Value,
    pub observations: BTreeMap<String, Value>,
    pub events: Vec<Value>,
    pub terminal: bool,
    pub result: Option<Value>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct GameDescriptor {
    pub key: GameKey,
    pub min_players: u32,
    pub max_players: u32,
    pub action_schema_digest: String,
    pub observation_schema_digest: String,
    pub public_schema_digest: String,
    pub capabilities: Vec<String>,
}

pub trait Simulation {
    fn spec(&self) -> &SimulationSpec;
    fn initial_frame(&mut self) -> Result<SimulationFrame, SimError>;
    fn advance(
        &mut self,
        actions: &ActionBatch,
    ) -> Result<SimulationFrame, SimError>;
    fn canonical_authoritative_state(&mut self) -> Result<Vec<u8>, SimError>;
}

pub trait GameModule: Send + Sync {
    fn descriptor(&self) -> &GameDescriptor;
    fn create(
        &self,
        spec: SimulationSpec,
    ) -> Result<Box<dyn Simulation>, SimError>;
}

/// Binary canonical encoder. Every field is length-delimited or fixed-width,
/// integers are big-endian, and floats reject non-finite values and normalize
/// negative zero. Callers must emit maps and entity collections in sorted key
/// order; helpers below do this for JSON values.
#[derive(Debug, Default, Clone)]
pub struct CanonicalEncoder {
    bytes: Vec<u8>,
}

impl CanonicalEncoder {
    pub fn new(domain: &str) -> Self {
        let mut encoder = Self::default();
        encoder.string(CANONICAL_ENCODING_VERSION);
        encoder.string(domain);
        encoder
    }

    pub fn into_bytes(self) -> Vec<u8> {
        self.bytes
    }

    pub fn u8(&mut self, value: u8) {
        self.bytes.push(value);
    }

    pub fn bool(&mut self, value: bool) {
        self.u8(u8::from(value));
    }

    pub fn u32(&mut self, value: u32) {
        self.bytes.extend_from_slice(&value.to_be_bytes());
    }

    pub fn u64(&mut self, value: u64) {
        self.bytes.extend_from_slice(&value.to_be_bytes());
    }

    pub fn i64(&mut self, value: i64) {
        self.bytes.extend_from_slice(&value.to_be_bytes());
    }

    pub fn bytes(&mut self, value: &[u8]) {
        self.u64(value.len() as u64);
        self.bytes.extend_from_slice(value);
    }

    pub fn string(&mut self, value: &str) {
        self.bytes(value.as_bytes());
    }

    pub fn f32(&mut self, value: f32) -> Result<(), SimError> {
        if !value.is_finite() {
            return Err(SimError::new(
                "non_finite_state",
                "canonical f32 must be finite",
            ));
        }
        let normalized = if value == 0.0 { 0.0 } else { value };
        self.u32(normalized.to_bits());
        Ok(())
    }

    pub fn f64(&mut self, value: f64) -> Result<(), SimError> {
        if !value.is_finite() {
            return Err(SimError::new(
                "non_finite_state",
                "canonical f64 must be finite",
            ));
        }
        let normalized = if value == 0.0 { 0.0 } else { value };
        self.u64(normalized.to_bits());
        Ok(())
    }

    pub fn json(&mut self, value: &Value) -> Result<(), SimError> {
        match value {
            Value::Null => self.u8(0),
            Value::Bool(value) => {
                self.u8(1);
                self.bool(*value);
            }
            Value::Number(value) => {
                self.u8(2);
                if let Some(unsigned) = value.as_u64() {
                    self.u8(0);
                    self.u64(unsigned);
                } else if let Some(signed) = value.as_i64() {
                    self.u8(1);
                    self.i64(signed);
                } else if let Some(float) = value.as_f64() {
                    self.u8(2);
                    self.f64(float)?;
                } else {
                    return Err(SimError::new(
                        "invalid_number",
                        "JSON number has no supported representation",
                    ));
                }
            }
            Value::String(value) => {
                self.u8(3);
                self.string(value);
            }
            Value::Array(values) => {
                self.u8(4);
                self.u64(values.len() as u64);
                for value in values {
                    self.json(value)?;
                }
            }
            Value::Object(values) => {
                self.u8(5);
                self.u64(values.len() as u64);
                let mut entries: Vec<_> = values.iter().collect();
                entries.sort_by(|left, right| left.0.cmp(right.0));
                for (key, value) in entries {
                    self.string(key);
                    self.json(value)?;
                }
            }
        }
        Ok(())
    }
}

pub fn sha256_hex(domain: &str, parts: &[&[u8]]) -> String {
    let mut hasher = Sha256::new();
    hasher.update((domain.len() as u64).to_be_bytes());
    hasher.update(domain.as_bytes());
    for part in parts {
        hasher.update((part.len() as u64).to_be_bytes());
        hasher.update(part);
    }
    let digest = hasher.finalize();
    digest.iter().map(|byte| format!("{byte:02x}")).collect()
}

pub fn canonical_json_digest(
    domain: &str,
    value: &Value,
) -> Result<String, SimError> {
    let mut encoder = CanonicalEncoder::new(domain);
    encoder.json(value)?;
    Ok(sha256_hex(domain, &[&encoder.into_bytes()]))
}

pub fn simulation_spec_digest(
    spec: &SimulationSpec,
) -> Result<String, SimError> {
    let value = serde_json::to_value(spec).map_err(|error| {
        SimError::new("spec_serialization_failed", error.to_string())
    })?;
    canonical_json_digest("agentrix.execution-spec/1", &value)
}

pub fn action_batch_digest(
    tick: u64,
    actions: &ActionBatch,
) -> Result<String, SimError> {
    let value = serde_json::to_value(actions).map_err(|error| {
        SimError::new("action_serialization_failed", error.to_string())
    })?;
    let mut encoder = CanonicalEncoder::new("agentrix.action-batch/1");
    encoder.u64(tick);
    encoder.json(&value)?;
    Ok(sha256_hex(
        "agentrix.action-batch/1",
        &[&encoder.into_bytes()],
    ))
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct StateCommitments {
    pub execution_spec_digest: String,
    pub action_batch_digest: String,
    pub authoritative_state_commitment: String,
    pub public_snapshot_hash: String,
    pub replay_chain_digest: String,
}

#[derive(Debug, Clone)]
pub struct CommitmentChain {
    execution_spec_digest: String,
    previous_state: [u8; 32],
    previous_replay: [u8; 32],
}

impl CommitmentChain {
    pub fn new(spec: &SimulationSpec) -> Result<Self, SimError> {
        let execution_spec_digest = simulation_spec_digest(spec)?;
        let previous_state = digest_bytes(
            "agentrix.authoritative-state/genesis",
            &[execution_spec_digest.as_bytes()],
        );
        let previous_replay = digest_bytes(
            "agentrix.replay-chain/genesis",
            &[execution_spec_digest.as_bytes()],
        );
        Ok(Self {
            execution_spec_digest,
            previous_state,
            previous_replay,
        })
    }

    pub fn commit(
        &mut self,
        tick: u64,
        actions: &ActionBatch,
        authoritative_state: &[u8],
        public_snapshot: &Value,
    ) -> Result<StateCommitments, SimError> {
        let action_digest = action_batch_digest(tick, actions)?;
        let public_hash = canonical_json_digest(
            "agentrix.public-snapshot/1",
            public_snapshot,
        )?;
        let state = digest_bytes(
            "agentrix.authoritative-state/1",
            &[
                &self.previous_state,
                self.execution_spec_digest.as_bytes(),
                action_digest.as_bytes(),
                authoritative_state,
            ],
        );
        let replay = digest_bytes(
            "agentrix.replay-chain/1",
            &[
                &self.previous_replay,
                action_digest.as_bytes(),
                &state,
                public_hash.as_bytes(),
            ],
        );
        self.previous_state = state;
        self.previous_replay = replay;
        Ok(StateCommitments {
            execution_spec_digest: self.execution_spec_digest.clone(),
            action_batch_digest: action_digest,
            authoritative_state_commitment: hex(&state),
            public_snapshot_hash: public_hash,
            replay_chain_digest: hex(&replay),
        })
    }
}

fn digest_bytes(domain: &str, parts: &[&[u8]]) -> [u8; 32] {
    let mut hasher = Sha256::new();
    hasher.update((domain.len() as u64).to_be_bytes());
    hasher.update(domain.as_bytes());
    for part in parts {
        hasher.update((part.len() as u64).to_be_bytes());
        hasher.update(part);
    }
    hasher.finalize().into()
}

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|byte| format!("{byte:02x}")).collect()
}

/// Explicit portable PRNG whose complete state is serializable and committed.
/// Seeding uses SplitMix64; generation uses xoshiro256** 1.0.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DeterministicRng {
    state: [u64; 4],
}

impl DeterministicRng {
    pub fn from_seed(seed: u64) -> Self {
        let mut splitmix = seed;
        let mut state = [0; 4];
        for item in &mut state {
            splitmix = splitmix.wrapping_add(0x9e3779b97f4a7c15);
            let mut value = splitmix;
            value = (value ^ (value >> 30)).wrapping_mul(0xbf58476d1ce4e5b9);
            value = (value ^ (value >> 27)).wrapping_mul(0x94d049bb133111eb);
            *item = value ^ (value >> 31);
        }
        Self { state }
    }

    pub fn state(&self) -> [u64; 4] {
        self.state
    }

    pub fn next_u64(&mut self) -> u64 {
        let result =
            self.state[1].wrapping_mul(5).rotate_left(7).wrapping_mul(9);
        let temporary = self.state[1] << 17;
        self.state[2] ^= self.state[0];
        self.state[3] ^= self.state[1];
        self.state[1] ^= self.state[2];
        self.state[0] ^= self.state[3];
        self.state[2] ^= temporary;
        self.state[3] = self.state[3].rotate_left(45);
        result
    }

    pub fn unit_f32(&mut self) -> f32 {
        let mantissa = (self.next_u64() >> 40) as u32;
        mantissa as f32 / 16_777_216.0
    }

    pub fn range_f32(&mut self, start: f32, end: f32) -> Result<f32, SimError> {
        if !start.is_finite() || !end.is_finite() || start >= end {
            return Err(SimError::new(
                "invalid_rng_range",
                "random range must be finite and non-empty",
            ));
        }
        Ok(start + (end - start) * self.unit_f32())
    }

    pub fn range_u32(&mut self, start: u32, end: u32) -> Result<u32, SimError> {
        if start >= end {
            return Err(SimError::new(
                "invalid_rng_range",
                "random integer range must be non-empty",
            ));
        }
        let width = u64::from(end - start);
        let threshold = width.wrapping_neg() % width;
        loop {
            let value = self.next_u64();
            if value >= threshold {
                return Ok(start + (value % width) as u32);
            }
        }
    }

    pub fn probability(&mut self, probability: f32) -> Result<bool, SimError> {
        if !probability.is_finite() || !(0.0..=1.0).contains(&probability) {
            return Err(SimError::new(
                "invalid_probability",
                "probability must be finite and between zero and one",
            ));
        }
        Ok(self.unit_f32() < probability)
    }

    pub fn encode_state(&self, encoder: &mut CanonicalEncoder) {
        encoder.string(RNG_ALGORITHM);
        for word in self.state {
            encoder.u64(word);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn spec() -> SimulationSpec {
        let config = json!({"difficulty": 2, "nested": {"enabled": true}});
        SimulationSpec {
            game: GameKey {
                game_id: "test".to_string(),
                game_version: "1".to_string(),
                game_digest: "test-digest".to_string(),
            },
            config_digest: canonical_json_digest("config", &config).unwrap(),
            config,
            tick_rate: TickRate::new(60, 1).unwrap(),
            seed: 7,
            slots: vec!["alpha".to_string(), "beta".to_string()],
            max_ticks: 10,
            determinism_tier: DeterminismTier::SameArtifactSameTarget,
            rng_algorithm: RNG_ALGORITHM.to_string(),
        }
    }

    #[test]
    fn tick_rate_is_reduced_and_rejects_zero() {
        assert_eq!(
            TickRate::new(120, 2).unwrap(),
            TickRate {
                numerator: 60,
                denominator: 1
            }
        );
        assert!(TickRate::new(0, 1).is_err());
        assert!(TickRate::new(60, 0).is_err());
    }

    #[test]
    fn canonical_json_is_independent_of_map_insertion_order() {
        let left = json!({"b": 2, "a": 1});
        let right = json!({"a": 1, "b": 2});
        assert_eq!(
            canonical_json_digest("test", &left).unwrap(),
            canonical_json_digest("test", &right).unwrap()
        );
    }

    #[test]
    fn canonical_floats_reject_non_finite_and_normalize_negative_zero() {
        let mut left = CanonicalEncoder::new("float");
        left.f32(-0.0).unwrap();
        let mut right = CanonicalEncoder::new("float");
        right.f32(0.0).unwrap();
        assert_eq!(left.into_bytes(), right.into_bytes());
        assert!(CanonicalEncoder::new("float").f32(f32::NAN).is_err());
        assert!(CanonicalEncoder::new("float").f64(f64::INFINITY).is_err());
    }

    #[test]
    fn rng_has_stable_vectors_and_serializable_state() {
        let mut rng = DeterministicRng::from_seed(7);
        assert_eq!(rng.next_u64(), 12_923_355_070_828_475_994);
        assert_eq!(rng.next_u64(), 5_142_052_590_334_782_674);
        let serialized = serde_json::to_string(&rng).unwrap();
        let restored: DeterministicRng =
            serde_json::from_str(&serialized).unwrap();
        assert_eq!(rng, restored);
    }

    #[test]
    fn hidden_state_and_actions_change_separate_commitments() {
        let spec = spec();
        let mut first = CommitmentChain::new(&spec).unwrap();
        let mut second = CommitmentChain::new(&spec).unwrap();
        let public = json!({"tick": 1, "score": 0});
        let no_actions = ActionBatch::new();
        let first_commit =
            first.commit(1, &no_actions, b"hidden-a", &public).unwrap();
        let second_commit =
            second.commit(1, &no_actions, b"hidden-b", &public).unwrap();
        assert_ne!(
            first_commit.authoritative_state_commitment,
            second_commit.authoritative_state_commitment
        );
        assert_eq!(
            first_commit.public_snapshot_hash,
            second_commit.public_snapshot_hash
        );

        let mut third = CommitmentChain::new(&spec).unwrap();
        let mut actions = ActionBatch::new();
        actions.insert(
            "alpha".to_string(),
            SlotAction {
                status: ActionStatus::Valid,
                requested: Some(json!({"move": 1})),
                applied: Some(json!({"move": 1})),
                policy_decision: "apply".to_string(),
                error_code: None,
            },
        );
        let third_commit =
            third.commit(1, &actions, b"hidden-a", &public).unwrap();
        assert_ne!(
            first_commit.action_batch_digest,
            third_commit.action_batch_digest
        );
        assert_ne!(
            first_commit.authoritative_state_commitment,
            third_commit.authoritative_state_commitment
        );
    }
}
