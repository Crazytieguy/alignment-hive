/* eslint-disable */
/**
 * Generated `api` utility.
 *
 * THIS CODE IS AUTOMATICALLY GENERATED.
 *
 * To regenerate, run `npx convex dev`.
 * @module
 */

import type * as auth from "../auth.js";
import type * as authorized from "../authorized.js";
import type * as consent from "../consent.js";
import type * as lib_admin from "../lib/admin.js";
import type * as lib_auth from "../lib/auth.js";
import type * as lib_consentVisibility from "../lib/consentVisibility.js";
import type * as lib_projectConsent from "../lib/projectConsent.js";
import type * as lib_users from "../lib/users.js";
import type * as migrations from "../migrations.js";
import type * as sessions from "../sessions.js";

import type {
  ApiFromModules,
  FilterApi,
  FunctionReference,
} from "convex/server";

declare const fullApi: ApiFromModules<{
  auth: typeof auth;
  authorized: typeof authorized;
  consent: typeof consent;
  "lib/admin": typeof lib_admin;
  "lib/auth": typeof lib_auth;
  "lib/consentVisibility": typeof lib_consentVisibility;
  "lib/projectConsent": typeof lib_projectConsent;
  "lib/users": typeof lib_users;
  migrations: typeof migrations;
  sessions: typeof sessions;
}>;

/**
 * A utility for referencing Convex functions in your app's public API.
 *
 * Usage:
 * ```js
 * const myFunctionReference = api.myModule.myFunction;
 * ```
 */
export declare const api: FilterApi<
  typeof fullApi,
  FunctionReference<any, "public">
>;

/**
 * A utility for referencing Convex functions in your app's internal API.
 *
 * Usage:
 * ```js
 * const myFunctionReference = internal.myModule.myFunction;
 * ```
 */
export declare const internal: FilterApi<
  typeof fullApi,
  FunctionReference<any, "internal">
>;

export declare const components: {};
