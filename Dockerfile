FROM rust:1-bookworm AS build
WORKDIR /app
COPY Cargo.toml Cargo.lock ./
COPY src ./src
COPY web ./web
COPY migrations ./migrations
RUN cargo build --locked --release
FROM debian:bookworm-slim
RUN apt-get update && apt-get install -y --no-install-recommends ca-certificates libssl3 && rm -rf /var/lib/apt/lists/*
COPY --from=build /app/target/release/noti-mesh /usr/local/bin/noti-mesh
COPY web/vendor/LICENSE /usr/share/licenses/noti-mesh/jkit-LICENSE
USER 65532:65532
EXPOSE 8080
ENTRYPOINT ["noti-mesh"]
