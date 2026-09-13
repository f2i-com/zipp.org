"""Bundle this project's fixed ES-module set for offline browser validation.

Not a general JavaScript bundler. Browser tests use page.set_content and evaluate
only local source; they do not navigate around network restrictions. Production
demo uses native ES modules instead of this file.
"""
from pathlib import Path
import json
import re
import posixpath
ROOT=Path(__file__).resolve().parents[1]
FILES=["src/graph.mjs","src/backends/cpu.mjs","src/backends/wasm.mjs",
       "src/backends/webgpu.mjs","src/backends/webgl2.mjs","src/runtime.mjs","tests/browser-cases.mjs"]

def bundle():
    result=["(() => { const modules = Object.create(null);"]
    for name in FILES:
        source=(ROOT/name).read_text(encoding="utf-8")
        exports=re.findall(r"export\s+(?:async\s+)?(?:function|class|const)\s+([A-Za-z_][\w]*)",source)
        def imports(match):
            symbols,path=match.groups(); resolved=posixpath.normpath(posixpath.join(posixpath.dirname(name),path))
            return "const {%s} = modules[%s];" % (symbols,json.dumps(resolved))
        source=re.sub(r"import\s*\{([^}]+)\}\s*from\s*['\"]([^'\"]+)['\"];",imports,source)
        source=re.sub(r"\bexport\s+", "",source)
        source=source.replace("import.meta.url",json.dumps("https://local-test.invalid/"+name))
        result.append("modules[%s] = (() => {\n%s\nreturn {%s};\n})();" % (json.dumps(name),source,",".join(exports)))
    result.append("globalThis.ZippGPULab = {...modules['src/runtime.mjs'], ...modules['tests/browser-cases.mjs']}; })();")
    return "\n".join(result)

if __name__=="__main__":print(bundle())
