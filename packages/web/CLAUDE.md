@README.md

## Convex Dev Server

Push backend functions to the dev deployment:

```bash
bun run --filter '@alignment-hive/web' dev:backend
```

This runs `convex dev` which watches for changes and pushes them. Use it to deploy schema changes, new functions, and migrations. Stop it when done.

## Convex Query Design

Make one query per frontend use case, not composable queries. Convex queries are reactive and should return exactly what a component needs.

## Session Data Access Control (CRITICAL)

Session data is privacy-sensitive. Access is enforced at three layers — all three MUST agree before data is exposed:

1. **CLI upload** (`packages/hive-cli/src/commands/hive-upload.ts`): Checks consent windows before uploading. Prevents sessions from revocation gaps from being uploaded.
2. **Backend mutations** (`convex/sessions.ts` → `verifyConsent()`): Checks both current consent state AND consent windows (when `lastModified` is provided). Rejects writes outside consent windows.
3. **Backend read queries** (`convex/authorized.ts` → `buildConsentFilter()` from `convex/lib/consentVisibility.ts`): Filters sessions based on consent windows.

### Authorization model

- **Data accessor** (`hasDataAccess: true` on users table + signed current data accessor agreement): Access only to sessions within consent windows.
- **Regular user**: No cross-user session access.

All queries in `convex/authorized.ts` MUST call `requireAuthorized()` (from `convex/lib/auth.ts`) and apply `buildConsentFilter()`.

### Data accessor agreement

Users with `hasDataAccess` must sign the current version of the data accessor agreement (`CURRENT_AGREEMENT_VERSION` in `convex/lib/agreement.ts`) before accessing any data. The agreement is stored in the `dataAccessorAgreements` table. The `getAccessList` query in `consent.ts` only shows users who have both `hasDataAccess` and a valid agreement.

### userId migration (phase 1 — in progress)

Sessions are migrating from `userId` (workosId string) to `userDocId` (Convex `Id<"users">`). During the transition:
- Both fields exist, `userId` is optional, `userDocId` is optional
- Write paths populate both fields
- Read paths check `userDocId` first, fall back to `userId` lookup
- Run `migrations.backfillUserDocId` after deploy to backfill existing sessions
- Phase 2 (follow-up): drop `userId`, make `userDocId` required

### Consent windows

Defined in `packages/session-data/src/consent-windows.ts` (shared between CLI and backend). A session is visible only if it has been uploaded AND its timestamp falls within a consent window for BOTH the global and project consent layers. The timestamp used is `lastModified` (raw file mtime), falling back to `upload.uploadedAt` for sessions that predate the `lastModified` field. First consent is retroactive (window starts at 0). Subsequent consents start at their time. Revocations close windows.

### Child sessions

Child (agent) sessions inherit their parent's consent — they are never checked individually. See `uploadParentWithAgents` (CLI) and `generateUploadUrls`/`saveUploads` (backend) for the implementation and rationale.

### Rules for modifying this code

- **Never add a new public query/mutation in `convex/` that returns cross-user session data without calling `requireAuthorized()` and applying `buildConsentFilter()`.** Internal queries/mutations/actions are exempt (not externally callable).
- **Never bypass `verifyConsent()` in session write mutations.**
- **Never filter sessions after Convex pagination** — this breaks page sizes. Use `stream().filterWith()` from `convex-helpers/server/stream` for pre-pagination filtering.
- If adding a new way to access session content (API endpoint, download, etc.), it must go through the same authorization and consent checks.
