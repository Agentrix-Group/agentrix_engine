# Agentrix Starfighter Engine

Motor autoritativo headless de Starfighter para Agentrix. Está escrito en Rust con Bevy y Avian2D y se comunica con el worker Go mediante JSON Lines por `stdin/stdout`.

## Responsabilidades

- inicializar una partida determinista de dos agentes;
- aplicar acciones Starfighter opacas para Go;
- avanzar exactamente `Action[N] -> State[N+1]`;
- producir percepciones privadas por slot;
- producir un `publicSnapshot` sanitizado por tick con naves, proyectiles, vida, escudos, eventos y `stateHash`;
- consolidar ganador, puntuaciones, causa de término y hash final.

El motor no ejecuta bots, no accede a PostgreSQL y no escribe el replay de Agentrix.

## Protocolo

El binario `starfighter-engine` implementa `agentrix-engine/1`:

1. emite `engine_ready`;
2. recibe `initialize_match` y responde `match_initialized` con State 0;
3. recibe `advance_tick` con acciones del tick actual y responde `tick_completed` con el estado siguiente;
4. recibe `finish_match` y responde `match_completed`;
5. recibe `shutdown` y responde `shutdown_ack`.

Una acción válida llega como:

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

## Compilación y pruebas

```bash
cargo test
cargo build --release --bin starfighter-engine
```

El binario generado queda en `target/release/starfighter-engine`. Agentrix lo espera en `bin/starfighter-engine`, salvo que `AGENTRIX_ENGINE_BIN` indique otra ruta.
