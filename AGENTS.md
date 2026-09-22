# Instrucciones de trabajo — Agentrix Engine

## Alcance

Este repositorio contiene el motor autoritativo headless de Starfighter. Es un
componente de Agentrix, no un producto independiente. Las decisiones vigentes
de producto, arquitectura y orden de trabajo viven en el repositorio principal,
en `docs/index.md`, `docs/decisions/` y `docs/roadmap/current.md`.

## Responsabilidades

- Rust, Bevy ECS y `rapier2d` implementan reglas, física, percepciones, eventos,
  puntajes y estado autoritativo de Starfighter.
- `starfighter-engine` habla `agentrix-engine/1` por JSON Lines.
- `stdout` queda reservado al protocolo; logs y diagnósticos van a `stderr`.
- El motor no ejecuta bots, no accede a PostgreSQL y no escribe el replay de
  la plataforma.

## Invariantes

- Una acción de `tick N` produce el estado de `tick N+1`.
- La semilla y la secuencia de acciones son entradas explícitas.
- No se afirma determinismo certificado solo porque exista `stateHash`.
- Starfighter es el único juego actual.
- La migración de Avian2D a `rapier2d` y a partidas de 2 a 5 jugadores
  todos contra todos está aprobada por ADR-0013 del repositorio principal y
  avanza por fases F0–F7 con criterios numéricos. Desde F3 la física es
  `rapier2d` 0.35 con `enhanced-determinism`, usado directamente desde
  `src/physics.rs` (sin `bevy_rapier2d`). Gym, sim-core y multi-juego siguen el orden
  del roadmap principal.
- El objetivo es 60 Hz exactos. La entrada actual en milisegundos enteros y la
  configuración de 17 ms son una divergencia pendiente; no se deben presentar
  como equivalentes a 60 Hz.

## Comprobaciones

Sin instalar ni actualizar dependencias:

```bash
cargo test --locked --offline
cargo build --locked --offline --release --bin starfighter-engine
```

Para evitar escribir artefactos en el repositorio puede definirse
`CARGO_TARGET_DIR` con una ruta temporal.

## Regla de cambio

Antes de cambiar el protocolo, las reglas competitivas o la física, relacione
el cambio con una decisión y un corte aprobado del repositorio principal. No
convierta constantes o stubs existentes en requisitos. Conserve compatibilidad
entre los esquemas de `Agentrix/protocol/engine/v1/`, el cliente Go y este
servidor Rust, o cambie los tres explícitamente en el mismo corte.
