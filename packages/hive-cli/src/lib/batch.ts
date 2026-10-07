/** Split into fixed-size chunks. */
export function chunk<T>(arr: Array<T>, size: number): Array<Array<T>> {
  const out: Array<Array<T>> = [];
  for (let i = 0; i < arr.length; i += size) out.push(arr.slice(i, i + size));
  return out;
}

/** Map over items with at most `size` calls in flight at once. */
export async function mapBatched<T, TResult>(
  items: Array<T>,
  size: number,
  fn: (item: T) => Promise<TResult>,
): Promise<Array<TResult>> {
  const out: Array<TResult> = [];
  for (const batch of chunk(items, size)) out.push(...(await Promise.all(batch.map(fn))));
  return out;
}
