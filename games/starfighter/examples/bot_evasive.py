#!/usr/bin/env python3
"""bot_evasive.py -- reference strategy: defensive, flees and shields.

If the nearest rival is within DANGER_RANGE, turn to face directly away
from it, thrust to open distance, and raise the shield once energy is
comfortably above the per-tick shield cost (a flat >20 threshold avoids
flickering the shield on and off every other tick right at the edge of
affording it -- a placeholder choice, not a balance decision). With no
rival that close, coast (thrust OFF) and don't shield, so
`ENERGY_REGEN_PER_TICK` (src/lib.rs) rebuilds the energy pool for the
next engagement instead of spending it on unnecessary movement.

Never shoots -- this is a pure survival/energy-conservation strategy,
the opposite end of the spectrum from bot_hunter.py.

Speaks the real Agentrix agent protocol (ATD-007) over stdin/stdout, one
JSON object per line -- no dependency beyond the Python standard library.
"""
import json
import math
import sys

PROTOCOL_VERSION = "1.0"

# Below this distance to the nearest rival, evasive maneuvers kick in.
# Half of the default radar_range (800, games/starfighter/manifest.yaml)
# -- close enough to be a real threat, far enough to start turning away
# before it's already in effective bullet range. Placeholder, not tuned
# against a real opponent.
DANGER_RANGE = 400.0

# Comfortable margin above SHIELD_ENERGY_COST_PER_TICK (1.0, src/lib.rs)
# before committing to holding the shield up, to avoid flicker right at
# the point where the engine would auto-drop it for lack of energy.
SHIELD_ENERGY_MARGIN = 20.0

AIM_TOLERANCE = 0.2


def send(msg: dict) -> None:
    sys.stdout.write(json.dumps(msg) + "\n")
    sys.stdout.flush()


def read() -> dict:
    line = sys.stdin.readline()
    if not line:
        sys.exit(0)
    return json.loads(line)


def normalize_angle(a: float) -> float:
    while a > math.pi:
        a -= 2 * math.pi
    while a < -math.pi:
        a += 2 * math.pi
    return a


def steer_toward(target_x: float, target_y: float, facing_x: float, facing_y: float) -> str:
    """Same convention as bot_hunter.py: Turn::Left increases the facing
    angle (counter-clockwise). Here the "target" passed in is the
    fleeing direction (away from the rival), not the rival itself."""
    target_angle = math.atan2(target_y, target_x)
    facing_angle = math.atan2(facing_y, facing_x)
    diff = normalize_angle(target_angle - facing_angle)
    if diff > AIM_TOLERANCE:
        return "LEFT"
    if diff < -AIM_TOLERANCE:
        return "RIGHT"
    return "NONE"


def choose_action(perception: dict) -> dict:
    myself = perception["myself"]
    rivals = perception["rivals"]

    if not rivals:
        return {"thrust": "OFF", "turn": "NONE", "shoot": False, "shield": False}

    nearest = min(
        rivals,
        key=lambda r: math.hypot(r["relative_position"]["x"], r["relative_position"]["y"]),
    )
    rel = nearest["relative_position"]
    distance = math.hypot(rel["x"], rel["y"])

    if distance >= DANGER_RANGE:
        return {"thrust": "OFF", "turn": "NONE", "shoot": False, "shield": False}

    # Flee: face and thrust in the direction directly opposite the rival.
    flee_x, flee_y = -rel["x"], -rel["y"]
    turn = steer_toward(flee_x, flee_y, myself["facing"]["x"], myself["facing"]["y"])
    shield = myself["energy"] > SHIELD_ENERGY_MARGIN

    return {"thrust": "FORWARD", "turn": turn, "shoot": False, "shield": shield}


def main() -> None:
    handshake = read()
    assert handshake["type"] == "handshake"
    send({"type": "handshake_ack", "protocol_version": PROTOCOL_VERSION})

    read()  # init

    while True:
        msg = read()
        if msg["type"] == "end":
            return
        assert msg["type"] == "perception"
        action = choose_action(msg["perception"])
        send({"type": "action", "tick": msg["perception"]["tick"], "action": action})


if __name__ == "__main__":
    main()
