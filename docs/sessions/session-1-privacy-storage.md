# Session 1: Privacy & Storage Architecture

**Date**: 2025-12-30

## Goal

Resolve the fundamental question: store raw transcripts in git or separate storage?

## What We Explored

### Storage Options

**Git-based storage** (like Sionic blog approach):
- Pro: Simple, version controlled, reprocessing enabled
- Con: Git history is permanent - leaked sensitive data very hard to remove

**Separate storage** (server + object storage):
- Pro: Verified deletion possible, better privacy isolation
- Con: More infrastructure to maintain

**Decision**: Separate storage. Yankability and privacy concerns outweigh simplicity benefits.

### Auth Providers

Evaluated Clerk, Auth0, Stytch, and WorkOS for CLI authentication:

| Provider | CLI Device Flow | Convex Integration | Invite-Only |
|----------|-----------------|-------------------|-------------|
| Clerk | Planned (not released) | Official | Built-in |
| Auth0 | First-class | Official | Custom |
| Stytch | First-class (Connected Apps) | Custom JWT (works) | Built-in |
| WorkOS | First-class | Official | Paid tiers |

**Decision**: Stytch. First-class CLI device flow via Connected Apps (Clerk's is not released), JWT-based tokens work with Convex custom auth, invite-only built-in.

**Tradeoff accepted**: No official Convex integration, but Stytch JWTs work with Convex's custom JWT auth provider.

### File Storage

Compared Convex built-in storage vs Cloudflare R2:

| Factor | Convex Built-in | Cloudflare R2 |
|--------|-----------------|---------------|
| Vendor lock-in | High | Low (S3-compatible) |
| Deletion verification | Via Convex API only | Direct S3 access |
| Cost at scale | $0.50/GB | $0.015/GB + zero egress |

**Decision**: Cloudflare R2. Better for yankability verification, portability, and debugging.

### GitHub Action Authentication

Options considered:
- Convex deploy key (actually for code deployment, not API access)
- HTTP Actions with shared secret (Convex-recommended for external services)
- Clerk/Stytch M2M tokens (adds complexity)

**Decision**: Shared secret. Simplest, Convex-recommended pattern.

## Decisions Made

- Store raw transcripts in R2, not git (privacy: git history permanent; R2 allows verified deletion)
- Use Stytch for auth (first-class CLI device flow; JWT-based works with Convex custom auth)
- Use Convex as backend (real-time, good DX, supports custom JWT auth)
- Use Cloudflare R2 for file storage (S3-compatible, zero egress, direct access for debugging/yankability)
- GitHub Action auth via shared secret (simpler than deploy keys; Convex-recommended)
- Use GitHub App for repo invitations (more secure than user tokens)
- Use Application Invitations not Organizations (simpler for single scholar group)

## Open Questions That Emerged

- **Login flow**: Exact pattern TBD - dedicated CLI binary vs script in plugin vs SessionStart hook
- **Cloud VM authentication**: Must support non-desktop environments without browsers
- **Submission mechanism**: Likely SessionEnd hook, but needs experimentation

## Reference Documentation

- [Stytch Connected Apps CLI](https://stytch.com/docs/guides/connected-apps/cli-app)
- [Stytch JWT Sessions](https://stytch.com/docs/guides/sessions/using-jwts)
- [Convex Custom JWT Auth](https://docs.convex.dev/auth/advanced/custom-jwt)
- [Convex R2 Component](https://www.convex.dev/components/cloudflare-r2)
- [Convex HTTP Actions](https://docs.convex.dev/functions/http-actions)
