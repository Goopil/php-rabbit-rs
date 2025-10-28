# RabbitRs Development Guide

## Build Commands
- Build release: `cargo build --release`
- Build debug: `cargo build`
- Run tests: `bash run-test.sh`
- Run single test: `cd php-tests && php -d extension="../target/debug/librabbit_rs.dylib" ./vendor/bin/pest --filter TestName`

## Benchmark Commands
- Run benchmarks: `cd benchmarks && ./run-benchmark-rabbitrs.sh`
- Build extension before running benchmarks
- Requires RabbitMQ server running locally

## Code Style Guidelines
- Follow Rust naming conventions (snake_case for variables/functions, PascalCase for types)
- Use descriptive variable names
- Prefer explicit typing over inference when clarity is improved
- Error handling: Use `anyhow::Result` and `?` operator for propagation
- Imports: Group std, external crates, and local modules separately
- Format with `cargo fmt` before committing
- Add doc comments for public functions
- Use `tracing` for logging instead of println!

## Project Structure
- Core Rust implementation in `src/`
- PHP tests in `php-tests/`
- PHP benchmarks in `benchmarks/`
- Extension built as cdylib
- Single connection per PHP process model