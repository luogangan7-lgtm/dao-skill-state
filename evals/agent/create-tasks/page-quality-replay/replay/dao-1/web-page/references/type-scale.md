---
name: type-scale
description: A deliberate, small set of text sizes and how to apply each one.
---

# Type scale

## The rule

Use a small, deliberate set of text sizes. One value per role. No one-off pixel sizes, no "dozens of slightly different font sizes."

## Adopted scale

| Role | Size | Line height | Use for |
|------|------|-------------|---------|
| `--text-xs` | 0.75rem (12 px) | 1.5 | Captions, timestamps, fine print |
| `--text-sm` | 0.875rem (14 px) | 1.5 | Labels, helper text, secondary text |
| `--text-base` | 1rem (16 px) | 1.6 | Body copy, paragraph text (default) |
| `--text-lg` | 1.125rem (18 px) | 1.5 | Lead paragraphs, intro text |
| `--text-xl` | 1.25rem (20 px) | 1.45 | Small headings, card titles |
| `--text-2xl` | 1.5rem (24 px) | 1.4 | Section headings |
| `--text-3xl` | 1.875rem (30 px) | 1.3 | Page headings |
| `--text-4xl` | 2.25rem (36 px) | 1.25 | Hero / marketing headings |

Use `rem` units so the scale respects user font-size preferences.

## How to apply

- Every text-bearing element maps to exactly one role above.
- Headings: `<h1>` → `--text-3xl` or `--text-4xl`, `<h2>` → `--text-2xl`, `<h3>` → `--text-xl`, `<h4>` → `--text-lg`.
- Body: default `--text-base`.
- Never set `font-size` in `px` or an arbitrary `rem` value outside this table.
- If a request asks for a size not in the scale, pick the nearest existing role or add a new role to this table (with justification) rather than inventing a one-off.

## Non-goals

- Fluid/responsive typography is out of scope unless the request asks for it.
- Font family selection is out of scope unless the request asks for it.
