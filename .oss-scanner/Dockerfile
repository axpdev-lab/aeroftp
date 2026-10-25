# AeroFTP build image for Anthropic's OSS scanner (https://github.com/anthropics/oss-scanner).
# Contract: the scanner clones the repository and builds this file with the checkout as the
# build context. Network access is available during the build and removed afterwards, so
# every dependency the audit may need is fetched here.
FROM ubuntu:24.04@sha256:68ee22004a9109b44973313b02ff7b1de250f1de0f5ecb628d73f579aaff33ac
SHELL ["/bin/bash", "-o", "pipefail", "-c"]
ENV DEBIAN_FRONTEND=noninteractive

# Toolchain + Tauri's Linux dependencies (see CONTRIBUTING.md -> Development Setup).
# ssh2 vendors OpenSSL (perl on PATH), whisper-rs-sys vendors whisper.cpp and runs
# bindgen (libclang), aws-lc-rs builds with cmake.
RUN apt-get update && apt-get install -y --no-install-recommends \
      ca-certificates curl xz-utils git build-essential clang libclang-dev cmake ninja-build \
      pkg-config perl python3 \
      libwebkit2gtk-4.1-dev libappindicator3-dev librsvg2-dev patchelf libfuse3-dev \
      libssl-dev libglib2.0-dev libgtk-3-dev libsoup-3.0-dev \
    && rm -rf /var/lib/apt/lists/*

# Versioned official archives; SHA-256 values from Node's v24.22.1 release
# SHASUMS256.txt and Rust's 2026-07-16 channel-rust-1.97.1.toml.
# Verify bytes BEFORE extracting or running an installer. Keep both image
# architectures pinned; a new architecture needs its own reviewed hashes.
RUN set -eu; \
    case "$(dpkg --print-architecture)" in \
      amd64) node_arch=x64; rust_arch=x86_64; \
        node_sha="6c172a9ab6718f7b801fb5a7bd486af2ee5e32758086a04b84fcce2aa77095e6"; \
        rust_sha="88f28fa9af20594179f85d6df67078dfd6fa93e2f6da5e1e9b0ac4997988ca4f" ;; \
      arm64) node_arch=arm64; rust_arch=aarch64; \
        node_sha="dbabf7a1cbf947e6059ffad2c27b80041db97dea1e553b62afdee7f13fca336e"; \
        rust_sha="9a7a2c336b4787f1b72f6bab7c35d5b7af2fd03cbd39b4fc721466a70d402a7d" ;; \
      *) echo "Unsupported scanner architecture: $(dpkg --print-architecture)" >&2; exit 1 ;; \
    esac; \
    cd /tmp; \
    curl --proto '=https' --tlsv1.2 -fsSLo node.tar.xz \
      "https://nodejs.org/dist/v24.22.1/node-v24.22.1-linux-${node_arch}.tar.xz"; \
    printf '%s  node.tar.xz\n' "$node_sha" > node.sha256; \
    sha256sum --check --strict node.sha256; \
    tar -xJf node.tar.xz -C /usr/local --strip-components=1; \
    curl --proto '=https' --tlsv1.2 -fsSLo rust.tar.xz \
      "https://static.rust-lang.org/dist/2026-07-16/rust-1.97.1-${rust_arch}-unknown-linux-gnu.tar.xz"; \
    printf '%s  rust.tar.xz\n' "$rust_sha" > rust.sha256; \
    sha256sum --check --strict rust.sha256; \
    tar -xJf rust.tar.xz; \
    "./rust-1.97.1-${rust_arch}-unknown-linux-gnu/install.sh" \
      --prefix=/usr/local \
      --components="rustc,cargo,rust-std-${rust_arch}-unknown-linux-gnu,rustfmt-preview,clippy-preview"; \
    rm -rf node.tar.xz node.sha256 rust.tar.xz rust.sha256 rust-1.97.1-*
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
