# Type scale reference

Copy these CSS custom properties into the page's stylesheet (or component theme) and use them for every text size. Do not add arbitrary pixel sizes.

```css
:root {
  --fs-caption: 0.75rem;  /* 12px */
  --fs-small:   0.875rem; /* 14px */
  --fs-base:    1rem;     /* 16px */
  --fs-lead:    1.125rem; /* 18px */
  --fs-h3:      1.25rem;  /* 20px */
  --fs-h2:      1.5rem;   /* 24px */
  --fs-h1:      2rem;     /* 32px */

  --lh-body: 1.5;
  --lh-heading: 1.25;

  --fw-regular: 400;
  --fw-bold: 700;
}
```

Usage example:

```css
body {
  font-size: var(--fs-base);
  line-height: var(--lh-body);
  font-weight: var(--fw-regular);
  color: var(--color-body);
}

.caption {
  font-size: var(--fs-caption);
  font-weight: var(--fw-regular);
  /* secondary meaning from size, not lightness */
}

h1 { font-size: var(--fs-h1); line-height: var(--lh-heading); font-weight: var(--fw-bold); }
h2 { font-size: var(--fs-h2); line-height: var(--lh-heading); font-weight: var(--fw-bold); }
h3 { font-size: var(--fs-h3); line-height: var(--lh-heading); font-weight: var(--fw-bold); }
```

Size → element mapping:

- `caption` → timestamps, tiny labels, photo credits
- `small` → helper text, footnotes, secondary labels
- `base` → body paragraphs, default text
- `lead` → intro paragraphs, deck text
- `h3` → subsection headings
- `h2` → section headings
- `h1` → page title (one per page)

If the page needs a size not in this table, it probably means you should reuse an existing size or rethink the hierarchy — do not add a one-off size.
