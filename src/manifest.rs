//! Carga de `games/<juego>/manifest.yaml` -- Fase 4.
//!
//! `Settings::radar_range` (Fase 3) quedó documentado como "sincronizado
//! a mano con el manifest hasta que Fase 4 lo lea de verdad". Esto cierra
//! esa promesa: `runner::run_match` carga el manifest real al arrancar y
//! lo usa como fuente de verdad para los parámetros que declara
//! (`radar_range`, `max_ticks`, `tick_hz`), no como documentación suelta.

use serde::Deserialize;
use std::collections::HashMap;
use std::path::Path;

/// Espejo de `contracts/game.schema.json` (campos relevantes). No
/// duplica `grid_width`/`grid_height` porque starfighter es continuo,
/// no un grid -- ver la nota en `games/starfighter/manifest.yaml`.
#[derive(Debug, Clone, Deserialize)]
pub struct GameManifest {
    pub id: String,
    pub name: String,
    pub version: String,
    #[serde(default)]
    pub description: String,
    pub min_players: u32,
    pub max_players: u32,
    pub max_ticks: u32,
    #[serde(default)]
    pub settings: HashMap<String, String>,
}

#[derive(Debug)]
pub enum ManifestError {
    Io(String, std::io::Error),
    Parse(String, serde_yaml::Error),
}

impl std::fmt::Display for ManifestError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ManifestError::Io(path, e) => write!(f, "no se pudo leer el manifest en {path}: {e}"),
            ManifestError::Parse(path, e) => {
                write!(f, "no se pudo parsear el manifest en {path} como YAML: {e}")
            }
        }
    }
}

impl std::error::Error for ManifestError {}

impl GameManifest {
    pub fn load(path: &Path) -> Result<Self, ManifestError> {
        let raw = std::fs::read_to_string(path)
            .map_err(|source| ManifestError::Io(path.display().to_string(), source))?;
        serde_yaml::from_str(&raw)
            .map_err(|source| ManifestError::Parse(path.display().to_string(), source))
    }

    /// Todos los valores de `settings` son strings en el contrato
    /// (`contracts/game.schema.json`: `additionalProperties: {"type":
    /// "string"}`) -- este helper hace el parseo numérico que cada
    /// consumidor necesita, sin asumir un tipo por adelantado.
    pub fn setting_f32(&self, key: &str) -> Option<f32> {
        self.settings.get(key)?.parse().ok()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn loads_the_real_starfighter_manifest() {
        let path =
            Path::new(env!("CARGO_MANIFEST_DIR")).join("games/starfighter/manifest.yaml");
        let manifest =
            GameManifest::load(&path).expect("el manifest real de Fase 2 debe cargar sin error");
        assert_eq!(manifest.id, "starfighter");
        assert_eq!(manifest.min_players, 2);
        assert_eq!(manifest.max_players, 4);
        assert_eq!(manifest.max_ticks, 3600);
        assert_eq!(manifest.setting_f32("radar_range"), Some(800.0));
        assert_eq!(manifest.setting_f32("tick_hz"), Some(60.0));
    }

    #[test]
    fn missing_file_reports_io_error_not_panic() {
        let path = Path::new("/no/existe/manifest.yaml");
        let result = GameManifest::load(path);
        assert!(matches!(result, Err(ManifestError::Io(_, _))));
    }

    #[test]
    fn malformed_yaml_reports_parse_error_not_panic() {
        let dir = std::env::temp_dir().join(format!(
            "starfighter-manifest-test-{}",
            std::process::id()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("bad.yaml");
        std::fs::write(&path, "id: [esto no cierra\n").unwrap();
        let result = GameManifest::load(&path);
        assert!(matches!(result, Err(ManifestError::Parse(_, _))));
        let _ = std::fs::remove_dir_all(&dir);
    }
}
