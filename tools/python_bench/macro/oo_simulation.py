"""Object-oriented simulation: a bank of accounts with inheritance, properties,
`super()`, class attributes and polymorphic methods, driven by an LCG."""
import sys
import time

DAYS = 360
ACCOUNTS = 60


class InsufficientFunds(Exception):
    pass


class Account:
    fee = 0
    opened = 0

    def __init__(self, owner, cents):
        self.owner = owner
        self._cents = cents
        self.history = 0
        Account.opened += 1

    @property
    def balance(self):
        return self._cents

    @balance.setter
    def balance(self, value):
        if value < 0:
            raise InsufficientFunds(self.owner)
        self._cents = value

    def deposit(self, cents):
        self.balance = self.balance + cents
        self.history += 1

    def withdraw(self, cents):
        self.balance = self.balance - cents - self.fee
        self.history += 1

    def month_end(self):
        return 0

    def __repr__(self):
        return "%s(%s, %d)" % (type(self).__name__, self.owner, self._cents)


class Savings(Account):
    rate_per_mille = 3

    def month_end(self):
        interest = self.balance * self.rate_per_mille // 1000
        self.deposit(interest)
        return interest


class Checking(Account):
    fee = 25

    def withdraw(self, cents):
        if cents > 50000:
            raise InsufficientFunds(self.owner)
        super().withdraw(cents)


class Bank:
    def __init__(self):
        self.accounts = []
        self.failed = 0
        self.moved = 0

    def open(self, kind, owner, cents):
        account = kind(owner, cents)
        self.accounts.append(account)
        return account

    def transfer(self, src, dst, cents):
        try:
            src.withdraw(cents)
        except InsufficientFunds:
            self.failed += 1
            return False
        dst.deposit(cents)
        self.moved += cents
        return True

    def total(self):
        return sum(a.balance for a in self.accounts)


def bench(days, accounts):
    bank = Bank()
    kinds = [Account, Savings, Checking]
    for i in range(accounts):
        bank.open(kinds[i % 3], "owner%d" % i, 10000 + i * 137)
    state = 7
    interest = 0
    for day in range(days):
        for step in range(accounts):
            state = (state * 1103515245 + 12345) % 2147483648
            src = bank.accounts[state // 65536 % accounts]
            dst = bank.accounts[(state // 1024) % accounts]
            bank.transfer(src, dst, state % 20000)
        if day % 30 == 29:
            for account in bank.accounts:
                interest += account.month_end()
    richest = max(bank.accounts, key=lambda a: (a.balance, a.owner))
    return bank.total(), bank.moved, bank.failed, interest, Account.opened, richest


t0 = time.perf_counter()
result = bench(DAYS, ACCOUNTS)
elapsed = time.perf_counter() - t0
print("oo_simulation", DAYS, ACCOUNTS, result[0], result[1], result[2], result[3], result[4])
print("richest", repr(result[5]))
print("@bench-time %.6f" % elapsed, file=sys.stderr)
