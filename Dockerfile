FROM rust:1-alpine AS build

# build-base: musl cc/make; cmake + perl: libssh2-sys / vendored OpenSSL builds.
RUN apk add --no-cache build-base cmake perl musl-dev

WORKDIR /build
COPY . .

# Build a musl static binary for the native Docker builder architecture.
# The default git2 dependency features provide SSH, HTTPS, and vendored OpenSSL.
RUN cargo build --release

# distroless has no shell/chown: stage the runtime layout here instead.
# nonroot in gcr.io/distroless/cc is uid/gid 65532.
RUN install -d -m 0755 -o 65532 -g 65532 /out/data \
    && cp target/release/memoro /out/memoro

FROM gcr.io/distroless/cc

COPY --from=build --chown=65532:65532 /out/memoro /usr/local/bin/memoro
COPY --from=build --chown=65532:65532 /out/data /data

ENV MEMORO_HOME=/data
VOLUME /data
EXPOSE 8000
USER nonroot:nonroot

CMD ["memoro", "serve", "--transport", "http", "--host", "0.0.0.0"]
