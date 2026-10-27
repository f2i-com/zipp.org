#!/usr/bin/env python3
"""Time zipp against Node on the TypeScript benchmarks (bench/typescript).

    python tools/bench_ts.py [--zipp target/release/zipp.exe] [--node node] [--reps 7]

Each program runs as a `.ts` file in both: `zipp <file.ts>`, and `node
<file.ts>` (Node's built-in type stripping; with `--experimental-transform-
types` for programs that use enums, namespaces or parameter properties,
which stripping alone rejects). Their outputs must agree. Times are wall
clock medians of interleaved runs, start-up included, which is what running
a `.ts` file costs.

A generated `large.ts` (several thousand typed declarations, little run
time) measures the cost of reading TypeScript itself.
"""
import argparse
import os
import statistics
import subprocess
import sys
import time
from pathlib import Path

ROOT = Path(__file__).resolve().parent.parent
BENCH = ROOT / "bench" / "typescript"


def generate_large(path: Path, units: int) -> None:
    """A large module of typed declarations: interfaces, generic classes,
    enums, overloads and arrow functions, mostly never called."""
    out = []
    for i in range(units):
        out.append(
            f"""
export interface Shape{i}<T extends object = {{}}> {{
  readonly id: number;
  name?: string;
  tags: ReadonlyArray<string>;
  meta: Record<string, T | null>;
  transform(input: T, options?: {{ deep: boolean; depth: number }}): Promise<T[]>;
}}
export type Union{i} = {{ kind: "a"; value: number }} | {{ kind: "b"; text: string }} | null;
export enum Kind{i} {{ First = {i}, Second, Third = First << 1, Label = "label{i}" }}
export class Store{i}<K extends string, V> implements Iterable<[K, V]> {{
  private readonly items = new Map<K, V>();
  constructor(private readonly label: string, protected limit: number = {i}) {{}}
  get(key: K): V | undefined {{ return this.items.get(key); }}
  set(key: K, value: V): this {{ if (this.items.size < this.limit) this.items.set(key, value); return this; }}
  *[Symbol.iterator](): Iterator<[K, V]> {{ yield* this.items; }}
  static of<K extends string, V>(entries: readonly (readonly [K, V])[]): Store{i}<K, V> {{
    const s = new Store{i}<K, V>("of", entries.length);
    for (const [k, v] of entries) s.set(k, v);
    return s;
  }}
}}
export function pick{i}(u: Union{i}): string;
export function pick{i}(u: Union{i}, fallback: string): string;
export function pick{i}(u: Union{i}, fallback = "none"): string {{
  if (u === null) return fallback;
  return u.kind === "a" ? String(u.value as number) : u.text!;
}}
export const helper{i} = <T,>(xs: readonly T[], f: (x: T, i: number) => boolean): T[] => xs.filter(f);
"""
        )
    out.append(
        "\nlet acc = 0;\n"
        + "".join(f"acc += Kind{i}.Third + pick{i}({{ kind: 'a', value: {i} }}).length;\n" for i in range(0, units, max(1, units // 50)))
        + "console.log(acc);\n"
    )
    path.write_text("".join(out), encoding="utf-8")


def run(cmd: list, cwd: Path) -> tuple:
    start = time.perf_counter()
    p = subprocess.run(cmd, cwd=cwd, capture_output=True, text=True, encoding="utf-8", errors="replace")
    return time.perf_counter() - start, p.returncode, p.stdout.strip(), p.stderr.strip()


def node_command(node: str, file: Path) -> list:
    t, code, out, err = run([node, "--no-warnings", str(file)], file.parent)
    if code != 0 and "ERR_UNSUPPORTED_TYPESCRIPT_SYNTAX" in err:
        return [node, "--no-warnings", "--experimental-transform-types", str(file)]
    return [node, "--no-warnings", str(file)]


def main() -> int:
    ap = argparse.ArgumentParser()
    ap.add_argument("--zipp", default=str(ROOT / "target" / "release" / ("zipp.exe" if os.name == "nt" else "zipp")))
    ap.add_argument("--node", default="node")
    ap.add_argument("--reps", type=int, default=7)
    ap.add_argument("--units", type=int, default=1500, help="declarations in the generated large.ts")
    ap.add_argument("only", nargs="*", help="benchmark names to run (default: all)")
    args = ap.parse_args()

    scratch = ROOT / "target" / "bench-ts"
    scratch.mkdir(parents=True, exist_ok=True)
    large = scratch / "large.ts"
    generate_large(large, args.units)
    files = sorted(BENCH.glob("*.ts")) + [large]
    if args.only:
        files = [f for f in files if f.stem in args.only]

    print(f"{'program':16} {'zipp ms':>9} {'node ms':>9} {'zipp/node':>10}  node mode")
    ratios = []
    for f in files:
        ncmd = node_command(args.node, f)
        zcmd = [args.zipp, str(f)]
        zt, nt = [], []
        zout = nout = None
        for _ in range(args.reps):
            t, code, out, err = run(zcmd, f.parent)
            if code != 0:
                print(f"{f.stem}: zipp failed: {err[:300]}")
                break
            zt.append(t)
            zout = out
            t, code, out, err = run(ncmd, f.parent)
            if code != 0:
                print(f"{f.stem}: node failed: {err[:300]}")
                break
            nt.append(t)
            nout = out
        if not zt or not nt:
            continue
        if zout != nout:
            print(f"{f.stem}: OUTPUTS DIFFER\n  zipp: {zout[:200]}\n  node: {nout[:200]}")
        z, n = statistics.median(zt) * 1000, statistics.median(nt) * 1000
        ratios.append(z / n)
        mode = "transform-types" if "--experimental-transform-types" in ncmd else "strip"
        print(f"{f.stem:16} {z:9.1f} {n:9.1f} {z / n:9.2f}x  {mode}")
    if ratios:
        geo = statistics.geometric_mean(ratios)
        print(f"{'geomean':16} {'':9} {'':9} {geo:9.2f}x")
    return 0


if __name__ == "__main__":
    sys.exit(main())
