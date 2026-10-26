Make a skill for building our web pages. The pages our agents write keep failing review in the
same ways: grey secondary text that is too faint to read, dozens of slightly different font sizes
instead of a deliberate type scale, and layouts that break on phones. Any agent that writes or
changes a page for us should end up with readable contrast, a small consistent set of text sizes,
and a layout that works at phone and desktop widths.

We have `dao look PAGE.html` on PATH: it renders a page at 1280 and 375 px and prints JSON listing
contrast, target-size, horizontal-scroll, type-scale and similar defects; it exits 0 when the page
is clean, 1 when it found defects.
