# CLAUDE.md

## Overview

Sunstead Atlas is the Sunstead homepage: one search over each user's own data
on Jupiter (files, photos, later notes, calendar and contacts), with previews,
light actions and deep links into each app. It was planned as "Horizon" in the
Cosmos roadmap (`Cosmos/docs/ROADMAP.md`, section E). Atlas is the final name.

- **Third-party apps stay independent.** Atlas reads originals on disk where it
  can (files are the truth) and calls each app's API where it has to, through
  one adapter per source.
- **Per user.** Users sign in through Authentik OIDC. Every query is scoped to
  the signed-in user, and a user only sees data they own or that is shared
  with them.
- **Derived vs state.** The search index is derived data: rebuildable, kept
  under `/srv/storage/derived/atlas`, never backed up. Only small state (users,
  sessions, connections with sealed credentials) is backed up.

Milestones: M0 scaffold, M1 sign-in and connections, M2 OpenCloud file search
from disk, M3 Immich, M4 OpenCloud links and thumbnails, M5 polish
(OpenSearch, suggestions, launcher, keyboard, mobile) (done); shared project
spaces (once any exist on Jupiter); M6 notes, after Solstice Sync exists.

## Commands

```sh
cp .env.example .env              # once: dev sign-in (ATLAS_DEV_USER) for debug builds
cargo test --workspace            # Rust tests; also writes app/src/generated/*.ts
cargo clippy --workspace --all-targets -- -D warnings
cargo run -p atlas-server         # API on :8080 (ATLAS_BIND); API only unless ATLAS_WEB_DIR is set

npm install                       # once, at the repo root (npm workspaces)
npm run dev                       # server (:8080) + Vite (:1420, proxying /v1, /auth, /healthz)
npm run dev:app / dev:server      # one side only
npm run lint                      # eslint, whole repo, 0 warnings expected
npm test                          # vitest (app)
npm run build                     # tsc + vite build into app/dist

docker build -t atlas .           # from the repo root
docker run -p 8080:8080 atlas     # serves the app and API on :8080
```

`app/src/generated/` is gitignored. Run `cargo test -p atlas-common` before the
first `npm run build` or `tsc`, and after changing a shared type.

## Layout

```
crates/
  atlas-common/      wire DTOs; #[derive(TS)] #[ts(export)] -> app/src/generated
  atlas-core/        Source traits (Source, Indexed, Federated), Doc, Preview, IndexSink
  atlas-state/       rusqlite state DB, backed up
  atlas-index/       Tantivy + index.db, derived, rebuildable
  atlas-fs/          on-disk walk/resolve/extract/watch
  atlas-server/      bin `atlas`: axum, auth, routes, indexer, serves the app
  sources/           one crate per source adapter
app/                 React web app
packages/sunstead-ui shared themes, tokens, shadcn primitives (source-only)
```

Dependency rules:
- Source crates depend on `atlas-core` (and `atlas-fs`), never on storage.
- `atlas-server` wires everything together.
- `packages/sunstead-ui` holds nothing Atlas-specific; it moves to its own repo
  once a second app adopts it.

## Sign-in and sessions

- **Modes** (`atlas-server/src/config.rs`): `ATLAS_OIDC_*` for Authentik
  (confidential client, PKCE + nonce, `auth/oidc.rs`); `ATLAS_DEV_USER` for
  local development (everyone is that user, logged loudly); neither means
  nobody can sign in and `/auth/login` says so (501).
- **Sessions** are server-side (`atlas-state/src/sessions.rs`): a random token
  in the cookie, only its SHA-256 in the database, 30 days sliding. The cookie
  is `__Host-atlas_session` (Secure) over HTTPS, `atlas_session` over HTTP.
- **Handlers** take `CurrentUser` (`auth/mod.rs`) and scope every query by
  `user.id`. `UserId` can only come from the state crate, never from input.
- **The app shell is gated** (`web.rs`): a page load without a session goes to
  `/auth/login?return_to=<path+query>`, so address-bar searches survive
  sign-in. `return_to` must be a same-origin path (`auth/return_to.rs`).
- **CSRF** (`api/csrf.rs`): non-GET requests need `X-Atlas-Request: 1` and, if
  sent, an `Origin` equal to `ATLAS_PUBLIC_URL`'s.
- **Credentials** (API keys) are sealed with XChaCha20-Poly1305 under
  `ATLAS_MASTER_KEY`, bound to `user|connection|kind`
  (`atlas-state/src/crypto.rs`), and never returned by the API. Debug and dev
  sign-in fall back to a built-in key.
- **Source kinds** (`sources.rs`) are offered only when the server has their
  settings (`ATLAS_IMMICH_URL`, `ATLAS_OPENCLOUD_URL` +
  `ATLAS_OPENCLOUD_USERS_DIR`). An OpenCloud connection pins its root to
  `<users dir>/<username>` when it's created.

## Sources and the index

- **Adapters** implement `atlas_core::Source` and, if Atlas keeps their text,
  `Indexed` (a sync reports `Doc`s to an `IndexSink`); if they're asked at
  query time, `Federated`. `sources.rs` builds one from a connection row;
  `indexer.rs` caches it until the row changes.
- **The indexer** (`atlas-server/src/indexer.rs`) syncs every enabled indexed
  connection at startup, on create/change/"Sync now", every 15 minutes, and
  from file watchers (changed files sync alone; a changed or vanished folder
  triggers a full sync). At most two syncs at once, one per connection, each
  on a blocking thread.
- **The index** (`atlas-index`) is `index.db` (rows) plus Tantivy (text,
  every document tagged with its owner). Every search filters on the user,
  and rows are re-checked against the user. If the two parts disagree, or
  `SCHEMA_VERSION` changed, it starts over and the syncs refill it. Never
  back it up.
- **OpenCloud** (`atlas-source-opencloud`) reads the user's PosixFS space
  read-only. Ids are root-relative paths; `atlas_fs::resolve` refuses `..`,
  hidden segments and symlinks out of the root. Links go straight to the file
  (`/f/<storage>$<space>!<file>`), built from the `user.oc.space.id` /
  `user.oc.id` attributes PosixFS keeps on disk plus
  `ATLAS_OPENCLOUD_STORAGE_ID`; without those, to the containing folder.
  An optional per-user **app token** (Basic `username:token`; needs
  `PROXY_ENABLE_APP_AUTH` on OpenCloud) adds thumbnails and previews from
  OpenCloud's renderer, and the space ids from Graph when they aren't on disk.
  Admins make tokens with `opencloud auth-app create --user-name=<user>
  --expiration=<n>h`. Any API failure falls back to disk.
- **Immich** (`atlas-source-immich`) is federated: each search goes to
  Immich's smart search and file name search with the user's own API key
  (needs `asset.read`, `asset.view`, `album.read`; `asset.download` for
  originals), plus album names (cached 5 minutes). Ids are `asset:<uuid>` /
  `album:<uuid>`. A key Immich refuses is rejected when it's saved; Immich
  being down doesn't stop a key being saved.
- **Search** (`api/search.rs`) asks the index and every federated source at
  once (each gets `FEDERATED_DEADLINE`, 2.5 s), reports each source's state,
  and merges ranked lists by Reciprocal Rank Fusion, since BM25 and CLIP
  scores can't be compared.
- **Ways in** (`api/discovery.rs`): `/opensearch.xml` (public; linked from
  `index.html`) makes Atlas a browser search engine; `/v1/suggest` returns
  OpenSearch suggestions (empty, not 401, when signed out); `/v1/apps` is the
  launcher, from `ATLAS_APPS_FILE` (TOML, `apps.rs`) or the configured
  sources.
- **Keyboard** (`lib/keyboard.ts`): `/` focuses search anywhere; on results,
  arrows move the selection, Escape closes the preview, Ctrl/Cmd+Enter opens
  the item in its app.
- **Item ids in URLs** are base64url (`api/items.rs`). Blobs are served with
  `CSP: sandbox` (except PDFs) and `nosniff`, so a user's HTML or SVG never
  runs as Atlas.

## Conventions (matching Cosmos)

**Rust**
- Edition 2021, one workspace, shared versions in `[workspace.dependencies]`.
- Shared types: ts-rs 10 with `#[ts(export)]`. The export dir comes from
  `TS_RS_EXPORT_DIR` in `.cargo/config.toml`, so don't add `export_to`.
  Annotate 64-bit integers with `#[ts(type = "number")]` (see
  `atlas-common/src/lib.rs`).
- Errors: `ApiError { code, message, detail }` (`atlas-common/src/error.rs`),
  turned into responses by `atlas-server/src/error.rs`. `NotEnabled` (501)
  means hide it; `Unavailable` (503) means show a retry. An unreachable
  identity provider is 503, not 401.
- Routes: `/v1/*`, plus public `/healthz`. Unknown `/v1` and `/auth` paths are
  404s, never the app shell (`atlas-server/src/web.rs`).
- Logs: paths only, never query strings (search terms are private). No ANSI
  colour outside a terminal.
- Storage: rusqlite (`bundled`) with `PRAGMA user_version` migrations, not
  sqlx, so the distroless image needs no system libraries.
- Config: `ATLAS_*` env vars (`atlas-server/src/config.rs`); secrets get a
  `_FILE` variant.

**Frontend**
- React 19, Vite 7, TS strict (`noUnusedLocals`/`Parameters`), TanStack Router
  (code-based routes in `src/router.tsx`) and Query, Zustand 5, Tailwind v4,
  shadcn (`radix-nova`), lucide, `@/` = `app/src`.
- Every view is reachable by URL. `/search?q=&type=&source=&preview=` is parsed
  in `src/lib/search-params.ts`; the browser search engine entry uses
  `/search?q=%s`.
- Open views are modelled as tabs (`src/lib/stores/tabs.ts`); the URL is the
  source of truth for the active tab. The MVP shows one.
- Server calls go through `api()` (`src/lib/api.ts`), which sends a 401 to
  sign-in (returning to the current page) and sends
  `X-Atlas-Request: 1` on state-changing requests.
- UI copy: sentence case, no em dashes or curly quotes (enforced by eslint in
  `pages/`, `components/`, `layouts/`).

**sunstead-ui**
- Import from `@sunstead/ui/...` (see the package's `exports`). Inside the
  package, use relative imports, never `@/`.
- Themes: a `[data-theme]` block in `packages/sunstead-ui/src/themes/` plus an
  entry in `src/lib/themes.ts`. `app/index.html` keeps a copy for the first
  paint; `src/lib/themes-sync.test.ts` keeps them in step.
- New shadcn primitives go in `packages/sunstead-ui/src/components/ui/`, with
  `@/lib/utils` rewritten to `../../lib/utils`.

## Deployment

Atlas runs on Jupiter (`Documents/Code/Jupiter`), built as
`ghcr.io/sunstead/atlas` by `.github/workflows/image.yml`:
- `compose/atlas.yml` on the `homelab` network, with pinned image tags and
  `cosmos.service*` labels.
- `extra_hosts: auth.jupiter.sunstead.net:host-gateway`.
- Runs as uid 1000 so it can read `/srv/storage/data`.
- Mounts: `atlas-state:/state` (backed up), `${STORAGE_PATH}/derived/atlas:/index`
  (not backed up), and source folders read-only.
- Caddy: an `@atlas host atlas.jupiter.sunstead.net` block in both `Caddyfile`
  and `Caddyfile.dev`.
- Authentik: a blueprint (`authentik/blueprints/atlas.yaml`) with a
  confidential client; its secret goes into both Authentik containers.

Changes to Jupiter go through a PR, because a push to its `main` deploys.
