# alignment-hive Web App

A TanStack Start + React web application for alignment researchers to share session learnings and contribute to the collective knowledge base.

## What it does

- **Authentication**: Sign in via WorkOS (invite-only)
- **Dashboard**: Access your profile and session history
- **Integration**: Connect with the hive-mind CLI for session extraction and submission

## Local Development

### Setup

```bash
# Copy environment template
cp .env.local.example .env.local

# Update with staging credentials (check .env.local.example for defaults)
# Note: HIVE_MIND_CLIENT_ID in .env.local allows the CLI to use staging

# Install dependencies (from repo root)
bun install

# Start dev server
cd web
bun dev
```

The dev server runs on `http://localhost:3000` and includes both the frontend and Convex backend.

### Environment Variables

**For local development** (staging credentials in `.env.local`):
- `HIVE_MIND_CLIENT_ID`: Staging WorkOS client ID (for CLI)
- `WORKOS_CLIENT_ID`: Staging WorkOS client ID (web app)
- `WORKOS_API_KEY`: Staging WorkOS API key
- `WORKOS_COOKIE_PASSWORD`: Secure 32+ char string for session cookies
- `WORKOS_REDIRECT_URI`: Should be `http://localhost:3000/callback`
- `VITE_CONVEX_URL`: Convex dev deployment URL

**For production** (configured in Vercel):
- Production WorkOS credentials
- Production Convex deployment URL

### Available Scripts

- `bun dev` - Start frontend and backend in parallel
- `bun run dev:frontend` - Just the Vite dev server
- `bun run dev:backend` - Just the Convex backend
- `bun run build` - Build for production
- `bun run lint` - Type check and lint

## Architecture

- **Frontend**: TanStack Start + React 19 + Tailwind CSS v4
- **UI Components**: shadcn/ui
- **Backend**: Convex (serverless)
- **Auth**: WorkOS AuthKit
- **Database**: Convex Cloud

## Pages

- `/` - Homepage with login for invited users
- `/welcome` - Post-signup welcome with installation instructions
- `/callback` - OAuth callback handler (redirects to welcome)
- `/authenticated/*` - Protected routes (TODO)

## Deployment

Deployment is automatic on push to `main` via Vercel. Environment variables are configured in Vercel dashboard.

The build command is: `bunx convex deploy --cmd 'bun run build'`

This ensures both Convex backend and Vite frontend are built for production.

## Dark Mode

The app automatically respects system dark mode preference via CSS `prefers-color-scheme` media query. No user toggle needed. All shadcn components include dark mode colors in `src/app.css`.

## TODO: Branding & Polish

- [ ] **WorkOS Branding**: Configure login page in [WorkOS Dashboard](https://dashboard.workos.com) → Organizations → Branding
- [ ] **Meta Tags**: Add Open Graph and social sharing metadata
- [ ] **Error Pages**: Create dedicated error pages for failure scenarios
- [ ] **Dashboard**: Build `/authenticated/*` routes for post-login experience
