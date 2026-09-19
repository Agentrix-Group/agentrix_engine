//! Acción Starfighter recibida dentro de `PlayerActionInput.payload`.
//!
//! Agentrix Go conserva este objeto como JSON opaco. Solo el motor Rust
//! interpreta sus controles y los convierte al modelo físico interno.

use crate::{FighterAction, Shield, Shoot, Thrust, Turn};
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct WireAction {
    pub thrust: WireThrust,
    pub turn: WireTurn,
    pub shoot: bool,
    pub shield: bool,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
pub enum WireThrust {
    #[serde(rename = "FORWARD")]
    Forward,
    #[serde(rename = "OFF")]
    Off,
    #[serde(rename = "BRAKE")]
    Brake,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
pub enum WireTurn {
    #[serde(rename = "LEFT")]
    Left,
    #[serde(rename = "RIGHT")]
    Right,
    #[serde(rename = "NONE")]
    Neutral,
}

impl From<WireAction> for FighterAction {
    fn from(wire: WireAction) -> Self {
        FighterAction {
            thrust: match wire.thrust {
                WireThrust::Forward => Thrust::On,
                WireThrust::Off => Thrust::Off,
                WireThrust::Brake => Thrust::Stop,
            },
            turn: match wire.turn {
                WireTurn::Left => Turn::Left,
                WireTurn::Right => Turn::Right,
                WireTurn::Neutral => Turn::None,
            },
            shoot: if wire.shoot { Shoot::On } else { Shoot::Off },
            shield: if wire.shield { Shield::On } else { Shield::Off },
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn wire_action_matches_starfighter_contract() {
        let json =
            r#"{"thrust":"FORWARD","turn":"LEFT","shoot":true,"shield":false}"#;
        let parsed: WireAction =
            serde_json::from_str(json).expect("debe parsear");
        let action: FighterAction = parsed.into();
        assert_eq!(action.thrust, Thrust::On);
        assert_eq!(action.turn, Turn::Left);
        assert_eq!(action.shoot, Shoot::On);
        assert_eq!(action.shield, Shield::Off);
    }

    #[test]
    fn malformed_or_incomplete_actions_are_rejected() {
        let missing = r#"{"turn":"LEFT","shoot":true,"shield":false}"#;
        assert!(serde_json::from_str::<WireAction>(missing).is_err());
        assert!(serde_json::from_str::<WireAction>("not json").is_err());
    }

    #[test]
    fn unknown_controls_and_fields_are_rejected() {
        let unknown_control =
            r#"{"thrust":"BOOST","turn":"NONE","shoot":false,"shield":false}"#;
        assert!(serde_json::from_str::<WireAction>(unknown_control).is_err());
        let unknown_field = r#"{"thrust":"OFF","turn":"NONE","shoot":false,"shield":false,"teleport":true}"#;
        assert!(serde_json::from_str::<WireAction>(unknown_field).is_err());
    }
}
