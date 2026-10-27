---
name: web-page-builder
description: Build web pages with readable contrast, a small consistent type scale, and responsive layouts that work at phone and desktop widths. Use this skill whenever an agent writes or changes a page — HTML, CSS, React, Vue, or any templated page — so every page ends up accessible and device-friendly, even when the user doesn't explicitly ask for accessibility or responsiveness.
---

# Web Page Builder

Agents that write or change our pages keep shipping the same three problems: faint grey secondary text that is hard to read, dozens of slightly different font sizes instead of a deliberate type scale, and layouts that break on phones.

This skill fixes those at the source. Any agent that writes or changes a page should end up with:

1. **Readable contrast** — text meets WCAG AA minimums against its background; no faint grey body or secondary text.
2. **A small consistent type scale** — a fixed, tiny set of text sizes used everywhere; no one-off pixel sizes.
3. **Responsive layout** — the page works at phone width (≈375px) and desktop width (≥1024px) without horizontal scrolling or cramped tap targets.

## When to use this

Trigger this skill whenever the task involves writing, editing, styling, or reviewing a user-facing page or component:

- "Build a landing page..."
- "Add a profile card / dashboard / settings page..."
- "Fix this page so it works on mobile..."
- "Make this text easier to read..."
- Tweaking CSS, tailwind classes, styled-components, or any inline styles on a page.

It does not matter what framework or templating language is used — the rules below apply to the rendered HTML/CSS regardless.

## Rule 1: Readable contrast

Every text color must pass WCAG AA contrast against its background:

- **Normal text (<18px, or <14px bold):** minimum 4.5:1 contrast ratio.
- **Large text (≥18px, or ≥14px bold):** minimum 3:1.
- **UI components (buttons, inputs, links, focus rings):** minimum 3:1 against adjacent colors.

**Do not use faint grey for secondary text.** Grey on white (or light grey on white) is the #1 review failure. If a label, caption, or helper text looks "secondary," make it smaller, darker, or give it more weight — do not just lighten the color. A good test: squint at the page. If the grey text disappears into the background, it fails.

Verify contrast before finishing. If you do not have a contrast checker, use the reference table in `references/contrast.md` and pick from the pre-approved pairs.

## Rule 2: A small consistent type scale

Use exactly these text sizes, and nothing else. Sizes are in `rem` so they respect user font preferences.

| Size | rem | px (approx) | Use for |
|------|-----|-------------|---------|
| caption | 0.75rem | 12px | captions, tiny labels, timestamps |
| small | 0.875rem | 14px | secondary labels, helper text, footnotes |
| base | 1rem | 16px | body text, default paragraph |
| lead | 1.125rem | 18px | intro paragraphs, deck text |
| heading-sm | 1.25rem | 20px | h3, subsection headings |
| heading-md | 1.5rem | 24px | h2, section headings |
| heading-lg | 2rem | 32px | h1, page titles |

**Never** use arbitrary pixel sizes like `13px`, `15px`, `17px`, `19px`, `21px`, etc. Every text size must come from this table.

Line height: use `1.5` for body text, `1.25` for headings. Letter spacing: normal unless a design system says otherwise.

Font weights: keep it to two — `400` (regular) for body and `600`/`700` (semibold/bold) for headings and emphasis. Do not introduce `300`, `500`, etc. one-off weights.

## Rule 3: Responsive layout (phone + desktop)

Build mobile-first, then layer desktop styles on top.

**Base (mobile, default styles — no media query needed):**
- Single-column stack. Full-width or a narrow centered container with side padding.
- `max-width` on the container (e.g., `max-width: 100%`, side padding `1rem`), so text lines never stretch edge-to-edge on small screens.
- Touch targets (buttons, links, form controls) at least `44px × 44px`.
- Images and media: `max-width: 100%; height: auto;` so nothing overflows.
- No fixed pixel widths (`width: 320px`, etc.) on layout containers.

**Desktop (≥768px):**
- One media query: `@media (min-width: 768px)`.
- Widen the container (e.g., `max-width: 800px` or `1024px`, centered), add two-column grids where appropriate, and increase side padding.
- Keep the same type scale and contrast — only spacing and layout change.

Check the page at two widths: ≈375px (phone) and ≈1024px (desktop). Resize the browser / dev-tools device toolbar. If anything breaks at either width, fix it before finishing.

## Pre-flight checklist

Before you consider a page done, confirm all of these:

- [ ] Every text color passes WCAG AA contrast on its background (normal 4.5:1, large 3:1).
- [ ] No faint grey secondary text — secondary meaning comes from size/weight, not lightness.
- [ ] Every font size is in the type scale table (no arbitrary px sizes).
- [ ] Line heights are 1.5 (body) / 1.25 (headings); font weights are regular or semibold/bold only.
- [ ] The page is usable at ≈375px wide: no horizontal scroll, tap targets ≥44px, images don't overflow.
- [ ] The page is usable at ≈1024px wide via a single `@media (min-width: 768px)` block.
- [ ] Heading order is logical (one h1, then h2, h3… not skipping levels).

## Bundled references

- `references/contrast.md` — pre-approved color pairs that pass WCAG AA, plus how to verify contrast.
- `references/type-scale.md` — copy/pasteable CSS custom properties for the type scale.
- `references/layout.md` — a mobile-first layout template (container, media query, touch targets).

Start by reading `references/type-scale.md` and `references/contrast.md`, then apply Rule 1–3 to the page.
