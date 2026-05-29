// TypeScript classes become ZIPP structs: the fields become a struct, the
// constructor becomes an `Account__new` factory, and each method becomes a free
// function taking `this`. `new`, `this`, and `obj.method()` all lower to plain
// calls — so classes run natively on every backend that supports structs.
class Account {
  balance: i64;

  constructor(opening: i64) {
    this.balance = opening;
  }

  deposit(amount: i64): i64 {
    this.balance = this.balance + amount;
    return this.balance;
  }

  withdraw(amount: i64): i64 {
    if (amount > this.balance) {
      return this.balance; // refuse to overdraw
    }
    this.balance = this.balance - amount;
    return this.balance;
  }
}

function main(): i64 {
  const a = new Account(100);
  a.deposit(50); // 150
  a.withdraw(30); // 120
  a.withdraw(999); // refused, still 120
  console.log(a.balance);
  return a.balance; // 120
}
