# syntax=docker/dockerfile:1.7
#
# atlas-server plus the web app it serves. Build from the repository root:
#   docker build -t atlas .
#
# Mirrors Cosmos's cosmos-agent/Dockerfile. Stages run in parallel, so each
# gets its own cache ids: cargo can't safely unpack into one registry from two
# builds at once.

# TypeScript bindings come from ts-rs, which writes them during tests
# (TS_RS_EXPORT_DIR in .cargo/config.toml points at app/src/generated).
FROM rust:1-bookworm AS bindings
WORKDIR /src
COPY . .
RUN --mount=type=cache,id=atlas-bindings-registry,sharing=locked,target=/usr/local/cargo/registry \
    --mount=type=cache,id=atlas-bindings-target,sharing=locked,target=/src/target \
    cargo test --locked -p atlas-common --quiet

# The web build is platform independent, so it runs natively on the builder.
# git fetches @sunstead/ui, a GitHub dependency pinned to a tag.
FROM --platform=$BUILDPLATFORM node:22-bookworm-slim AS web
RUN apt-get update && apt-get install -y --no-install-recommends git ca-certificates  && rm -rf /var/lib/apt/lists/*
WORKDIR /src
COPY package.json package-lock.json ./
COPY app/package.json app/
RUN --mount=type=cache,id=atlas-npm,sharing=locked,target=/root/.npm npm ci --no-audit --no-fund
COPY app/ app/
COPY --from=bindings /src/app/src/generated app/src/generated
RUN npm run build

FROM rust:1-bookworm AS build
WORKDIR /src
COPY . .
RUN --mount=type=cache,id=atlas-server-registry,sharing=locked,target=/usr/local/cargo/registry \
    --mount=type=cache,id=atlas-server-target,sharing=locked,target=/src/target \
    cargo build --locked --release -p atlas-server \
 && cp /src/target/release/atlas /atlas \
 && mkdir -p /state /index

# distroless/cc: glibc and libgcc, no shell and no package manager. SQLite is
# compiled in (rusqlite `bundled`) and HTTPS uses rustls with bundled roots,
# so nothing else is needed.
FROM gcr.io/distroless/cc-debian12:nonroot
COPY --from=build /atlas /usr/local/bin/atlas
# uid 1000, not distroless's 65532: Atlas reads originals under
# /srv/storage/data, which uid 1000 owns. Docker seeds a new named volume with
# the mount point's ownership, so these must be owned by that uid too.
COPY --from=build --chown=1000:1000 /state /state
COPY --from=build --chown=1000:1000 /index /index
COPY --from=web /src/app/dist /usr/share/atlas/web
ENV ATLAS_WEB_DIR=/usr/share/atlas/web \
    ATLAS_STATE_DIR=/state \
    ATLAS_INDEX_DIR=/index
USER 1000:1000
EXPOSE 8080
ENTRYPOINT ["/usr/local/bin/atlas"]
