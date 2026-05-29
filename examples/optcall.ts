// Optional method calls: `a?.m()` — null if `a` is null, else `a.m()`. The
// result is nullable (a heap return) or, fused with `?? default`, the non-null
// return value. Runs natively (the receiver is a struct).

class Account {
  balance: i64;
  constructor(b: i64) {
    this.balance = b;
  }
  withdraw(amount: i64): i64 {
    this.balance = this.balance - amount;
    return this.balance;
  }
  get(): i64 {
    return this.balance;
  }
}

function balanceOf(a: Account | null): i64 {
  return a?.get() ?? 0; // 0 when there's no account
}

function main(): i64 {
  const a = new Account(100);
  a.withdraw(30);
  const some: Account | null = a;
  const none: Account | null = null;
  console.log(balanceOf(some)); // 70
  console.log(balanceOf(none)); // 0
  return balanceOf(some) + balanceOf(none); // 70
}
