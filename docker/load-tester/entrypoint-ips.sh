#!/usr/bin/env bash
set -e

echo "[Load Tester] Configuring 20 Virtual Secondary IPs (172.28.0.10 - 172.28.0.29)..."

IFACE="eth0"

for i in $(seq 10 29); do
    ip addr add 172.28.0.$i/16 dev $IFACE 2>/dev/null || true
done

echo "[Load Tester] Secondary IPs assigned to $IFACE:"
ip -br addr show $IFACE

# Maximize file descriptors for load tester container
ulimit -n 1048576 2>/dev/null || true

# Maximize ephemeral port range and enable tw_reuse
sysctl -w net.ipv4.ip_local_port_range="1024 65535" 2>/dev/null || true
sysctl -w net.ipv4.tcp_tw_reuse=1 2>/dev/null || true
sysctl -w net.ipv4.tcp_fin_timeout=15 2>/dev/null || true

exec "$@"
