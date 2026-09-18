#!/usr/bin/env python3
"""Wrapper de dummy_bot.py en modo bad_version. Ver dummy_bot.py."""
import pathlib
import sys

sys.path.insert(0, str(pathlib.Path(__file__).parent))
import dummy_bot  # noqa: E402

dummy_bot.MODE = "bad_version"
dummy_bot.main()
