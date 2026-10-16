"""Runbook command; depends on warehouse, called by the agent. Each attempt is audited before
the upstream boundary; a down upstream fails batch 5 before any load can commit."""
import sys

import warehouse

n = int(sys.argv[1])
warehouse.attempt(n)
if n == 5 and open("upstream.status").read().strip() != "up":
    sys.exit("step 5: connection reset by upstream; the load may or may not have completed")
warehouse.load(n)
print(f"step {n}: batch loaded")
