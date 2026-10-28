#!/usr/bin/env bash
set -e

echo "[Phase 4] Configuring Kernel-Bypass User-Space Micro-TCP..."

# 1. Drop TCP port 80 in kernel input chain so kernel never sends TCP RST
if command -v iptables >/dev/null 2>&1; then
    iptables -A INPUT -p tcp --dport 80 -j DROP 2>/dev/null || echo "Warning: iptables DROP port 80 skipped (requires NET_ADMIN)"
    iptables -t raw -A PREROUTING -j NOTRACK 2>/dev/null || true
    iptables -t raw -A OUTPUT -j NOTRACK 2>/dev/null || true
fi

# 2. Maximize file descriptors and memory locking
ulimit -n 1048576 2>/dev/null || true
ulimit -l unlimited 2>/dev/null || true

# 3. Enable promiscuous mode on eth0 for user-space raw packet capture
ip link set dev eth0 promisc on 2>/dev/null || true

# 4. Tune buffer sizes
sysctl -w net.core.rmem_max=67108864 2>/dev/null || true
sysctl -w net.core.wmem_max=67108864 2>/dev/null || true
sysctl -w net.core.rmem_default=16777216 2>/dev/null || true
sysctl -w net.core.wmem_default=16777216 2>/dev/null || true

echo "[Phase 4] Kernel-bypass network stack configured. Launching Micro-TCP Engine..."
exec "$@"
