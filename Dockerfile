# ---- 前端构建(仅 user-ui;产出 dist/ 供 rust-embed 编译期嵌入) ----
# 注:admin-ui 已改为静态手写 UI(admin-ui-v2/),无需构建,直接进 Rust 构建上下文嵌入。
# 钉 --platform=$BUILDPLATFORM:JS 产物与目标架构无关,固定在构建机(amd64)原生跑一次,
# 多架构构建时不被 QEMU 模拟(否则 node 也会被模拟拖慢)。
FROM --platform=$BUILDPLATFORM node:22-alpine AS frontend-builder
# 钉 pnpm 9.15.4(本地验证过的版本;pnpm 10+ 默认 minimumReleaseAge 会拒装新发布的依赖)
RUN npm install -g pnpm@9.15.4

# user-ui
WORKDIR /app/user-ui
COPY user-ui/package.json user-ui/pnpm-lock.yaml ./
RUN pnpm install --frozen-lockfile --ignore-scripts \
    || pnpm install --no-frozen-lockfile --ignore-scripts
COPY user-ui ./
RUN pnpm build

# ---- Rust 构建(交叉编译:builder 固定在构建机原生跑,按 TARGETARCH 交叉编出目标架构二进制)----
# 关键:--platform=$BUILDPLATFORM 让 builder 永远原生(amd64)运行,再用交叉工具链编 arm64,
# 避免在 QEMU 里模拟编译整个 Rust 依赖树(那会慢 5-10 倍、单次上小时)。TLS 后端为 ring,
# 交叉编译干净(不含 aws-lc-rs/openssl 等难移植 C 依赖)。
FROM --platform=$BUILDPLATFORM rust:1-slim AS builder
ARG TARGETARCH
WORKDIR /build
# 目标架构的交叉 gcc + libc 头(供 ring 的 C/asm 交叉编译与链接)。
# perl + make:vendored OpenSSL(native-tls 后端)自带一套 Perl 写的 ./Configure,
# rust:1-slim 里两样都没有,缺了会在 `./Configure line 15` 处直接失败。
RUN apt-get update && apt-get install -y --no-install-recommends \
      pkg-config gcc-aarch64-linux-gnu libc6-dev-arm64-cross perl make \
    && rm -rf /var/lib/apt/lists/*
RUN rustup target add x86_64-unknown-linux-gnu aarch64-unknown-linux-gnu
# arm64 目标的链接器与 C 编译器指向交叉工具链(amd64 目标用镜像自带的原生 gcc)。
# AR 也要指向交叉工具链:OpenSSL 交叉编译时要打静态库,用宿主 ar 会产出 x86 归档、
# 链接阶段才报错(错误信息与真实原因相距很远)。
ENV CARGO_TARGET_AARCH64_UNKNOWN_LINUX_GNU_LINKER=aarch64-linux-gnu-gcc \
    CC_aarch64_unknown_linux_gnu=aarch64-linux-gnu-gcc \
    AR_aarch64_unknown_linux_gnu=aarch64-linux-gnu-ar
COPY Cargo.toml Cargo.lock ./
COPY src ./src
# build.rs 在编译期捕获 rustc 版本注入 KIRO_RUST_VERSION,必须在 cargo build 前拷入
COPY build.rs ./
# 静态 admin UI(admin-ui-v2/)是源码,直接进构建上下文供 rust-embed 编译期嵌入
COPY admin-ui-v2 ./admin-ui-v2
# 从前端阶段拷入 user-ui 真实 dist,供 rust-embed 在 cargo build 前找到
COPY --from=frontend-builder /app/user-ui/dist ./user-ui/dist
# TARGETARCH(docker buildx 注入:amd64/arm64)→ Rust 目标三元组 → 交叉编译。
RUN case "$TARGETARCH" in \
      amd64) RUST_TARGET=x86_64-unknown-linux-gnu ;; \
      arm64) RUST_TARGET=aarch64-unknown-linux-gnu ;; \
      *) echo "unsupported TARGETARCH=$TARGETARCH" && exit 1 ;; \
    esac && \
    cargo build --release --locked --target "$RUST_TARGET" && \
    cp "target/$RUST_TARGET/release/kiro2api" /build/kiro2api

# ---- 运行镜像(不钉 platform → 继承 TARGETPLATFORM,产出对应架构的最终镜像)----
FROM debian:bookworm-slim
RUN apt-get update && apt-get install -y --no-install-recommends ca-certificates wget gosu \
    && rm -rf /var/lib/apt/lists/* \
    && useradd -m -u 1000 appuser
# 凭据路径**不**在镜像里烘焙成 ENV,也不写进 CMD 的 --credentials:那两层的优先级都高于
# config.json,会把用户在 config.json 里设的自定义路径静默改道(CMD 更甚,连 .env 里的
# CREDENTIALS_PATH 都会被压掉)。改由应用把"内置默认"就近解析到 `-c` 所指配置文件的目录
# (见 `Config::resolve_default_credentials_beside_config`):容器以 -c /app/data/config.json
# 启动,默认便落在挂载卷 /app/data 内(凭据及由其父目录推断的用量统计 / api_keys.json /
# 余额缓存都必须在卷内,否则容器重建即丢),而 config.json / CREDENTIALS_PATH / --credentials
# 三层显式配置依然按文档的优先级各自生效。
ENV HOST=0.0.0.0
WORKDIR /app
COPY --from=builder /build/kiro2api /usr/local/bin/kiro2api
COPY docker-entrypoint.sh /usr/local/bin/docker-entrypoint.sh
RUN chmod +x /usr/local/bin/docker-entrypoint.sh && mkdir -p /app/data && chown -R appuser:appuser /app
EXPOSE 8080
# 探活端口按与应用完全相同的优先级解析:PORT 环境变量 > 挂载的 config.json 里的 port > 8080。
# 少了 PORT 这一层,用户按文档改端口后健康检查仍打旧端口,容器会永远 unhealthy。
HEALTHCHECK --interval=30s --timeout=10s --start-period=20s --retries=3 \
    CMD P="${PORT:-$(grep -oE '"port"[[:space:]]*:[[:space:]]*[0-9]+' /app/data/config.json 2>/dev/null | grep -oE '[0-9]+' | head -1)}"; wget -q -O /dev/null "http://localhost:${P:-8080}/health" || exit 1
ENTRYPOINT ["docker-entrypoint.sh"]
CMD ["kiro2api", "-c", "/app/data/config.json"]
