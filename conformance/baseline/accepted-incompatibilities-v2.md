# Incompatibilidades aceptadas para el corte sim-core

Este corte se apoya en ADR-0002 y en el siguiente corte del roadmap principal,
`sim-core -> segundo juego`. No cambia todavía el protocolo IPC v1 ni el
consumidor Go.

- La simulación compartida usa `xoshiro256starstar/1`; por ello el adaptador v1
  conserva su wire format, pero no las posiciones aleatorias de asteroides
  producidas antes por `SmallRng`.
- El estado autoritativo nuevo usa IDs lógicos y codificación binaria canónica;
  sus commitments no son comparables con `stateHash` v1, que sigue siendo un
  hash encadenado del snapshot público.
- En el camino `sim-core`, `State[0]` ejecuta solo reset/startup con duración
  cero. El lifecycle y los envelopes del adaptador v1 permanecen vigentes hasta
  el corte coordinado de protocolo.
- La identidad nueva de Starfighter es `starfighter/0.3.0-core.1` y no
  reinterpreta evidencia producida por el juego anterior.
- El tier certificado en este checkpoint es exclusivamente mismo artefacto,
  `x86_64-unknown-linux-gnu`. No se afirma igualdad ARM64/multi-OS.
