// Optional chaining `a?.b`. Chain through nullable struct fields, and use
// `?? default` to read a possibly-null value as a non-null one. Runs natively
// on the JIT (a null is a 0 pointer).

interface Address {
  city: str;
  zip: i64;
}

interface User {
  name: str;
  address: Address | null;
}

function zipOf(u: User | null): i64 {
  return u?.address?.zip ?? 0; // 0 if the user or its address is null
}

function main(): i64 {
  const home: Address = { city: "Beverly Hills", zip: 90210 };
  const ada: User = { name: "Ada", address: home };
  const ghost: User = { name: "Ghost", address: null };

  console.log(zipOf(ada)); // 90210
  console.log(zipOf(ghost)); // 0 (no address)
  console.log(zipOf(null)); // 0 (no user)

  return zipOf(ada) + zipOf(ghost) + zipOf(null); // 90210
}
