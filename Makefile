.PHONY: help build clean bench-phase1 bench-phase2 bench-phase3 prep-host stop

help:
	@echo "Hyperion High-Concurrency Rust API Challenge Benchmark"
	@echo "=========================================================="
	@echo "make bench-phase1   : Build & run Phase 1 (Axum + Nginx baseline ~14k conns)"
	@echo "make bench-phase2   : Build & run Phase 2 (io_uring bare-metal ~64k conns)"
	@echo "make bench-phase3   : Build & run Phase 3 (Multi-Core SO_REUSEPORT 150k+ conns)"
	@echo "make prep-host      : Run host-level kernel & conntrack prep for Phase 3"
	@echo "make build          : Build all Docker images"
	@echo "make stop           : Stop all running containers"
	@echo "make clean          : Tear down containers, networks, and volumes"

build:
	docker compose --profile phase1 --profile phase2 --profile phase3 build

bench-phase1:
	@echo ">>> Starting Phase 1 Benchmark: Baseline Axum + Nginx Reverse Proxy"
	docker compose down -v --remove-orphans
	docker compose --profile phase1 up --build --abort-on-container-exit load-tester-phase1
	docker compose down

bench-phase2:
	@echo ">>> Starting Phase 2 Benchmark: Maxxed-Out Bare-Metal io_uring Server"
	docker compose down -v --remove-orphans
	docker compose --profile phase2 up --build --abort-on-container-exit load-tester-phase2
	docker compose down

bench-phase3:
	@echo ">>> Starting Phase 3 Benchmark: Kernel Unleashed (Multi-Core SO_REUSEPORT)"
	docker compose down -v --remove-orphans
	docker compose --profile phase3 up --build --abort-on-container-exit load-tester-phase3
	docker compose down

prep-host:
	sudo bash scripts/host-kernel-prep.sh

stop:
	docker compose down

clean:
	docker compose down -v --remove-orphans
