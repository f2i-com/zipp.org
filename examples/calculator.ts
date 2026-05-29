// A recursive-descent arithmetic calculator — a real program exercising classes,
// `this`, recursion, string methods (charCodeAt), and control flow. It parses
// directly from the source string (no token array), so it uses only
// native-eligible features and runs on the interpreter, --jit, AND --llvm.
//
// Grammar (standard precedence):
//   expr   = term   (('+' | '-') term)*
//   term   = factor (('*' | '/') factor)*
//   factor = number | '(' expr ')' | '-' factor

class Parser {
  src: str;
  pos: i64;

  constructor(src: str) {
    this.src = src;
    this.pos = 0;
  }

  // Current byte, or -1 at end of input.
  peek(): i64 {
    return this.pos < len(this.src) ? this.src.charCodeAt(this.pos) : -1;
  }

  skipSpaces(): i64 {
    while (this.peek() === 32) {
      this.pos = this.pos + 1;
    }
    return this.pos;
  }

  // One or more decimal digits.
  parseNumber(): i64 {
    let n = 0;
    while (true) {
      const c = this.peek();
      if (c < 48 || c > 57) {
        break; // not '0'..'9'
      }
      n = n * 10 + (c - 48);
      this.pos = this.pos + 1;
    }
    return n;
  }

  // number | '(' expr ')' | unary minus
  parseFactor(): i64 {
    this.skipSpaces();
    const c = this.peek();
    if (c === 40) {
      // '('
      this.pos = this.pos + 1;
      const v = this.parseExpr();
      this.skipSpaces();
      this.pos = this.pos + 1; // skip ')'
      return v;
    }
    if (c === 45) {
      // unary '-'
      this.pos = this.pos + 1;
      return -this.parseFactor();
    }
    return this.parseNumber();
  }

  parseTerm(): i64 {
    let v = this.parseFactor();
    while (true) {
      this.skipSpaces();
      const c = this.peek();
      if (c === 42) {
        this.pos = this.pos + 1;
        v = v * this.parseFactor();
      } else if (c === 47) {
        this.pos = this.pos + 1;
        v = v / this.parseFactor(); // i64 division
      } else {
        break;
      }
    }
    return v;
  }

  parseExpr(): i64 {
    let v = this.parseTerm();
    while (true) {
      this.skipSpaces();
      const c = this.peek();
      if (c === 43) {
        this.pos = this.pos + 1;
        v = v + this.parseTerm();
      } else if (c === 45) {
        this.pos = this.pos + 1;
        v = v - this.parseTerm();
      } else {
        break;
      }
    }
    return v;
  }
}

function evalExpr(s: str): i64 {
  const p = new Parser(s);
  return p.parseExpr();
}

function main(): i64 {
  console.log(evalExpr("1 + 2 * 3")); // 7   (precedence)
  console.log(evalExpr("(1 + 2) * 3")); // 9   (parens)
  console.log(evalExpr("10 - 2 - 3")); // 5   (left associative)
  console.log(evalExpr("2 * (3 + 4) - 5")); // 9
  console.log(evalExpr("100 / 7")); // 14  (integer division)
  console.log(evalExpr("-(3 + 4) + 10")); // 3   (unary minus)
  console.log(evalExpr("2 * -3 + 8")); // 2
  return evalExpr("(2 + 3) * (4 + 5)"); // 45
}
