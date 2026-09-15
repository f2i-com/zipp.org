# Small object-oriented workloads: an inventory with events, a priority queue simulation,
# a tiny stack VM, a graph search, and a matrix-free linear model trained with plain floats.
import heapq
from collections import deque


class Event:
    __slots__ = ("time", "kind", "payload")

    def __init__(self, time, kind, payload):
        self.time, self.kind, self.payload = time, kind, payload

    def __lt__(self, other):
        return (self.time, self.kind) < (other.time, other.kind)


class Inventory:
    def __init__(self):
        self.stock = {}
        self.log = []

    def apply(self, ev):
        item, qty = ev.payload
        have = self.stock.get(item, 0)
        if ev.kind == "buy":
            self.stock[item] = have + qty
        elif ev.kind == "sell":
            if have < qty:
                self.log.append(f"t={ev.time} short {item}: {have}<{qty}")
                return False
            self.stock[item] = have - qty
            if self.stock[item] == 0:
                del self.stock[item]
        return True


queue = []
items = ["apple", "pear", "fig"]
for t in range(40):
    kind = "buy" if t % 3 else "sell"
    heapq.heappush(queue, Event((t * 7) % 23, kind, (items[t % 3], (t % 5) + 1)))
inv = Inventory()
ok = 0
while queue:
    if inv.apply(heapq.heappop(queue)):
        ok += 1
print("inventory", ok, sorted(inv.stock.items()), len(inv.log), inv.log[:2])


class StackVM:
    def __init__(self):
        self.stack = []
        self.ops = {"add": self.add, "mul": self.mul, "dup": self.dup, "swap": self.swap}

    def push(self, v):
        self.stack.append(v)

    def add(self):
        b, a = self.stack.pop(), self.stack.pop()
        self.push(a + b)

    def mul(self):
        b, a = self.stack.pop(), self.stack.pop()
        self.push(a * b)

    def dup(self):
        self.push(self.stack[-1])

    def swap(self):
        self.stack[-1], self.stack[-2] = self.stack[-2], self.stack[-1]

    def run(self, program):
        for instr in program:
            if isinstance(instr, int):
                self.push(instr)
            else:
                self.ops[instr]()
        return self.stack


prog = [2, 3, "add", "dup", "mul", 7, "swap", 1, "add"]
print("stackvm", StackVM().run(prog), StackVM().run([1] + ["dup", "add"] * 70)[-1])
graph = {}
for i in range(30):
    graph.setdefault(i, []).extend(sorted({(i * 2) % 30, (i + 7) % 30, (i * i) % 30} - {i}))


def bfs(start, goal):
    prev = {start: None}
    dq = deque([start])
    while dq:
        node = dq.popleft()
        if node == goal:
            path = []
            while node is not None:
                path.append(node)
                node = prev[node]
            return path[::-1]
        for nxt in graph[node]:
            if nxt not in prev:
                prev[nxt] = node
                dq.append(nxt)
    return None


print("bfs", bfs(1, 29), bfs(0, 13), len(bfs(5, 6) or []))


def dijkstra(src):
    dist = {src: 0}
    pq = [(0, src)]
    while pq:
        d, u = heapq.heappop(pq)
        if d > dist.get(u, float("inf")):
            continue
        for v in graph[u]:
            nd = d + (u * v) % 7 + 1
            if nd < dist.get(v, float("inf")):
                dist[v] = nd
                heapq.heappush(pq, (nd, v))
    return dist


dist = dijkstra(0)
print("dijkstra", len(dist), sum(dist.values()), max(dist.values()), [dist.get(k) for k in range(0, 30, 6)])


class Linear:
    def __init__(self, n):
        self.w = [0.0] * n
        self.b = 0.0

    def predict(self, x):
        return sum(wi * xi for wi, xi in zip(self.w, x)) + self.b

    def step(self, batch, lr):
        gw = [0.0] * len(self.w)
        gb = 0.0
        for x, y in batch:
            err = self.predict(x) - y
            for i, xi in enumerate(x):
                gw[i] += err * xi
            gb += err
        n = len(batch)
        self.w = [wi - lr * g / n for wi, g in zip(self.w, gw)]
        self.b -= lr * gb / n
        return sum((self.predict(x) - y) ** 2 for x, y in batch) / n


data = []
for i in range(24):
    x = [((i * 5) % 7) / 7.0, ((i * 3) % 11) / 11.0, (i % 4) / 4.0]
    y = 2.0 * x[0] - 1.5 * x[1] + 0.5 * x[2] + 0.25
    data.append((x, y))
model = Linear(3)
losses = [model.step(data, 0.5) for _ in range(40)]
print("linear", round(losses[0], 10), round(losses[-1], 10), [round(w, 6) for w in model.w], round(model.b, 6))


class Account:
    interest = 0.01

    def __init__(self, owner, balance=0):
        self.owner = owner
        self.balance = balance
        self.history = []

    def deposit(self, amount):
        if amount <= 0:
            raise ValueError("deposit must be positive")
        self.balance += amount
        self.history.append(("dep", amount))

    def withdraw(self, amount):
        if amount > self.balance:
            raise ValueError(f"insufficient funds for {self.owner}")
        self.balance -= amount
        self.history.append(("wd", amount))


class Savings(Account):
    interest = 0.03

    def accrue(self):
        gain = round(self.balance * self.interest, 2)
        self.deposit(gain)
        return gain


accounts = [Account("ann", 100), Savings("bob", 1000), Savings("cy")]
errors = []
for day in range(30):
    for i, acct in enumerate(accounts):
        try:
            if (day + i) % 4 == 0:
                acct.withdraw(75 + day)
            else:
                acct.deposit((day * 13 + i) % 50)
        except ValueError as e:
            errors.append(str(e))
    if day % 10 == 9:
        for acct in accounts:
            if isinstance(acct, Savings):
                acct.accrue()
print("accounts", [(a.owner, round(a.balance, 2), len(a.history)) for a in accounts], len(errors), errors[:2])
