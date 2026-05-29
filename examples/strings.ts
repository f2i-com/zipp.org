// String methods (byte-level / ASCII in v0). Each is a zipp_str_* runtime call,
// so they run NATIVELY on --jit and --llvm — string-heavy code stays on the fast
// path. (charCodeAt/charAt/slice/indexOf are byte-based: ASCII-exact vs TS.)

function main(): i64 {
  const s = "hello, world";

  console.log(s.charCodeAt(0)); // 104  ('h')
  console.log(s.indexOf("world")); // 7
  console.log(s.lastIndexOf("o")); // 8
  console.log(s.includes("lo") ? 1 : 0); // 1
  console.log(s.startsWith("hello") ? 1 : 0); // 1
  console.log(s.endsWith("world") ? 1 : 0); // 1

  const hello = s.slice(0, 5); // "hello"
  const tail = s.slice(-5); // "world"  (negative index)
  console.log(hello + " " + tail); // "hello world"

  console.log(s.charAt(7)); // "w"
  console.log("ab".repeat(3)); // "ababab"

  return s.indexOf("world") + len(hello) + ("ha".repeat(2) === "haha" ? 1 : 0); // 7 + 5 + 1
}
