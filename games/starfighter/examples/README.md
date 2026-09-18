# Starfighter reference bots

Three reference strategies, each a plain Python 3 script speaking the
real Agentrix agent protocol (ATD-007, `src/protocol.rs`) over
stdin/stdout -- no dependencies beyond the standard library, no need to
compile anything.

- `bot_random.py` -- uniformly random action every tick. Comparison floor.
- `bot_hunter.py` -- aggressive: closes distance, turns toward and shoots
  the nearest rival in radar range.
- `bot_evasive.py` -- defensive: flees and raises shield when a rival
  gets within its danger range, coasts and regenerates energy otherwise.

These are distinct from `tests/fixtures/*.py`, which exist only to
exercise the Fase 4 protocol implementation itself (timeouts, malformed
JSON, version mismatches) and are not meant as gameplay examples.

## Running two of them against each other

From the repository root:

```sh
cargo run --bin native-launcher -- \
  --scripts games/starfighter/examples/bot_hunter.py,games/starfighter/examples/bot_evasive.py \
  --match-id demo-1 --seed 1 --max-ticks 600 \
  --output-replay /tmp/demo-1.json
```

`--max-ticks` overrides `games/starfighter/manifest.yaml`'s `max_ticks`
for a shorter demo run; drop it to use the real value. See
`src/runner.rs` for the full set of CLI flags.
