In slug.py, implement slugify(text: str) -> str for URL slugs: lowercase; letters with accents become
their plain letter ("Crème Brûlée" -> "creme-brulee"); every run of characters that are not a-z or 0-9
becomes a single "-"; no leading or trailing "-"; an input with no letters or digits gives "".
Also write tests for it in test_slug.py (pytest) and make sure they pass.
