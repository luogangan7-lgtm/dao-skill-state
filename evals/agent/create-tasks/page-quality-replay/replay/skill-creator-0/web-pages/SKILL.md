---
name: web-pages
description: Use this skill whenever an agent writes or changes a web page — HTML, CSS, or frontend markup — for our pages. It guarantees readable text contrast (WCAG AA), a small deliberate type scale instead of ad-hoc font sizes, and a responsive layout that works at phone and desktop widths. Trigger on phrases like "build a page", "write a landing page", "make this page responsive", "fix the contrast", "desktop and mobile", "type scale", or any request to author or edit frontend markup/CSS. Apply it even when the user doesn't explicitly ask for accessibility or responsiveness — every page we ship should meet this bar.
---

# Build our web pages

Use this skill any time an agent writes or changes a page — HTML, CSS, or frontend markup. Its whole job is to make sure every page we ship clears three bars:

1. **Readable contrast** — grey secondary text that's easy to read, not faint.
2. **A deliberate type scale** — a small, consistent set of text sizes instead of dozens of one-off values.
3. **A responsive layout** — works at phone widths and desktop widths without breaking.

Apply these rules even when the user doesn't ask for accessibility or responsiveness. Every page we ship meets this bar by default.

---

## 1. Readable contrast

Text must pass WCAG AA contrast against its background:

- Normal text (below ~18px / ~14px bold): **at least 4.5:1** contrast ratio.
- Large text (18px+ / 14px+ bold) and UI components: **at least 3:1**.

Never use light grey on white/light backgrounds just because it "looks secondary". Secondary text is still information — if a user can't read it, it's broken.

### How to check

- Pick your foreground and background colors.
- Compute the contrast ratio (or check it in DevTools: Rendering panel → "Emulate vision deficiencies" / color picker contrast line).
- If it's below the target, darken the text or lighten the background until it passes.

### Default palette you can use

| Role | Color | Use |
|------|-------|-----|
| Primary text | `#111827` | Headings, body copy, labels |
| Secondary text | `#4B5563` | Captions, muted meta, helper text |
| Tertiary | `#6B7280` | Borders, dividers, disabled states |
| Background | `#FFFFFF` | Page background |
| Surface | `#F9FAFB` | Cards, sections |

`#4B5563` on `#FFFFFF` is about 4.6:1 — safe for normal text. `#6B7280` on white is about 4.6:1 too. Anything lighter than `#6B7280` on white fails for normal text; reserve very light greys for large decorative text only, and even then keep it ≥ 3:1.

**Rule of thumb:** if you're tempted to use a hex lighter than `#9CA3AF` for body-sized text on white, stop and pick a darker shade.

---

## 2. Type scale

Define one small type scale and use only those sizes. Do not invent new pixel values per component.

### Recommended scale (rem-based)

| Token | Size | Use |
|-------|------|-----|
| `--text-xs` | `0.75rem` (12px) | Captions, fine print |
| `--text-sm` | `0.875rem` (14px) | Secondary text, labels |
| `--text-base` | `1rem` (16px) | Body copy (default) |
| `--text-lg` | `1.125rem` (18px) | Lead paragraphs |
| `--text-xl` | `1.25rem` (20px) | Small headings |
| `--text-2xl` | `1.5rem` (24px) | Section headings |
| `--text-3xl` | `1.875rem` (30px) | Page headings |

### Line height

- Body copy: `1.5`–`1.6`
- Headings: `1.2`–`1.3`

### How to apply

```css
:root {
  --text-xs: 0.75rem;
  --text-sm: 0.875rem;
  --text-base: 1rem;
  --text-lg: 1.125rem;
  --text-xl: 1.25rem;
  --text-2xl: 1.5rem;
  --text-3xl: 1.875rem;
  --leading-body: 1.5;
  --leading-tight: 1.25;
}

body { font-size: var(--text-base); line-height: var(--leading-body); color: var(--color-text); }
.small { font-size: var(--text-sm); }
h1 { font-size: var(--text-3xl); line-height: var(--leading-tight); }
h2 { font-size: var(--text-2xl); line-height: var(--leading-tight); }
```

If a component needs a size not on the scale, add it to the scale once (with a token name) rather than hard-coding a one-off `font-size`. That keeps the scale deliberate and shared.

---

## 3. Responsive layout

Build mobile-first, then layer desktop overrides on top. A page must look intentional at both phone (~320–480px) and desktop (≥ 1024px) widths.

### Mobile-first structure

```css
/* Base styles = mobile (single column, full width) */
.container {
  width: 100%;
  padding: 1rem;
  margin: 0 auto;
}

/* Desktop: widen, cap max-width, maybe multi-column */
@media (min-width: 1024px) {
  .container {
    max-width: 960px;
    padding: 2rem;
  }
  .grid {
    display: grid;
    grid-template-columns: repeat(2, 1fr);
    gap: 1.5rem;
  }
}
```

### Breakpoints (keep to two)

| Name | Min width | Use |
|------|-----------|-----|
| `--bp-mobile` | default (0–1023px) | Phone, single column |
| `--bp-desktop` | `1024px` | Desktop, wider layouts |

Don't add a third breakpoint unless a real layout problem demands it. Two breakpoints are enough for nearly every page.

### Layout rules

- **No fixed widths.** Use percentages, `max-width`, or grid/flex. A `width: 1000px` container breaks on a 375px phone.
- **Images/media:** `max-width: 100%; height: auto;` so nothing overflows the screen.
- **Touch targets:** buttons and links at least `44px` tall/tappable.
- **No horizontal scroll** on phone. If content overflows, it's a bug — fix the container, not the viewport.

### Quick responsive checklist

- [ ] Page reads fine at 375px wide (single column, no horizontal scroll).
- [ ] Text is at least 16px body on phone (don't shrink body copy to fit).
- [ ] Buttons/taps are ≥ 44px.
- [ ] At 1024px+, layout uses the available width deliberately (max-width cap, multi-column where it helps).
- [ ] No element is clipped or cut off at either width.

---

## 4. Page skeleton to copy

Start every new page from this skeleton so the three bars are met by default:

```html
<!DOCTYPE html>
<html lang="en">
<head>
  <meta charset="UTF-8">
  <meta name="viewport" content="width=device-width, initial-scale=1.0">
  <title>Page title</title>
  <style>
    :root {
      --color-text: #111827;
      --color-text-secondary: #4B5563;
      --color-border: #6B7280;
      --color-bg: #FFFFFF;
      --color-surface: #F9FAFB;

      --text-xs: 0.75rem;
      --text-sm: 0.875rem;
      --text-base: 1rem;
      --text-lg: 1.125rem;
      --text-xl: 1.25rem;
      --text-2xl: 1.5rem;
      --text-3xl: 1.875rem;
      --leading-body: 1.5;
      --leading-tight: 1.25;

      --bp-desktop: 1024px;
    }

    *, *::before, *::after { box-sizing: border-box; }
    html { -webkit-text-size-adjust: 100%; }

    body {
      margin: 0;
      font-family: system-ui, -apple-system, BlinkMacSystemFont, "Segoe UI", Roboto, sans-serif;
      font-size: var(--text-base);
      line-height: var(--leading-body);
      color: var(--color-text);
      background: var(--color-bg);
    }

    h1 { font-size: var(--text-3xl); line-height: var(--leading-tight); margin: 0 0 1rem; }
    h2 { font-size: var(--text-2xl); line-height: var(--leading-tight); margin: 0 0 0.75rem; }
    h3 { font-size: var(--text-xl); line-height: var(--leading-tight); margin: 0 0 0.5rem; }
    p { margin: 0 0 1rem; }
    a { color: inherit; text-decoration: underline; }
    .secondary { color: var(--color-text-secondary); font-size: var(--text-sm); }

    .container {
      width: 100%;
      padding: 1rem;
      margin: 0 auto;
    }

    @media (min-width: var(--bp-desktop)) {
      .container {
        max-width: 960px;
        padding: 2rem;
      }
      .grid { display: grid; grid-template-columns: repeat(2, 1fr); gap: 1.5rem; }
    }

    img, video { max-width: 100%; height: auto; display: block; }
    button, a.button {
      appearance: none;
      padding: 0.6rem 1rem;
      font-size: var(--text-base);
      min-height: 44px;
      border: 1px solid var(--color-border);
      border-radius: 6px;
      background: var(--color-bg);
      color: var(--color-text);
      cursor: pointer;
    }
    button:hover, a.button:hover { background: var(--color-surface); }
  </style>
</head>
<body>
  <div class="container">
    <h1>Page title</h1>
    <p class="secondary">Secondary, readable grey text — passes WCAG AA at this size.</p>
    <p>Body copy at the base size, readable on phone and desktop.</p>
    <div class="grid">
      <section>Left column on desktop, stacked on phone.</section>
      <section>Right column on desktop, stacked on phone.</section>
    </div>
  </div>
</body>
</html>
```

---

## 5. Before you finish a page

Check each one:

- [ ] All body text ≥ 4.5:1 contrast against its background (or ≥ 3:1 if it's large text).
- [ ] No font-size values outside the defined type scale (or a new token was added to the scale).
- [ ] Page reads at 375px wide without horizontal scroll or unreadably small text.
- [ ] Page uses the desktop width deliberately at 1024px+ (max-width cap, not edge-to-edge stretching).
- [ ] Buttons/taps are at least 44px.
- [ ] Images/media don't overflow their container.

If you changed CSS, open the page in a browser at both a phone width (toggle device toolbar) and a desktop width to confirm.

---

## When to use this skill

Trigger on any request to author or edit a page — "build a landing page", "make this page responsive", "fix the grey text", "add a section to the homepage", "the layout breaks on my phone", "write some HTML/CSS for...". Apply it by default even when accessibility or responsiveness isn't mentioned; every page we ship clears these three bars.