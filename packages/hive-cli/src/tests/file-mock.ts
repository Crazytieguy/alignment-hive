import { afterAll, mock } from 'bun:test';

type Fn = (...args: Array<any>) => unknown;

// mock.module stays in place for every later test file in the process (bun 1.3.11, which CI
// pins), so each stub here hands back to the real export once the calling file is done.
// `specifier` resolves from this directory, so callers must live in it too.
export function mockForFile<T extends Record<string, unknown>>(
  specifier: string,
  real: T,
  stubs: { [K in keyof T]?: Fn },
): void {
  let active = true;
  afterAll(() => {
    active = false;
  });
  const stubbed: Record<string, unknown> = { ...real };
  for (const [name, stub] of Object.entries(stubs)) {
    const original = real[name] as Fn;
    stubbed[name] = (...args: Array<any>) => (active ? stub!(...args) : original(...args));
  }
  mock.module(specifier, () => stubbed);
}
