/** Run `items` through `fn` with at most `limit` in flight. */
export async function pool<T>(items: Array<T>, limit: number, fn: (x: T, i: number) => Promise<void>): Promise<void> {
  let next = 0;
  const workers = Array.from({ length: Math.max(1, Math.min(limit, items.length)) }, async () => {
    while (next < items.length) {
      const i = next++;
      await fn(items[i]!, i);
    }
  });
  await Promise.all(workers);
}

/** Append `x` to `m`'s list at `k`, starting the list if there is none. */
export function pushTo<K, V>(m: Map<K, Array<V>>, k: K, x: V): void {
  const xs = m.get(k);
  if (xs) xs.push(x);
  else m.set(k, [x]);
}
