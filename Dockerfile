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

# trixie (not bookworm): runs the SAME chromium build the pierre server image
# proves in production, eliminating a base-image version divergence for the
# headless-Chrome launch. `apt-get upgrade -y` + the cache-bust ARG patch CVEs
# on every build — this container renders untrusted provider login pages, so
# staying current matters (mirrors the pierre image's posture).
FROM debian:trixie-slim

# 2026-08-21: chromium 151.0.7922.169-1~deb13u1 closes the CVE-2026-76033..76052
# batch (22 HIGH — site-isolation bypass, arbitrary code execution in Dawn and
# V8, information disclosure). The same batch gated the platform's dev deploy on
# the same day; this image ships the same chromium, so it reds on its next build
# until the layer is rebuilt too. Bumping the two together is the whole point of
# the shared ARG.
#
# 2026-08-31: chromium 151.0.7922.173-1~deb13u1 closes CVE-2026-76018, arbitrary
# code execution via a crafted file in the Import component (2 HIGH — chromium,
# chromium-common). It gated the platform's dev deploy the same day on a commit
# that touched no Docker and no dependency: the cached apt layer was still
# serving .169. Bumped here in the same session rather than left for this image's
# next build to discover, which is the lag the ARG exists to prevent.
#
# 2026-09-03: chromium 152.0.7977.75-1~deb13u1 closes CVE-2026-84349 (use after
# free), CVE-2026-84351 (buffer overflow in GPU) and CVE-2026-84357 (improper
# input validation) — 6 HIGH on the platform image, the same three CVEs counted
# once per source package (chromium, chromium-common). Bumped here in the same
# session the platform bumped, which is the lag the shared ARG exists to prevent:
# this image had been left a batch behind twice running, and its next build would
# have red on CVEs already fixed elsewhere.
#
# A major-version jump (151 -> 152) is the one that can break scraping rather
# than just patch it, so it was tested rather than assumed. `tests/
# automation_canary.rs` — real `launch_browser` + `apply_minimal_stealth` against
# the local fixture — passes on the 152 engine (verified against Chrome
# 152.0.7977.76, one patch off Debian's build): cdp_runtime_enable, webdriver_set,
# plugins_empty, languages_empty, notif_mismatch and ua_headless_substring all
# clean. Re-run it on the next major bump; that is what it is for.
ARG APT_SECURITY_EPOCH=2026-09-03

# nodejs + npm + git: required by the Copilot CLI the vision login's LLM
# provider (embacle copilot_headless) spawns at runtime.
RUN echo "apt security epoch: ${APT_SECURITY_EPOCH}" \
    && apt-get update && apt-get upgrade -y \
    && apt-get install -y --no-install-recommends \
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
