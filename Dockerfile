# syntax=docker/dockerfile:1
# FloraStack in the browser, self-hosted: builds the static site (`cargo xtask web`) and serves it with nginx and the
# headers it needs (packaging/web/nginx.conf). From the repository's root:
#
#     docker build -t florastack-web .
#     docker run --rm -p 8080:8080 florastack-web
#
# then open http://localhost:8080/. The server runs as an unprivileged user on port 8080. Put it behind a proxy that
# terminates HTTPS for other machines (browsers keep threads and the private file system to secure pages, and
# localhost), and make sure the proxy passes the Cross-Origin-* headers on (packaging/web/README.md).
#
# With a site built already, `docker build --build-context site=target/web-site -t florastack-web .` skips the build
# stage and serves that one (CI checks the image's headers this way). The base images are pinned to a digest; update
# them deliberately.

FROM rust:1.99.1-slim-trixie@sha256:59f72d5265cfcf281fcfa4d990e75f09c53f911def9913c47a4a89dca690a887 AS build
WORKDIR /src
# The nightly that rebuilds the standard library with threads, and the wasm-bindgen CLI at Cargo.lock's version.
RUN rustup toolchain install nightly-2026-11-01 --profile minimal --component rust-src \
    && cargo install wasm-bindgen-cli --version 0.2.129 --locked
COPY . .
RUN cargo xtask web --out /site

# The site's files alone, at the root: what `--build-context site=<folder>` replaces.
FROM scratch AS site
COPY --from=build /site /

FROM nginxinc/nginx-unprivileged:1.30-alpine@sha256:f5116ed4606f3eb015dd4a8e28cb9b351476dc0ad4bc6e9f93b19cec130cdf03
COPY packaging/web/nginx.conf /etc/nginx/conf.d/default.conf
COPY --from=site / /usr/share/nginx/html
EXPOSE 8080
