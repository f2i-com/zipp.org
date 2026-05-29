// Optional / nullable struct references (`T | null`). Default with `??`, or
// narrow with `if (x !== null)`. Nullable values run on the interpreter; the
// native tiers (--jit/--llvm/--wasm) cleanly fall back, since they don't yet
// carry a null representation.

interface User {
  name: str;
  age: i64;
}

function findUser(id: i64): User | null {
  if (id === 1) {
    return { name: "Ada", age: 36 };
  }
  return null; // not found
}

function greet(u: User | null): str {
  if (u !== null) {
    return "hi " + u.name; // `u` is narrowed to User here
  }
  return "hi guest";
}

function main(): i64 {
  console.log(greet(findUser(1))); // "hi Ada"
  console.log(greet(findUser(2))); // "hi guest"

  const guest: User = { name: "guest", age: 0 };
  const u = findUser(2) ?? guest; // coalesce to a non-null default
  console.log(u.age); // 0

  const found = findUser(1);
  if (found !== null) {
    return found.age; // 36
  }
  return -1;
}
