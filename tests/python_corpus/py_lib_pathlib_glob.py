# pathlib: `**` anywhere in a glob pattern, `match` from the right, `.`/trailing
# slash cleanup without collapsing `..`, and mkdir(exist_ok=True) still needs
# the parent unless parents=True.
from pathlib import Path, PurePosixPath as P

print(P("a/b/c.py").match("b/*.py"), P("a/b/c.py").match("*/c.py"), P("a/b/c.py").match("a/b/*.py"))
print(P("a/b/c.py").match("*.py"), P("a/b/c.py").match("a/*.py"), P("a/b/c.py").match("x/a/b/c.py"))
print(P("/a/b/c.py").match("/b/c.py"), P("/a/b").match("/a/*"), P("a/b").match("/a/b"))
print(str(P("./data") / "x.csv"), str(P("out/")), P("./x").parts, str(P("a//b/./c/")), str(P(".")), P("").parts)
print(P("a/../b") == P("b"), P("a") / "b" == P("a/./b"), P("/a") == P("a"), str(P("a/b") / ".." / "c"))
print([p.as_posix() for p in sorted([P("a/b"), P("a-b"), P("a")])])
print(P("./x").name, P(".").name, P("x/.").name, P("./a/b").parent.as_posix(), P("a").parent.as_posix())

root = Path("pl_glob_probe")
(root / "src" / "pkg" / "sub").mkdir(parents=True, exist_ok=True)
for f in ["src/a.py", "src/pkg/b.py", "src/pkg/sub/c.py", "src/pkg/sub/d.txt", "top.py"]:
    (root / f).write_text("x")
rel = lambda ps: sorted(p.relative_to(root).as_posix() for p in ps)
print(rel(root.glob("src/**/*.py")))
print(rel(root.glob("**/sub/*.py")))
print(rel(root.rglob("sub/*.py")))
print(rel(root.glob("**/*.txt")))
print(rel(root.glob("**")))
print(rel(root.glob("*/")))
print(rel(root.glob("*/*.py")))
print(rel(root.rglob("*.py")))
try:
    (root / "nope" / "deeper").mkdir(exist_ok=True)
    print("no error")
except FileNotFoundError as e:
    print(type(e).__name__)
(root / "nope" / "deeper").mkdir(parents=True, exist_ok=True)
(root / "nope").mkdir(exist_ok=True)
print((root / "nope" / "deeper").is_dir())
try:
    (root / "top.py").mkdir(exist_ok=True)
except FileExistsError as e:
    print(type(e).__name__)
for f in ["src/a.py", "src/pkg/b.py", "src/pkg/sub/c.py", "src/pkg/sub/d.txt", "top.py"]:
    (root / f).unlink()
for d in ["src/pkg/sub", "src/pkg", "src", "nope/deeper", "nope", "."]:
    if (root / d).exists():
        (root / d).rmdir()
print(root.exists())
