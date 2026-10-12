#!/usr/bin/python3
"""Test helper: inspect or press buttons in other apps' windows via AT-SPI
(the accessibility API), which works on Wayland where synthetic clicks don't.

    a11y.py dump            print every app's accessibility tree
    a11y.py click <label>   press the first button with that label (exit 1 if none)
"""
import sys

import pyatspi


def walk(node, depth=0):
    try:
        yield node, depth
        for i in range(node.childCount):
            yield from walk(node.getChildAtIndex(i), depth + 1)
    except Exception:  # apps may vanish mid-walk
        return


desktop = pyatspi.Registry.getDesktop(0)
apps = [desktop.getChildAtIndex(i) for i in range(desktop.childCount)]
if sys.argv[1] == "dump":
    for app in apps:
        for node, depth in walk(app):
            print("  " * depth + f"[{node.getRoleName()}] {node.name!r}")
    sys.exit(0)

label = sys.argv[2]
for app in apps:
    for node, _ in walk(app):
        if node.getRoleName() in ("push button", "button") and node.name.strip() == label:
            node.queryAction().doAction(0)
            sys.exit(0)
sys.exit(1)
