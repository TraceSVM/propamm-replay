FROM rust:1-bookworm AS builder

RUN apt-get update && apt-get install -y --no-install-recommends \
        protobuf-compiler \
    && rm -rf /var/lib/apt/lists/*

WORKDIR /build
COPY Cargo.toml Cargo.lock ./
COPY recorder/ recorder/
COPY quoter/ quoter/

RUN --mount=type=cache,target=/usr/local/cargo/registry \
    --mount=type=cache,target=/build/target \
    cargo build --release \
    && mkdir -p /out \
    && cp target/release/recorder target/release/quoter /out/

FROM python:3.12-slim-bookworm

RUN apt-get update && apt-get install -y --no-install-recommends \
        ca-certificates \
        libgomp1 \
    && rm -rf /var/lib/apt/lists/*

WORKDIR /app

COPY requirements.txt ./
RUN pip install --no-cache-dir -r requirements.txt

COPY --from=builder /out/recorder /out/quoter /usr/local/bin/

COPY scripts/analysis.py scripts/analysis.py
COPY pools_sol_usdc/ pools_sol_usdc/

ENV RUST_LOG=info \
    MPLCONFIGDIR=/tmp/matplotlib

VOLUME ["/data/recordings"]

CMD ["recorder", "--help"]
