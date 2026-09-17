# Akramium Home for a NAS or any Docker host. Builds a static binary from this source tree
# and ships it alone: no shell, no package manager, nothing else to keep patched.
#
#   docker build -f deploy/nas/Dockerfile -t akramium-home .
ARG RUST_VERSION=1
FROM rust:${RUST_VERSION}-alpine AS build
RUN apk add --no-cache build-base
WORKDIR /src
COPY . .
RUN cargo build --release --locked -p akramium-home \
 && mkdir -p /out/data && cp target/release/akramium-home /out/ && chown -R 1000:1000 /out/data

FROM scratch
COPY --from=build /out/akramium-home /akramium-home
COPY deploy/nas/home.toml /etc/akramium-home/home.toml
# The data directory exists in the image, owned by the user the daemon runs as, so a fresh
# named volume starts out writable.
COPY --from=build --chown=1000:1000 /out/data /var/lib/akramium-home
USER 1000:1000
VOLUME /var/lib/akramium-home
EXPOSE 11720
HEALTHCHECK --interval=30s --timeout=5s --start-period=5s CMD ["/akramium-home", "--config", "/etc/akramium-home/home.toml", "--health"]
ENTRYPOINT ["/akramium-home", "--config", "/etc/akramium-home/home.toml"]
