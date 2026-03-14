// This file defines the tRPC router type for the review app.
// The actual router implementation lives in hive-cli (the server).
// This import is used by the tRPC client for type inference.
import type { AppRouter } from "../../hive-cli/src/lib/review-router";

export type { AppRouter };
