# The cloud deployment: one image, one container, Linux only.
#
# §20 asks for Docker and no Kubernetes. What is here is a server, its SQLite
# database and its media on one volume — which is what a handful of concurrent
# broadcasts actually needs, and what can be understood at three in the morning.

# --- the web bundle --------------------------------------------------------
FROM node:20-bookworm AS web
WORKDIR /app
COPY package.json package-lock.json ./
# The desktop's Tauri CLI is in devDependencies and is not needed here, but
# npm ci installs from the lock file as a whole, which is what keeps the build
# reproducible.
RUN npm ci --no-audit --no-fund
# No tsconfig at the repository root: `apps/web`'s extends `apps/desktop`'s,
# and both are copied below. A `tsconfig*.json` glob here matches nothing and
# fails the build.
COPY vite.web.config.ts tailwind.config.js postcss.config.js ./
COPY apps/desktop/tsconfig.json apps/desktop/tsconfig.json
COPY apps/desktop/src apps/desktop/src
COPY apps/web apps/web
RUN npm run build:cloud

# --- the server ------------------------------------------------------------
FROM rust:1-bookworm AS server
WORKDIR /src
# The whole workspace, because `cargo` reads every member's manifest even when
# building one of them. Only `louver-server` is compiled.
COPY Cargo.toml Cargo.lock ./
COPY crates crates
COPY apps/server apps/server
COPY apps/desktop/src-tauri/Cargo.toml apps/desktop/src-tauri/Cargo.toml
COPY apps/desktop/src-tauri/build.rs apps/desktop/src-tauri/build.rs
COPY apps/desktop/src-tauri/src apps/desktop/src-tauri/src
RUN cargo build --release -p louver-server

# --- what actually runs ----------------------------------------------------
FROM debian:bookworm-slim
# ffmpeg from the distro, so the image carries one copy that apt keeps patched.
# The desktop app ships a pinned sidecar because a user's machine has no package
# manager we control; a server does.
RUN apt-get update \
    && apt-get install -y --no-install-recommends ffmpeg ca-certificates \
    && rm -rf /var/lib/apt/lists/*

COPY --from=server /src/target/release/louver-server /usr/local/bin/louver-server
COPY --from=web /app/apps/web/dist /srv/louver/web

ENV LOUVER_DATA_DIR=/var/lib/louver \
    LOUVER_WEB_DIR=/srv/louver/web \
    LOUVER_FFMPEG_DIR=/usr/bin \
    LOUVER_BIND=0.0.0.0:8080 \
    # A container is a server that stays on. The UI says so, and the warning
    # about closing a laptop is not shown.
    LOUVER_DEPLOYMENT=cloud
# The database, the uploaded originals and the prepared copies. Lose this and
# the broadcasts are gone; back it up, not the image.
VOLUME ["/var/lib/louver"]
EXPOSE 8080

# No LOUVER_MASTER_KEY here on purpose: a key baked into an image is a key in
# every registry that mirrors it. The server refuses to start without one.
HEALTHCHECK --interval=30s --timeout=5s --start-period=10s \
    CMD ["/usr/local/bin/louver-server", "--health-check"]
CMD ["/usr/local/bin/louver-server"]
