---
name: web-page-standard
description: "Use when an agent writes or changes a web page (HTML/CSS) for this project: enforce WCAG AA readable contrast (no faint grey secondary text), a small deliberate type scale instead of ad-hoc font sizes, and layouts that work at phone and desktop widths."
metadata:
  version: "1.0.0"
---

# Web page standard

Any agent that writes or changes a page for this project must leave it with three properties:

1. **Readable contrast** — no faint grey secondary text; all text meets WCAG AA contrast.
2. **A deliberate type scale** — a small, named set of text sizes instead of dozens of ad-hoc sizes.
3. **Responsive layout** — works at phone width and desktop width.

## When to use

Invoke this skill when an agent authors a new page or edits an existing page's markup or styles (HTML, CSS, or CSS-in-JS). It applies to the final state of the page, not to every intermediate edit.

## The three rules

### Rule 1 — Readable contrast (no faint grey text)

Never use low-contrast "secondary" grey text as a substitute for hierarchy. Hierarchy comes from size and weight, not from washing color out.

- Every text color, on every background it appears over, must meet WCAG AA:
  - **4.5:1** for normal text (under ~18.5px bold or ~24px regular).
  - **3:1** for large text (18.5px+ bold or 24px+ regular).
- Do not use `#999`, `#aaa`, `#bbb`, `#ccc`, `rgba(0,0,0,0.3-0.6)`, or similar faint greys for body or caption text.
- If a label or caption must look secondary, make it smaller (per the type scale) and/or bolder — keep its contrast at AA or higher.
- Check contrast with a real tool (DevTools color picker / contrast checker), not by eye.

### Rule 2 — A deliberate type scale

Use exactly one type scale for the page. Do not pick arbitrary `font-size` values per element.

Named sizes (copy the tokens from `assets/page-tokens.css`):

| Name | Use | Approx size |
|------|-----|-------------|
| `text-xs` | captions, fine print | ~0.75rem |
| `text-sm` | labels, helper text | ~0.875rem |
| `text-base` | body text | 1rem |
| `text-lg` | slightly emphasized body | ~1.125rem |
| `text-xl` | section headings | ~1.25rem |
| `text-2xl` | larger headings | ~1.5rem |
| `text-3xl` | page titles | ~1.875rem |

- Body text defaults to `text-base`. Nothing smaller than `text-xs` for readable content.
- Headings use `text-xl`/`text-2xl`/`text-3xl`; never mix in raw `px` or random `rem` values.
- Line length: cap readable text at ~45–75 characters per line (use `max-width` on prose containers).

### Rule 3 — Responsive layout (phone + desktop)

The page must be usable at both phone width (~320–480px) and desktop width (1024px+).

- Mobile first: write base styles for a single-column phone layout, then add desktop changes inside a min-width breakpoint.
- One breakpoint is enough for most pages: `@media (min-width: 768px)`.
- At desktop: allow side-by-side columns, wider containers, and larger touch targets.
- At phone: stack vertically, full-width or near-full-width containers, tap targets at least 44×44px.
- Never use fixed widths that overflow a phone screen; use percentages or `max-width` instead.
- Check the page at both widths (resize the browser or use DevTools device toolbar) before considering it done.

## How to apply

When authoring or editing a page, run this checklist and stop only when every item passes:

- [ ] Contrast: every text/background pair checked at AA; no faint grey secondary text remains.
- [ ] Type scale: all font sizes come from the named scale in `assets/page-tokens.css`; no raw `px`/random `rem` sizes.
- [ ] Responsive: page verified usable at phone width (~375px) and desktop width (~1024px+); mobile-first with a single `@media (min-width: 768px)` block.
- [ ] Line length: prose containers capped at ~45–75 characters.
- [ ] Touch targets: phone tap targets ≥ 44×44px.

If an existing page violates any rule, fix that violation as part of the same change — do not ship a page that fails the standard just because it already failed before your edit.

## Reference

`assets/page-tokens.css` — copy-paste starting point for the type scale, contrast-safe text colors, and responsive breakpoint. Edit the values to match the project's brand, but keep the scale small and the contrast at AA or higher.
