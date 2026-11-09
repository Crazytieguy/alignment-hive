// Shared setup for the convex-test suites.
import { dirname, join } from "node:path";
import type { TestConvex } from "convex-test";
import { api } from "../convex/_generated/api";
import type schema from "../convex/schema";

const convexDir = join(dirname(import.meta.dir), "convex");
export const modules = Object.fromEntries(
  [...new Bun.Glob("**/*.{ts,js}").scanSync(convexDir)].map((path) => [
    `../convex/${path}`,
    () => import(join(convexDir, path)),
  ]),
);

export const directory = "/project";
export const consent = {
  sessionSharing: true,
  communityFeatures: false,
  publicationExcerpts: false,
  creditByName: false,
} as const;

/** A signed-in user who has consented to sharing, and to sharing `project` (default `directory`). */
export async function consentingUser(
  t: TestConvex<typeof schema>,
  subject: string,
  project: { directory: string; gitRemote?: string } = { directory },
) {
  const user = t.withIdentity({ subject, email: `${subject}@example.com` });
  await user.mutation(api.consent.submitConsent, { consent });
  await user.mutation(api.consent.updateProjectSharing, {
    changes: [{ identifier: project, sessionSharing: true }],
  });
  return user;
}
