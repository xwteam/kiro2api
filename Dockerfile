# ---- 前端构建(仅 user-ui;产出 dist/ 供 rust-embed 编译期嵌入) ----
# 注:admin-ui 已改为静态手写 UI(admin-ui-v2/),无需构建,直接进 Rust 构建上下文嵌入。
FROM node:22-alpine AS frontend-builder
# 钉 pnpm 9.15.4(本地验证过的版本;pnpm 10+ 默认 minimumReleaseAge 会拒装新发布的依赖)
RUN npm install -g pnpm@9.15.4

# user-ui
WORKDIR /app/user-ui
COPY user-ui/package.json user-ui/pnpm-lock.yaml ./
RUN pnpm install --frozen-lockfile --ignore-scripts \
    || pnpm install --no-frozen-lockfile --ignore-scripts
COPY user-ui ./
RUN pnpm build

# ---- Rust 构建 ----
FROM rust:1-slim AS builder
WORKDIR /build
RUN apt-get update && apt-get install -y --no-install-recommends pkg-config && rm -rf /var/lib/apt/lists/*
COPY Cargo.toml Cargo.lock ./
COPY src ./src
# build.rs 在编译期捕获 rustc 版本注入 KIRO_RUST_VERSION,必须在 cargo build 前拷入
COPY build.rs ./
# 静态 admin UI(admin-ui-v2/)是源码,直接进构建上下文供 rust-embed 编译期嵌入
COPY admin-ui-v2 ./admin-ui-v2
# 从前端阶段拷入 user-ui 真实 dist,供 rust-embed 在 cargo build 前找到
COPY --from=frontend-builder /app/user-ui/dist ./user-ui/dist
RUN cargo build --release --locked

# ---- 运行镜像 ----
FROM debian:bookworm-slim
RUN apt-get update && apt-get install -y --no-install-recommends ca-certificates wget gosu \
    && rm -rf /var/lib/apt/lists/* \
    && useradd -m -u 1000 appuser
ENV HOST=0.0.0.0
WORKDIR /app
COPY --from=builder /build/target/release/kiro2api /usr/local/bin/kiro2api
COPY docker-entrypoint.sh /usr/local/bin/docker-entrypoint.sh
RUN chmod +x /usr/local/bin/docker-entrypoint.sh && mkdir -p /app/data && chown -R appuser:appuser /app
EXPOSE 8990
# Probe the port the app actually binds — read it from the mounted config.json
# (falls back to 8080 if the file is absent), so the check tracks the runtime
# port instead of a hard-coded guess.
HEALTHCHECK --interval=30s --timeout=10s --start-period=20s --retries=3 \
    CMD P=$(grep -oE '"port"[[:space:]]*:[[:space:]]*[0-9]+' /app/data/config.json 2>/dev/null | grep -oE '[0-9]+' | head -1); wget -q -O /dev/null "http://localhost:${P:-8080}/health" || exit 1
ENTRYPOINT ["docker-entrypoint.sh"]
CMD ["kiro2api", "-c", "/app/data/config.json", "--credentials", "/app/data/credentials.json"]
