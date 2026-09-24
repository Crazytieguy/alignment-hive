import { existsSync } from 'node:fs';
import { mkdir } from 'node:fs/promises';
import { isAbsolute, join, resolve } from 'node:path';
import { parseArgs } from 'node:util';
import { debriefRoundDir } from '../lib/config';
import { reviewCliMessages } from '../lib/messages';
import { captureReview } from '../debrief/capture';
import { reviewPreflight } from '../debrief/preflight';
import { sessionIdSchema } from '../debrief/parse';
import { renderReview } from '../debrief/render';

const helpOptions = {
  help: { type: 'boolean' as const, short: 'h' },
};
const roundOptions = { session: { type: 'string' as const }, round: { type: 'string' as const }, data: { type: 'string' as const } };

/** The round directory `--session`, `--round` and optional `--data` name, or undefined when none is given. */
function roundDir(values: { session?: string; round?: string; data?: string }, usage: string): { dir: string; round: number } | undefined {
  if (values.session === undefined && values.round === undefined && values.data === undefined) return undefined;
  if (values.session === undefined || values.round === undefined) throw new Error(usage);
  if (!sessionIdSchema.safeParse(values.session.toLowerCase()).success) throw new Error(reviewCliMessages.invalidSession);
  const round = /^[1-9]\d{0,5}$/.test(values.round) ? Number(values.round) : NaN;
  if (!Number.isSafeInteger(round)) throw new Error(reviewCliMessages.invalidRound);
  // An unsubstituted `${CLAUDE_PLUGIN_DATA}` or an empty value must not become a directory under the cwd.
  if (values.data !== undefined && (!isAbsolute(values.data) || values.data.includes('${'))) throw new Error(reviewCliMessages.invalidData);
  return { dir: debriefRoundDir(process.cwd(), values.session.toLowerCase(), round, values.data), round };
}

export async function reviewCommand(args: Array<string>): Promise<number> {
  const [verb, ...rest] = args;
  if (!verb || ['help', '--help', '-h'].includes(verb)) {
    console.log(reviewCliMessages.usage);
    return verb ? 0 : 2;
  }
  switch (verb) {
    case 'dir': {
      const { values, positionals } = parseArgs({ args: rest, options: { ...helpOptions, ...roundOptions }, strict: true, allowPositionals: true });
      if (values.help) {
        console.log(reviewCliMessages.dirUsage);
        return 0;
      }
      const named = positionals.length ? undefined : roundDir(values, reviewCliMessages.dirUsage);
      if (!named) throw new Error(reviewCliMessages.dirUsage);
      await mkdir(named.dir, { recursive: true });
      console.log(named.dir);
      return 0;
    }
    case 'render': {
      const { values, positionals } = parseArgs({
        args: rest,
        options: { ...helpOptions, ...roundOptions, out: { type: 'string' }, prev: { type: 'string' } },
        strict: true,
        allowPositionals: true,
      });
      if (values.help) {
        console.log(reviewCliMessages.renderUsage);
        return 0;
      }
      const named = roundDir(values, reviewCliMessages.renderUsage);
      if (named ? positionals.length > 0 || values.out !== undefined : positionals.length !== 1 || !values.out) throw new Error(reviewCliMessages.renderUsage);
      const outDir = named ? named.dir : resolve(values.out!);
      // A named round reads the previous round's manifest beside it unless --prev says otherwise.
      const previous = named && named.round > 1 ? join(named.dir, '..', `round-${named.round - 1}`, 'manifest.json') : undefined;
      const result = await renderReview(named ? join(named.dir, 'debrief.md') : resolve(positionals[0]), {
        outDir,
        previousManifestPath: values.prev ? resolve(values.prev) : previous && existsSync(previous) ? previous : undefined,
      });
      for (const warning of result.warnings) console.error(warning);
      if (result.coverage?.changedFiles) console.error(reviewCliMessages.coverage(result.coverage));
      console.log(join(outDir, 'page.html'));
      return 0;
    }
    case 'capture': {
      const separator = rest.indexOf('--');
      const { values, positionals } = parseArgs({
        args: separator < 0 ? rest : rest.slice(0, separator),
        options: { ...helpOptions, ...roundOptions, name: { type: 'string' }, out: { type: 'string' } },
        strict: true,
        allowPositionals: true,
      });
      if (values.help) {
        console.log(reviewCliMessages.captureUsage);
        return 0;
      }
      const named = roundDir(values, reviewCliMessages.captureUsage);
      if (separator < 0 || !values.name || positionals.length || separator === rest.length - 1 || (named && values.out !== undefined)) {
        throw new Error(reviewCliMessages.captureUsage);
      }
      const result = await captureReview({
        name: values.name,
        command: rest.slice(separator + 1),
        cwd: process.cwd(),
        outDir: named ? named.dir : values.out ? resolve(values.out) : process.cwd(),
      });
      console.log(result.path);
      return result.capture.exit;
    }
    case 'preflight': {
      const { values, positionals } = parseArgs({
        args: rest,
        options: { ...helpOptions, min: { type: 'string' }, session: { type: 'string' } },
        strict: true,
        allowPositionals: true,
      });
      if (values.help) {
        console.log(reviewCliMessages.preflightUsage);
        return 0;
      }
      if (positionals.length || !values.min || !values.session) {
        throw new Error(reviewCliMessages.preflightUsage);
      }
      const { repository } = await reviewPreflight({ minimumVersion: values.min, session: values.session });
      console.log(reviewCliMessages.preflightOk);
      if (!repository) console.log(reviewCliMessages.preflightNoGit);
      return 0;
    }
    default:
      throw new Error(reviewCliMessages.usage);
  }
}
