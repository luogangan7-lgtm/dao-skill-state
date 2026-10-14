import hashlib
open(".tests.sha", "w").write(hashlib.sha256(open("test_pages.py", "rb").read()).hexdigest())
