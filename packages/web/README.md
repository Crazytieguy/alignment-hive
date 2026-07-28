# alignment-hive Web App

A TanStack Start + React web application for alignment researchers to share session learnings and contribute to the collective knowledge base.

## What it does

- **Authentication**: Sign in via WorkOS (invite-only)
- **Consent**: Data sharing preferences wizard and management
- **Dashboard**: Admin dashboard for sessions and users
- **Integration**: Backend for the hive CLI (session uploads, consent, heartbeats)
- **Booking**: Public per-office pages (`/book/<office>`) to book in-person meetings with Yoav, backed by Google Calendar (see `src/lib/booking/` and `src/routes/booking/`; env vars in `scripts/google-calendar-auth.ts`).

## Local Development

### Setup

```bash
# Install dependencies (from repo root)
bun install

# Configure Convex + WorkOS (interactive, one-time)
bash scripts/setup-web.sh

# Start dev server (from repo root)
bun run --filter '@alignment-hive/web' dev
```

The dev server runs on `http://localhost:3000` and includes both the frontend and Convex backend.

### Environment Variables

Non-secret defaults (`WORKOS_CLIENT_ID`, `WORKOS_REDIRECT_URI`) live in the checked-in `.env` file. Secrets and per-dev config go in `.env.local` (gitignored), created by the setup script:

- `CONVEX_DEPLOYMENT`: Dev deployment name (written by `convex dev`)
- `VITE_CONVEX_URL`: Dev deployment URL (written by `convex dev`)
- `WORKOS_API_KEY`: Staging WorkOS API key (ask a team member)
- `WORKOS_COOKIE_PASSWORD`: Auto-generated during setup

**For production** (configured in Vercel):

- Production WorkOS credentials
- Production Convex deployment URL

### Available Scripts

From repo root:

- `bun run --filter '@alignment-hive/web' lint` - Type check
- `bun run --filter '@alignment-hive/web' build` - Build for production

From `web/` directory:

- `bun dev` - Start frontend and backend in parallel
- `bun run dev:frontend` - Just the Vite dev server
- `bun run dev:backend` - Just the Convex backend

## Architecture

- **Frontend**: TanStack Start + React 19 + Tailwind CSS v4
- **UI Components**: shadcn/ui
- **Backend**: Convex (serverless)
- **Auth**: WorkOS AuthKit (web UI + CLI use JWTs, HTTP API uses WorkOS API keys)
- **HTTP API**: Hono router on Convex HTTP actions (`convex/http.ts`), auto-generated OpenAPI spec at `/api/doc`
- **Database**: Convex Cloud

### Data access

Session data is available via the web UI and HTTP API (`/api/*`). All paths require a signed data accessor agreement and apply consent-based session filtering.

The HTTP API authenticates with WorkOS organization API keys. An API key stays valid until it is deleted in WorkOS, so revoking someone's access (or bumping the agreement version) means revoking their keys by hand. API key management uses the WorkOS `<ApiKeys />` widget embedded on the authorized index page. One-time WorkOS dashboard setup is required: create organizations, configure CORS, and create a `data-accessor` role with the `widgets:api-keys:manage` permission.

## Deployment

Deployment is automatic on push to `main` via Vercel. Environment variables are configured in Vercel dashboard.

The `nitro` vite plugin in `vite.config.ts` is required for TanStack Start to deploy correctly on Vercel (it auto-detects the Vercel environment and builds for serverless).

The build command is: `bunx convex deploy --cmd 'bun run build'`

This ensures both Convex backend and Vite frontend are built for production.

## Dark Mode

The app automatically respects system dark mode preference via CSS `prefers-color-scheme` media query. No user toggle needed. All shadcn components include dark mode colors in `src/app.css`.

## TODO: Branding & Polish

- [ ] **WorkOS Branding**: Configure login page in [WorkOS Dashboard](https://dashboard.workos.com) → Organizations → Branding
- [ ] **Meta Tags**: Add Open Graph and social sharing metadata
- [ ] **Error Pages**: Create dedicated error pages for failure scenarios
- [ ] **Dashboard**: Build `/authenticated/*` routes for post-login experience
