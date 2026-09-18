//! Protocolo de agente de Agentrix (ATD-007): JSON Lines por
//! stdin/stdout entre el runner y el proceso de cada bot -- Fase 4.
//!
//! `games/starfighter/contracts/action.schema.json` y
//! `games/starfighter/contracts/perception.schema.json` (Fases 2 y 3)
//! documentan el contrato de las partes reutilizables de un mensaje
//! (acción, percepción). Este archivo define el sobre del protocolo en
//! sí -- handshake, init y fin de partida -- que es genérico de la
//! plataforma, no específico de starfighter, así que no tiene un JSON
//! Schema propio en `games/starfighter/` (viviría a nivel de plataforma
//! si Agentrix decide fijarlo formalmente más adelante).

use crate::{FighterAction, Perception, Shield, Shoot, Thrust, Turn};
use serde::{Deserialize, Serialize};

pub const PROTOCOL_VERSION: &str = "1.0";

/// Mensajes que el runner le manda a un proceso de bot, uno por línea de
/// stdin. `#[serde(tag = "type")]` produce el campo discriminador
/// `"type"` que exige ATD-007.
#[derive(Debug, Clone, Serialize)]
#[serde(tag = "type")]
pub enum RunnerMessage {
    #[serde(rename = "handshake")]
    Handshake {
        protocol_version: String,
        match_id: String,
        player_id: usize,
        seed: u64,
    },
    #[serde(rename = "init")]
    Init {
        protocol_version: String,
        match_id: String,
        /// Presupuesto de tiempo por decisión, en milisegundos. Ver
        /// `runner::run_match` para por qué es un placeholder.
        timeout_ms: u64,
        radar_range: f32,
        max_ticks: u32,
        tick_hz: f64,
    },
    #[serde(rename = "perception")]
    Perception {
        protocol_version: String,
        match_id: String,
        perception: Perception,
    },
    #[serde(rename = "end")]
    End {
        protocol_version: String,
        match_id: String,
        winner: Option<usize>,
        reason: String,
    },
}

/// Acción tal como llega por el wire, en la forma exacta de
/// `games/starfighter/contracts/action.schema.json` -- nombres de string
/// (`"FORWARD"`, `"LEFT"`, ...), no los enums internos del motor
/// (`Thrust::On`, `Turn::Left`, ...). La conversión a `FighterAction` es
/// explícita (`From<WireAction>`), nunca implícita, para que un cambio
/// en el contrato o en el motor no se filtre en silencio al otro lado.
#[derive(Debug, Clone, Copy, Deserialize)]
pub struct WireAction {
    pub thrust: WireThrust,
    pub turn: WireTurn,
    pub shoot: bool,
    pub shield: bool,
}

#[derive(Debug, Clone, Copy, Deserialize)]
pub enum WireThrust {
    #[serde(rename = "FORWARD")]
    Forward,
    #[serde(rename = "OFF")]
    Off,
    #[serde(rename = "BRAKE")]
    Brake,
}

#[derive(Debug, Clone, Copy, Deserialize)]
pub enum WireTurn {
    #[serde(rename = "LEFT")]
    Left,
    #[serde(rename = "RIGHT")]
    Right,
    /// El wire format usa la cadena `"NONE"` (ver `action.schema.json`);
    /// el identificador Rust se llama `Neutral` a propósito, para no
    /// convivir visualmente con `Option::None` en el mismo archivo.
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

/// Mensajes que un proceso de bot le manda al runner, uno por línea de
/// stdout. Cualquier línea que no matchee esta forma (JSON inválido,
/// campo faltante, `type` desconocido) falla a parsear con `Err` -- el
/// runner trata eso como acción ausente, nunca como panic (RF-044).
#[derive(Debug, Clone, Deserialize)]
#[serde(tag = "type")]
pub enum BotMessage {
    #[serde(rename = "handshake_ack")]
    HandshakeAck { protocol_version: String },
    #[serde(rename = "action")]
    Action { tick: u64, action: WireAction },
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn runner_message_produces_expected_type_tag() {
        let msg = RunnerMessage::Init {
            protocol_version: PROTOCOL_VERSION.to_string(),
            match_id: "m1".to_string(),
            timeout_ms: 50,
            radar_range: 800.0,
            max_ticks: 3600,
            tick_hz: 60.0,
        };
        let json = serde_json::to_string(&msg).expect("RunnerMessage siempre serializa");
        assert!(json.contains("\"type\":\"init\""));
    }

    #[test]
    fn wire_action_matches_action_schema_shape_and_converts() {
        let json = r#"{"type":"action","tick":42,"action":{"thrust":"FORWARD","turn":"LEFT","shoot":true,"shield":false}}"#;
        let parsed: BotMessage = serde_json::from_str(json).expect("debe parsear");
        match parsed {
            BotMessage::Action { tick, action } => {
                assert_eq!(tick, 42);
                let fa: FighterAction = action.into();
                assert_eq!(fa.thrust, Thrust::On);
                assert_eq!(fa.turn, Turn::Left);
                assert_eq!(fa.shoot, Shoot::On);
                assert_eq!(fa.shield, Shield::Off);
            }
            _ => panic!("esperaba Action"),
        }
    }

    #[test]
    fn invalid_json_fails_to_parse_not_panics() {
        let parsed: Result<BotMessage, _> = serde_json::from_str("esto no es json {{{");
        assert!(parsed.is_err());
    }

    #[test]
    fn action_missing_required_field_fails_to_parse() {
        let bad = r#"{"type":"action","tick":1,"action":{"turn":"LEFT","shoot":true,"shield":false}}"#;
        let parsed: Result<BotMessage, _> = serde_json::from_str(bad);
        assert!(parsed.is_err(), "falta thrust, el parseo debe fallar");
    }

    #[test]
    fn unknown_thrust_value_fails_to_parse() {
        let bad = r#"{"type":"action","tick":1,"action":{"thrust":"BOOST","turn":"NONE","shoot":false,"shield":false}}"#;
        let parsed: Result<BotMessage, _> = serde_json::from_str(bad);
        assert!(
            parsed.is_err(),
            "BOOST todavía no existe en el contrato (Decisión 2.2 simplificada, Fase 2), debe fallar"
        );
    }
}
