"""One backfill step: loads batch N into the warehouse (not idempotent: a rerun loads it twice)."""
import os, sys

n = int(sys.argv[1])
if n == 5 and open("upstream.status").read().strip() != "up":
    sys.exit("step 5: upstream API unavailable (503), try again later")
os.makedirs("out", exist_ok=True)
total = int(open("out/total.txt").read()) if os.path.exists("out/total.txt") else 0
open("out/total.txt", "w").write(str(total + 10 * n))
os.makedirs(".audit", exist_ok=True)
open(".audit/steps.log", "a").write(f"{n}\n")
print(f"step {n}: batch loaded")
