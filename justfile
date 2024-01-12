default: fmt lint dev

fmt:
    cargo fmt --check

lint:
    cargo clippy --all-targets --all-features -- -W clippy::all -W \
        clippy::pedantic -W clippy::nursery -W rustdoc::all -D warnings

dev:
    cargo run

run:
    cargo run --release

build:
    cargo build

clean:
    cargo clean
