# syntax=docker/dockerfile:1.7

FROM --platform=$TARGETPLATFORM debian:bookworm-slim AS runner
ARG TARGETOS
ARG TARGETARCH
RUN <<'EOF'
set -eu
apt-get update
apt-get install -y --no-install-recommends ca-certificates tzdata wget
rm -rf /var/lib/apt/lists/*
mkdir -p /app
EOF
WORKDIR /app
COPY --chmod=755 docker-artifacts/${TARGETOS}-musl/${TARGETARCH}/komf-app ./komf-app
# WebUI 构建产物（release 流程先 `npm run build` 生成 web/dist，再随上下文送入）；
# 不存在则跳过——后端无产物时 /ui 返回 404 说明，不影响 API。
COPY web/dist ./ui
VOLUME /config
ENV KOMF_CONFIG_DIR="/config"
ENV LANG='C.UTF-8' LC_ALL='C.UTF-8'
ENTRYPOINT ["./komf-app"]
HEALTHCHECK --interval=30s --timeout=5s --start-period=15s --retries=3 CMD wget -qO- http://127.0.0.1:8085/api/health || exit 1
EXPOSE 8085
LABEL org.opencontainers.image.source="https://github.com/Snd-R/komf"
