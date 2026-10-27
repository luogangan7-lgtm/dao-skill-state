# Contrast reference

## WCAG AA minimums

| Text size | Minimum contrast ratio |
|-----------|------------------------|
| Normal (<18px, or <14px bold) | 4.5:1 |
| Large (≥18px, or ≥14px bold) | 3:1 |
| UI components (buttons, inputs, links, focus rings) | 3:1 |

## Pre-approved color pairs

These pass WCAG AA on white (#ffffff). Use them instead of inventing greys.

| Role | Text color | Background | Ratio |
|------|------------|------------|-------|
| Body / primary | #1a1a1a | #ffffff | ~15.7:1 |
| Secondary / helper | #4b5563 | #ffffff | ~10.8:1 |
| Muted caption | #6b7280 | #ffffff | ~7.4:1 |
| Link (dark mode-safe) | #1d4ed8 | #ffffff | ~6.3:1 |
| White text on dark | #ffffff | #111827 | ~15.7:1 |

**Grey rule:** Do not use anything lighter than `#6b7280` for body-sized text on white, and never use `#9ca3af`, `#d1d5db`, or similar for normal text — those fail WCAG AA.

## How to verify

If you have a dev tool or contrast checker, run each text/background pair through it. Without one, compare against the pre-approved pairs above — if your text is lighter than `#6b7280` on white (or equivalent lightness on any background), it is too faint and fails review.

Dark mode: invert the logic — light text on dark backgrounds, same ratios. `#ffffff` on `#111827` passes; `#a1a1aa` on `#1f2937` does not.
