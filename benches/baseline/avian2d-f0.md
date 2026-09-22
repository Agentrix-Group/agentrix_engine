# Línea base de throughput — Avian2D (F0 del corte Rapier/FFA)

Referencia contra la que se compara la migración a Rapier (F3). Las
estimaciones crudas de criterion están en `avian2d-f0/*.estimates.json`.

## Condiciones

| Campo | Valor |
| --- | --- |
| Fecha | 2026-09-22 |
| Commit base | `486e22b` + bench de F0 |
| Física | `avian2d` 0.7.0, `bevy` 0.19.1 |
| Toolchain | rustc 1.98.1, perfil `bench` (release) |
| Máquina | Intel Core i5-1135G7, 8 hilos, x86_64, portátil sin aislamiento de CPU |
| Comando | `cargo bench --locked --offline --bench tick_throughput` |

## Qué mide cada escenario

- Unidad: tiempo de pared de un lote completo; el `setup` (construir la app
  y el Startup) queda fuera de la medición, el `drop` de la app queda dentro.
- Estadístico: estimación puntual de criterion (media) con intervalo de
  confianza del 95 %, sobre 100 muestras.
- `2_fighters_5_asteroids`: 100 `app.update()` con 2 naves sin acciones y
  5 asteroides.
- `N_fighters_combat_600_ticks`: 600 ticks de una partida creada por
  `setup` (semilla 7, 5 asteroides), con una acción pseudoaleatoria por
  nave y tick (semilla 11; disparo 85 %, escudo 20 %).

## Resultados

| Escenario | Tiempo del lote (IC 95 %) | µs/tick | ticks/s |
| --- | --- | --- | --- |
| 2 naves, sin acciones, 100 ticks | 5.473 – **5.511** – 5.556 ms | 55.1 | ~18 100 |
| 2 naves, combate, 600 ticks | 26.559 – **26.689** – 26.823 ms | 44.5 | ~22 500 |
| 5 naves, combate, 600 ticks | 29.962 – **30.137** – 30.341 ms | 50.2 | ~19 900 |

## Advertencias

- Con 5 naves el `setup` actual ubica dos naves fuera de la arena
  (y ≈ ±570 con `half_height` = 500); el escenario mide costo, no una
  partida válida. Se corrige en F4.
- El escenario sin acciones cuesta más por tick que el de combate: los 100
  ticks amortizan peor el costo fijo de los primeros `update()` y del
  `drop`. Compárese cada escenario solo consigo mismo entre backends.
- Portátil con escalado de frecuencia activo: variaciones de
  ±5 % entre corridas no son significativas.
