# Agentrix Engine

Workspace del motor autoritativo headless de Agentrix. Starfighter continúa
usando Bevy y Avian2D; un juego discreto de conformidad prueba la frontera
multi-juego sin introducir otro backend físico.

## Capas compilables

- `crates/sim-core`: contratos in-process, razón de tick, RNG portable,
  codificación canónica y commitments separados; no depende de Bevy ni IPC.
- `crates/engine-host`: registry y lifecycle genéricos sobre payloads opacos;
  no importa tipos de Starfighter.
- `crates/games/conformance-game`: segundo juego mínimo sin física.
- crate raíz `bevy-starfighter`: reglas y física Avian2D de Starfighter y el
  adaptador temporal del protocolo v1.
- `bins/conformance-runner`: vectores D1/D2 ejecutables en procesos limpios.

## Responsabilidades actuales

- Inicializa una partida a partir de semilla, timestep, participantes, límite
  de ticks y `radar_range` opcional.
- Aplica acciones de Starfighter y avanza `Action[N] -> State[N+1]`.
- Produce percepciones privadas, eventos, snapshot público y `stateHash` en el
  adaptador v1.
- Consolida causa de término, ganador, puntuaciones y hash final.
- En el camino `sim-core`, calcula por separado digest de spec, acciones,
  estado autoritativo, snapshot público y cadena de evidencia.

El motor no ejecuta bots, no accede a PostgreSQL y no escribe el replay de
Agentrix. Algunas reglas y parámetros competitivos todavía son constantes del
crate; recibir una configuración no significa que todos sus campos se apliquen.

## Protocolo

El binario `starfighter-engine` implementa `agentrix-engine/1`:

1. emite `engine_ready`;
2. recibe `initialize_match` y responde `match_initialized` con State 0;
3. recibe `advance_tick` y responde `tick_completed` con el estado siguiente;
4. recibe `finish_match` y responde `match_completed`;
5. recibe `shutdown` y responde `shutdown_ack`.

Una acción válida tiene esta forma:

```json
{
  "status": "valid",
  "payload": {
    "thrust": "FORWARD",
    "turn": "NONE",
    "shoot": false,
    "shield": false
  }
}
```

`stdout` se reserva al protocolo y los logs se escriben en `stderr`. Los
esquemas y la documentación completa del contrato están en
`Agentrix/protocol/engine/v1/`.

El protocolo v1 permanece activo deliberadamente durante este checkpoint. Los
schemas v2 están versionados en `schemas/`, pero todavía no son el transporte
Rust↔Go; esa migración corresponde a la fase siguiente y debe ser atómica con
el consumidor Go.

## Frecuencia y determinismo

El core representa el tiempo con `TickRate { numerator, denominator }`; `60/1`
es una única autoridad exacta. El adaptador v1 todavía acepta sus campos
históricos hasta la migración coordinada de protocolo.

El tier certificado por el corpus actual es D1/D2 para el mismo artefacto y
target Linux x86_64. No se afirma igualdad entre arquitecturas. Gym y la
evaluación Avian/Rapier siguen fuera de este corte.

## Compilación y pruebas

Sin descargar o actualizar dependencias:

```bash
cargo test --workspace --all-targets --locked --offline
cargo clippy --workspace --all-targets --all-features --locked --offline -- -D warnings
cargo build --workspace --locked --offline --release
```

El binario IPC compatible se genera en `target/release/starfighter-engine`. El
runner de certificación se genera como `agentrix-conformance-vector`.

## Relación con Agentrix

El repositorio principal define las decisiones vigentes en `docs/decisions/`,
el estado real en `docs/architecture/` y el orden de implementación en
`docs/roadmap/current.md`. Este repositorio no mantiene un roadmap alternativo.
