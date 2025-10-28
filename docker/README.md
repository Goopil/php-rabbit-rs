# RabbitRs Docker Builders

Prebuilt Dockerfiles that compile the `rabbit_rs` extension for different
platforms. They are used by GitHub Actions and can be invoked locally.

## linux-gnu

Targets glibc-based distributions (Ubuntu, Debian, etc.) using the
`ghcr.io/shivammathur/php` images. Example:

```bash
docker build -f docker/linux-gnu/Dockerfile \
  --build-arg PHP_VERSION=8.2 \
  --build-arg RUST_VERSION=1.89.0 \
  -t rabbit-rs-linux-gnu:8.2 .
docker create --name rabbit-rs-linux-gnu rabbit-rs-linux-gnu:8.2
docker cp rabbit-rs-linux-gnu:/artifacts/rabbit_rs.so ./dist/
docker rm rabbit-rs-linux-gnu
```

## linux-alpine

Targets musl-based distributions using the Alpine variants of the same base
images. Example:

```bash
docker build -f docker/linux-alpine/Dockerfile \
  --build-arg PHP_VERSION=8.2 \
  --build-arg RUST_VERSION=1.89.0 \
  -t rabbit-rs-linux-alpine:8.2 .
docker create --name rabbit-rs-linux-alpine rabbit-rs-linux-alpine:8.2
docker cp rabbit-rs-linux-alpine:/artifacts/rabbit_rs.so ./dist/
docker rm rabbit-rs-linux-alpine
```

Both Dockerfiles accept the build arguments:

* `PHP_VERSION` (default `8.2`)
* `PHP_VARIANT` (`cli`, `cli-debug`, `cli-alpine`, …)
* `RUST_VERSION` (default `1.89.0`)

The compiled extension is placed in `/artifacts/rabbit_rs.so`.

