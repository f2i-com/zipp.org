# argparse: flag actions advance past their token (they used to loop forever),
# a string default goes through `type`, and everything after `--` is positional.
import argparse


def parse(argv, *specs, defaults=None):
    p = argparse.ArgumentParser(prog="prog")
    for flags, kw in specs:
        p.add_argument(*flags, **kw)
    if defaults:
        p.set_defaults(**defaults)
    ns = p.parse_args(argv)
    return sorted(vars(ns).items())


print(parse(["--verbose"], (["--verbose"], dict(action="store_true"))))
print(parse(["--no-x"], (["--no-x"], dict(dest="x", action="store_false"))))
print(parse(["--const"], (["--const"], dict(action="store_const", const=42, default=0))))
print(parse(["-v", "-v", "-v"], (["-v"], dict(action="count", default=0))))
print(parse(["--flag", "pos", "--q"], (["--flag"], dict(action="store_true")), (["pos"], {}), (["--q"], dict(action="store_true"))))
print(parse([], (["--d"], dict(default="5", type=int)), (["--e"], dict(default="2.5", type=float)), (["--n"], dict(default=None, type=int))))
print(parse(["--d", "7"], (["--d"], dict(default="5", type=int))))
print(parse([], (["--d"], dict(default="5", type=int)), defaults={"d": "9"}))
print(parse([], (["p"], dict(nargs="?", default="3", type=int))))
print(parse([], (["--f"], dict(default="a,b", type=lambda s: s.split(",")))))
print(parse(["--", "-x"], (["x"], {})))
print(parse(["--", "--o", "1"], (["--o"], {}), (["rest"], dict(nargs="*"))))
print(parse(["a", "--", "--o", "1"], (["--o"], {}), (["rest"], dict(nargs="*"))))
p = argparse.ArgumentParser(prog="prog")
p.add_argument("x")
print(p.parse_known_args(["--", "-x", "extra", "--", "y"]))
p = argparse.ArgumentParser(prog="prog")
p.add_argument("--opt", nargs="?", const="C", default="D")
print(p.parse_args(["--opt"]), p.parse_args([]), p.parse_args(["--opt", "v"]))
