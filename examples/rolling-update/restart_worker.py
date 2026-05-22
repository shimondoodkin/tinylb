#!/usr/bin/env python3
"""Example: rolling restart for a worker that compiles from source on
cold start. Uses a generous startup timeout since the first build can
take several minutes.
"""
from rolling_update_lib import cli

if __name__ == "__main__":
    cli(target="worker", host="worker.example.com", default_startup=600)
