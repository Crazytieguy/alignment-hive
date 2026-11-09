// Size limits on public mutation arguments. Convex's own limits are 1 MiB per document and 8,192
// items per array. These limits are far above anything the hive CLI (any release) or the web app
// sends, so they only turn away malformed or abusive input.
import { ConvexError } from "convex/values";

/**
 * Longest identifier a mutation accepts: a project directory, a git remote, or a session, run or
 * checkout id. Paths stop at PATH_MAX (4,096 bytes on Linux, 1,024 on macOS), and ids are file
 * names (at most 255 bytes) or UUIDs.
 */
export const MAX_IDENTIFIER_LENGTH = 16_384;

/**
 * Most agent ids, and most workflow run ids, in one generateUploadUrls call. The CLI asks for 25 at
 * a time, and the old releases that ask for every agent at once can't save more than about 800.
 */
export const MAX_UPLOAD_URL_IDS = 1_000;

/**
 * Throws if any string in `fields`, or any string in a list in `fields`, is longer than
 * MAX_IDENTIFIER_LENGTH. Other values are skipped, so a mutation can pass its whole args when
 * every string in them is an identifier.
 */
export function assertIdentifierLengths(fields: Record<string, unknown>): void {
  for (const [name, value] of Object.entries(fields)) {
    for (const item of Array.isArray(value) ? value : [value]) {
      if (typeof item === "string" && item.length > MAX_IDENTIFIER_LENGTH) {
        throw new ConvexError(`${name} is longer than ${MAX_IDENTIFIER_LENGTH} characters`);
      }
    }
  }
}

/** Throws if a list argument has more than `max` items. */
export function assertMaxItems(name: string, items: ReadonlyArray<unknown>, max: number): void {
  if (items.length > max) {
    throw new ConvexError(`${name} has more than ${max} items`);
  }
}
