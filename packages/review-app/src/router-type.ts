// The review server's tRPC router type, from hive-cli where the router lives. A type-only import,
// erased from the bundle; the client infers every call from it.
import type { AppRouter } from "../../hive-cli/src/lib/review-router";

export type { AppRouter };
