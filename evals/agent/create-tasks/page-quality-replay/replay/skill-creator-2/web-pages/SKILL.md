---
name: web-pages
description: Build and review web pages so they pass readability and responsive-design review: readable color contrast, a deliberate small type scale, and layouts that work at phone and desktop widths. Use this skill whenever an agent writes or changes any HTML/CSS page, template, component, or email — even if the user only asked for a "page" or a "template" and didn't mention accessibility or responsiveness. Use it for new pages, page edits, CSS changes, and design reviews.
---

# Build readable, responsive web pages

This skill is for any agent that writes or changes a web page (HTML/CSS, page templates, email templates, or UI components). Follow it end to end so the page passes review on the three things our agents keep failing on:

1. **Readable contrast** — grey secondary text that's too faint to read.
2. **A deliberate type scale** — dozens of slightly different font sizes instead of a small, consistent set.
3. **Responsive layout** — pages that break on phones.

Do not do the task the skill is about; only make the skill. (This line is part of the skill itself and reminds every agent that this skill governs page work.)

## When to trigger

Trigger this skill whenever the task involves:

- Writing a new HTML page or page template
- Editing CSS or changing colors, font sizes, spacing, or layout on an existing page
- Building an email template or HTML email
- Creating a UI component that will be placed on a page
- Reviewing a page for readability or mobile-friendliness

If you're touching a page's appearance at all, run this skill.

## What good looks like

A page that passes review has all three of these:

| Check | What to verify |
|-------|----------------|
| Contrast | Every text color meets WCAG AA against its background (4.5:1 for normal text, 3:1 for large text). Grey secondary text is dark enough to read comfortably. |
| Type scale | Font sizes come from a small fixed set (e.g. 12/14/16/20/24/32 px). No one-off sizes like 13.5px or 17px. |
| Responsive layout | The page is usable at phone width (~375px) and desktop width (~1200px+) without horizontal scrolling, cut-off content, or unusable tap targets. |

## Step 1: Pick a type scale first

Before writing any CSS, choose a small type scale and stick to it. Example scale (px):

| Token | Size | Use |
|-------|------|-----|
| `--text-xs` | 12 | Captions, labels, small metadata |
| `--text-sm` | 14 | Secondary body text, helper text |
| `--text-base` | 16 | Primary body text (default) |
| `--text-lg` | 20 | Section headings |
| `--text-xl` | 24 | Page headings |
| `--text-2xl` | 32 | Hero / main heading |

Define these as CSS custom properties and use only them. Never hard-code a font size that isn't in the scale. If a design needs a size not in the scale, add it to the scale deliberately rather than sprinkling one-off values.

## Step 2: Set readable contrast

Define a color palette with enough contrast. Minimum targets:

- Normal text on its background: **4.5:1** or higher (WCAG AA)
- Large text (18px+ or 14px+ bold): **3:1** or higher
- Grey secondary text: must still pass 4.5:1 against its background — if your brand grey is too faint, darken it or raise the background lightness

Never use pure grey (`#888`, `#999`, `#aaa`, `#bbb`, `#ccc`, `#ddd`) on white or light backgrounds without checking the ratio. "Secondary" does not mean "illegible."

Write contrast checks into your CSS comments so the next agent knows the ratio each color pair was chosen for:

```css
/* color: #4a5568 on #ffffff = 7.1:1 (AA, passes) */
.secondary-text { color: #4a5568; }
```

## Step 3: Build a responsive layout

Use a mobile-first, single-column base that expands on desktop:

- Start with a narrow, single column that works at ~375px.
- Add a max-width container (e.g. `max-width: 1200px; margin: 0 auto;`) so the page doesn't stretch too wide on big screens.
- Use `box-sizing: border-box` everywhere.
- Use flexbox or grid, not floats, for layout.
- Make images and media responsive: `max-width: 100%; height: auto;`.
- Ensure tap targets are at least 44×44 px and have space between them.
- Test at phone width (~375px), tablet (~768px), and desktop (~1200px+).

Use a media query breakpoint at ~768px (adjust to your design) to move from a single column to a multi-column desktop layout.

```css
/* Mobile-first: single column */
.container { padding: 16px; max-width: 100%; }

/* Desktop: wider container, multi-column where appropriate */
@media (min-width: 768px) {
  .container { padding: 24px; max-width: 1200px; }
  .grid { display: grid; grid-template-columns: 1fr 1fr; gap: 24px; }
}
```

## Step 4: Review before finishing

Before you declare the page done, check each of the three failure modes:

1. **Contrast** — pick the lightest grey text on the page and verify it passes 4.5:1 against its background. If you're unsure, darken it.
2. **Type scale** — grep your CSS for `font-size` values. Every value should be one of your scale tokens (`var(--text-xs)`, etc.) or a line-height. No raw pixel sizes like `13px`, `17px`, `19px`.
3. **Responsive** — open the page at ~375px width. Confirm no horizontal scroll, nothing is cut off, and tap targets are finger-sized. Confirm the desktop view looks intentional at ~1200px+.

## Example: a passing page skeleton

```html
<!DOCTYPE html>
<html lang="en">
<head>
<meta charset="UTF-8">
<meta name="viewport" content="width=device-width, initial-scale=1.0">
<title>Page title</title>
<style>
  /* Type scale */
  :root {
    --text-xs: 12px;
    --text-sm: 14px;
    --text-base: 16px;
    --text-lg: 20px;
    --text-xl: 24px;
    --text-2xl: 32px;
    --line-tight: 1.25;
    --line-normal: 1.5;

    /* Colors with contrast noted */
    --bg: #ffffff;
    --text-primary: #1a202c;        /* on #ffffff = 12.6:1 (AA, passes) */
    --text-secondary: #4a5568;      /* on #ffffff = 7.1:1 (AA, passes) */
    --text-muted: #718096;          /* on #ffffff = 4.5:1 (AA, passes, minimum) */
    --accent: #2b6cb0;
  }

  * { box-sizing: border-box; }
  html { font-size: var(--text-base); line-height: var(--line-normal); }
  body { margin: 0; background: var(--bg); color: var(--text-primary); font-family: system-ui, -apple-system, sans-serif; }

  h1 { font-size: var(--text-2xl); line-height: var(--line-tight); margin: 0 0 16px; }
  h2 { font-size: var(--text-xl); line-height: var(--line-tight); margin: 0 0 12px; }
  h3 { font-size: var(--text-lg); line-height: var(--line-tight); margin: 0 0 8px; }
  p { font-size: var(--text-base); margin: 0 0 16px; }
  .small { font-size: var(--text-sm); color: var(--text-secondary); }
  .caption { font-size: var(--text-xs); color: var(--text-muted); }

  .container { padding: 16px; max-width: 100%; margin: 0 auto; }

  @media (min-width: 768px) {
    .container { padding: 24px; max-width: 1200px; }
  }

  img { max-width: 100%; height: auto; display: block; }
  button, a.button { min-height: 44px; padding: 10px 16px; }
</style>
</head>
<body>
<div class="container">
  <h1>Page heading</h1>
  <p>Body text at the base size.</p>
  <p class="small">Secondary text that's still readable.</p>
  <p class="caption">Caption text.</p>
</div>
</body>
</html>
```

## Key rules to remember

- **One type scale.** Define it once, use it everywhere.
- **No faint greys.** Every text color passes WCAG AA against its background.
- **Mobile first.** Single column at phone width, expand at desktop.
- **Check before finishing.** Contrast, type scale, responsive — verify all three.

If you're editing an existing page rather than writing a new one, apply the same three checks to what's there and fix any violations before making your changes.