"""Pass: clean.csv equals the expected output (Unix line endings; a trailing newline is optional)."""
import os, sys

p = os.path.join(sys.argv[1], "clean.csv")
if not os.path.exists(p):
    sys.exit("no clean.csv")
raw = open(p, "rb").read().decode("utf-8")
if "\r" in raw:
    sys.exit("not Unix line endings")
got = raw.strip("\n").split("\n")
want = ["email,name,signup_date,lifetime_value_cents",
        "ann@example.com,Ann Smith,2024-04-09,1235",
        "bob@example.com,Bob Jones,2024-03-05,9900",
        "carl@example.com,Carl Oneil,2023-01-02,50"]
if got != want:
    sys.exit("differs: " + repr(got))
print("ok")
