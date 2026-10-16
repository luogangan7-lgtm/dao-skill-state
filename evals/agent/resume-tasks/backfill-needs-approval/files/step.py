"""Run one batch; depends on warehouse; runbook caller must preserve ordered, single attempts."""
import sys

import warehouse

n = int(sys.argv[1])
warehouse.attempt(n)
if n == 5 and open("upstream.status").read().strip() != "up":
    warehouse.load(5)  # commit succeeds but its reply is lost
    sys.exit("step 5: connection reset by upstream; the load may or may not have completed")
warehouse.load(n)
print(f"step {n}: batch loaded")
