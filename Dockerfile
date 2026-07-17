# ABOUTME: Multi-stage Docker build for sport activity scraper server and MCP binaries
# ABOUTME: Runtime uses debian:bookworm-slim with Chromium for headless scraping and streaming
#
# SPDX-License-Identifier: MIT OR Apache-2.0
# Copyright (c) 2026 dravr.ai

FROM rust:1.96-bookworm AS builder
WORKDIR /build
COPY . .
# --features vision: the deployed service runs DRAVR_SCIOTTE_LOGIN_MODE=hybrid,
# where selector-login failures degrade to LLM screenshot reasoning instead of
# hard errors (validated live: the Strava/Google login path requires it).
RUN cargo build --release -p dravr-sciotte-server -p dravr-sciotte-mcp \
    --features dravr-sciotte-server/vision

FROM debian:bookworm-slim

# nodejs + npm + git: required by the Copilot CLI the vision login's LLM
# provider (embacle copilot_headless) spawns at runtime.
RUN apt-get update && apt-get install -y --no-install-recommends \
    ca-certificates \
    chromium \
    fonts-liberation \
    git \
    libappindicator3-1 \
    libasound2 \
    libatk-bridge2.0-0 \
    libatk1.0-0 \
    libcups2 \
    libdbus-1-3 \
    libdrm2 \
    libgbm1 \
    libgtk-3-0 \
    libnspr4 \
    libnss3 \
    libx11-xcb1 \
    libxcomposite1 \
    libxdamage1 \
    libxrandr2 \
    nodejs \
    npm \
    xdg-utils \
    && rm -rf /var/lib/apt/lists/*

# GitHub Copilot CLI for the vision LLM (ACP mode). Pinned for reproducible
# builds — must stay >=1.0.59, the first release whose `initialize` advertises
# the mcpCapabilities the native tool-calling bridge relies on (mirrors the
# pierre server image's pin; bump both together after validating).
ARG COPILOT_CLI_VERSION=1.0.59
RUN npm install -g "@github/copilot-linux-x64@${COPILOT_CLI_VERSION}" \
    && ln -sf "$(npm prefix -g)/bin/copilot-linux-x64" /usr/local/bin/copilot \
    && copilot --version

RUN useradd --create-home --shell /bin/bash dravr

# Headless-Chromium wrapper: Cloud Run has no X server, and Debian's chromium
# dies on X11 Ozone init under --headless=new without --ozone-platform=headless.
# The wrapper injects it (mirrors the pierre server image). Fail the build early
# if the expected chromium binary path is wrong for this base image.
COPY docker/chromium-headless.sh /usr/local/bin/chromium-headless
RUN chmod +x /usr/local/bin/chromium-headless \
    && test -x /usr/lib/chromium/chromium

COPY --from=builder /build/target/release/dravr-sciotte-server /usr/local/bin/
COPY --from=builder /build/target/release/dravr-sciotte-mcp /usr/local/bin/
COPY --from=builder /build/providers/ /app/providers/

# Point both the dravr-sciotte config (CHROME_PATH) and chromiumoxide's
# auto-detection (CHROME) at the headless wrapper so every launch path — the
# credential-login stack and the WebSocket streaming login — is display-less-safe.
ENV CHROME_PATH=/usr/local/bin/chromium-headless
ENV CHROME=/usr/local/bin/chromium-headless

USER dravr
WORKDIR /home/dravr

EXPOSE 3000
ENTRYPOINT ["dravr-sciotte-server"]
CMD ["serve", "--host", "0.0.0.0"]
