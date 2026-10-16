"""Query committed batches via warehouse; called by the agent; never changes a load or attempt."""
import sys

import warehouse

print("loaded" if warehouse.loaded(int(sys.argv[1])) else "not loaded")
