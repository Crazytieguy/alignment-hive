import { formatRemaining } from '@alignment-hive/session-data';
import { parseCommandArgs, usageError } from '../lib/args';
import { ensureStateDir, getStateDir } from '../lib/config';
import { hive } from '../lib/messages';
import {
  UPLOAD_DELAY_MINUTES,
  alignDue,
  loadSharingSnapshot,
  sessionShare,
  summarizeUploads,
  uploadScheduledAt,
} from '../lib/sharing';
import type { SharingSnapshot } from '../lib/sharing';

/**
 * A row of the hive plugin's band above the prompt; the plugin's `hooks/notice-band.tsx`
 * declares the same shape. `plugin` actions are the band's own: `snooze`, `review` and
 * `keep-private`. `when`: `start` rows show until the person's first message (the upload
 * decisions), `working` rows after it (this session's own state); without it, throughout.
 */
export type Notice = {
  id: string;
  severity: 'urgent' | 'problem' | 'action' | 'info';
  text: string;
  actions?: Array<{ id: string; label: string; kind: 'command' | 'copy' | 'plugin'; command?: string }>;
  when?: 'start' | 'working';
};

const copy = hive.notices;

/**
 * The rows, and whether a later call could say more about this session: `isSessionKnown` is false
 * while sharing is on and the session is not discovered yet (no assistant reply), so the band
 * asks again after the next turn rather than on its slow timer.
 *
 * `isSessionOnly`: the band is past the start, where the `start` rows are not shown, so only this
 * session's files are discovered and the project-wide upload rows are left out. A full discovery
 * lists every session's subagents and is most of a full call's time.
 */
async function notices(
  sessionId: string,
  isSessionOnly: boolean,
): Promise<{ notices: Array<Notice>; isSessionKnown: boolean }> {
  const cwd = process.cwd();
  const stateDir = getStateDir(cwd);
  await ensureStateDir(stateDir);
  const rows: Array<Notice> = [];

  const [align, snapshot] = await Promise.all([
    alignDue(stateDir, process.env.HIVE_PLUGIN_VERSION),
    loadSharingSnapshot(stateDir, cwd, isSessionOnly ? sessionId : undefined),
  ]);
  if (align) {
    rows.push({
      id: 'align',
      severity: 'action',
      text: align === 'new' ? copy.alignNew : copy.alignUpdate,
      actions: [{ id: 'align', label: copy.alignAction, kind: 'command', command: '/hive:align' }],
    });
  }

  if (snapshot.kind === 'login-expired') {
    rows.push({
      id: 'login-expired',
      severity: 'problem',
      text: copy.loginExpired,
      actions: [{ id: 'login', label: copy.loginAction, kind: 'copy', command: 'hive login' }],
    });
  }
  if (snapshot.kind !== 'on') return { notices: rows, isSessionKnown: true };

  const share = sessionShare(snapshot, sessionId);
  if (!isSessionOnly) rows.push(...(await uploadRows(snapshot, stateDir)));
  if (share.state !== null) {
    rows.push({
      id: 'session',
      severity: 'info',
      text:
        share.state === 'excludable'
          ? share.isPending
            ? copy.thisSessionPending
            : copy.thisSessionReady
          : copy.thisSession(share.state, share.hadPriorUpload),
      // Partly uploaded sessions cannot be excluded: a state, no button.
      actions: share.state === 'excludable' ? [{ id: 'keep-private', label: copy.keepPrivate, kind: 'plugin' }] : [],
      when: 'working',
    });
  }
  return { notices: rows, isSessionKnown: share.isKnown };
}

/** The project's upload rows, shown until the person's first message: the upload decisions. */
async function uploadRows(snapshot: Extract<SharingSnapshot, { kind: 'on' }>, stateDir: string): Promise<Array<Notice>> {
  const rows: Array<Notice> = [];
  const { eligibleIds, pendingCount, earliestRemainingMs } = summarizeUploads(snapshot);
  const scheduledAt = eligibleIds.length > 0 && !snapshot.snoozeUntil ? await uploadScheduledAt(stateDir) : null;
  const review = { id: 'review', label: copy.review, kind: 'plugin' as const };

  if (scheduledAt !== null) {
    const minutes = Math.max(1, Math.ceil(UPLOAD_DELAY_MINUTES - (Date.now() - scheduledAt) / 60_000));
    rows.push({
      id: 'uploading',
      severity: 'info',
      text: copy.uploading(eligibleIds.length, minutes),
      actions: [{ id: 'snooze', label: copy.snooze, kind: 'plugin' }],
      when: 'start',
    });
  }
  if (eligibleIds.length > 0 && snapshot.snoozeUntil) {
    rows.push({ id: 'snoozed', severity: 'info', text: copy.snoozed(eligibleIds.length), actions: [review], when: 'start' });
  }
  if (pendingCount > 0) {
    rows.push({
      id: 'pending',
      severity: 'info',
      text: copy.pending(pendingCount, formatRemaining(earliestRemainingMs)),
      actions: [review],
      when: 'start',
    });
  }
  return rows;
}

/** The hive plugin's band rows for one session, as JSON: `{ notices, isSessionKnown }`. */
export async function hiveNotices(args: Array<string>): Promise<number> {
  const parsed = parseCommandArgs({ bool: ['--session-only'], value: [] }, args, copy.usage);
  if (typeof parsed === 'number') return parsed;
  if (parsed.positional.length !== 1) return usageError(copy.takesOne, copy.usage);
  console.log(JSON.stringify(await notices(parsed.positional[0], parsed.flags.has('--session-only'))));
  return 0;
}
