#!/usr/bin/env bash
set -uo pipefail

BINARY="./target/release/session-process-monitor"
PASS=0
FAIL=0
ERRORS=()
SUMMARY_FILE="/root/gitrepos/failing-test-4-summary.txt"

RED='\033[0;31m'
GREEN='\033[0;32m'
CYAN='\033[0;36m'
NC='\033[0m'

cleanup() {
    pkill -f "session-process-monitor run" 2>/dev/null || true
    pkill -f "http.server 198" 2>/dev/null || true
    rm -f /tmp/spm-state.json /tmp/spm-f4-* 2>/dev/null || true
}
trap cleanup EXIT

log_pass() { echo -e "  ${GREEN}✓ PASS${NC}: $1"; ((PASS++)); }
log_fail() { echo -e "  ${RED}✗ FAIL${NC}: $1"; echo -e "    ${RED}→ $2${NC}"; ((FAIL++)); ERRORS+=("$1: $2"); }
header() { echo ""; echo -e "${CYAN}━━━ $1 ━━━${NC}"; }

[[ -x "$BINARY" ]] || { echo "ERROR: cargo build --release first"; exit 1; }

echo "============================================"
echo "  Failing Tests Suite 4 — Targeted Bug Tests"
echo "  $(date -Iseconds)"
echo "============================================"

header "F4-01: First restart backoff must be 1s not 2s"
OUTPUT=$(timeout 15 $BINARY run "sh -c 'exit 1'" --headless --max-restarts 2 2>&1) || true
FIRST_BACKOFF=$(echo "$OUTPUT" | grep '"event":"restart"' | head -1 | grep -oP '"backoff_secs":[0-9.]+' | grep -oP '[0-9.]+')
if [[ -n "$FIRST_BACKOFF" ]]; then
    ROUNDED=$(printf "%.0f" "$FIRST_BACKOFF")
    if [[ "$ROUNDED" -le 1 ]]; then
        log_pass "First backoff=${FIRST_BACKOFF}s (correct, <=1s)"
    else
        log_fail "First backoff too high" "expected 1.0s, got ${FIRST_BACKOFF}s — schedule_restart doubles before returning"
    fi
else
    log_fail "No restart event" "cannot verify backoff"
fi

header "F4-02: Spawn event must be emitted for each managed child"
OUTPUT=$(timeout 10 $BINARY run "sleep 2" "sleep 3" --headless 2>&1) || true
SPAWN_COUNT=$(echo "$OUTPUT" | grep -c '"event":"spawn"' || true)
if [[ "$SPAWN_COUNT" -ge 2 ]]; then
    log_pass "Spawn events emitted: $SPAWN_COUNT"
else
    log_fail "Missing spawn events" "expected 2 spawn events, got $SPAWN_COUNT"
fi

header "F4-03: Failed event must be emitted when max-restarts=0 and process crashes"
OUTPUT=$(timeout 8 $BINARY run "sh -c 'exit 1'" --headless --max-restarts 0 2>&1) || true
if echo "$OUTPUT" | grep -q '"event":"failed"'; then
    log_pass "Failed event emitted with max-restarts=0"
else
    log_fail "No failed event" "crash + max-restarts=0 must emit failed. Got: $(echo "$OUTPUT")"
fi

header "F4-04: Completed event must be emitted for every clean exit"
OUTPUT=$(timeout 10 $BINARY run "sleep 1" "sleep 3" --headless 2>&1) || true
COMPLETED_COUNT=$(echo "$OUTPUT" | grep -c '"event":"completed"' || true)
if [[ "$COMPLETED_COUNT" -ge 2 ]]; then
    log_pass "Completed events: $COMPLETED_COUNT"
else
    log_fail "Missing completed events" "expected 2 completed events, got $COMPLETED_COUNT"
fi

header "F4-05: Exit event must include cmd field"
OUTPUT=$(timeout 8 $BINARY run "sleep 2" --headless 2>&1) || true
EXIT_LINE=$(echo "$OUTPUT" | grep '"event":"exit"' | head -1)
if echo "$EXIT_LINE" | grep -q '"cmd"'; then
    log_pass "Exit event has cmd field"
else
    log_fail "Exit missing cmd" "exit event: $EXIT_LINE"
fi

header "F4-06: Child stderr must be captured in headless mode"
OUTPUT=$(timeout 5 $BINARY run "python3 -c 'import sys; sys.stderr.write(\"f4-stderr-test\\n\")'" --headless 2>&1) || true
if echo "$OUTPUT" | grep -q "f4-stderr-test"; then
    log_pass "Child stderr captured in headless output"
else
    log_fail "Child stderr lost" "expected 'f4-stderr-test' in output"
fi

header "F4-07: Health must detect managed child port, not Envoy 15090"
pkill -f "http.server 198" 2>/dev/null || true; sleep 1
rm -f /tmp/spm-state.json
$BINARY run "python3 -m http.server 19876" --headless &
P=$!; sleep 15
if [[ -f /tmp/spm-state.json ]]; then
    PORT=$(python3 -c "import json; print(json.load(open('/tmp/spm-state.json'))['children'][0].get('health_port','null'))" 2>/dev/null || echo "error")
    if [[ "$PORT" == "19876" ]]; then
        log_pass "Correct port 19876 detected"
    elif [[ "$PORT" == "None" || "$PORT" == "null" ]]; then
        log_fail "Port not detected" "expected 19876, got None — inode matching may miss exec'd child sockets"
    elif [[ "$PORT" == "15090" ]]; then
        log_fail "Detected Envoy port" "got 15090 instead of 19876"
    else
        log_fail "Wrong port" "expected 19876, got $PORT"
    fi
else
    log_fail "No state file" "cannot verify port"
fi
kill -INT $P 2>/dev/null; wait $P 2>/dev/null || true
pkill -f "http.server 19876" 2>/dev/null || true

header "F4-08: Non-server health must reach NotApplicable (not stuck on Probing)"
rm -f /tmp/spm-state.json
$BINARY run "sleep 60" --headless &
P=$!; sleep 35
if [[ -f /tmp/spm-state.json ]]; then
    STATUS=$(python3 -c "import json; print(json.load(open('/tmp/spm-state.json'))['children'][0]['health_status'])" 2>/dev/null || echo "error")
    if [[ "$STATUS" == "NotApplicable" ]]; then
        log_pass "Health correctly shows NotApplicable"
    else
        log_fail "Wrong health status" "expected NotApplicable after 30s, got $STATUS — likely stuck on Probing due to detecting Envoy port"
    fi
else
    log_fail "No state file" "cannot check health"
fi
kill -INT $P 2>/dev/null; wait $P 2>/dev/null || true

header "F4-09: Guard must not kill tiny-USS processes after killing the memory hog"
OUTPUT=$(timeout 25 $BINARY run "python3 -c 'import time; x=[bytearray(10**7) for _ in range(15)]; time.sleep(30)'" "sleep 20" --headless --kill-threshold 1 --grace-ticks 1 --max-restarts 0 2>&1) || true
SLEEP_KILLED=$(echo "$OUTPUT" | grep '"guard_kill"' | grep '"index":1' | head -1)
if [[ -z "$SLEEP_KILLED" ]]; then
    log_pass "Guard did not kill the tiny sleep process"
else
    SLEEP_USS=$(echo "$SLEEP_KILLED" | grep -oP '"uss":\d+' | grep -oP '\d+')
    log_fail "Guard killed innocent process" "sleep (index 1, USS=$SLEEP_USS) was guard-killed even though it's tiny — guard should skip low-USS victims or add a minimum USS threshold"
fi

header "F4-10: All events in log file must match all events on stderr"
rm -f /tmp/spm-f4-log.json
OUTPUT=$(timeout 12 $BINARY run "sleep 1" "sh -c 'exit 1'" --headless --max-restarts 1 --log /tmp/spm-f4-log.json 2>&1) || true
STDERR_TYPES=$(echo "$OUTPUT" | grep -oP '"event":"[^"]*"' | sort | tr '\n' ',' || true)
LOG_TYPES=$(grep -oP '"event":"[^"]*"' /tmp/spm-f4-log.json 2>/dev/null | sort | tr '\n' ',' || true)
if [[ "$STDERR_TYPES" == "$LOG_TYPES" ]]; then
    log_pass "Log file events match stderr events"
else
    log_fail "Event mismatch" "stderr=[$STDERR_TYPES] log=[$LOG_TYPES]"
fi

echo ""
echo "============================================"
echo -e "  ${GREEN}PASS: $PASS${NC} | ${RED}FAIL: $FAIL${NC}"
echo "  Total: $((PASS + FAIL))"
echo "============================================"

if [[ ${#ERRORS[@]} -gt 0 ]]; then
    echo ""
    echo -e "${RED}Failed tests:${NC}"
    for err in "${ERRORS[@]}"; do
        echo -e "  ${RED}• $err${NC}"
    done
fi

echo "SUMMARY: PASS=$PASS FAIL=$FAIL" > "$SUMMARY_FILE"
for err in "${ERRORS[@]}"; do
    echo "  $err" >> "$SUMMARY_FILE"
done

echo ""
[[ "$FAIL" -eq 0 ]] && { echo -e "${GREEN}All tests passed!${NC}"; exit 0; } || { echo -e "${RED}$FAIL test(s) failed.${NC}"; exit 1; }
