"""One backfill step: loads batch N into the warehouse (an external system; not idempotent)."""
import sys

import warehouse

n = int(sys.argv[1])
if n == 5 and open("upstream.status").read().strip() != "up":
    warehouse.load(5)  # the load commits, then the connection drops before the reply
    sys.exit("step 5: connection reset by upstream; the load may or may not have completed")
warehouse.load(n)
print(f"step {n}: batch loaded")
