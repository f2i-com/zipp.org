// Two ergonomics wins: `switch` on strings, and early-return narrowing — after
// `if (x === null) return …;` the value is non-null for the rest of the block.

interface User {
  name: str;
  role: str;
}

function rank(role: str): i64 {
  switch (role) {
    case "admin":
      return 3;
    case "editor":
    case "author": // an empty case stacks onto the next
      return 2;
    default:
      return 1;
  }
}

function rankOf(u: User | null): i64 {
  if (u === null) {
    return 0; // early return — `u` is narrowed to User below
  }
  return rank(u.role);
}

function main(): i64 {
  const ada: User = { name: "Ada", role: "admin" };
  const bob: User = { name: "Bob", role: "author" };
  console.log(rankOf(ada)); // 3
  console.log(rankOf(bob)); // 2
  console.log(rankOf(null)); // 0
  return rankOf(ada) * 100 + rankOf(bob) * 10 + rankOf(null); // 320
}
