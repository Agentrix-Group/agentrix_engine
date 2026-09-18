//! Grabación autoritativa de la partida y sellado del replay -- Fase 5
//! (ATD-011).
//!
//! ## La brecha real entre `ATD-011` y `contracts/replay.schema.json`
//!
//! `ATD-011` (arquitectura de Agentrix) dice: "cada tick produce un
//! lote... con checksum acumulado. Al finalizar se sella el log con
//! digest." Pero `contracts/replay.schema.json` (copiado real desde
//! `github.com/F4nk1/Agentrix` para esta fase) **no tiene ningún campo**
//! para checksum ni digest -- solo `game_id`, `match_id`, `seed`,
//! `players`, `max_ticks`, `winner`, `scores`, `frames[]`. Es una
//! brecha real entre la arquitectura descrita y el contrato JSON
//! concreto que existe hoy en el repo de Agentrix, no algo que yo haya
//! inventado.
//!
//! El schema no tiene `"additionalProperties": false` en ningún nivel,
//! así que agregar `digest` como propiedad adicional del objeto raíz es
//! válido contra el schema tal como está hoy -- lo hago así, documentado
//! como extensión no contemplada por el contrato actual (mismo criterio
//! que la simplificación del schema de acción en Fase 2: se declara la
//! desviación, no se oculta). Si Agentrix formaliza esto en su propio
//! schema más adelante, este campo ya está listo para alinearse.
//!
//! ## Por qué `bincode` para el hashing y no `serde_json` tick a tick
//!
//! `ReplaySealer::push_frame` serializa cada frame a `bincode` solo para
//! calcular el checksum encadenado -- más rápido que JSON y es lo único
//! que hace falta para el hash. El frame en sí (el struct Rust) se
//! guarda tal cual en memoria durante la partida; la conversión a
//! `serde_json` pasa una sola vez, al sellar el replay completo al
//! final (`ReplaySealer::seal`). Para una partida de miles de ticks
//! corriendo dentro de un worker que puede tener varias partidas
//! concurrentes, evitar pagar el costo de serialización JSON en cada
//! tick es la parte que importa; guardar bytes de bincode en vez de los
//! structs nativos sería una optimización de memoria adicional posible
//! pero no implementada acá porque no cambia ningún comportamiento
//! observable de esta fase -- se documenta como simplificación
//! consciente, no como un atajo escondido.
//!
//! ## Qué NO hace este módulo
//!
//! No sanitiza el estado para una vista pública -- `FrameState` incluye
//! `health`/`energy`/`shield_active` completos de cada nave, porque este
//! es el registro **autoritativo** interno (la fuente de verdad para
//! reconstruir o auditar una partida), no la proyección sanitizada para
//! espectadores que describe `06_ux_y_lenguaje_visual.md` (ese filtrado
//! es responsabilidad de la capa de proyección del lado de Agentrix,
//! fuera del alcance de este motor).

use crate::protocol::WireAction;
use crate::{BulletSnapshot, FighterSnapshot, Vec2Data};
use serde::Serialize;
use sha2::{Digest, Sha256};
use std::collections::BTreeMap;

#[derive(Debug, Clone, Serialize)]
pub struct FighterState {
    pub player_id: usize,
    pub position: Vec2Data,
    pub velocity: Vec2Data,
    pub facing: Vec2Data,
    pub health: f32,
    pub energy: f32,
    pub shield_active: bool,
}

impl From<&FighterSnapshot> for FighterState {
    fn from(s: &FighterSnapshot) -> Self {
        FighterState {
            player_id: s.player_id,
            position: s.position.into(),
            velocity: s.velocity.into(),
            facing: s.facing.into(),
            health: s.health,
            energy: s.energy,
            shield_active: s.shield_active,
        }
    }
}

#[derive(Debug, Clone, Serialize)]
pub struct BulletState {
    pub player_id: usize,
    pub position: Vec2Data,
    pub velocity: Vec2Data,
}

impl From<&BulletSnapshot> for BulletState {
    fn from(s: &BulletSnapshot) -> Self {
        BulletState {
            player_id: s.player_id,
            position: s.position.into(),
            velocity: s.velocity.into(),
        }
    }
}

#[derive(Debug, Clone, Serialize)]
pub struct FrameState {
    pub fighters: Vec<FighterState>,
    pub bullets: Vec<BulletState>,
}

/// Un frame del replay -- espejo directo de `frames[]` en
/// `contracts/replay.schema.json` (`tick`, `events`, `state`, `actions`
/// son exactamente esos cuatro campos, sin nada extra a este nivel).
#[derive(Debug, Clone, Serialize)]
pub struct ReplayFrame {
    pub tick: u64,
    pub events: Vec<String>,
    pub state: FrameState,
    /// Clave = `player_id` como string (`"0"`, `"1"`, ...) -- la acción
    /// efectivamente aplicada ese tick, sea que vino de un bot real o de
    /// `default_action()` (timeout/JSON inválido/proceso muerto).
    pub actions: BTreeMap<String, WireAction>,
}

/// Espejo del objeto raíz de `MatchReplay` en
/// `contracts/replay.schema.json`, más `digest` (ver doc de módulo).
#[derive(Debug, Serialize)]
pub struct MatchReplay {
    pub game_id: String,
    pub match_id: String,
    pub seed: u64,
    pub players: Vec<String>,
    pub max_ticks: u32,
    /// El schema no lo marca como requerido -- se omite en vez de
    /// serializar `null` cuando la partida terminó en empate o no
    /// llegó a resolverse.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub winner: Option<String>,
    /// Placeholder, no una fórmula de puntuación real: `PE-005` del
    /// blueprint deja la fórmula de puntuación explícitamente pendiente
    /// de definición "antes de partidas oficiales". `1` para el
    /// ganador, `0` para el resto; `0` para todos en empate. Mismo
    /// estatus que el balance de HP/energía de Fase 1 -- se reemplaza
    /// cuando un concurso real fije la fórmula real, no antes.
    pub scores: BTreeMap<String, i64>,
    pub frames: Vec<ReplayFrame>,
    pub digest: String,
}

/// Acumula frames durante la partida y calcula un checksum encadenado
/// (`ATD-011`: "checksum acumulado"). `digest_n =
/// SHA256(digest_{n-1} || bytes_bincode(frame_n))`, con una semilla
/// inicial derivada de `match_id` + `seed` -- así el digest final
/// depende del orden completo de los frames, no solo de su contenido
/// concatenado, y dos partidas con frames casualmente idénticos pero
/// distinto `match_id`/`seed` no colisionan.
pub struct ReplaySealer {
    running_digest: Vec<u8>,
    frames: Vec<ReplayFrame>,
}

impl ReplaySealer {
    pub fn new(match_id: &str, seed: u64) -> Self {
        let seed_digest = Sha256::digest(format!("{match_id}:{seed}").as_bytes()).to_vec();
        ReplaySealer {
            running_digest: seed_digest,
            frames: Vec::new(),
        }
    }

    pub fn push_frame(&mut self, frame: ReplayFrame) {
        let bytes = bincode::serialize(&frame).expect("ReplayFrame siempre serializa a bincode");
        let mut hasher = Sha256::new();
        hasher.update(&self.running_digest);
        hasher.update(&bytes);
        self.running_digest = hasher.finalize().to_vec();
        self.frames.push(frame);
    }

    #[allow(clippy::too_many_arguments)]
    pub fn seal(
        self,
        game_id: String,
        match_id: String,
        seed: u64,
        players: Vec<String>,
        max_ticks: u32,
        winner: Option<usize>,
        scores: BTreeMap<String, i64>,
    ) -> MatchReplay {
        let digest = self
            .running_digest
            .iter()
            .map(|b| format!("{b:02x}"))
            .collect::<String>();
        MatchReplay {
            game_id,
            match_id,
            seed,
            players,
            max_ticks,
            winner: winner.map(|w| w.to_string()),
            scores,
            frames: self.frames,
            digest,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::protocol::{WireThrust, WireTurn};

    fn sample_frame(tick: u64) -> ReplayFrame {
        let mut actions = BTreeMap::new();
        actions.insert(
            "0".to_string(),
            WireAction {
                thrust: WireThrust::Forward,
                turn: WireTurn::Neutral,
                shoot: false,
                shield: false,
            },
        );
        ReplayFrame {
            tick,
            events: vec![],
            state: FrameState {
                fighters: vec![],
                bullets: vec![],
            },
            actions,
        }
    }

    #[test]
    fn same_frames_same_seed_produce_same_digest() {
        let mut a = ReplaySealer::new("m1", 42);
        let mut b = ReplaySealer::new("m1", 42);
        for t in 0..5 {
            a.push_frame(sample_frame(t));
            b.push_frame(sample_frame(t));
        }
        let ra = a.seal(
            "starfighter".into(),
            "m1".into(),
            42,
            vec!["0".into(), "1".into()],
            10,
            Some(0),
            BTreeMap::new(),
        );
        let rb = b.seal(
            "starfighter".into(),
            "m1".into(),
            42,
            vec!["0".into(), "1".into()],
            10,
            Some(0),
            BTreeMap::new(),
        );
        assert_eq!(ra.digest, rb.digest);
        assert_eq!(ra.digest.len(), 64, "SHA-256 en hex son 64 caracteres");
    }

    #[test]
    fn different_seed_produces_different_digest() {
        let mut a = ReplaySealer::new("m1", 1);
        let mut b = ReplaySealer::new("m1", 2);
        a.push_frame(sample_frame(0));
        b.push_frame(sample_frame(0));
        let ra = a.seal("g".into(), "m1".into(), 1, vec![], 1, None, BTreeMap::new());
        let rb = b.seal("g".into(), "m1".into(), 2, vec![], 1, None, BTreeMap::new());
        assert_ne!(ra.digest, rb.digest);
    }

    #[test]
    fn winner_omitted_when_none() {
        let sealer = ReplaySealer::new("m1", 0);
        let replay = sealer.seal("g".into(), "m1".into(), 0, vec![], 1, None, BTreeMap::new());
        let json = serde_json::to_string(&replay).unwrap();
        assert!(!json.contains("\"winner\""));
    }
}
