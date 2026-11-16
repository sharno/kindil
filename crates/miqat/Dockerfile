FROM rust:1-slim AS build
WORKDIR /src
COPY . .
RUN cargo build --release --locked

# TLS roots are compiled in (webpki-roots), so no CA bundle is needed at runtime.
FROM gcr.io/distroless/cc-debian12
COPY --from=build /src/target/release/miqat /usr/local/bin/miqat
EXPOSE 8080
ENTRYPOINT ["/usr/local/bin/miqat", "--config", "/config/config.toml", "serve"]
