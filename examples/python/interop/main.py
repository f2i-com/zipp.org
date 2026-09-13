# Build with --features python-js-interop (trusted mixed-language projects).
import javascript
from js import eval as js_eval

result = javascript.eval("[1, 2, 3].map(x => x * 2)")
print(result)
javascript.eval("globalThis.sharedCounter = 41")
print(js_eval("++globalThis.sharedCounter"))
print(javascript.engine)
