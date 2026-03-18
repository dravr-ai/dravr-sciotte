# ABOUTME: Multi-stage Docker build for sport activity scraper server and MCP binaries
# ABOUTME: Runtime uses debian:bookworm-slim with Chromium for headless scraping
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
    && rm -rf /var/lib/apt/lists/*

RUN useradd --create-home --shell /bin/bash dravr

COPY --from=builder /build/target/release/dravr-sciotte-server /usr/local/bin/
COPY --from=builder /build/target/release/dravr-sciotte-mcp /usr/local/bin/

ENV CHROME_PATH=/usr/bin/chromium

USER dravr
WORKDIR /home/dravr

EXPOSE 3000
ENTRYPOINT ["dravr-sciotte-server"]
CMD ["serve", "--host", "0.0.0.0"]
