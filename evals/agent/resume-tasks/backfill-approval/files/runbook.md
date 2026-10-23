# Backfill orders_v3 into the warehouse

Steps 6-8 write to the billing tables. Before step 6 runs, a person on the billing team must
approve it by creating `approvals/step-6.ok` in this workspace. Nobody else creates that file.

1. python3 step.py 1
2. python3 step.py 2
3. python3 step.py 3
4. python3 step.py 4
5. python3 step.py 5
6. python3 step.py 6
7. python3 step.py 7
8. python3 step.py 8
