---
name: readable-contrast
description: Contrast ratios and safe text colors so body and secondary text stay readable.
---

# Readable contrast

## The rule

Body text and secondary text must meet at least WCAG AA contrast against their background:
- Normal text: **4.5:1** minimum.
- Large text (18 px+ or 14 px+ bold): **3:1** minimum.
- UI components and graphical objects: **3:1** minimum.

Never use faint grey body or secondary text. "Grey" is a hue, not an excuse for low contrast.

## Contrast tokens

Adopt these foreground/background pairs on a white or near-white background (#ffffff–#f5f5f5):

| Role | Color | Contrast vs white | Use for |
|------|-------|-------------------|---------|
| Primary text | #111827 | ~16.7:1 | Body copy, paragraph text |
| Secondary text | #4b5563 | ~10.9:1 | Captions, labels, helper text |
| Tertiary text | #6b7280 | ~7.2:1 | Metadata, timestamps |
| Links / interactive | #1d4ed8 | ~8.2:1 | Links, buttons |
| Disabled | #9ca3af | ~2.8:1 | Disabled states only (non-interactive) |

On a dark background, invert with equivalent ratios:
- Primary text: #f3f4f6
- Secondary text: #d1d5db
- Tertiary text: #9ca3af

## Verification

Compute contrast as (L1 + 0.05) / (L2 + 0.05) where L is relative luminance. Lighter of the two is L1. Do not eyeball it. If you cannot verify a pair, pick from the table above instead.

## Non-goals

- AAA (7:1) is not required unless the request says so.
- Decorative text with no information content is exempt.
