// Synthetic transcripts for the parser's contract tests. Every record is shaped like Claude Code's
// own (parentUuid chains, tool_use/tool_result pairs, compact boundaries with logicalParentUuid,
// queued_command attachments, inline sidechains). Texts are inert; nothing here is ever run.

export interface FixtureFile {
  project: string; // dir under projects/
  path: string; // relative to the project dir
  content: string;
}

export interface Fixture {
  name: string;
  session: string;
  files: Array<FixtureFile>;
  uuid: (label: string) => string; // label -> uuid of that record in the parent file
  line: (label: string) => number; // label -> physical line in the parent file
}

type Rec = Record<string, unknown>;

class Builder {
  records: Array<Rec> = [];
  labels = new Map<string, string>();
  lines = new Map<string, number>();
  private k = 0;
  private clock: number;
  constructor(
    public session: string,
    start: string,
    public cwd = '/fixture/branches',
    public sidechainDefault = false,
    public agentId?: string,
  ) {
    this.clock = Date.parse(start);
  }
  id(label: string): string {
    let u = this.labels.get(label);
    if (!u) {
      u = `${this.session.slice(0, 8)}-0000-4000-8000-${(++this.k).toString(16).padStart(12, '0')}`;
      this.labels.set(label, u);
    }
    return u;
  }
  at(iso: string) {
    this.clock = Date.parse(iso);
    return this;
  }
  private tick(): string {
    this.clock += 5000;
    return new Date(this.clock).toISOString();
  }
  private parent(label: string | null): string | null {
    if (label === null) return null;
    if (label.startsWith('!')) return label.slice(1); // a raw uuid that no record has
    return this.id(label);
  }
  private push(label: string, r: Rec) {
    this.lines.set(label, this.records.length + 1);
    this.records.push(r);
  }
  private common(label: string, parent: string | null, extra: Rec = {}): Rec {
    const r: Rec = {
      parentUuid: this.parent(parent),
      isSidechain: this.sidechainDefault,
      ...(this.agentId ? { agentId: this.agentId } : {}),
      uuid: this.id(label),
      timestamp: this.tick(),
      userType: 'external',
      cwd: this.cwd,
      sessionId: this.session,
      version: '2.1.250',
      gitBranch: 'main',
      ...extra,
    };
    return r;
  }
  user(label: string, parent: string | null, content: unknown, extra: Rec = {}) {
    const base = this.common(label, parent, extra);
    this.push(label, { ...base, type: 'user', message: { role: 'user', content } });
    return this;
  }
  prompt(label: string, parent: string | null, text: string, extra: Rec = {}) {
    return this.user(label, parent, text, { origin: { kind: 'human' }, promptSource: 'typed', ...extra });
  }
  assistant(label: string, parent: string | null, msgId: string, blocks: Array<Rec>, extra: Rec = {}) {
    const base = this.common(label, parent, extra);
    this.push(label, {
      ...base,
      type: 'assistant',
      message: { id: msgId, role: 'assistant', model: 'claude-fixture-1', type: 'message', content: blocks },
      requestId: `req_${msgId}`,
    });
    return this;
  }
  say(label: string, parent: string | null, msgId: string, text: string, extra: Rec = {}) {
    return this.assistant(label, parent, msgId, [{ type: 'text', text }], extra);
  }
  call(label: string, parent: string | null, msgId: string, toolId: string, name: string, input: Rec, extra: Rec = {}) {
    return this.assistant(label, parent, msgId, [{ type: 'tool_use', id: toolId, name, input }], extra);
  }
  result(label: string, parent: string | null, toolId: string, text: string, extra: Rec = {}, toolUseResult?: unknown) {
    const base = this.common(label, parent, extra);
    this.push(label, {
      ...base,
      type: 'user',
      message: {
        role: 'user',
        content: [{ tool_use_id: toolId, type: 'tool_result', content: text, is_error: false }],
      },
      ...(toolUseResult !== undefined ? { toolUseResult } : {}),
    });
    return this;
  }
  system(label: string, parent: string | null, subtype: string, extra: Rec = {}) {
    const base = this.common(label, parent, extra);
    this.push(label, {
      ...base,
      type: 'system',
      subtype,
      content: subtype === 'turn_duration' ? '' : subtype,
      level: 'info',
      isMeta: false,
    });
    return this;
  }
  boundary(label: string, logicalParent: string) {
    const base = this.common(label, null);
    this.push(label, {
      ...base,
      logicalParentUuid: this.parent(logicalParent),
      type: 'system',
      subtype: 'compact_boundary',
      content: 'Conversation compacted',
      isMeta: false,
      level: 'info',
      compactMetadata: { trigger: 'manual', preTokens: 1000 },
    });
    return this;
  }
  compactSummary(label: string, parent: string) {
    return this.user(
      label,
      parent,
      'This session is being continued from a previous conversation that ran out of context. Summary: fixture work so far.',
      { isCompactSummary: true, isVisibleInTranscriptOnly: true },
    );
  }
  queued(label: string, parent: string | null, prompt: string, attachment: Rec = {}) {
    const base = this.common(label, parent);
    this.push(label, {
      ...base,
      type: 'attachment',
      attachment: { type: 'queued_command', prompt, timestamp: base.timestamp, ...attachment },
    });
    return this;
  }
  raw(label: string, r: Rec) {
    this.push(label, r);
    return this;
  }
  text(): string {
    return this.records.map((r) => JSON.stringify(r)).join('\n') + '\n';
  }
}

const BR = '-fixture-branches';
const OV = '-fixture-overlap';

function fixture(name: string, b: Builder, project = BR, extra: Array<FixtureFile> = []): Fixture {
  return {
    name,
    session: b.session,
    files: [{ project, path: `${b.session}.jsonl`, content: b.text() }, ...extra],
    uuid: (l) => {
      const u = b.labels.get(l);
      if (!u) throw new Error(`no label ${l} in ${name}`);
      return u;
    },
    line: (l) => {
      const n = b.lines.get(l);
      if (n === undefined) throw new Error(`no record ${l} in ${name}`);
      return n;
    },
  };
}

const sid = (hex: string) => `${hex}-0000-4000-8000-000000000000`;

export function branchFixtures(): Array<Fixture> {
  const out: Array<Fixture> = [];

  // F1: a new root after earlier history. Nothing is abandoned; the root is a `reset`.
  {
    const b = new Builder(sid('fb000001'), '2026-09-10T10:00:00Z');
    b.prompt('U1', null, 'Sketch a plan for the reading list page.')
      .say('A1', 'U1', 'msg_f1_1', 'Here is a plan with three sections.')
      .system('S1', 'A1', 'turn_duration')
      .prompt('U2', 'S1', 'Make it two sections.')
      .say('A2', 'U2', 'msg_f1_2', 'Two sections, then.')
      .prompt('U3', null, 'Unrelated: what is the capital of Portugal?')
      .say('A3', 'U3', 'msg_f1_3', 'Lisbon.')
      .prompt('U4', 'A3', 'Thanks.')
      .say('A4', 'U4', 'msg_f1_4', 'You are welcome.');
    out.push(fixture('new-root', b));
  }

  // F2: a missing link on the final chain. U3's parent was never written. Nothing is abandoned.
  {
    const b = new Builder(sid('fb000002'), '2026-09-10T11:00:00Z');
    b.prompt('U1', null, 'List the open questions.')
      .say('A1', 'U1', 'msg_f2_1', 'Three open questions.')
      .prompt('U2', 'A1', 'Answer the first.')
      .say('A2', 'U2', 'msg_f2_2', 'The first one resolves to yes.')
      .prompt('U3', `!${b.session.slice(0, 8)}-dead-4000-8000-00000000000a`, 'And the second?')
      .say('A3', 'U3', 'msg_f2_3', 'The second is still open.')
      .prompt('U4', 'A3', 'OK.')
      .say('A4', 'U4', 'msg_f2_4', 'Noted.');
    out.push(fixture('missing-final', b));
  }

  // F3: a missing link on the abandoned walk. R rewinds to A1; walking back from A3 hits TR2,
  // whose parent is absent. A3 is `unknown`; U2 and A2 are not reached and stay current.
  {
    const b = new Builder(sid('fb000003'), '2026-09-10T12:00:00Z');
    b.prompt('U1', null, 'Check the word count of the draft.')
      .say('A1', 'U1', 'msg_f3_1', 'Which draft?')
      .prompt('U2', 'A1', 'The newest one.')
      .call('A2', 'U2', 'msg_f3_2', 'toolu_f3_2', 'Bash', { command: 'wc -w draft.md', description: 'Count words' })
      .result('TR2', `!${b.session.slice(0, 8)}-dead-4000-8000-00000000000b`, 'toolu_f3_2', '812 draft.md')
      .say('A3', 'TR2', 'msg_f3_3', 'It has 812 words.')
      .prompt('R', 'A1', 'The one in the notes folder.')
      .say('A4', 'R', 'msg_f3_4', 'Looking in the notes folder.');
    out.push(fixture('missing-walk', b));
  }

  // F4: a rewind across a compaction whose logicalParentUuid is usable: abandoned, boundary and
  // summary included.
  {
    const b = new Builder(sid('fb000004'), '2026-09-10T13:00:00Z');
    b.prompt('U1', null, 'Start the migration notes.')
      .say('A1', 'U1', 'msg_f4_1', 'Started.')
      .prompt('U2', 'A1', 'Add the schema section.')
      .say('A2', 'U2', 'msg_f4_2', 'Added the schema section.')
      .boundary('B', 'A2')
      .compactSummary('S', 'B')
      .prompt('U3', 'S', 'Now the rollback section.')
      .say('A3', 'U3', 'msg_f4_3', 'Added the rollback section.')
      .prompt('R', 'A1', 'Actually start with rollback, skip schema.')
      .say('A4', 'R', 'msg_f4_4', 'Rollback first, then.');
    out.push(fixture('compact-usable', b));
  }

  // F5: the same, but the boundary's logicalParentUuid target is absent: unknown.
  {
    const b = new Builder(sid('fb000005'), '2026-09-10T14:00:00Z');
    b.prompt('U1', null, 'Start the migration notes.')
      .say('A1', 'U1', 'msg_f5_1', 'Started.')
      .prompt('U2', 'A1', 'Add the schema section.')
      .say('A2', 'U2', 'msg_f5_2', 'Added the schema section.')
      .boundary('B', `!${b.session.slice(0, 8)}-dead-4000-8000-00000000000c`)
      .compactSummary('S', 'B')
      .prompt('U3', 'S', 'Now the rollback section.')
      .say('A3', 'U3', 'msg_f5_3', 'Added the rollback section.')
      .prompt('R', 'A1', 'Actually start with rollback, skip schema.')
      .say('A4', 'R', 'msg_f5_4', 'Rollback first, then.');
    out.push(fixture('compact-absent', b));
  }

  // F6: a message.id reused non-contiguously is not one node (no false suppression), and a
  // contiguous response with parallel calls is one node (no false transition).
  {
    const b = new Builder(sid('fb000006'), '2026-09-10T15:00:00Z');
    b.prompt('U1', null, 'Name three colors.')
      .say('A1', 'U1', 'msg_f6_reused', 'Red, green, blue.')
      .prompt('U2', 'A1', 'Now three more.')
      .say('A2', 'U2', 'msg_f6_2', 'Let me think.')
      .say('A3', 'A2', 'msg_f6_reused', 'Cyan, magenta, yellow.')
      .prompt('R', 'A1', 'Instead, three shades of red.')
      .say('A4', 'R', 'msg_f6_4', 'Crimson, scarlet, maroon.')
      .prompt('U5', 'A4', 'Look up both files.')
      .call('A5', 'U5', 'msg_f6_par', 'toolu_f6_a', 'Read', { file_path: '/fixture/a.txt' })
      .call('A6', 'A5', 'msg_f6_par', 'toolu_f6_b', 'Read', { file_path: '/fixture/b.txt' })
      .result('TRa', 'A5', 'toolu_f6_a', 'alpha')
      .result('TRb', 'A6', 'toolu_f6_b', 'beta')
      .prompt('U6', 'TRa', 'Which is longer?')
      .say('A7', 'U6', 'msg_f6_7', 'They are the same length.');
    out.push(fixture('message-id-reuse', b));
  }

  // F7: parallel tool calls and sibling blocks of one record: no transition.
  {
    const b = new Builder(sid('fb000007'), '2026-09-10T16:00:00Z');
    b.prompt('U1', null, 'Check both config files.')
      .assistant('A1', 'U1', 'msg_f7_1', [
        { type: 'thinking', thinking: 'Two files; read the first, then both in parallel.', signature: 'sig' },
        { type: 'text', text: 'Reading the first file.' },
        { type: 'tool_use', id: 'toolu_f7_1', name: 'Read', input: { file_path: '/fixture/one.json' } },
      ])
      .result('TR1', 'A1', 'toolu_f7_1', '{"a":1}')
      .call('A2', 'TR1', 'msg_f7_par', 'toolu_f7_a', 'Read', { file_path: '/fixture/two.json' })
      .call('A3', 'A2', 'msg_f7_par', 'toolu_f7_b', 'Read', { file_path: '/fixture/three.json' })
      .result('TRa', 'A3', 'toolu_f7_a', '{"b":2}')
      .result('TRb', 'TRa', 'toolu_f7_b', '{"c":3}')
      .say('A4', 'TRb', 'msg_f7_4', 'All three parse.')
      .system('S', 'A4', 'turn_duration')
      .prompt('U2', 'S', 'Good. Anything odd?')
      .say('A5', 'U2', 'msg_f7_5', 'Nothing odd.');
    out.push(fixture('parallel-siblings', b));
  }

  // F8: inline sidechain records in a parent file (Dec 2025 to Jan 2026 layout). They form their
  // own chain; skipping them means no false transition and no reset.
  {
    const b = new Builder(sid('fb000008'), '2026-01-10T10:00:00Z');
    const sc = { isSidechain: true, agentId: 'fb08aa01' };
    b.prompt('U1', null, 'Research the release dates.')
      .say('A1', 'U1', 'msg_f8_1', 'Delegating the lookup.')
      .user('SU1', null, 'Find the release dates of the three versions.', sc)
      .say('SA1', 'SU1', 'msg_f8_s1', 'Version 1 shipped in March.', sc)
      .prompt('U2', 'A1', 'While that runs, draft the intro.')
      .say('A2', 'U2', 'msg_f8_2', 'Intro drafted.')
      .user('SU2', 'SA1', 'Continue with versions 2 and 3.', sc)
      .say('SA2', 'SU2', 'msg_f8_s2', 'Version 2 in June, version 3 in August.', sc)
      .prompt('U3', 'A2', 'Merge the dates in.')
      .say('A3', 'U3', 'msg_f8_3', 'Merged.');
    out.push(fixture('inline-sidechain', b));
  }

  // F9: a queued message inside an abandoned run (abandoned) and one after the transition
  // (current); plus an untagged agent message (origin peer) and a queued task notification.
  {
    const b = new Builder(sid('fb000009'), '2026-09-11T10:00:00Z');
    b.prompt('U1', null, 'Tidy the changelog.')
      .say('A1', 'U1', 'msg_f9_1', 'Which release?')
      .prompt('U2', 'A1', 'The last one.')
      .call('A2', 'U2', 'msg_f9_2', 'toolu_f9_2', 'Read', { file_path: '/fixture/CHANGELOG.md' })
      .result('TR2', 'A2', 'toolu_f9_2', '## 0.4.1\n- fixed a typo')
      .queued('QC1', 'TR2', 'also sort the entries by date', { commandMode: 'prompt', origin: { kind: 'human' } })
      .say('A3', 'QC1', 'msg_f9_3', 'Sorted and tidied 0.4.1.')
      .prompt('R', 'A1', 'The one before the last.')
      .call('A4', 'R', 'msg_f9_4', 'toolu_f9_4', 'Read', { file_path: '/fixture/CHANGELOG.md' })
      .result('TR4', 'A4', 'toolu_f9_4', '## 0.4.0\n- new export')
      .queued('QC2', 'TR4', 'keep the headings as they are', { commandMode: 'prompt', origin: { kind: 'human' } })
      .say('A5', 'QC2', 'msg_f9_5', 'Tidied 0.4.0; headings unchanged.')
      .queued('QC3', 'A5', '<agent-message from="afb09aa01">\nThe export note is out of date.\n</agent-message>', {
        commandMode: 'prompt',
      })
      .queued(
        'QC4',
        'QC3',
        '<task-notification>\n<task-id>bfixture9</task-id>\n<status>completed</status>\n</task-notification>',
        { commandMode: 'task-notification' },
      );
    out.push(fixture('queued', b));
  }

  // F10: a return to an abandoned branch. R1 rewinds to A1 (U2, A2 abandoned); U4 then continues
  // from A2, on the branch R1 left. The returned-to entries are current again; the branch left by
  // U4 forks from A1, an ancestor of U4, so it is abandoned.
  {
    const b = new Builder(sid('fb00000a'), '2026-09-11T11:00:00Z');
    b.prompt('U1', null, 'Pick a name for the script.')
      .say('A1', 'U1', 'msg_fa_1', 'How about sync-notes?')
      .prompt('U2', 'A1', 'Shorter.')
      .say('A2', 'U2', 'msg_fa_2', 'sn?')
      .prompt('R1', 'A1', 'Something about backups instead.')
      .say('A3', 'R1', 'msg_fa_3', 'backup-notes?')
      .prompt('U4', 'A2', 'sn is fine after all, go with it.')
      .say('A4', 'U4', 'msg_fa_4', 'Going with sn.');
    out.push(fixture('return', b));
  }

  // F11: a plain edit (rewind through system records), a resend after an interrupt (no
  // transition), and an agent started by a call in the abandoned run, with its own transcript.
  {
    const s = sid('fb00000b');
    const agent = 'afb00000b00000a11';
    const b = new Builder(s, '2026-09-11T12:00:00Z');
    b.prompt('U1', null, 'Summarize the incident notes.')
      .say('A1', 'U1', 'msg_fb_1', 'The outage lasted 40 minutes.')
      .system('S1', 'A1', 'turn_duration')
      .prompt('U2', 'S1', 'Have an agent check the timeline.')
      .call('A2', 'U2', 'msg_fb_2', 'toolu_fb_2', 'Agent', {
        description: 'Check timeline',
        prompt: 'Check the incident timeline.',
        subagent_type: 'general-purpose',
      })
      .result(
        'TR2',
        'A2',
        'toolu_fb_2',
        'The timeline is consistent.',
        {},
        { status: 'completed', agentId: agent, content: [] },
      )
      .say('A3', 'TR2', 'msg_fb_3', 'The agent found the timeline consistent.')
      .system('S3', 'A3', 'turn_duration')
      .prompt('R', 'S1', 'Check the timeline yourself, no agent.')
      .call('A4', 'R', 'msg_fb_4', 'toolu_fb_4', 'Bash', {
        command: 'cat notes/timeline.md',
        description: 'Read timeline',
      })
      .result('TR4', 'A4', 'toolu_fb_4', '10:02 alert\n10:42 recovered')
      .user('INT', 'TR4', [{ type: 'text', text: '[Request interrupted by user for tool use]' }])
      .prompt('U5', 'INT', 'Just tell me the recovery time.')
      .say('A5', 'U5', 'msg_fb_5', '10:42.');
    const ab = new Builder(s, '2026-09-11T12:00:30Z', '/fixture/branches', true, agent);
    ab.user('AU1', null, 'Check the incident timeline.')
      .call('AA1', 'AU1', 'msg_fb_a1', 'toolu_fb_a1', 'Bash', {
        command: 'cat notes/timeline.md',
        description: 'Read timeline',
      })
      .result('ATR1', 'AA1', 'toolu_fb_a1', '10:02 alert\n10:42 recovered')
      .say('AA2', 'ATR1', 'msg_fb_a2', 'The timeline is consistent.');
    const meta = {
      agentType: 'general-purpose',
      description: 'Check timeline',
      toolUseId: 'toolu_fb_2',
      spawnDepth: 1,
    };
    out.push(
      fixture('edit-interrupt-agent', b, BR, [
        { project: BR, path: `${s}/subagents/agent-${agent}.jsonl`, content: ab.text() },
        { project: BR, path: `${s}/subagents/agent-${agent}.meta.json`, content: JSON.stringify(meta) + '\n' },
      ]),
    );
  }
  // F12: a rewind to the start. The first prompt and R both hang off the SessionStart hook, so R's
  // parent resolves to a root; its line meets Q's at the hook, so everything before R is abandoned.
  {
    const b = new Builder(sid('fb00000c'), '2026-09-11T13:00:00Z');
    b.raw('H', { type: 'attachment', uuid: b.id('H'), parentUuid: null, attachment: { type: 'hook_success' } })
      .prompt('U1', 'H', 'Draft the release note.')
      .say('A1', 'U1', 'msg_fc_1', 'Drafted.')
      .prompt('U2', 'A1', 'Make it shorter.')
      .say('A2', 'U2', 'msg_fc_2', 'Shortened.')
      .prompt('R', 'H', 'Actually, draft the changelog entry instead.')
      .say('A3', 'R', 'msg_fc_3', 'Changelog entry drafted.');
    out.push(fixture('rewind-to-start', b));
  }

  // F13: a resume copy. Resuming rewrote U2 with its original timestamp and text under A1; it is a
  // copy, not an edit, so nothing is abandoned.
  {
    const b = new Builder(sid('fb00000d'), '2026-09-11T14:00:00Z');
    b.prompt('U1', null, 'Count the open issues.')
      .say('A1', 'U1', 'msg_fd_1', 'There are 12.')
      .prompt('U2', 'A1', 'Which are bugs?');
    const u2 = b.records.at(-1)!;
    b.say('A2', 'U2', 'msg_fd_2', 'Five of them.').raw('C', { ...u2, uuid: b.id('C'), parentUuid: b.id('A1') });
    out.push(fixture('resume-copy', b));
  }

  // F14: a command-only walk. /exit and its output sit between A1 and R, which continues from A1:
  // plumbing, not an edit.
  {
    const b = new Builder(sid('fb00000e'), '2026-09-11T15:00:00Z');
    b.prompt('U1', null, 'Summarize the plan.')
      .say('A1', 'U1', 'msg_fe_1', 'Three steps.')
      .user(
        'CMD',
        'A1',
        '<command-name>/exit</command-name>\n<command-message>exit</command-message>\n<command-args></command-args>',
      )
      .user('OUT', 'CMD', '<local-command-stdout>Goodbye!</local-command-stdout>')
      .prompt('R', 'A1', 'Go on with step one.')
      .say('A2', 'R', 'msg_fe_2', 'Step one done.');
    out.push(fixture('command-only', b));
  }

  // F15: a rewind right after a compaction. R is the first message after the boundary and continues
  // from A1, before it; Q is the last conversational record before the boundary.
  {
    const b = new Builder(sid('fb00000f'), '2026-09-11T16:00:00Z');
    b.prompt('U1', null, 'Start the migration notes.')
      .say('A1', 'U1', 'msg_ff_1', 'Started.')
      .prompt('U2', 'A1', 'Add the schema section.')
      .say('A2', 'U2', 'msg_ff_2', 'Added the schema section.')
      .boundary('B', 'A2')
      .compactSummary('S', 'B')
      .prompt('R', 'A1', 'Skip schema; start with rollback.')
      .say('A3', 'R', 'msg_ff_3', 'Rollback first.');
    out.push(fixture('after-compaction', b));
  }
  // F16 (held-out H20): the run is one task notification. An agent's notice arrives after the call
  // that started it; the interrupt that follows continues from the call, so the notice is left
  // behind. The noise rules hide it, so the run is plumbing, not an edit.
  {
    const b = new Builder(sid('fb000010'), '2026-09-11T17:00:00Z');
    b.prompt('U1', null, 'Have an agent audit the docs.')
      .call('A1', 'U1', 'msg_f10_1', 'toolu_f10_1', 'Agent', { description: 'Audit docs', prompt: 'Audit the docs.' })
      .user(
        'TN',
        'A1',
        '<task-notification>\n<task-id>afb0010</task-id>\n<status>completed</status>\n</task-notification>',
        {
          origin: { kind: 'task-notification' },
        },
      )
      .user('INT', 'A1', [{ type: 'text', text: '[Request interrupted by user for tool use]' }])
      .prompt('U2', 'INT', 'Summarize what it found.')
      .say('A2', 'U2', 'msg_f10_2', 'Two stale links.');
    out.push(fixture('task-notification-run', b));
  }
  return out;
}

// -fixture-overlap: the session that ended last holds older prompts than the next one (Q3 question 6).
export function overlapFixtures(): Array<Fixture> {
  const cwd = '/fixture/overlap';
  const out: Array<Fixture> = [];
  const turn = (b: Builder, n: string, text: string, at: string, reply: string, prev: string | null) => {
    b.at(at)
      .prompt(`P${n}`, prev, text)
      .say(`R${n}`, `P${n}`, `msg_${b.session.slice(0, 8)}_${n}`, reply);
    return `R${n}`;
  };

  // A: ends last (Sep 14) but its prompts are the oldest of A-C (Sep 10).
  {
    const b = new Builder(sid('fc00000a'), '2026-09-10T09:00:00Z', cwd);
    let p: string | null = null;
    p = turn(
      b,
      '1',
      'A1: plan the garden beds for the north fence',
      '2026-09-10T16:00:00Z',
      'Three beds along the fence.',
      p,
    );
    p = turn(
      b,
      '2',
      'A2: add a compost corner to the plan',
      '2026-09-10T16:10:00Z',
      'Compost goes in the shady corner.',
      p,
    );
    p = turn(
      b,
      '3',
      'A3: start the soil test script in the background',
      '2026-09-10T16:20:00Z',
      'Started; it will report when done.',
      p,
    );
    b.at('2026-09-14T18:00:00Z')
      .user(
        'TN',
        p,
        '<task-notification>\n<task-id>bsoiltest</task-id>\n<status>completed</status>\n</task-notification>',
        { origin: { kind: 'task-notification' } },
      )
      .say('RTN', 'TN', 'msg_fc0a_tn', 'The soil test finished: pH 6.5 in all beds.');
    b.raw('T', { type: 'custom-title', customTitle: 'garden beds plan', sessionId: b.session });
    out.push(fixture('overlap-A', b, OV));
  }
  // B: ends Sep 13 and holds the four newest typed prompts; its last rows are an interrupt and a
  // peer message, which are not typed prompts.
  {
    const b = new Builder(sid('fc00000b'), '2026-09-12T17:00:00Z', cwd);
    let p: string | null = null;
    p = turn(b, '1', 'B1: price out raised-bed kits', '2026-09-11T21:00:00Z', 'Kits run 80 to 140 dollars.', p);
    p = turn(
      b,
      '2',
      'B2: compare cedar and steel beds',
      '2026-09-12T22:00:00Z',
      'Cedar is cheaper; steel lasts longer.',
      p,
    );
    b.at('2026-09-13T15:59:00Z')
      .prompt('P3a', p, 'B3a: build a comparison table of the three cheapest kits')
      .call('C3', 'P3a', 'msg_fc0b_c3', 'toolu_fc0b_3', 'WebSearch', { query: 'raised bed kit prices' });
    b.at('2026-09-13T15:59:55Z').result('TR3', 'C3', 'toolu_fc0b_3', 'three results');
    b.at('2026-09-13T16:00:00Z').queued('P3', 'TR3', 'B3: include shipping costs in the table', {
      commandMode: 'prompt',
      origin: { kind: 'human' },
    });
    b.say('R3', 'P3', 'msg_fc0b_r3', 'Table built, shipping included.');
    p = turn(b, '4', 'B4: order the cedar kit', '2026-09-13T18:00:00Z', 'Ordering the cedar kit.', 'R3');
    b.at('2026-09-13T18:05:00Z')
      .call('C5', p, 'msg_fc0b_c5', 'toolu_fc0b_5', 'Bash', {
        command: 'echo order-placeholder',
        description: 'Placeholder order step',
      })
      .result('TR5', 'C5', 'toolu_fc0b_5', 'order-placeholder')
      .user('INT', 'TR5', [{ type: 'text', text: '[Request interrupted by user]' }])
      .user(
        'PEER',
        'INT',
        'Another Claude session sent a message:\n<agent-message from="afc0b0peer0000001">\nThe cedar kit is back-ordered until October.\n</agent-message>',
        { isMeta: true, origin: { kind: 'peer' } },
      )
      .say('R6', 'PEER', 'msg_fc0b_r6', 'Noted: the cedar kit is back-ordered.');
    out.push(fixture('overlap-B', b, OV));
  }
  // C: ends Sep 11; its newest prompt is the fifth newest overall.
  {
    const b = new Builder(sid('fc00000c'), '2026-09-11T15:00:00Z', cwd);
    let p: string | null = null;
    p = turn(b, '1', 'C1: list shade-tolerant vegetables', '2026-09-11T15:00:00Z', 'Lettuce, chard, peas.', p);
    p = turn(b, '2', 'C2: which of those grow in pots', '2026-09-11T19:00:00Z', 'All three do.', p);
    p = turn(b, '3', 'C3: make a planting calendar for them', '2026-09-12T03:00:00Z', 'Calendar drafted.', p);
    out.push(fixture('overlap-C', b, OV));
  }
  // D: ends Sep 9, before the fifth newest prompt: a reader following the stopping rule never opens it.
  {
    const b = new Builder(sid('fc00000d'), '2026-09-09T15:00:00Z', cwd);
    let p: string | null = null;
    p = turn(b, '1', 'D1: measure the yard', '2026-09-09T15:00:00Z', 'About 12 by 20 meters.', p);
    p = turn(b, '2', 'D2: sketch the yard to scale', '2026-09-09T16:00:00Z', 'Sketch saved.', p);
    out.push(fixture('overlap-D', b, OV));
  }
  return out;
}

// Plumbing records in the parent chain: a hook attachment and a progress record, which uploads
// drop. R rewinds to A1 through the hook; locally U2..A3 are abandoned.
export function plumbingFixture(): Fixture {
  const b = new Builder(sid('fd000001'), '2026-09-12T10:00:00Z');
  b.prompt('U1', null, 'Rename the helper.')
    .say('A1', 'U1', 'msg_fd1_1', 'Which helper?')
    .raw('H1', {
      type: 'attachment',
      uuid: b.id('H1'),
      parentUuid: b.id('A1'),
      attachment: { type: 'hook_success', hookName: 'Stop' },
    })
    .prompt('U2', 'H1', 'The date formatter.')
    .call('A2', 'U2', 'msg_fd1_2', 'toolu_fd1_2', 'Grep', { pattern: 'formatDate' })
    .raw('P1', { type: 'progress', uuid: b.id('P1'), parentUuid: b.id('A2'), data: { type: 'hook_progress' } })
    .result('TR2', 'P1', 'toolu_fd1_2', 'src/date.ts')
    .say('A3', 'TR2', 'msg_fd1_3', 'Found it in src/date.ts.')
    .prompt('R', 'H1', 'The number formatter, not the date one.')
    .say('A4', 'R', 'msg_fd1_4', 'Looking for the number formatter.');
  return fixture('plumbing', b);
}

// One record for each cause that changed entry numbers at the switch from the old parser: legacy
// summaries, multi-block assistant records, assistant records with only unknown blocks, records the
// old schema rejected, queued messages, and user records whose string content is one reminder. An
// image-only message numbers the same in both.
export function migrationFixture(): Fixture {
  const b = new Builder(sid('fe000001'), '2026-09-13T10:00:00Z');
  b.raw('SUM1', { type: 'summary', summary: 'Earlier title', leafUuid: 'x' })
    .prompt('U1', null, 'Check the config.')
    .assistant('A1', 'U1', 'msg_fe1_1', [
      { type: 'thinking', thinking: 'Read it first.', signature: 's' },
      { type: 'text', text: 'Reading the config.' },
      { type: 'tool_use', id: 'toolu_fe1_1', name: 'Read', input: { file_path: '/fixture/config.json' } },
    ])
    .result('TR1', 'A1', 'toolu_fe1_1', '{"debug":false}')
    .assistant('A2', 'TR1', 'msg_fe1_2', [{ type: 'server_tool_use', id: 'srvtoolu_fe1', name: 'web_search' }])
    .raw('U2', {
      type: 'user',
      uuid: b.id('U2'),
      parentUuid: b.id('A2'),
      message: { role: 'user', content: 'no timestamp here' },
    })
    .queued('QC', 'U2', 'and the env file too', { commandMode: 'prompt', origin: { kind: 'human' } })
    .user('IMG', 'QC', [{ type: 'image', source: { type: 'base64', media_type: 'image/png', data: 'iVBOR' } }])
    .say('A3', 'IMG', 'msg_fe1_3', 'Both look fine.')
    .raw('SUM2', { type: 'summary', summary: 'Later title', leafUuid: 'x' })
    .user('REM', 'A3', '<system-reminder>\nThe agent handed control back.\n</system-reminder>', { isMeta: true })
    .system('S', 'REM', 'turn_duration');
  return fixture('migration', b);
}
