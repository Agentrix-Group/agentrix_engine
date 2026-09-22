# Throughput con Rapier: F3 del corte Rapier/FFA

Resultado de la migración a `rapier2d`, comparado con la línea base de Avian
en [`avian2d-f0.md`](avian2d-f0.md). Las estimaciones crudas de criterion
están en `rapier2d-f3/*.estimates.json`.

## Condiciones

Iguales a F0, salvo el backend físico:

| Campo | Valor |
| --- | --- |
| Fecha | 2026-09-22 |
| Física | `rapier2d` 0.35.3 con `enhanced-determinism`, `bevy` 0.19.1 |
| Toolchain | rustc 1.98.1, perfil `bench` (release) |
| Máquina | Intel Core i5-1135G7, 8 hilos, x86_64, portátil sin aislamiento de CPU |
| Comando | `cargo bench --locked --offline --bench tick_throughput` |

Mismo código de benchmark, mismas semillas y misma cantidad de ticks por
lote. La unidad y el estadístico son los mismos que en F0: tiempo de pared
del lote, media de criterion con intervalo de confianza del 95 % sobre 100
muestras, `setup` excluido y `drop` incluido.

## Resultados

| Escenario | Avian F0 (µs/tick) | Rapier F3 (IC 95 % del lote) | Rapier (µs/tick) | Cambio |
| --- | --- | --- | --- | --- |
| 2 naves, sin acciones, 100 ticks | 55.1 | 1.029 – **1.031** – 1.034 ms | 10.3 | −81.3 % |
| 2 naves, combate, 600 ticks | 44.5 | 6.825 – **6.849** – 6.874 ms | 11.4 | −74.3 % |
| 5 naves, combate, 600 ticks | 50.2 | 10.371 – **10.401** – 10.430 ms | 17.3 | −65.5 % |

## Qué se compara y qué no

- Se compara el costo de un tick completo del motor: reglas, física y
  resolución de contactos. Las partidas no son idénticas entre backends:
  con Rapier las balas no son cuerpos rígidos (se barren) y los contactos se
  resuelven en el mismo tick. Ambas cosas son parte del diseño de ADR-0013.
- Buena parte de la mejora viene de dejar de correr los plugins de Avian y
  el acumulador de `Time<Fixed>`, no solo del solver. No se aisló cuánto
  aporta cada parte.
- El escenario de 5 naves sigue midiendo costo, no una partida válida: el
  spawn fuera de la arena se corrige en F4.
