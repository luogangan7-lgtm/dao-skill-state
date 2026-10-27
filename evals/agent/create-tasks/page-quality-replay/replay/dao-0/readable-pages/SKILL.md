---
name: readable-pages
description: Use when writing or changing a web page. Teaches the agent to self-check for readable WCAG-AA contrast, a small deliberate type scale, and mobile-first responsive layouts that work at phone and desktop widths before review.
metadata:
  version: "0.1.0"
---

# readable-pages

Build and self-check web pages so they survive review. The recurring failures this skill prevents are:

1. **Faint grey secondary text** that is too faint to read (fails WCAG contrast).
2. **Dozens of ad-hoc font sizes** instead of a deliberate type scale.
3. **Layouts that break on phones** (fixed widths, desktop-first grids, cramped touch targets).

Any agent that writes or changes a page should apply the three rules below and run the self-check before asking for review. This skill does not design the page for you — it defines the non-negotiable review bar and how to prove you cleared it.

## When to use

Use this skill whenever you:

- Write a new HTML page, email template, or dashboard view.
- Edit CSS that sets colors, font sizes, spacing, or layout.
- Change an existing page's structure, typography, or responsive behavior.

Non-goals: brand aesthetics, copywriting, animation, accessibility beyond contrast and layout (keyboard navigation, ARIA, motion preferences are out of scope here).

## Inputs

- The page source (HTML/CSS, or a template that renders HTML/CSS).
- A browser or the contrast checker script (see [Tools](#tools)) to verify colors.
- The intended smallest supported width (default: phone, 320 px CSS pixels).

## The three review rules

### Rule 1 — Readable contrast (WCAG AA)

No text may fall below these contrast ratios against its background:

| Text size | Minimum ratio | Typical use |
|-----------|---------------|-------------|
| Normal (< 18.66 px bold, < 24 px regular) | **4.5:1** | Body, labels, captions, links |
| Large (≥ 18.66 px bold, ≥ 24 px regular) | **3:1** | Headings, lead text |

Never use faint grey secondary text on a light background. On `#FFFFFF`, these **pass** normal text:

- `#111827` — 17.74:1 (primary text)
- `#4B5563` — 7.56:1 (secondary text — the darkest grey you should use for body)
- `#666666` — 5.74:1 (muted, large text only unless background is darker)

On `#FFFFFF` these **fail** normal text and must not be used for body/captions:

- `#777777` — 4.48:1 (just under 4.5:1)
- `#888888` — 3.54:1
- `#9CA3AF` — 2.54:1 (common Tailwind "grey-400" — do not use for normal text on white)

Dark mode equivalent: `#FFFFFF` on `#1A1A1A` = 17.40:1; `#FFFFFF` on `#2B2B2B` = 14.16:1.

**How to verify:** for every text/background pair, run `scripts/contrast-check.py <foreground> <background>` (or paste the pair into a contrast checker) and confirm the ratio meets the threshold for the text size. If any pair is below threshold, darken the foreground or lighten the background until it passes.

### Rule 2 — A deliberate type scale

Use exactly one fixed type scale. Do not invent ad-hoc `font-size` values per component.

| Token | Size | Use |
|-------|------|-----|
| `--text-caption` | 0.75rem (12px) | Labels, captions, timestamps |
| `--text-body` | 1rem (16px) | Body text, form inputs, buttons |
| `--text-lead` | 1.125rem (18px) | Intro paragraphs, prominent body |
| `--text-heading-sm` | 1.25rem (20px) | Section subheadings |
| `--text-heading-md` | 1.5rem (24px) | Page/section headings |
| `--text-heading-lg` | 2rem (32px) | Major headings |
| `--text-heading-xl` | 2.5rem (40px) | Hero/landing headings |

Reference sizes by token (`var(--text-body)`), never by raw pixel or arbitrary rem values. If a page needs a size not in the table, add it to the scale once and reuse it — do not inline a one-off value. Keep line lengths readable: ~45–75 characters for body text (`max-width: 65ch` on prose).

### Rule 3 — Mobile-first responsive layout

Design for phone first, then enhance for desktop. Requirements:

- **Mobile-first:** write base styles for a single-column phone layout; add desktop changes only inside `min-width` media queries (never `max-width` for the base layout).
- **Breakpoints:** one small, fixed set — `sm: 640px`, `md: 768px`, `lg: 1024px`. Use the same names everywhere.
- **Fluid widths:** use percentages, `fr` units, `auto`, `minmax()`, or `clamp()`; avoid fixed `px` widths on containers. Give pages a `max-width` container centered with side padding instead of stretching edge-to-edge on desktop.
- **Grid:** use CSS Grid or Flexbox; a phone stack (`grid-template-columns: 1fr`) becomes a desktop row/grid at `md` or `lg`.
- **Touch targets:** interactive elements at least 44 × 44 CSS px with adequate spacing.
- **No horizontal overflow:** `overflow-x: hidden` on the root; nothing wider than the viewport at 320 px.
- **Viewport meta tag:** `<meta name="viewport" content="width=device-width, initial-scale=1">`.

**Phone widths to test:** 320 px, 375 px, 390 px. **Desktop widths to test:** 1024 px, 1440 px.

## Supporting files

- `assets/page-review.css` — copy-pasteable CSS variables for the type scale, a contrast-safe palette, and mobile-first responsive utilities.
- `scripts/contrast-check.py` — computes the WCAG contrast ratio of a foreground/background pair and reports pass/fail for normal and large text.

## Tools

- `scripts/contrast-check.py <fg-hex> <bg-hex>` — contrast ratio + WCAG AA pass/fail.
- Browser dev tools (Computed → rendered font size; Color picker → contrast ratio) and responsive design mode (device toolbar) at 320/375/1024/1440 px.

## Decision workflow

1. **Pick your palette** from the contrast-safe pairs in Rule 1. Document the foreground/background pair for every text style.
2. **Verify contrast** for each pair with the script at the text's actual size category. Record the ratios.
3. **Set font sizes** only from the type scale in Rule 2. Grep the page for any other `font-size` values and remove them.
4. **Write mobile-first:** base styles = single column phone; add `min-width` overrides at `sm`/`md`/`lg` only.
5. **Run the self-check** below. Fix anything that fails before review.

## Self-check (run before every review)

Tick each item. If any item is "no," fix it first.

- [ ] Every text/background pair passes WCAG AA for its size (script run, ratios recorded).
- [ ] No faint grey secondary text below 4.5:1 on its background at normal size.
- [ ] All font sizes come from the type scale; no ad-hoc `font-size` values remain.
- [ ] Base layout is single-column phone; desktop changes live only in `min-width` media queries.
- [ ] Breakpoints used are only `640px` / `768px` / `1024px`.
- [ ] Viewport meta tag present.
- [ ] Page renders without horizontal overflow at 320 px, 375 px, 390 px, 1024 px, 1440 px.
- [ ] Interactive targets are ≥ 44 × 44 CSS px.

## Pass / fail

**Pass:** All three rules verified, self-check complete, contrast ratios recorded.
**Fail:** Any contrast pair below threshold, any ad-hoc font size, any layout that breaks or overflows at phone width, or any self-check item left unchecked.

Do not request review until the skill's self-check passes.
