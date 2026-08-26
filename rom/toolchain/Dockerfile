# syntax=docker/dockerfile:1.7
# Pinned bare-metal RV32IM toolchain image for ClickDOOM (rom workstream,
# SPEC §1 / §4). Every input here is pinned by content digest or verified
# checksum, never a floating tag, so `docker build` reproduces byte-identical
# tool binaries on any host. Bumping either pin is a `ci:` PR (CLAUDE.md).
#
# Toolchain: xPack GNU RISC-V Embedded GCC v15.2.0-1 (`riscv-none-elf-*`) —
# a bare-metal ("no known OS") target, which matches the charter: rom/ brings
# its own crt0 and libc shims rather than linking newlib against an OS that
# doesn't exist here.
# https://github.com/xpack-dev-tools/riscv-none-elf-gcc-xpack/releases/tag/v15.2.0-1

FROM debian:bookworm-slim@sha256:88200866dfff7ea7f5cbcb6ec7c8a701889efe6fe859fe64d6990e4b07ea4171

ARG TOOLCHAIN_VERSION=15.2.0-1
ARG TARGETARCH

RUN set -eu; \
    apt-get update; \
    apt-get install -y --no-install-recommends ca-certificates curl xz-utils; \
    rm -rf /var/lib/apt/lists/*

# One case arm per supported TARGETARCH, each pinned to its own verified
# sha256 (from the GitHub Releases API asset digest, not xpack's own .sha
# file, so the check is independent of the upload it is checking). Adding a
# platform means adding a line here with a freshly verified digest.
RUN set -eu; \
    case "${TARGETARCH}" in \
      amd64) \
        ARCHIVE="xpack-riscv-none-elf-gcc-${TOOLCHAIN_VERSION}-linux-x64.tar.gz"; \
        SHA256="aaaa8060c914851a3e5ee1ba82cc3d6f80972f90638a05c6e823a37557a33758"; \
        ;; \
      arm64) \
        ARCHIVE="xpack-riscv-none-elf-gcc-${TOOLCHAIN_VERSION}-linux-arm64.tar.gz"; \
        SHA256="4e60e2a54c16385e4e2476d08240f857495d5a61609d97e1ee49f72875a6ec1e"; \
        ;; \
      *) \
        echo "unsupported TARGETARCH: ${TARGETARCH}" >&2; \
        exit 1 \
        ;; \
    esac; \
    curl -fsSL -o "/tmp/${ARCHIVE}" \
      "https://github.com/xpack-dev-tools/riscv-none-elf-gcc-xpack/releases/download/v${TOOLCHAIN_VERSION}/${ARCHIVE}"; \
    echo "${SHA256}  /tmp/${ARCHIVE}" | sha256sum -c -; \
    mkdir -p /opt/riscv; \
    tar -xzf "/tmp/${ARCHIVE}" -C /opt/riscv --strip-components=1; \
    rm "/tmp/${ARCHIVE}"

ENV PATH="/opt/riscv/bin:${PATH}"
ENV LC_ALL=C
ENV TZ=UTC
ENV SOURCE_DATE_EPOCH=0

WORKDIR /work
