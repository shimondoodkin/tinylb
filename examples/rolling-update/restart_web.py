#!/usr/bin/env python3
"""Example: rolling restart for a Next.js / web frontend."""
from rolling_update_lib import cli

if __name__ == "__main__":
    cli(target="web", host="example.com", default_startup=180)
