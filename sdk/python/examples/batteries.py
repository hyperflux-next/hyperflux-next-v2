#!/usr/bin/env python3
# SPDX-License-Identifier: GPL-2.0-only
"""Prints the battery of every device paired through HyperFlux Next V2, once,
or every time it changes with --watch.

    PYTHONPATH=sdk/python python3 sdk/python/examples/batteries.py
"""

import sys

from hyperflux_sdk import Client, HyperFluxError, age_phrase, boottime_ms


def battery_line(device, now_ms):
    name = device.name or f"Unknown device {device.vendor_id:04x}:{device.product_id:04x}"
    battery = device.battery
    if battery.level is None:
        return f"{name}: not reported" + ("" if battery.unreported == "battery" else " yet")
    line = f"{name}: {battery.level}%"
    if battery.charge == "charging":
        line += " full" if battery.level == 100 else " charging"
    if battery.stale and battery.updated_ms is not None and battery.updated_ms <= now_ms:
        line += f" ({age_phrase(now_ms - battery.updated_ms)} old)"
    return line


def show(snapshot):
    lines = [battery_line(device, boottime_ms()) for device in snapshot.devices()]
    print("\n".join(lines) if lines else "No devices found", flush=True)


def main(argv):
    watch = "--watch" in argv[1:]
    try:
        with Client.connect(client_name="Battery example") as client:
            snapshot = client.snapshot()
            show(snapshot)
            while watch and client.has_feature("wait"):
                if not client.wait(state_after=snapshot.revision).timed_out:
                    snapshot = client.snapshot()
                    print()
                    show(snapshot)
        return 0
    except HyperFluxError as error:
        print(f"batteries.py: {error}", file=sys.stderr)
        return 1
    except KeyboardInterrupt:
        return 0


if __name__ == "__main__":
    sys.exit(main(sys.argv))
