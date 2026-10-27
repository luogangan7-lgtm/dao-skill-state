---
name: web-page
description: "Use when an agent writes or changes a page for us: builds readable contrast, a deliberate type scale, and responsive phone/desktop layouts."
metadata:
  version: "0.1.0"
---

# Web page standard

Build pages our agents write so they pass review: readable contrast, a small consistent set of text sizes, and a layout that works at phone and desktop widths.

## When to use

- The agent is writing a new page, or editing an existing page's markup or styles.
- The request is about presentation: text, color, spacing, layout, or breakpoints.

## When NOT to use

- Backend logic, data modeling, API design, or non-visual work.
- Images, icons, charts, or brand illustration (route those to their owner).
- You are only changing copy words with no style or layout impact.

## Required input

Read the request as written. You need:
- What the page is (purpose and audience).
- The content to render (headings, body, controls).
- Any existing page or CSS tokens to extend, if one exists.

Do not demand a file format the request did not specify. If none is given, produce semantic HTML plus an embedded or adjacent CSS file.

## Decision workflow

1. **Set the type scale first.** Pick a small, deliberate set of text sizes (one value per role) from `references/type-scale.md`. Do not invent one-off pixel sizes.
2. **Set readable contrast.** Pick foreground/background pairs that meet WCAG AA minimums from `references/readable-contrast.md`. Never use faint grey body or secondary text.
3. **Lay out mobile-first.** Build the phone layout first, then widen for desktop using `references/responsive-layout.md`.
4. **Verify.** Run the pass/fail checks below before finishing.

## Output

- Semantic HTML page.
- CSS using the chosen type scale, contrast tokens, and responsive layout rules.
- No layout or text style outside the adopted tokens.

## Stopping conditions

Stop and report when:
- A requested color or size cannot meet the contrast or type-scale rules, OR
- You are unsure whether a layout breakpoint behaves at phone width.

Do not add a human-clearable gate the request did not ask for.

## Tool requirements

- Read / Write / Edit for HTML and CSS files.
- No build step, framework, or network fetch required.

## Pass / fail check

Before finishing, confirm each of these:
- [ ] Body text and secondary text meet at least WCAG AA 4.5:1 contrast against their background (3:1 for large text only).
- [ ] No font size appears outside the adopted type scale.
- [ ] The page is usable at phone width (≈375 px) without horizontal scrolling or unreadable text.
- [ ] The page is usable at desktop width (≥1024 px) without over-wide line lengths.
- [ ] Touch targets are at least 44 × 44 px.

If any box is unchecked, fix it before reporting done.

## References

- `references/readable-contrast.md` — contrast ratios and safe text colors.
- `references/type-scale.md` — the deliberate text-size set and how to use it.
- `references/responsive-layout.md` — mobile-first layout and breakpoints.
