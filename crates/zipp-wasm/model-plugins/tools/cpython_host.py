#!/usr/bin/env python3
"""Import a plugin under CPython exactly the way the JavaScript registry does.

`PluginRegistry.install` copies a plugin's Python files into a fixed
`zipp_plugin` package and runs a generated bootstrap that imports the entry
module from there, so a multi-file plugin imports its siblings by absolute name
(`from zipp_plugin.graph import Graph`). The fixed namespace is what stops a
plugin file shadowing the `json` the bootstrap itself imports, and the absolute
form names where the code actually ends up. Mirroring that layout here keeps the
reference tests, the fixture tools and the engine running identical source.
"""
import importlib
from pathlib import Path
import sys
import types

NAMESPACE = 'zipp_plugin'

def load_plugin(folder, entry='architecture'):
    """Return the entry module of the plugin in `folder`, imported as installed."""
    path = str(Path(folder).resolve())
    package = sys.modules.get(NAMESPACE)
    if package is None or list(getattr(package, '__path__', [])) != [path]:
        for name in [n for n in sys.modules if n == NAMESPACE or n.startswith(NAMESPACE + '.')]:
            del sys.modules[name]
        package = types.ModuleType(NAMESPACE)
        package.__path__ = [path]
        sys.modules[NAMESPACE] = package
    return importlib.import_module(NAMESPACE + '.' + entry)

def plugin_support(module):
    """The checkpoint/tokenizer families the source declares, for a manifest."""
    return dict(checkpoint_format=module.CHECKPOINT_FORMAT,
                tokenizer_formats=list(module.TOKENIZER_FORMATS))

def deliver_assets(module, layout, folder):
    """Hand a plugin its tokenizer assets the way ModelSession does.

    The host reads each file and decomposes it by the declared form -- one
    string, its lines, or the pairs of a flat JSON object -- because parsing a
    megabyte inside ZIPP is quadratic. Mirroring that here means the CPython
    tests exercise the same `load_asset` path the engine does.
    """
    import json as _json
    folder = Path(folder)
    for name, entry in layout["assets"].items():
        text = (folder/entry["path"]).read_text(encoding="utf-8")
        form = entry["form"]
        if form == "text":
            values = [text]
        elif form == "lines":
            values = text.split(chr(10))
            if values and values[-1] == "":
                values.pop()
        elif form == "json-pairs":
            parsed = _json.loads(text)
            if not isinstance(parsed, dict):
                raise ValueError("json-pairs needs a flat object")
            values = []
            for key, value in parsed.items():
                values.append(key)
                values.append(value)
        else:
            raise ValueError("unknown asset form " + form)
        module.load_asset(name, form, values)
    return {name: name for name in layout["assets"]}


def write_json(path, value):
    """JSON with LF endings: these bytes are hashed, on every platform."""
    Path(path).write_bytes((__import__('json').dumps(value, indent=2) + '\n').encode())
