# AeroFTP build image for Anthropic's OSS scanner (https://github.com/anthropics/oss-scanner).
# Contract: the scanner clones the repository and builds this file with the checkout as the
# build context. Network access is available during the build and removed afterwards, so
# every dependency the audit may need is fetched here.
FROM ubuntu:24.04
SHELL ["/bin/bash", "-o", "pipefail", "-c"]
ENV DEBIAN_FRONTEND=noninteractive

# Toolchain + Tauri's Linux dependencies (see CONTRIBUTING.md -> Development Setup).
# ssh2 vendors OpenSSL (perl on PATH), whisper-rs-sys vendors whisper.cpp and runs
# bindgen (libclang), aws-lc-rs builds with cmake.
RUN apt-get update && apt-get install -y --no-install-recommends \
      ca-certificates curl git build-essential clang libclang-dev cmake ninja-build \
      pkg-config perl python3 \
      libwebkit2gtk-4.1-dev libappindicator3-dev librsvg2-dev patchelf libfuse3-dev \
      libssl-dev libglib2.0-dev libgtk-3-dev libsoup-3.0-dev \
    && rm -rf /var/lib/apt/lists/*

# Node.js 24 LTS, the version CI builds on.
RUN curl -fsSL https://deb.nodesource.com/setup_24.x | bash - \
    && apt-get install -y --no-install-recommends nodejs \
    && rm -rf /var/lib/apt/lists/*

# Rust pinned to the same toolchain as rust-toolchain.toml (1.97.1), so the image
# matches what CI lints and builds with.
RUN curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs \
      | sh -s -- -y --default-toolchain 1.97.1 --profile minimal
ENV PATH="/root/.cargo/bin:${PATH}"

# scanner contract: the checkout lives inside the image, at /src
COPY . /src
WORKDIR /src

# Frontend first: the Tauri build script expects ../dist to exist.
RUN npm ci && npm run build

# Fetch the locked graph while the network is available, including test dependencies.
# The later build/test commands must work offline. Debug builds retain symbols.
RUN cargo fetch --locked --manifest-path src-tauri/Cargo.toml \
    && cargo build --frozen --manifest-path src-tauri/Cargo.toml

# Compile every library test now: the offline scanner can rerun focused tests without
# fetching crates or rebuilding the full suite on its smaller analysis machine.
# Compilation errors must fail setup. Leave the slow full suite for a separate run;
# spending 15 minutes on it here eats into the scanner's 45-minute setup budget.
RUN cargo test --frozen --manifest-path src-tauri/Cargo.toml --lib --no-run

# Small, offline security regressions. A failed smoke test must fail setup.
RUN timeout 60 cargo test --frozen --manifest-path src-tauri/Cargo.toml --lib \
      ai_core::runner::worker_credentials::tests \
    && timeout 60 cargo test --frozen --manifest-path src-tauri/Cargo.toml --lib \
      ai_tools::session_grant_tests
RUN ./src-tauri/target/debug/aeroftp-cli --version
