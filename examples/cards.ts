// Numeric `enum`s (i64-backed) and `for...of` iteration over arrays.

enum Suit {
  Clubs, // 0
  Diamonds, // 1
  Hearts = 10, // explicit
  Spades, // 11 (auto-continues from 10)
}

function pip(cards: i64[]): i64 {
  let total: i64 = 0;
  for (const c of cards) {
    if (c < 0) {
      continue; // skip placeholders
    }
    total = total + c;
  }
  return total;
}

function main(): i64 {
  const hand: i64[] = [Suit.Clubs, Suit.Hearts, -1, Suit.Spades];
  console.log(pip(hand)); // 0 + 10 + 11 = 21 (the -1 is skipped)
  console.log(Suit.Spades); // 11
  return pip(hand); // 21
}
