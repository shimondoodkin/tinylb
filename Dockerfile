# Multi-stage build: compile a static-musl binary on Alpine, then drop it
# into a minimal Alpine runtime. The result is a ~10 MB image suitable
# for scratch/distroless-style deployments.

FROM rust:1.83-alpine AS build
RUN apk add --no-cache musl-dev
WORKDIR /app
COPY Cargo.toml Cargo.lock ./
COPY src ./src
RUN cargo build --release --locked

FROM alpine:3.20
RUN apk add --no-cache ca-certificates
COPY --from=build /app/target/release/tinylb /usr/local/bin/tinylb
EXPOSE 8080
ENTRYPOINT ["/usr/local/bin/tinylb"]
CMD ["/etc/tinylb/lb.toml"]
