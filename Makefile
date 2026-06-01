.PHONY: help install dev build server release test test-quiet bots clean reset-db bundle-sodium

help: ## Show this help
	@grep -E '^[a-zA-Z_-]+:.*?## .*$$' $(MAKEFILE_LIST) | sort | \
		awk 'BEGIN {FS = ":.*?## "}; {printf "\033[36m%-18s\033[0m %s\n", $$1, $$2}'

dev: ## Build the Rust server in debug mode
	cargo build

build: ## Build the Rust server in debug mode (alias for dev)
	cargo build

release: ## Build the Rust server in release mode
	cargo build --release

server: ## Run the Rust server (cargo run)
	cargo run

test: ## Run all native Rust integration tests
	cargo test

test-quiet: ## Run all tests with minimal output
	cargo test --quiet

bots: ## Run the bot simulation against a running server
	uv run python scripts/bots.py

clean: ## Remove Rust + Python build artifacts
	cargo clean
	rm -rf desktop/src-tauri/target
	find . -name "__pycache__" -type d -prune -exec rm -rf {} +

reset-db: ## Delete the database and start fresh
	rm -rf data/chat.lance
	@echo "Database reset. Restart the server to recreate tables."

bundle-sodium: ## Rebuild the vendored libsodium bundle
	bash scripts/bundle-sodium.sh
