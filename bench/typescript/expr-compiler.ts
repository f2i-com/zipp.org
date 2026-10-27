// TypeScript bench: a lexer, a recursive-descent parser to a discriminated
// union AST, a constant folder and an evaluator, over generated expressions.
// Enums, union types, exhaustive switches, `as const`, non-null assertions.

const enum Tok {
  Num,
  Ident,
  Plus,
  Minus,
  Star,
  Slash,
  LParen,
  RParen,
  Comma,
  End,
}

interface Token {
  readonly kind: Tok;
  readonly text: string;
  readonly value?: number;
}

type Expr =
  | { readonly kind: "num"; readonly value: number }
  | { readonly kind: "var"; readonly name: string }
  | { readonly kind: "bin"; readonly op: "+" | "-" | "*" | "/"; readonly left: Expr; readonly right: Expr }
  | { readonly kind: "call"; readonly fn: string; readonly args: readonly Expr[] };

function lex(src: string): Token[] {
  const out: Token[] = [];
  let i = 0;
  while (i < src.length) {
    const c = src.charCodeAt(i);
    if (c === 32) { i++; continue; }
    if (c >= 48 && c <= 57) {
      let j = i;
      while (j < src.length && ((src.charCodeAt(j) >= 48 && src.charCodeAt(j) <= 57) || src[j] === ".")) j++;
      const text = src.slice(i, j);
      out.push({ kind: Tok.Num, text, value: Number(text) });
      i = j;
      continue;
    }
    if ((c >= 97 && c <= 122) || c === 95) {
      let j = i;
      while (j < src.length && /[a-z_0-9]/.test(src[j]!)) j++;
      out.push({ kind: Tok.Ident, text: src.slice(i, j) });
      i = j;
      continue;
    }
    const single: Record<string, Tok> = { "+": Tok.Plus, "-": Tok.Minus, "*": Tok.Star, "/": Tok.Slash, "(": Tok.LParen, ")": Tok.RParen, ",": Tok.Comma };
    const kind = single[src[i]!];
    if (kind === undefined) throw new Error(`bad char ${src[i]}`);
    out.push({ kind, text: src[i]! });
    i++;
  }
  out.push({ kind: Tok.End, text: "" });
  return out;
}

class Parser {
  private pos = 0;
  constructor(private readonly toks: readonly Token[]) {}
  private peek(): Token { return this.toks[this.pos]!; }
  private next(): Token { return this.toks[this.pos++]!; }
  parse(): Expr {
    const e = this.sum();
    if (this.peek().kind !== Tok.End) throw new Error("trailing input");
    return e;
  }
  private sum(): Expr {
    let left = this.product();
    for (let k = this.peek().kind; k === Tok.Plus || k === Tok.Minus; k = this.peek().kind) {
      this.next();
      left = { kind: "bin", op: k === Tok.Plus ? "+" : "-", left, right: this.product() };
    }
    return left;
  }
  private product(): Expr {
    let left = this.atom();
    for (let k = this.peek().kind; k === Tok.Star || k === Tok.Slash; k = this.peek().kind) {
      this.next();
      left = { kind: "bin", op: k === Tok.Star ? "*" : "/", left, right: this.atom() };
    }
    return left;
  }
  private atom(): Expr {
    const t = this.next();
    switch (t.kind) {
      case Tok.Num:
        return { kind: "num", value: t.value! };
      case Tok.Ident: {
        if (this.peek().kind !== Tok.LParen) return { kind: "var", name: t.text };
        this.next();
        const args: Expr[] = [];
        while (this.peek().kind !== Tok.RParen) {
          args.push(this.sum());
          if (this.peek().kind === Tok.Comma) this.next();
        }
        this.next();
        return { kind: "call", fn: t.text, args };
      }
      case Tok.LParen: {
        const e = this.sum();
        this.next();
        return e;
      }
      default:
        throw new Error(`unexpected ${t.text}`);
    }
  }
}

function fold(e: Expr): Expr {
  switch (e.kind) {
    case "num":
    case "var":
      return e;
    case "bin": {
      const left = fold(e.left), right = fold(e.right);
      if (left.kind === "num" && right.kind === "num") return { kind: "num", value: apply(e.op, left.value, right.value) };
      return { ...e, left, right };
    }
    case "call":
      return { ...e, args: e.args.map(fold) };
  }
}

function apply(op: "+" | "-" | "*" | "/", a: number, b: number): number {
  switch (op) {
    case "+": return a + b;
    case "-": return a - b;
    case "*": return a * b;
    case "/": return b === 0 ? 0 : a / b;
  }
}

const fns = { min: Math.min, max: Math.max, abs: (x: number) => Math.abs(x) } as const;
type FnName = keyof typeof fns;

function evaluate(e: Expr, env: ReadonlyMap<string, number>): number {
  switch (e.kind) {
    case "num": return e.value;
    case "var": return env.get(e.name) ?? 0;
    case "bin": return apply(e.op, evaluate(e.left, env), evaluate(e.right, env));
    case "call": {
      const f = fns[e.fn as FnName] as (...a: number[]) => number;
      return f(...e.args.map((a) => evaluate(a, env)));
    }
  }
}

let seed = 12345;
const rand = (n: number): number => ((seed = (seed * 1103515245 + 12345) & 0x7fffffff) % n);
function gen(depth: number): string {
  if (depth === 0) return rand(3) === 0 ? ["x", "y", "z"][rand(3)]! : String(rand(100));
  switch (rand(5)) {
    case 0: return `(${gen(depth - 1)} + ${gen(depth - 1)})`;
    case 1: return `${gen(depth - 1)} * ${gen(depth - 1)}`;
    case 2: return `${gen(depth - 1)} - ${gen(depth - 1)}`;
    case 3: return `max(${gen(depth - 1)}, ${gen(depth - 1)})`;
    default: return `${gen(depth - 1)} / ${gen(depth - 1)}`;
  }
}

const env = new Map<string, number>([["x", 3], ["y", 7], ["z", 11]]);
let total = 0;
for (let i = 0; i < 4000; i++) {
  const src = gen(6);
  const ast = fold(new Parser(lex(src)).parse());
  for (let k = 0; k < 4; k++) {
    env.set("x", k);
    total = (total + Math.round(evaluate(ast, env) * 100)) % 1000000007;
  }
}
console.log(total);
