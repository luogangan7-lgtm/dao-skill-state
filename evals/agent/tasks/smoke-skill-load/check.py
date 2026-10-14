"""Pass when GREETING.txt starts with the code word only the skill knows (so the skill was used)."""
import os
import sys

p = os.path.join(sys.argv[1], "GREETING.txt")
text = open(p, encoding="utf-8").read().strip() if os.path.exists(p) else ""
print(repr(text[:80]))
sys.exit(0 if text.startswith("AURORA-7731:") else 1)
