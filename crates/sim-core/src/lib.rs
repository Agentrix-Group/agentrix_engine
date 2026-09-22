//! Deterministic, game-agnostic simulation contracts.
//!
//! This crate deliberately has no Bevy, Avian, transport, filesystem, or
//! renderer dependency. Games validate their own payloads and expose only
//! canonical state plus opaque JSON views at this boundary.

use serde::{
    de::Error as DeError, Deserialize, Deserializer, Serialize, Serializer,
};
use serde_json::Value;
use sha2::{Digest, Sha256};
use std::collections::BTreeMap;
use std::fmt::{Display, Formatter};

pub const PROTOCOL_VERSION: &str = "agentrix-engine/2";
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

/// Validates that a string is a 64-character lowercase hexadecimal sha256 string.
pub fn is_valid_sha256(s: &str) -> bool {
    s.len() == 64
        && s.chars()
            .all(|c| c.is_ascii_hexdigit() && !c.is_ascii_uppercase())
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TickRate {
    pub numerator: u32,
    pub denominator: u32,
}

impl TickRate {
    pub const SIXTY_HZ: TickRate = TickRate {
        numerator: 60,
        denominator: 1,
    };

    pub fn new(numerator: u32, denominator: u32) -> Result<Self, SimError> {
        if numerator == 0 || denominator == 0 {
            return Err(SimError::new(
                "invalid_tick_rate",
                "tick-rate numerator and denominator must be positive",
            ));
        }
        if numerator > 10_000 || denominator > 10_000 {
            return Err(SimError::new(
                "invalid_tick_rate",
                "tick-rate numerator and denominator must not exceed 10000",
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

impl Serialize for TickRate {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        #[derive(Serialize)]
        struct RawTickRate {
            numerator: u32,
            denominator: u32,
        }
        RawTickRate {
            numerator: self.numerator,
            denominator: self.denominator,
        }
        .serialize(serializer)
    }
}

impl<'de> Deserialize<'de> for TickRate {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        #[derive(Deserialize)]
        #[serde(deny_unknown_fields)]
        struct RawTickRate {
            numerator: u32,
            denominator: u32,
        }
        let raw = RawTickRate::deserialize(deserializer)?;
        TickRate::new(raw.numerator, raw.denominator).map_err(D::Error::custom)
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
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct GameKey {
    pub game_id: String,
    pub game_version: String,
    pub game_digest: String,
}

impl GameKey {
    pub fn validate(&self) -> Result<(), SimError> {
        if self.game_id.trim().is_empty() {
            return Err(SimError::new(
                "invalid_game_key",
                "game_id must not be empty",
            ));
        }
        if self.game_version.trim().is_empty() {
            return Err(SimError::new(
                "invalid_game_key",
                "game_version must not be empty",
            ));
        }
        if !is_valid_sha256(&self.game_digest) {
            return Err(SimError::new(
                "invalid_game_key",
                "game_digest must be a 64-character lowercase hex sha256",
            ));
        }
        Ok(())
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct SchemaDigests {
    pub action: String,
    pub observation: String,
    pub public: String,
    pub replay: String,
}

impl SchemaDigests {
    pub fn validate(&self) -> Result<(), SimError> {
        for (name, digest) in [
            ("action", &self.action),
            ("observation", &self.observation),
            ("public", &self.public),
            ("replay", &self.replay),
        ] {
            if !is_valid_sha256(digest) {
                return Err(SimError::new(
                    "invalid_schema_digest",
                    format!("{name} schema digest must be a 64-character lowercase hex sha256"),
                ));
            }
        }
        Ok(())
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ExecutionSlotSpec {
    pub slot_id: String,
    pub artifact_digest: String,
}

impl ExecutionSlotSpec {
    pub fn validate(&self) -> Result<(), SimError> {
        if self.slot_id.trim().is_empty() {
            return Err(SimError::new(
                "invalid_slot",
                "slot_id must not be empty",
            ));
        }
        if !is_valid_sha256(&self.artifact_digest) {
            return Err(SimError::new(
                "invalid_slot",
                "artifact_digest must be a 64-character lowercase hex sha256",
            ));
        }
        Ok(())
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ExecutionLimits {
    pub max_ticks: u64,
    pub max_players: u32,
    pub max_entities: u32,
    pub max_message_bytes: u32,
}

impl ExecutionLimits {
    pub fn validate(&self) -> Result<(), SimError> {
        if self.max_ticks == 0 {
            return Err(SimError::new(
                "invalid_limits",
                "max_ticks must be positive",
            ));
        }
        if self.max_players == 0 || self.max_players > 64 {
            return Err(SimError::new(
                "invalid_limits",
                "max_players must be between 1 and 64",
            ));
        }
        if self.max_entities == 0 {
            return Err(SimError::new(
                "invalid_limits",
                "max_entities must be positive",
            ));
        }
        if self.max_message_bytes < 1024 {
            return Err(SimError::new(
                "invalid_limits",
                "max_message_bytes must be at least 1024",
            ));
        }
        Ok(())
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DeterminismTier {
    SameArtifactSameTarget,
    CertifiedTargetMatrix,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ExecutionSpec {
    pub protocol_version: String,
    pub run_id: String,
    pub match_id: String,
    pub engine_version: String,
    pub engine_digest: String,
    pub build_identity: String,
    pub target: String,
    pub game: GameKey,
    pub schema_digests: SchemaDigests,
    pub config: Value,
    pub config_digest: String,
    pub tick_rate: TickRate,
    pub seed: u64,
    pub slots: Vec<ExecutionSlotSpec>,
    pub limits: ExecutionLimits,
    pub failure_policy_version: String,
    pub determinism_tier: DeterminismTier,
    pub rng_algorithm: String,
}

impl ExecutionSpec {
    pub fn slot_ids(&self) -> Vec<String> {
        self.slots.iter().map(|s| s.slot_id.clone()).collect()
    }

    pub fn validate(&self) -> Result<(), SimError> {
        if self.protocol_version != PROTOCOL_VERSION {
            return Err(SimError::new(
                "invalid_protocol_version",
                format!(
                    "expected protocol_version {PROTOCOL_VERSION}, got {}",
                    self.protocol_version
                ),
            ));
        }
        if self.run_id.trim().is_empty() {
            return Err(SimError::new(
                "invalid_spec",
                "run_id must not be empty",
            ));
        }
        if self.match_id.trim().is_empty() {
            return Err(SimError::new(
                "invalid_spec",
                "match_id must not be empty",
            ));
        }
        if self.engine_version.trim().is_empty() {
            return Err(SimError::new(
                "invalid_spec",
                "engine_version must not be empty",
            ));
        }
        if !is_valid_sha256(&self.engine_digest) {
            return Err(SimError::new(
                "invalid_engine_digest",
                "engine_digest must be a 64-character lowercase hex sha256",
            ));
        }
        if self.build_identity.trim().is_empty() {
            return Err(SimError::new(
                "invalid_spec",
                "build_identity must not be empty",
            ));
        }
        if self.target.trim().is_empty() {
            return Err(SimError::new(
                "invalid_spec",
                "target must not be empty",
            ));
        }
        self.game.validate()?;
        self.schema_digests.validate()?;
        if !self.config.is_object() {
            return Err(SimError::new(
                "invalid_config",
                "config must be a JSON object",
            ));
        }
        if !is_valid_sha256(&self.config_digest) {
            return Err(SimError::new(
                "invalid_config_digest",
                "config_digest must be a 64-character lowercase hex sha256",
            ));
        }
        self.limits.validate()?;
        if self.slots.is_empty() {
            return Err(SimError::new(
                "invalid_slots",
                "at least one slot is required",
            ));
        }
        if self.slots.len() > self.limits.max_players as usize {
            return Err(SimError::new(
                "invalid_slots",
                format!(
                    "slot count {} exceeds max_players {}",
                    self.slots.len(),
                    self.limits.max_players
                ),
            ));
        }
        for slot in &self.slots {
            slot.validate()?;
        }
        let mut slot_ids = self.slot_ids();
        slot_ids.sort();
        slot_ids.dedup();
        if slot_ids.len() != self.slots.len() {
            return Err(SimError::new(
                "invalid_slots",
                "slot identifiers must be unique",
            ));
        }
        if self.failure_policy_version.trim().is_empty() {
            return Err(SimError::new(
                "invalid_spec",
                "failure_policy_version must not be empty",
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
        Ok(())
    }

    pub fn digest(&self) -> Result<String, SimError> {
        execution_spec_digest(self)
    }
}

pub type SimulationSpec = ExecutionSpec;

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
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct SlotAction {
    pub status: ActionStatus,
    pub requested: Option<Value>,
    pub applied: Option<Value>,
    pub policy_decision: String,
    pub error_code: Option<String>,
}

pub type ActionBatch = BTreeMap<String, SlotAction>;

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct SimulationFrame {
    pub tick: u64,
    pub public_snapshot: Value,
    pub observations: BTreeMap<String, Value>,
    pub events: Vec<Value>,
    pub terminal: bool,
    pub result: Option<Value>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct GamePlayerLimits {
    pub minimum: u32,
    pub maximum: u32,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct GameSchemaDigests {
    pub action: String,
    pub observation: String,
    pub public: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct GameDescriptor {
    pub key: GameKey,
    pub players: GamePlayerLimits,
    pub schemas: GameSchemaDigests,
    pub capabilities: Vec<String>,
}

pub trait Simulation {
    fn spec(&self) -> &ExecutionSpec;
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
        spec: ExecutionSpec,
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

pub fn execution_spec_digest(spec: &ExecutionSpec) -> Result<String, SimError> {
    let value = serde_json::to_value(spec).map_err(|error| {
        SimError::new("spec_serialization_failed", error.to_string())
    })?;
    canonical_json_digest("agentrix.execution-spec/2", &value)
}

pub fn simulation_spec_digest(
    spec: &ExecutionSpec,
) -> Result<String, SimError> {
    execution_spec_digest(spec)
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
#[serde(rename_all = "camelCase", deny_unknown_fields)]
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
    pub fn new(spec: &ExecutionSpec) -> Result<Self, SimError> {
        let execution_spec_digest = execution_spec_digest(spec)?;
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

    fn sample_spec() -> ExecutionSpec {
        let config = json!({"difficulty": 2, "nested": {"enabled": true}});
        ExecutionSpec {
            protocol_version: PROTOCOL_VERSION.to_string(),
            run_id: "run-test-1".to_string(),
            match_id: "match-test-1".to_string(),
            engine_version: "0.3.0".to_string(),
            engine_digest: "a".repeat(64),
            build_identity: "test-build".to_string(),
            target: "x86_64-unknown-linux-gnu".to_string(),
            game: GameKey {
                game_id: "test".to_string(),
                game_version: "1".to_string(),
                game_digest: "b".repeat(64),
            },
            schema_digests: SchemaDigests {
                action: "c".repeat(64),
                observation: "d".repeat(64),
                public: "e".repeat(64),
                replay: "f".repeat(64),
            },
            config_digest: canonical_json_digest("test-config", &config)
                .unwrap(),
            config,
            tick_rate: TickRate::new(60, 1).unwrap(),
            seed: 7,
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
                max_players: 2,
                max_entities: 100,
                max_message_bytes: 65536,
            },
            failure_policy_version: "failure-policy/1".to_string(),
            determinism_tier: DeterminismTier::SameArtifactSameTarget,
            rng_algorithm: RNG_ALGORITHM.to_string(),
        }
    }

    #[test]
    fn tick_rate_is_reduced_and_rejects_zero_and_overflow() {
        assert_eq!(
            TickRate::new(120, 2).unwrap(),
            TickRate {
                numerator: 60,
                denominator: 1
            }
        );
        assert!(TickRate::new(0, 1).is_err());
        assert!(TickRate::new(60, 0).is_err());
        assert!(TickRate::new(10_001, 1).is_err());
        assert!(TickRate::new(1, 10_001).is_err());

        // Serde deserialization auto-reduces and validates
        let json_data = r#"{"numerator": 120, "denominator": 2}"#;
        let tr: TickRate = serde_json::from_str(json_data).unwrap();
        assert_eq!(tr.numerator, 60);
        assert_eq!(tr.denominator, 1);

        let bad_json = r#"{"numerator": 0, "denominator": 1}"#;
        assert!(serde_json::from_str::<TickRate>(bad_json).is_err());

        let unknown_field =
            r#"{"numerator": 60, "denominator": 1, "extra": 1}"#;
        assert!(serde_json::from_str::<TickRate>(unknown_field).is_err());
    }

    #[test]
    fn golden_execution_spec_matches_fixture() {
        let manifest_dir = env!("CARGO_MANIFEST_DIR");
        let fixture_path = std::path::Path::new(manifest_dir)
            .join("../../contracts/fixtures/execution_spec_golden.json");
        let fixture_bytes = std::fs::read(&fixture_path).unwrap_or_else(|e| {
            panic!("failed to read fixture at {:?}: {}", fixture_path, e)
        });
        let spec: ExecutionSpec =
            serde_json::from_slice(&fixture_bytes).unwrap();
        spec.validate().unwrap();
        let digest = execution_spec_digest(&spec).unwrap();
        assert_eq!(
            digest,
            "18568c8b3fab8f4d45a9d87f2ff39df18871d37c75117f011f51aadf879d4e72"
        );
    }

    #[test]
    fn spec_validation_and_tamper_detection() {
        let spec = sample_spec();
        assert!(spec.validate().is_ok());

        let mut bad_protocol = spec.clone();
        bad_protocol.protocol_version = "agentrix-engine/1".to_string();
        assert_eq!(
            bad_protocol.validate().unwrap_err().code,
            "invalid_protocol_version"
        );

        let mut bad_engine_digest = spec.clone();
        bad_engine_digest.engine_digest = "short".to_string();
        assert_eq!(
            bad_engine_digest.validate().unwrap_err().code,
            "invalid_engine_digest"
        );

        let mut bad_slots = spec.clone();
        bad_slots.slots.clear();
        assert_eq!(bad_slots.validate().unwrap_err().code, "invalid_slots");

        let mut dup_slots = spec.clone();
        dup_slots.slots[1].slot_id = "alpha".to_string();
        assert_eq!(dup_slots.validate().unwrap_err().code, "invalid_slots");

        let mut bad_rng = spec.clone();
        bad_rng.rng_algorithm = "other".to_string();
        assert_eq!(bad_rng.validate().unwrap_err().code, "unsupported_rng");
    }

    #[test]
    fn alter_any_field_changes_spec_digest() {
        let baseline = sample_spec();
        let base_digest = execution_spec_digest(&baseline).unwrap();

        let mut changed_seed = baseline.clone();
        changed_seed.seed += 1;
        assert_ne!(base_digest, execution_spec_digest(&changed_seed).unwrap());

        let mut changed_tick_rate = baseline.clone();
        changed_tick_rate.tick_rate = TickRate::new(30, 1).unwrap();
        assert_ne!(
            base_digest,
            execution_spec_digest(&changed_tick_rate).unwrap()
        );

        let mut changed_slot_digest = baseline.clone();
        changed_slot_digest.slots[0].artifact_digest = "9".repeat(64);
        assert_ne!(
            base_digest,
            execution_spec_digest(&changed_slot_digest).unwrap()
        );

        let mut changed_config = baseline.clone();
        changed_config.config =
            json!({"difficulty": 3, "nested": {"enabled": true}});
        changed_config.config_digest =
            canonical_json_digest("test-config", &changed_config.config)
                .unwrap();
        assert_ne!(
            base_digest,
            execution_spec_digest(&changed_config).unwrap()
        );
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
        let spec = sample_spec();
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
