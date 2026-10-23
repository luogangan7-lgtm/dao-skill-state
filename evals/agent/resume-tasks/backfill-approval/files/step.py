"""One backfill step: loads batch N into the warehouse (an external system; not idempotent)."""
import sys

import warehouse

warehouse.load(int(sys.argv[1]))
print(f"step {sys.argv[1]}: batch loaded")
