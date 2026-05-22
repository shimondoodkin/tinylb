#!/usr/bin/env python3
"""Example: rolling restart for an API service.

Assumes lb.toml has a route for `api.example.com` with multiple backends.
Replace the host and tweak --startup-timeout to match your service.
"""
from rolling_update_lib import cli

if __name__ == "__main__":
    cli(target="api", host="api.example.com", default_startup=300)
