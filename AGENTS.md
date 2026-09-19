# Instrucciones de trabajo — Agentrix Engine

## Alcance

Este repositorio contiene el motor autoritativo headless de Starfighter. Es un
componente de Agentrix, no un producto independiente. Las decisiones vigentes
de producto, arquitectura y orden de trabajo viven en el repositorio principal,
en `docs/index.md`, `docs/decisions/` y `docs/roadmap/current.md`.

## Responsabilidades

- Rust, Bevy y Avian2D implementan reglas, física, percepciones, eventos,
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
- Avian2D es la física aceptada para el MVP. Rapier, Gym, sim-core y
  multi-juego son trabajo posterior en el orden del roadmap principal.
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
