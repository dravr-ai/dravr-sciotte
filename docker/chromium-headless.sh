#!/bin/sh
# ABOUTME: Wrapper for headless Chromium in containers without X11/Wayland (Cloud Run).
# ABOUTME: Injects --ozone-platform=headless so chromiumoxide can launch Chrome with no display.
#
# SPDX-License-Identifier: MIT OR Apache-2.0
# Copyright (c) 2026 dravr.ai
#
# Debian's chromium still initializes the X11 Ozone platform under
# chromiumoxide's `--headless=new`, dying with "Missing X server or $DISPLAY"
# in a display-less container. Forcing the headless Ozone backend fixes both
# the headless and (vision-login) headed launch paths. Exec the real binary
# directly to bypass Debian's /usr/bin/chromium wrapper, then pass through the
# caller's flags. Mirrors the pierre server image's proven wrapper.

exec /usr/lib/chromium/chromium \
    --ozone-platform=headless \
    --disable-software-rasterizer \
    "$@"
