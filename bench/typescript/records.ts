// TypeScript bench: a typed data pipeline. Records described by interfaces
// and enums, JSON round trips, grouping into Map<string, Aggregate>, sorting
// with typed comparators, a namespace of helpers.

enum Region {
  North = "north",
  South = "south",
  East = "east",
  West = "west",
}

enum Status {
  Pending,
  Shipped,
  Delivered,
  Returned,
}

interface LineItem {
  sku: string;
  qty: number;
  price: number;
}

interface Order {
  id: number;
  customer: string;
  region: Region;
  status: Status;
  items: LineItem[];
  note?: string;
}

interface Aggregate {
  orders: number;
  revenue: number;
  returned: number;
  topSku: Map<string, number>;
}

namespace Money {
  export const round = (n: number): number => Math.round(n * 100) / 100;
  export function total(items: readonly LineItem[]): number {
    let t = 0;
    for (const it of items) t += it.qty * it.price;
    return round(t);
  }
}

function makeOrders(n: number): Order[] {
  const regions = Object.values(Region);
  const out: Order[] = [];
  let s = 7;
  const rnd = (m: number) => ((s = (s * 16807) % 2147483647) % m);
  for (let id = 0; id < n; id++) {
    const items: LineItem[] = [];
    const count = 1 + rnd(5);
    for (let k = 0; k < count; k++) items.push({ sku: `SKU-${rnd(300)}`, qty: 1 + rnd(4), price: (rnd(10000) + 99) / 100 });
    out.push({
      id,
      customer: `cust${rnd(5000)}`,
      region: regions[rnd(regions.length)]!,
      status: rnd(4) as Status,
      items,
      ...(rnd(10) === 0 ? { note: "gift" } : {}),
    });
  }
  return out;
}

function aggregate(orders: readonly Order[]): Map<Region, Aggregate> {
  const by = new Map<Region, Aggregate>();
  for (const o of orders) {
    let a = by.get(o.region);
    if (!a) by.set(o.region, (a = { orders: 0, revenue: 0, returned: 0, topSku: new Map() }));
    a.orders++;
    if (o.status === Status.Returned) {
      a.returned++;
      continue;
    }
    a.revenue = Money.round(a.revenue + Money.total(o.items));
    for (const it of o.items) a.topSku.set(it.sku, (a.topSku.get(it.sku) ?? 0) + it.qty);
  }
  return by;
}

let digest = 0;
for (let round = 0; round < 6; round++) {
  const orders = makeOrders(25000);
  const wire = JSON.stringify(orders);
  const back = JSON.parse(wire) as Order[];
  const agg = aggregate(back);
  const ranked = [...agg.entries()].sort(([, a], [, b]) => b.revenue - a.revenue);
  for (const [region, a] of ranked) {
    const top = [...a.topSku].sort((x, y) => y[1] - x[1] || (x[0] < y[0] ? -1 : 1))[0]!;
    digest = (digest * 31 + Math.round(a.revenue) + a.returned * 7 + top[1] + region.length) % 1000000007;
  }
  digest = (digest + wire.length) % 1000000007;
}
console.log(digest);
