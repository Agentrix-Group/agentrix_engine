#!/usr/bin/env python3
"""Wrapper de dummy_bot.py en modo hang. Ver dummy_bot.py."""
import pathlib
import sys

sys.path.insert(0, str(pathlib.Path(__file__).parent))
import dummy_bot  # noqa: E402

dummy_bot.MODE = "hang"
dummy_bot.main()
