#!/usr/bin/env python3
"""bot_random.py -- reference strategy: uniformly random action every tick.

Thrust/turn/shoot/shield are each sampled independently, with no regard
for the perception it receives. Serves as the comparison floor: any
other reference bot should beat this one, and a game balance change that
makes bot_random.py suddenly competitive against bot_hunter.py or
bot_evasive.py is a signal worth investigating.

Speaks the real Agentrix agent protocol (ATD-007) over stdin/stdout, one
JSON object per line -- no dependency beyond the Python standard library.
"""
import json
import random
import sys

PROTOCOL_VERSION = "1.0"


def send(msg: dict) -> None:
    sys.stdout.write(json.dumps(msg) + "\n")
    sys.stdout.flush()


def read() -> dict:
    line = sys.stdin.readline()
    if not line:
        sys.exit(0)
    return json.loads(line)


def random_action() -> dict:
    return {
        "thrust": random.choice(["FORWARD", "OFF", "BRAKE"]),
        "turn": random.choice(["LEFT", "RIGHT", "NONE"]),
        "shoot": random.random() < 0.3,
        "shield": random.random() < 0.1,
    }


def main() -> None:
    handshake = read()
    assert handshake["type"] == "handshake"
    send({"type": "handshake_ack", "protocol_version": PROTOCOL_VERSION})

    read()  # init -- bot_random doesn't need any of the limits it carries

    while True:
        msg = read()
        if msg["type"] == "end":
            return
        assert msg["type"] == "perception"
        send({"type": "action", "tick": msg["perception"]["tick"], "action": random_action()})


if __name__ == "__main__":
    main()
