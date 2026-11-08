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

/**
 * Promise.all that, on a rejection, still waits for every other promise to settle before rejecting
 * with the first rejected reason in array order, so none of the work is still running once the
 * caller moves on.
 */
export async function allSettledFirstError<T>(promises: Array<Promise<T>>): Promise<Array<T>> {
  const settled = await Promise.allSettled(promises);
  const rejected = settled.find((s): s is PromiseRejectedResult => s.status === 'rejected');
  if (rejected) throw rejected.reason;
  return settled.map((s) => (s as PromiseFulfilledResult<T>).value);
}
