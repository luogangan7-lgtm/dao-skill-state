---
name: responsive-layout
description: Mobile-first layout and breakpoints so pages work at phone and desktop widths.
---

# Responsive layout

## The rule

Build the phone layout first, then widen for desktop. A page must work at phone width (≈375 px) and desktop width (≥1024 px) without breaking.

## Breakpoints

| Name | Min width | Use |
|------|-----------|-----|
| Base (phone) | 0 px | Single column, full-width, stacked |
| `--bp-sm` | 640 px | Phones in landscape, small tablets |
| `--bp-md` | 768 px | Tablets |
| `--bp-lg` | 1024 px | Desktops, multi-column |

Define breakpoints with `min-width` media queries only. Start from the narrowest layout and add rules as width increases.

## Layout rules

- **Container:** Center content with a max-width (e.g. 1200 px) and horizontal padding. On phones, reduce padding to ~16 px; on desktop, ~24–32 px.
- **Columns:** Stack vertically on phone. On desktop (≥1024 px), use a grid/flex row with `minmax(0, 1fr)` columns so nothing overflows.
- **Line length:** Limit readable text to 45–75 characters per line (max-width ~65 ch on body text).
- **Images/media:** `max-width: 100%; height: auto;` so they never overflow their container.
- **Touch targets:** At least 44 × 44 px for buttons and links.
- **No fixed widths:** Avoid `width` in `px` on layout containers; use percentages, `fr`, or `max-width`.

## Mobile-first CSS pattern

```css
/* Base: phone */
.page {
  padding: 16px;
  max-width: 100%;
}
.grid {
  display: flex;
  flex-direction: column;
  gap: 16px;
}

/* Desktop */
@media (min-width: 1024px) {
  .page {
    padding: 24px;
    max-width: 1200px;
    margin: 0 auto;
  }
  .grid {
    flex-direction: row;
    gap: 24px;
  }
}
```

## Verification

- At ≈375 px: no horizontal scroll, text ≥14 px readable, touch targets ≥44 × 44 px.
- At ≥1024 px: no over-wide line lengths, multi-column where appropriate, nothing clipped.

## Non-goals

- Print styles, reduced-motion, and high-contrast media queries are out of scope unless requested.
