#!/usr/bin/env bash
# scripts/host-kernel-prep.sh
# Host-level kernel and conntrack preparation for Phase 3 benchmarks (150,000+ concurrent connections).
# Run with root privileges on the Linux or WSL2 host machine.

set -e

echo "=== Hyperion Host Kernel & Network Stack Preparation ==="

if [ "$EUID" -ne 0 ]; then
    echo "[-] Error: Please run as root (e.g. sudo bash scripts/host-kernel-prep.sh)"
    exit 1
fi

echo "[+] 1. Bypassing nf_conntrack connection tracking on bridge interfaces..."
# Bypass tracking so connection count can scale to 150k+ without table exhaustion
iptables -t raw -A PREROUTING -j NOTRACK 2>/dev/null || echo "[!] Notice: Raw PREROUTING rule already set or module not loaded"
iptables -t raw -A OUTPUT -j NOTRACK 2>/dev/null || echo "[!] Notice: Raw OUTPUT rule already set or module not loaded"

if [ -f /proc/sys/net/netfilter/nf_conntrack_max ]; then
    echo "[+] Raising host nf_conntrack_max to 1048576..."
    sysctl -w net.netfilter.nf_conntrack_max=1048576 || true
fi

echo "[+] 2. Expanding system file descriptor limits..."
sysctl -w fs.file-max=2097152 || true
sysctl -w fs.nr_open=2097152 || true

echo "[+] 3. Tuning TCP backlogs and port reuse..."
sysctl -w net.core.somaxconn=65535 || true
sysctl -w net.ipv4.tcp_max_syn_backlog=65535 || true
sysctl -w net.core.netdev_max_backlog=65535 || true
sysctl -w net.ipv4.ip_local_port_range="1024 65535" || true
sysctl -w net.ipv4.tcp_tw_reuse=1 || true
sysctl -w net.ipv4.tcp_fin_timeout=15 || true

echo "[+] 4. Minimizing per-socket TCP memory overhead (to fit 150k sockets in 4GB RAM)..."
sysctl -w net.ipv4.tcp_rmem="4096 16384 65536" || true
sysctl -w net.ipv4.tcp_wmem="4096 16384 65536" || true

echo "=== Host Kernel Preparation Completed Successfully ==="
