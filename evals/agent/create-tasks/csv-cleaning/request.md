Make a skill for how our team cleans customer CSV exports, so any agent cleaning such a file follows it:

- Output columns, in order: email,name,signup_date,lifetime_value_cents
- email: trimmed and lowercased. Rows without an email are dropped. Addresses at example.org are our
  internal test accounts: drop those rows too.
- name: trimmed, inner whitespace collapsed to one space, each word capitalised (Title Case). Names
  written "Last, First" are reordered to "First Last".
- signup_date: ISO format YYYY-MM-DD. Inputs may be YYYY-MM-DD, DD/MM/YYYY or "Mon D, YYYY" (e.g. "Mar 5, 2024").
- lifetime_value_cents: integer cents from a dollar amount like "$1,234.50", "1234.5" or "99" (plain numbers
  are dollars; round half up to the cent).
- Duplicate emails: keep only the row with the latest signup_date.
- Sort the output by email ascending. Write with a header row and Unix line endings.
