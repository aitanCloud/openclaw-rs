#!/bin/bash
set -e

PASS=0
FAIL=0
TOTAL=0

run_test() {
    local name="$1"
    local cmd="$2"
    local expected="$3"
    TOTAL=$((TOTAL + 1))

    echo -n "  TEST $TOTAL: $name ... "
    output=$(eval "$cmd" 2>&1) || true

    if echo "$output" | grep -q "$expected"; then
        echo "✓ PASS"
        PASS=$((PASS + 1))
    else
        echo "✗ FAIL"
        echo "    Expected: $expected"
        echo "    Got: $output"
        FAIL=$((FAIL + 1))
    fi
}

run_timing_test() {
    local name="$1"
    local cmd="$2"
    local max_ms="$3"
    TOTAL=$((TOTAL + 1))

    echo -n "  TEST $TOTAL: $name ... "
    start=$(date +%s%N)
    eval "$cmd" > /dev/null 2>&1 || true
    end=$(date +%s%N)
    elapsed_ms=$(( (end - start) / 1000000 ))

    if [ "$elapsed_ms" -lt "$max_ms" ]; then
        echo "✓ PASS (${elapsed_ms}ms < ${max_ms}ms)"
        PASS=$((PASS + 1))
    else
        echo "✗ FAIL (${elapsed_ms}ms >= ${max_ms}ms)"
        FAIL=$((FAIL + 1))
    fi
}

echo ""
echo "═══════════════════════════════════════════"
echo "  OpenClaw Rust CLI — Docker Test Suite"
echo "═══════════════════════════════════════════"
echo ""

# ── Version ──
echo "── Version ──"
run_test "version output" "openclaw --version" "openclaw"
run_timing_test "version speed (<50ms)" "openclaw --version" 50

# ── Config ──
echo ""
echo "── Config ──"
run_test "config show" "openclaw config show" "Port"
run_test "config show gateway mode" "openclaw config show" "local"
run_test "config path" "openclaw config path" "openclaw-manual.json"

# ── Sessions ──
echo ""
echo "── Sessions ──"
run_test "sessions list" "openclaw sessions list" "main"
run_test "sessions list shows label" "openclaw sessions list" "Dashboard fixes"
run_test "sessions list shows model" "openclaw sessions list" "ollama/llama3.2:1b"
run_test "sessions list filter" "openclaw sessions list --agent main" "main"
run_test "sessions list bad filter" "openclaw sessions list --agent nonexistent" "not found"

# ── Skills ──
echo ""
echo "── Skills ──"
run_test "skills list" "openclaw skills list" "web-search"
run_test "skills list shows description" "openclaw skills list" "Search the web"
run_test "skills list shows all" "openclaw skills list" "github-ops"
run_test "skills list count" "openclaw skills list" "3 skill"

# ── Cron ──
echo ""
echo "── Cron ──"
run_test "cron list" "openclaw cron list" "System health check"
run_test "cron list shows schedule" "openclaw cron list" "30 6"
run_test "cron list shows model" "openclaw cron list" "deepseek-chat"
run_test "cron list shows status" "openclaw cron list" "ok"
run_test "cron list count" "openclaw cron list" "2 job"

# ── Models ──
echo ""
echo "── Models ──"
run_test "models list" "openclaw models list" "ollama"
run_test "models list shows moonshot" "openclaw models list" "moonshot"
run_test "models list shows model id" "openclaw models list" "llama3.2:1b"
run_test "models list provider count" "openclaw models list" "2 provider"
run_test "models fallback" "openclaw models fallback" "Fallback Chain"

# ── Help ──
echo ""
echo "── Help ──"
run_test "help flag" "openclaw --help" "OpenClaw"
run_test "sessions help" "openclaw sessions --help" "List"
run_test "models help" "openclaw models --help" "scan"
run_test "rust binary in PATH" "which openclaw" "/usr/local/bin/openclaw"

# ── Summary ──
echo ""
echo "═══════════════════════════════════════════"
echo "  Results: $PASS passed, $FAIL failed, $TOTAL total"
echo "═══════════════════════════════════════════"
echo ""

if [ "$FAIL" -gt 0 ]; then
    exit 1
fi
exit 0
