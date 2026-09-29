# 基础镜像走 mirror.gcr.io，本机直连 Docker Hub 会被拒绝。
FROM mirror.gcr.io/library/rust:1.90-bookworm AS build
WORKDIR /src
COPY Cargo.toml Cargo.lock ./
COPY crates crates
COPY bin bin
COPY xtask xtask
RUN cargo build --release -p nm-domain

FROM mirror.gcr.io/library/debian:bookworm-slim
RUN mkdir -p /data
COPY --from=build /src/target/release/nm-domain /usr/local/bin/nm-domain
EXPOSE 8080
ENTRYPOINT ["/usr/local/bin/nm-domain"]
CMD ["--listen", "0.0.0.0:8080", "--db", "/data/nm-domain.redb", "--state", "/data/domaind.state.json"]
