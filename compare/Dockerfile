# Builds the C original (ssleert/riffdb) and this port in one Linux image, so
# both run on the same kernel, CPU and loopback. Build from the repo root:
#
#   docker build -f compare/Dockerfile -t riffdb-compare .
#   docker run --rm riffdb-compare            # see compare/run.sh for options
FROM rust:1-bookworm

RUN apt-get update \
 && apt-get install -y --no-install-recommends cmake curl procps \
 && rm -rf /var/lib/apt/lists/*

# C original, release config from its own CMakeLists.txt (-Ofast -march=native,
# LTO, mimalloc). libs.sh clones mimalloc and wolfssl at their default branch.
ARG RIFFDB_C_REV=cb5b374c959704904eee470fd7ca0f968a91b025
RUN git clone https://github.com/ssleert/riffdb.git /c \
 && cd /c && git checkout -q "$RIFFDB_C_REV" \
 && sh libs.sh \
 && cmake -B build -DCMAKE_BUILD_TYPE=Release -Wno-dev >/dev/null \
 && cmake --build build --parallel --target riffdb >/dev/null \
 && { echo "riffdb   $(git rev-parse --short HEAD)"; \
      echo "mimalloc $(git -C libs/mimalloc rev-parse --short HEAD)"; \
      echo "wolfssl  $(git -C libs/wolfssl rev-parse --short HEAD)"; } > /c/REVISIONS

# This port.
WORKDIR /rust
COPY Cargo.toml Cargo.lock ./
COPY .cargo .cargo
COPY src src
COPY examples examples
COPY tests tests
RUN cargo build --release --bins --examples

COPY compare/run.sh /run.sh
ENTRYPOINT ["sh", "/run.sh"]
