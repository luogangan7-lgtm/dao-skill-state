"""Ask the warehouse whether batch N is loaded: prints `loaded` or `not loaded`."""
import sys

import warehouse

print("loaded" if warehouse.loaded(int(sys.argv[1])) else "not loaded")
