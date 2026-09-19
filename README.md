# Agentrix Starfighter Engine

Motor autoritativo headless de Starfighter para Agentrix. Está escrito en Rust
con Bevy y Avian2D y se comunica con el worker Go mediante JSON Lines por
`stdin/stdout`.

## Responsabilidades actuales

- Inicializa una partida a partir de semilla, timestep, participantes, límite
  de ticks y `radar_range` opcional.
- Aplica acciones de Starfighter y avanza `Action[N] -> State[N+1]`.
- Produce percepciones privadas, eventos, snapshot público y `stateHash`.
- Consolida causa de término, ganador, puntuaciones y hash final.

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

La implementación aún no valida exhaustivamente la versión, secuencia e
identidad de cada mensaje entrante. Esa brecha debe cerrarse antes de certificar
el protocolo.

## Frecuencia y determinismo

El objetivo aprobado es **60 Hz exactos**. El contrato actual recibe
`fixedTimestepMs` como entero y Agentrix todavía envía `17` ms, equivalente a
aproximadamente 58.82 Hz. Esa divergencia está documentada y no se considera
resuelta por este cambio editorial.

El hash encadenado ayuda a detectar divergencias. No constituye por sí solo una
certificación de determinismo entre arquitecturas. El futuro sim-core, Gym, un
segundo juego y la evaluación Avian/Rapier están fuera del MVP actual.

## Compilación y pruebas

Sin descargar o actualizar dependencias:

```bash
cargo test --locked --offline
cargo build --locked --offline --release --bin starfighter-engine
```

El binario se genera en `target/release/starfighter-engine`. El repositorio
principal lo instala en `bin/starfighter-engine`; `AGENTRIX_ENGINE_BIN` puede
indicar otra ruta al ejecutar Agentrix.

## Relación con Agentrix

El repositorio principal define las decisiones vigentes en `docs/decisions/`,
el estado real en `docs/architecture/` y el orden de implementación en
`docs/roadmap/current.md`. Este repositorio no mantiene un roadmap alternativo.
