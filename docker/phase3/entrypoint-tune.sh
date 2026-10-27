#!/usr/bin/env bash
set -e

echo "[Phase 3] Applying Linux Kernel & Network Tuning..."

# 1. Bypass nf_conntrack connection tracking to prevent packet drops at 65,536 connections
if command -v iptables >/dev/null 2>&1; then
    iptables -t raw -A PREROUTING -j NOTRACK 2>/dev/null || echo "Warning: NOTRACK on PREROUTING skipped (requires NET_ADMIN)"
    iptables -t raw -A OUTPUT -j NOTRACK 2>/dev/null || echo "Warning: NOTRACK on OUTPUT skipped (requires NET_ADMIN)"
fi

# 2. Maximize file descriptors
ulimit -n 1048576 2>/dev/null || true

# 3. Apply sysctl socket backlogs, buffer minimums, and port reuse
sysctl -w net.core.somaxconn=65535 2>/dev/null || true
sysctl -w net.ipv4.tcp_max_syn_backlog=65535 2>/dev/null || true
sysctl -w net.core.netdev_max_backlog=65535 2>/dev/null || true
sysctl -w net.ipv4.ip_local_port_range="1024 65535" 2>/dev/null || true
sysctl -w net.ipv4.tcp_tw_reuse=1 2>/dev/null || true
sysctl -w net.ipv4.tcp_fin_timeout=15 2>/dev/null || true
sysctl -w net.ipv4.tcp_rmem="4096 16384 65536" 2>/dev/null || true
sysctl -w net.ipv4.tcp_wmem="4096 16384 65536" 2>/dev/null || true

echo "[Phase 3] Kernel tuning applied. Launching Phase 3 Kernel Unleashed..."
exec "$@"
