// One `hive local` call served by `hive local-server`: while a request is open, what the one-shot CLI reads from its
// process (working directory, CLAUDE_CODE_SESSION_ID, HIVE_LOCAL_CUTOFF) and writes to it (stdout, stderr) comes from
// and goes to the request. The server never changes its own working directory or environment.

import { AsyncLocalStorage } from 'node:async_hooks';

export interface RequestContext {
  cwd: string;
  /** The variables `hive local` reads; one left out is unset for the request, whatever the server's environment holds. */
  env: { CLAUDE_CODE_SESSION_ID?: string; HIVE_LOCAL_CUTOFF?: string };
  stdout: Array<string>;
  stderr: Array<string>;
  /** Resolves when the request may go on; until then other requests run (see pausePoint). */
  pause: () => Promise<void>;
}

const current = new AsyncLocalStorage<RequestContext>();

/** Runs `fn` as the request `context`. */
export function inRequest<T>(context: RequestContext, fn: () => Promise<T>): Promise<T> {
  return current.run(context, fn);
}

export const requestContext = (): RequestContext | undefined => current.getStore();

/** The directory the command runs in. */
export const requestCwd = (): string => current.getStore()?.cwd ?? process.cwd();

export function requestEnv(name: keyof RequestContext['env']): string | undefined {
  const context = current.getStore();
  return context ? context.env[name] : process.env[name];
}

/** A point between units of work (outline's transcripts, a stretch of sessions' files) where other requests may run. */
export const pausePoint = (): Promise<void> | undefined => current.getStore()?.pause();
