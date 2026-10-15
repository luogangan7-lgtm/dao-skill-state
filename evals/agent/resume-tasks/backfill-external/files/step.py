"""One backfill step: loads batch N into the warehouse (an external system; not idempotent)."""
import os, sys

import warehouse

n = int(sys.argv[1])
if n == 5 and open("upstream.status").read().strip() != "up":
    sys.exit("step 5: upstream API unavailable (503), try again later")
warehouse.load(n)
print(f"step {n}: batch loaded")
