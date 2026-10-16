"""Authoritative read command; depends on warehouse.loaded, called by the agent. Never loads."""
import sys

import warehouse

print("loaded" if warehouse.loaded(int(sys.argv[1])) else "not loaded")
