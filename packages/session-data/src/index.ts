/**
 * The one transcript parser, shared by the CLI and the web app, plus the upload, consent and status
 * rules both sides must agree on. Layers: records.ts reads JSONL lines into records typed by `type`
 * only; transcript.ts turns records into numbered entries (tool results joined, titles, branch
 * states); noise.ts hides or selects entries without renumbering; upload.ts decides what of each
 * record an upload keeps.
 */
export * from './records';
export * from './transcript';
export * from './noise';
export * from './summary';
export * from './upload';
export * from './schemas';
export * from './consent-windows';
export * from './workflow-run';
export * from './session-status';
