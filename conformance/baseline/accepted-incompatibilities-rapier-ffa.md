# Incompatibilidades aceptadas para el corte Rapier + todos contra todos

Este corte aplica ADR-0013 del repositorio principal (fases F0–F5 en el
motor). No cambia el protocolo `agentrix-engine/2`, pero publica una
identidad de juego nueva: `starfighter/0.4.0`, motor `0.4.0`.

- La física pasa de Avian2D a `rapier2d` 0.35.3 con `enhanced-determinism`.
  Ninguna partida de `starfighter/0.3.0-core.1` se reproduce con este motor:
  cambian el solver, el orden de resolución de contactos (ahora en el mismo
  tick) y las balas, que dejan de ser cuerpos rígidos.
- `conformance/vectors/starfighter-0.3.0-core.1.json` queda como evidencia
  histórica de Avian2D y ya no se verifica. Sus reemplazos son
  `starfighter-0.4.0.json` (8 ticks, 2 jugadores),
  `starfighter-0.4.0-long-2p.json` y `starfighter-0.4.0-ffa-long-5p.json`
  (partidas completas de hasta 3600 ticks, terminadas por eliminación).
- El estado autoritativo incluye ahora handles de arena e impulsos de
  warm-start de Rapier, el marcador de eliminaciones y bajas, y el autor de
  cada `Destroyed`. Sus commitments no son comparables con los de 0.3.0.
- El binario solo registra `starfighter/0.4.0`: una spec de
  `0.3.0-core.1` se rechaza con `no compiled game matches`. La plataforma Go
  sigue fijada al motor anterior en `engine.lock` hasta el corte F6.
- `rankings` de `match_completed` pasa de ganador/perdedor a clasificación
  por eliminación con puestos de competencia (1, 2, 2, 4) y `score` = bajas.
- El tier certificado sigue siendo mismo artefacto sobre
  `x86_64-unknown-linux-gnu`. Los vectores coinciden además entre perfil
  debug y release del mismo commit, pero no se afirma igualdad ARM64 ni
  multi-OS hasta F7.
