# Layout reference

## Mobile-first container

```css
.page {
  padding: 1rem;              /* side padding on small screens */
  max-width: 100%;
  margin: 0 auto;
}

.page-content {
  max-width: 100%;
}
```

At mobile width (≈375px) this gives a comfortable single column with side padding. Text lines stay short enough to read; nothing overflows horizontally.

## Desktop override (single media query)

```css
@media (min-width: 768px) {
  .page {
    padding: 2rem;
  }

  .page-content {
    max-width: 800px;   /* or 1024px for wider pages */
    margin: 0 auto;
  }

  /* optional: two-column grid here */
  .grid {
    display: grid;
    grid-template-columns: 1fr 1fr;
    gap: 1.5rem;
  }
}
```

Only layout and spacing change here — type scale, contrast, and font weights stay identical.

## Touch targets

```css
button, a, [role="button"], input, select, textarea {
  min-height: 44px;
  min-width: 44px;
  padding: 0.5rem 1rem;
}
```

## Images and media

```css
img, video, canvas, svg {
  max-width: 100%;
  height: auto;
  display: block;
}
```

## Check at two widths

Resize the browser (or use dev-tools device toolbar) to ≈375px and ≈1024px. At each width confirm:
- No horizontal scrollbar.
- Text is readable and lines are a comfortable length.
- Buttons/links are easy to tap (≥44px).
- Nothing is cramped or cut off.

If anything fails at either width, adjust the CSS before finishing.
