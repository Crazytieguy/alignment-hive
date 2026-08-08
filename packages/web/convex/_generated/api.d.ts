/* eslint-disable */
/**
 * Generated `api` utility.
 *
 * THIS CODE IS AUTOMATICALLY GENERATED.
 *
 * To regenerate, run `npx convex dev`.
 * @module
 */

import type * as agreement from "../agreement.js";
import type * as auth from "../auth.js";
import type * as authorized from "../authorized.js";
import type * as consent from "../consent.js";
import type * as feedback from "../feedback.js";
import type * as github from "../github.js";
import type * as githubWebhook from "../githubWebhook.js";
import type * as http from "../http.js";
import type * as lib_agreement from "../lib/agreement.js";
import type * as lib_apiKeyAuth from "../lib/apiKeyAuth.js";
import type * as lib_auth from "../lib/auth.js";
import type * as lib_authorizedQueries from "../lib/authorizedQueries.js";
import type * as lib_consentVisibility from "../lib/consentVisibility.js";
import type * as lib_projectConsent from "../lib/projectConsent.js";
import type * as lib_schemas from "../lib/schemas.js";
import type * as lib_users from "../lib/users.js";
import type * as sessions from "../sessions.js";

import type {
  ApiFromModules,
  FilterApi,
  FunctionReference,
} from "convex/server";

declare const fullApi: ApiFromModules<{
  agreement: typeof agreement;
  auth: typeof auth;
  authorized: typeof authorized;
  consent: typeof consent;
  feedback: typeof feedback;
  github: typeof github;
  githubWebhook: typeof githubWebhook;
  http: typeof http;
  "lib/agreement": typeof lib_agreement;
  "lib/apiKeyAuth": typeof lib_apiKeyAuth;
  "lib/auth": typeof lib_auth;
  "lib/authorizedQueries": typeof lib_authorizedQueries;
  "lib/consentVisibility": typeof lib_consentVisibility;
  "lib/projectConsent": typeof lib_projectConsent;
  "lib/schemas": typeof lib_schemas;
  "lib/users": typeof lib_users;
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
