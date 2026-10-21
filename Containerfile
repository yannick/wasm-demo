# agent-demo: one static binary in an empty (FROM scratch) image, no OS, no dependencies.
#
# The web UI and the GridCraft sheet (WebAssembly) are built on the host first and embedded by
# the binary (`just apple-build` does it: `just web` and `just sheet`), so the build stage only
# needs Rust and protoc. The build context is a staged copy of this repo and GridCraft
# (.build-ctx/app/agent-demo and .build-ctx/rust/gridcraft), laid out so that GridCraft's
# relative path in Cargo.toml resolves the same way inside the image.
#
#   just apple-build    (Apple container CLI)
#   just apple-run      (http://127.0.0.1:7400, the API key from ./.env)

# ---- build stage ----------------------------------------------------------
FROM rust:1.99.1-alpine AS build
RUN apk add --no-cache musl-dev build-base protobuf-dev
WORKDIR /src
COPY rust/gridcraft/ rust/gridcraft/
COPY app/agent-demo/ app/agent-demo/
WORKDIR /src/app/agent-demo
# web/dist (UI + sheet) comes prebuilt from the host: no node or wasm toolchain here.
ENV AGENT_DEMO_SKIP_WEB_BUILD=1 CARGO_TARGET_DIR=/src/target
RUN test -f web/dist/static/islands/AgentConsole.js && test -f web/dist/static/sheet/sheet-web_bg.wasm \
 && cargo build --release --locked -p agent-demo \
 && cp /src/target/release/agent-demo /agent-demo \
 && mkdir -p /out/data

# ---- runtime stage --------------------------------------------------------
FROM scratch
COPY --from=build /agent-demo /agent-demo
# Settings live here; mount a volume to keep them.
COPY --from=build /out/data /data
ENV AGENT_DEMO_LISTEN=0.0.0.0:7400 AGENT_DEMO_DATA_DIR=/data RUST_LOG=info
EXPOSE 7400
VOLUME ["/data"]
ENTRYPOINT ["/agent-demo"]
