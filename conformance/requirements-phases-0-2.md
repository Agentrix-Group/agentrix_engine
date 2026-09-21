# Matriz requisito → código → prueba → evidencia (fases 0–2)

| Requisito | Código/artefacto | Prueba o comando | Evidencia esperada |
| --- | --- | --- | --- |
| Baseline reproducible | `Cargo.lock`, `rust-toolchain.toml`, `conformance/baseline/` | `cargo test --workspace --all-targets --locked --offline` | suites y métricas ligadas a commits/toolchain |
| Host sin tipos concretos | `crates/engine-host` | `rg 'FighterAction|StarfighterConfig|bevy::|avian2d' crates/engine-host` | cero coincidencias |
| Registry por triple exacta | `GameRegistry` | `exact_game_triple_is_required_before_state_creation` | digest incorrecto falla antes de crear estado |
| Segundo juego sin física | `crates/games/conformance-game` | `conformance_game_runs_to_completion_without_host_branches` | partida completa por el host común |
| Tick como razón exacta | `TickRate` | `tick_rate_is_reduced_and_rejects_zero` | razón reducida, cero rechazado |
| RNG portable/versionado | `DeterministicRng`, `RNG_ALGORITHM` | `rng_has_stable_vectors_and_serializable_state` | vector fijo y estado round-trip |
| IDs lógicos | `StableEntityId`, `EntityIdAllocator` | tests Starfighter y encoding autoritativo | ninguna identidad persistente usa `bevy::Entity` |
| Encoding canónico | `CanonicalEncoder` | tests de orden, floats y no finitos | mapas equivalentes codifican igual; NaN/inf fallan |
| Hashes separados | `CommitmentChain` | `hidden_state_and_actions_change_separate_commitments` | estado oculto cambia commitment sin cambiar public hash |
| Estado autoritativo Starfighter | `game_module::encode_authoritative_state` | repetición D1/D2 Starfighter | incluye reglas, RNG, IDs, naves, balas, asteroides y terminal |
| Estado 0 y paso exacto | `StarfighterGame::create`, `Simulation::advance` | `state_zero_is_reset_only_and_each_advance_is_exactly_one_tick` | reset = tick 0; acción N = estado N+1 |
| D1/D2 en procesos limpios | `agentrix-conformance-vector` | `process_determinism.rs` | 100 procesos por vector producen bytes idénticos |
| Schemas cerrados v2 | `schemas/v2`, `schemas/games` | parseo/hash en suite de conformidad | `additionalProperties: false` en objetos contractuales |
| Avian conservado | `Cargo.toml`, `src/lib.rs` | corpus físico existente | Starfighter continúa sobre Avian2D 0.7 |
