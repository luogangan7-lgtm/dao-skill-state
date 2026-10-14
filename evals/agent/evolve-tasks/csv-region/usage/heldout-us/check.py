"""Pass: clean.csv equals the expected output (Unix line endings; a trailing newline is optional)."""
import os, sys

p = os.path.join(sys.argv[1], "clean.csv")
if not os.path.exists(p):
    sys.exit("no clean.csv")
raw = open(p, "rb").read().decode("utf-8")
if "\r" in raw:
    sys.exit("not Unix line endings")
if raw.strip("\n").split("\n") != ['email,name,signup_date,lifetime_value_cents', 'jon@example.com,Jon Park,2024-04-07,300', 'kim@example.com,Kim Ray,2024-01-15,1250']:
    sys.exit("clean.csv does not match the rules")
print("ok")
