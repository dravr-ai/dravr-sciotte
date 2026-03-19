# ABOUTME: Multi-stage Docker build for sport activity scraper server and MCP binaries
# ABOUTME: Runtime uses debian:bookworm-slim with Chromium for headless scraping and streaming
#
# SPDX-License-Identifier: MIT OR Apache-2.0
# Copyright (c) 2026 dravr.ai

FROM rust:1-bookworm AS builder
WORKDIR /build
COPY . .
RUN cargo build --release -p dravr-sciotte-server -p dravr-sciotte-mcp

FROM debian:bookworm-slim

RUN apt-get update && apt-get install -y --no-install-recommends \
    ca-certificates \
    chromium \
    fonts-liberation \
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
    xdg-utils \
    && rm -rf /var/lib/apt/lists/*

RUN useradd --create-home --shell /bin/bash dravr

COPY --from=builder /build/target/release/dravr-sciotte-server /usr/local/bin/
COPY --from=builder /build/target/release/dravr-sciotte-mcp /usr/local/bin/
COPY --from=builder /build/providers/ /app/providers/

ENV CHROME_PATH=/usr/bin/chromium

USER dravr
WORKDIR /home/dravr

EXPOSE 3000
ENTRYPOINT ["dravr-sciotte-server"]
CMD ["serve", "--host", "0.0.0.0"]
