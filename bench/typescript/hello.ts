// TypeScript bench: start-up. The time to read, compile and run a trivial
// typed program is the floor every `.ts` run pays.

interface Greeting {
  readonly to: string;
  times?: number;
}

const greet = ({ to, times = 1 }: Greeting): string => `hello, ${to}`.repeat(times);
console.log(greet({ to: "typescript" }));
