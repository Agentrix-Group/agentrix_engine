#!/usr/bin/env python3
"""bot_hunter.py -- reference strategy: aggressive, closes distance and shoots.

Every tick: pick the nearest rival in `perception.rivals` (empty means no
rival in radar range -- thrust forward to go looking for one). Turn
toward it using `relative_position` (already relative to this ship, per
the Fase 3 perception contract) compared against `myself.facing`. Shoot
once roughly aligned and the ship's own `remaining_bullet_cooldown` says
it's ready -- the engine also silently ignores a shoot request it can't
afford (RF-044), so this check is about not wasting the ammo *slot* in
the action, not about correctness. Never shields; it's an aggression
strategy, not a defensive one -- that's bot_evasive.py's job.

Speaks the real Agentrix agent protocol (ATD-007) over stdin/stdout, one
JSON object per line -- no dependency beyond the Python standard library.
"""
import json
import math
import sys

PROTOCOL_VERSION = "1.0"

# Alignment window judged "aimed enough to shoot", in radians (~11.5deg).
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


def steer_toward(rel_x: float, rel_y: float, facing_x: float, facing_y: float) -> str:
    """Turn::Left increases the facing angle (counter-clockwise, matches
    the engine's angular-velocity sign convention in fighter_actions);
    Turn::Right decreases it. See src/lib.rs::facing_direction."""
    target_angle = math.atan2(rel_y, rel_x)
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
        # No rival in radar range. With the symmetric 2-player circular
        # spawn (games/starfighter, Fase 1: opposite ends of a 600-unit
        # radius, 1200 apart) this is the *starting* condition, not an
        # edge case -- it's farther than the default radar_range (800).
        # Just continuing to thrust in whatever direction this ship
        # happened to spawn facing never reliably closes that gap (both
        # ships can drift in parallel forever). Turn toward the arena
        # center instead, using this ship's own absolute `position`
        # (SelfState has it; only rival/bullet contacts are
        # radar-relative) -- a much better search heuristic than a fixed
        # spawn-facing direction, since the center is where a rival
        # spawned elsewhere on the same circle is statistically likeliest
        # to be crossed.
        pos = myself["position"]
        toward_center_x, toward_center_y = -pos["x"], -pos["y"]
        turn = steer_toward(
            toward_center_x, toward_center_y, myself["facing"]["x"], myself["facing"]["y"]
        )
        return {"thrust": "FORWARD", "turn": turn, "shoot": False, "shield": False}

    nearest = min(
        rivals,
        key=lambda r: math.hypot(r["relative_position"]["x"], r["relative_position"]["y"]),
    )
    rel = nearest["relative_position"]
    turn = steer_toward(rel["x"], rel["y"], myself["facing"]["x"], myself["facing"]["y"])
    aligned = turn == "NONE"
    ready = myself["remaining_bullet_cooldown"] <= 0

    return {
        "thrust": "FORWARD",
        "turn": turn,
        "shoot": aligned and ready,
        "shield": False,
    }


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
