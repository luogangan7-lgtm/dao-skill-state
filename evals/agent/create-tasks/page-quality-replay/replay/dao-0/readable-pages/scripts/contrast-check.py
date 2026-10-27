#!/usr/bin/env python3
"""WCAG contrast checker for the readable-pages skill.

Prints the contrast ratio of a foreground/background color pair and whether it
passes WCAG AA for normal text (4.5:1) and large text (3:1).

Usage:
    python3 scripts/contrast-check.py <fg-hex> <bg-hex>

Example:
    python3 scripts/contrast-check.py #4b5563 #ffffff
"""
from __future__ import annotations

import sys
from typing import Optional


def linear(c: float) -> float:
    c = c / 255.0
    return c / 12.92 if c <= 0.03928 else ((c + 0.055) / 1.055) ** 2.4


def luminance(hexcolor: str) -> float:
    h = hexcolor.lstrip("#")
    if len(h) == 3:
        h = "".join(ch * 2 for ch in h)
    r, g, b = (int(h[i : i + 2], 16) for i in (0, 2, 4))
    return 0.2126 * linear(r) + 0.7152 * linear(g) + 0.0722 * linear(b)


def ratio(fg: str, bg: str) -> float:
    lf, lb = luminance(fg), luminance(bg)
    light, dark = max(lf, lb), min(lf, lb)
    return (light + 0.05) / (dark + 0.05)


def main(argv: list[str]) -> int:
    if len(argv) != 3:
        print("usage: contrast-check.py <fg-hex> <bg-hex>", file=sys.stderr)
        return 2
    fg, bg = argv[1], argv[2]
    try:
        r = ratio(fg, bg)
    except ValueError as exc:
        print(f"error: {exc}", file=sys.stderr)
        return 2

    normal = r >= 4.5  # WCAG AA normal text (<18.66px bold / <24px regular)
    large = r >= 3.0   # WCAG AA large text
    print(f"{fg} on {bg}: {r:.2f}:1")
    print(f"  WCAG AA normal text (4.5:1): {'PASS' if normal else 'FAIL'}")
    print(f"  WCAG AA large text  (3:1):   {'PASS' if large else 'FAIL'}")
    return 0 if normal and large else 1


if __name__ == "__main__":
    sys.exit(main(sys.argv))
