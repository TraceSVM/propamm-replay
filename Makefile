.PHONY: build record record-1h record-8h record-12h record-forever quote analyze clean \
        docker-build docker-record docker-quote docker-analyze \
        fmt fmt-rust fmt-python fmt-check lint lint-rust lint-python

GRPC_ENDPOINT ?= http://127.0.0.1:10000
POOLS_DIR     ?= pools_sol_usdc
OUTPUT_DIR    ?= /var/solana/data/recordings
DURATION      ?= 12h

DOCKER_UID     := $(shell id -u)
DOCKER_GID     := $(shell id -g)
RECORDINGS_DIR ?= $(OUTPUT_DIR)
export DOCKER_UID DOCKER_GID RECORDINGS_DIR

build:
	cargo build --release

record: build
	RUST_LOG=info ./target/release/recorder record \
		--grpc $(GRPC_ENDPOINT) \
		--pools $(POOLS_DIR) \
		--output $(OUTPUT_DIR) \
		--duration $(DURATION)

record-1h: DURATION=1h
record-1h: record

record-8h: DURATION=8h
record-8h: record

record-12h: DURATION=12h
record-12h: record

record-forever: build
	RUST_LOG=info ./target/release/recorder record \
		--grpc $(GRPC_ENDPOINT) \
		--pools $(POOLS_DIR) \
		--output $(OUTPUT_DIR)

SESSION ?=

quote: build
	@if [ -z "$(SESSION)" ]; then \
		echo "Usage: make quote SESSION=/var/solana/data/recordings/<timestamp>"; \
		exit 1; \
	fi
	RUST_LOG=info ./target/release/quoter quote --session $(SESSION)

analyze:
	@if [ -z "$(SESSION)" ]; then \
		echo "Usage: make analyze SESSION=/var/solana/data/recordings/<timestamp>"; \
		exit 1; \
	fi
	python3 scripts/analysis.py $(SESSION)

clean:
	cargo clean

fmt: fmt-rust fmt-python

fmt-rust:
	cargo fmt --all

fmt-python:
	ruff format scripts
	ruff check --fix scripts

fmt-check:
	cargo fmt --all -- --check
	ruff format --check scripts
	ruff check scripts

lint: lint-rust lint-python

lint-rust:
	cargo clippy --all-targets --release -- -D warnings

lint-python:
	ruff check scripts

docker-build:
	docker compose build recorder

docker-record: docker-build
	docker compose run --rm recorder

docker-quote: docker-build
	@if [ -z "$(SESSION)" ]; then \
		echo "Usage: make docker-quote SESSION=<session-id>"; \
		exit 1; \
	fi
	SESSION=$(SESSION) docker compose run --rm quoter

docker-analyze: docker-build
	@if [ -z "$(SESSION)" ]; then \
		echo "Usage: make docker-analyze SESSION=<session-id>"; \
		exit 1; \
	fi
	SESSION=$(SESSION) docker compose run --rm analyzer
