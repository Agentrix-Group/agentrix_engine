#!/usr/bin/env python3
"""Bot de prueba para el protocolo de agente de Agentrix (Fase 4, ATD-007).

NO es el bot de referencia de la Fase 6 (bot_random.py/bot_hunter.py/
bot_evasive.py, que van en games/starfighter/examples/). Esto es
scaffolding minimo para probar que el runner de Rust habla el protocolo
correctamente frente a distintos comportamientos de un bot externo real,
sin compilar nada -- justo lo que la Fase 4 tiene que demostrar.

`--scripts` en el runner real solo acepta una ruta por slot (asi seria
en produccion: un script, sin argumentos extra), asi que el modo de
prueba no se pasa por linea de comandos -- lo fijan los wrappers
bot_*.py de al lado, que importan este modulo y pisan MODE antes de
llamar a main(). Ejecutar este archivo directo corre en modo
"cooperative" (el default de MODE).

Modos:
  cooperative -- responde siempre con una accion valida.
  hang        -- duerme mas que cualquier timeout_ms razonable de prueba
                 antes de responder a una percepcion (prueba el timeout).
  garbage     -- despues del handshake, contesta basura no-JSON a todo
                 (prueba el manejo de JSON invalido).
  bad_version -- confirma el handshake con una version de protocolo que
                 no es la real (prueba el rechazo de version).
"""
import json
import sys
import time

MODE = "cooperative"


def send(msg):
    print(json.dumps(msg), flush=True)


def read_line():
    line = sys.stdin.readline()
    if not line:
        return None
    return line.strip()


def main():
    handshake_raw = read_line()
    if handshake_raw is None:
        return

    if MODE == "bad_version":
        send({"type": "handshake_ack", "protocol_version": "9.9"})
        return

    send({"type": "handshake_ack", "protocol_version": "1.0"})

    init_raw = read_line()  # se ignora el contenido, solo se consume la linea
    if init_raw is None:
        return

    while True:
        line = read_line()
        if line is None:
            return

        if MODE == "garbage":
            print("esto no es json valido {{{", flush=True)
            continue

        msg = json.loads(line)
        if msg.get("type") == "end":
            return
        if msg.get("type") != "perception":
            continue

        tick = msg["perception"]["tick"]

        if MODE == "hang":
            time.sleep(5)

        send(
            {
                "type": "action",
                "tick": tick,
                "action": {
                    "thrust": "FORWARD",
                    "turn": "NONE",
                    "shoot": True,
                    "shield": False,
                },
            }
        )


if __name__ == "__main__":
    main()
