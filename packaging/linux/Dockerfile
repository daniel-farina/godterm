# Build image for GodTerm's Linux release artifacts. Ubuntu 22.04 sets the
# glibc baseline (2.35: Ubuntu 22.04+, Debian 12+, Fedora 36+, RHEL 10).
# The prebuilt onnxruntime that ort links needs GNU libstdc++, so the
# binaries are built natively here rather than cross linked with zig.
# BASE is the per architecture official image (amd64/ubuntu:22.04 or
# arm64v8/ubuntu:22.04), so the legacy builder, which ignores --platform,
# still builds for the right architecture.
ARG BASE=ubuntu:22.04
FROM ${BASE}
ARG RUST_VERSION=stable
ENV DEBIAN_FRONTEND=noninteractive \
    RUSTUP_HOME=/usr/local/rustup \
    CARGO_HOME=/usr/local/cargo \
    PATH=/usr/local/cargo/bin:$PATH
RUN apt-get update && apt-get install -y --no-install-recommends \
      build-essential pkg-config libasound2-dev libssl-dev ca-certificates curl git file desktop-file-utils \
    && rm -rf /var/lib/apt/lists/*
RUN curl -sSf https://sh.rustup.rs | sh -s -- -y --profile minimal --default-toolchain "$RUST_VERSION" \
    && cargo install --locked cargo-deb cargo-generate-rpm \
    && rm -rf /usr/local/cargo/registry /usr/local/cargo/git
# appimagetool (an AppImage itself; run with --appimage-extract-and-run, no FUSE).
ARG APPIMAGETOOL=1.9.0
RUN curl -sSfL -o /usr/local/bin/appimagetool \
      "https://github.com/AppImage/appimagetool/releases/download/${APPIMAGETOOL}/appimagetool-$(uname -m).AppImage" \
    && chmod +x /usr/local/bin/appimagetool
WORKDIR /src
