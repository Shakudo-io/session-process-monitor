#!/usr/bin/env bash
set -uo pipefail

BINARY="./target/release/session-process-monitor"
PASS=0
FAIL=0
ERRORS=()

RED='\033[0;31m'
GREEN='\033[0;32m'
CYAN='\033[0;36m'
NC='\033[0m'

SUMMARY_FILE="/root/gitrepos/failing-test-summary.txt"
cleanup_pids() {
    pkill -f "spm-test-sentinel" 2>/dev/null || true
    pkill -f "session-process-monitor run" 2>/dev/null || true
    pkill -f "http.server 1987" 2>/dev/null || true
    rm -f /tmp/spm-state.json /tmp/spm-fail-test-* 2>/dev/null || true
}
trap cleanup_pids EXIT

log_pass() { echo -e "  ${GREEN}✓ PASS${NC}: $1"; ((PASS++)); }
log_fail() { echo -e "  ${RED}✗ FAIL${NC}: $1"; echo -e "    ${RED}→ $2${NC}"; ((FAIL++)); ERRORS+=("$1: $2"); }
header() { echo ""; echo -e "${CYAN}━━━ $1 ━━━${NC}"; }

[[ -x "$BINARY" ]] || { echo "ERROR: Build first: cargo build --release"; exit 1; }

echo "============================================"
echo "  Failing Tests — bugs to fix"
echo "  Binary: $BINARY"
echo "  Date: $(date -Iseconds)"
echo "============================================"

header "F01: Spawn event must be emitted in JSON output"

OUTPUT=$(timeout 8 ./target/release/session-process-monitor run "sleep 2" --headless 2>&1) || true
SPAWN_COUNT=$(echo "$OUTPUT" | grep -c '"event":"spawn"' || true)
if [[ "$SPAWN_COUNT" -ge 1 ]]; then
    log_pass "Spawn event emitted"
else
    log_fail "No spawn event" "expected at least 1 spawn event in JSON output, got $SPAWN_COUNT. Output: $(echo "$OUTPUT" | head -3)"
fi

header "F02: Completed event must be emitted for clean exit"

OUTPUT=$(timeout 8 ./target/release/session-process-monitor run "sleep 1" --headless 2>&1) || true
if echo "$OUTPUT" | grep -q '"event":"completed"'; then
    log_pass "Completed event emitted"
else
    log_fail "No completed event" "clean exit (code 0) should emit a completed event. Got: $(echo "$OUTPUT")"
fi

header "F03: Failed event must be emitted after max restarts"

OUTPUT=$(timeout 20 ./target/release/session-process-monitor run "sh -c 'exit 1'" --headless --max-restarts 2 2>&1) || true
if echo "$OUTPUT" | grep -q '"event":"failed"'; then
    log_pass "Failed event emitted after max restarts"
else
    log_fail "No failed event" "expected 'failed' event after exceeding max-restarts. Got: $(echo "$OUTPUT" | tail -3)"
fi

header "F04: Health check must detect the MANAGED process port, not Envoy"

rm -f /tmp/spm-state.json
./target/release/session-process-monitor run "python3 -m http.server 19871" &
SPM_PID=$!
sleep 8

if [[ -f /tmp/spm-state.json ]]; then
    DETECTED_PORT=$(python3 -c "import json; d=json.loads(open('/tmp/spm-state.json').read()); print(d['children'][0].get('health_port','none'))" 2>/dev/null || echo "parse_error")
    if [[ "$DETECTED_PORT" == "19871" ]]; then
        log_pass "Correct port 19871 detected"
    else
        log_fail "Wrong health port" "expected 19871, detected $DETECTED_PORT (probably Envoy's 15090)"
    fi
else
    log_fail "No state file" "state file not written, can't verify port detection"
fi
kill -INT $SPM_PID 2>/dev/null; wait $SPM_PID 2>/dev/null || true
pkill -f "http.server 19871" 2>/dev/null || true

header "F05: Log file must capture ALL event types, not just exit+shutdown"

rm -f /tmp/spm-fail-test-events.json
OUTPUT=$(timeout 20 ./target/release/session-process-monitor run "sleep 2" "sh -c 'exit 1'" --headless --max-restarts 1 --log /tmp/spm-fail-test-events.json 2>&1) || true

LOG_CONTENT=$(cat /tmp/spm-fail-test-events.json 2>/dev/null || echo "")
EVENT_TYPES=$(echo "$LOG_CONTENT" | grep -oP '"event":"[^"]*"' | sort -u | tr '\n' ',')
if echo "$EVENT_TYPES" | grep -q "restart"; then
    log_pass "Log file contains restart events"
else
    log_fail "Log file missing events" "expected restart event in log file. Event types found: $EVENT_TYPES"
fi

header "F06: Shared state JSON must be valid standalone JSON (not concatenated)"

rm -f /tmp/spm-state.json
./target/release/session-process-monitor run "sleep 10" --headless &
SPM_PID=$!
sleep 3

if [[ -f /tmp/spm-state.json ]]; then
    if python3 -c "import json; json.load(open('/tmp/spm-state.json'))" 2>/dev/null; then
        log_pass "Shared state file is valid JSON"
    else
        CONTENT=$(cat /tmp/spm-state.json | head -c 200)
        log_fail "Invalid state JSON" "file is not valid JSON: $CONTENT"
    fi
else
    log_fail "No state file" "shared state file not created"
fi
kill -INT $SPM_PID 2>/dev/null; wait $SPM_PID 2>/dev/null || true

header "F07: Each JSON event line must be independently parseable"

OUTPUT=$(timeout 15 ./target/release/session-process-monitor run "sleep 1" "sh -c 'exit 1'" --headless --max-restarts 1 2>&1) || true
TOTAL_LINES=$(echo "$OUTPUT" | wc -l)
VALID_LINES=0
INVALID_LINES=0
while IFS= read -r line; do
    [[ -z "$line" ]] && continue
    if python3 -c "import json,sys; json.loads(sys.stdin.read())" <<< "$line" 2>/dev/null; then
        ((VALID_LINES++))
    else
        ((INVALID_LINES++))
    fi
done <<< "$OUTPUT"

if [[ "$INVALID_LINES" -eq 0 && "$VALID_LINES" -gt 0 ]]; then
    log_pass "All $VALID_LINES JSON lines are valid"
else
    log_fail "Invalid JSON lines" "$INVALID_LINES of $TOTAL_LINES lines failed JSON parse"
fi

header "F08: Restart events must include correct backoff escalation"

OUTPUT=$(timeout 30 ./target/release/session-process-monitor run "sh -c 'exit 1'" --headless --max-restarts 3 2>&1) || true
BACKOFFS=$(echo "$OUTPUT" | grep '"event":"restart"' | grep -oP '"backoff_secs":[0-9.]+' | grep -oP '[0-9.]+')
PREV=0
ESCALATING=true
for b in $BACKOFFS; do
    B_INT=$(printf "%.0f" "$b")
    if [[ "$B_INT" -le "$PREV" && "$PREV" -gt 0 ]]; then
        ESCALATING=false
    fi
    PREV=$B_INT
done

if [[ "$ESCALATING" == "true" ]] && [[ -n "$BACKOFFS" ]]; then
    log_pass "Backoff escalates: $BACKOFFS"
else
    log_fail "Backoff not escalating" "expected increasing delays, got: $BACKOFFS"
fi

header "F09: Exit event must include PID matching the spawned process"

OUTPUT=$(timeout 8 ./target/release/session-process-monitor run "sleep 2" --headless 2>&1) || true
EXIT_PID=$(echo "$OUTPUT" | grep '"event":"exit"' | head -1 | grep -oP '"pid":\d+' | grep -oP '\d+')
if [[ -n "$EXIT_PID" && "$EXIT_PID" -gt 1 ]]; then
    log_pass "Exit event has valid PID: $EXIT_PID"
else
    log_fail "Missing PID in exit event" "expected pid > 1, got: $EXIT_PID"
fi

header "F10: Guard warning event must be emitted when memory is above threshold"

OUTPUT=$(timeout 15 ./target/release/session-process-monitor run "python3 -c 'x=[bytearray(10**7) for _ in range(50)]'" --headless --kill-threshold 1 --grace-ticks 5 2>&1) || true
if echo "$OUTPUT" | grep -q '"event":"guard_warning"'; then
    log_pass "Guard warning event emitted"
else
    log_fail "No guard warning" "with --kill-threshold 1, guard should always trigger warnings. Got: $(echo "$OUTPUT" | head -5)"
fi

header "F11: Guard kill event must be emitted when threshold exceeded"

OUTPUT=$(timeout 15 ./target/release/session-process-monitor run "python3 -c 'x=[bytearray(10**7) for _ in range(50)]'" --headless --kill-threshold 1 --grace-ticks 1 2>&1) || true
if echo "$OUTPUT" | grep -q '"event":"guard_kill"'; then
    log_pass "Guard kill event emitted"
else
    log_fail "No guard kill event" "with --kill-threshold 1 --grace-ticks 1, guard should kill immediately. Got: $(echo "$OUTPUT" | head -5)"
fi

header "F12: Health OK event must be emitted for server processes"

rm -f /tmp/spm-fail-test-health.json
./target/release/session-process-monitor run "python3 -m http.server 19873" --headless --log /tmp/spm-fail-test-health.json &
HPID=$!
sleep 15
kill -INT $HPID 2>/dev/null; sleep 2; wait $HPID 2>/dev/null || true
HEALTH_LOG=$(cat /tmp/spm-fail-test-health.json 2>/dev/null || echo "")

if echo "$HEALTH_LOG" | grep -q '"event":"health_ok"'; then
    log_pass "Health OK event emitted for server"
else
    log_fail "No health_ok event" "server should be detected and health probed. Log: $(echo "$HEALTH_LOG" | head -3)"
fi
pkill -f "http.server 19873" 2>/dev/null || true

header "F13: Shared state timestamp must be ISO 8601 format"

rm -f /tmp/spm-state.json
./target/release/session-process-monitor run "sleep 10" --headless &
SPM_PID=$!
sleep 3

if [[ -f /tmp/spm-state.json ]]; then
    TS=$(python3 -c "import json; print(json.load(open('/tmp/spm-state.json')).get('timestamp',''))" 2>/dev/null || echo "")
    if [[ "$TS" =~ ^[0-9]{4}-[0-9]{2}-[0-9]{2}T[0-9]{2}:[0-9]{2}:[0-9]{2}Z$ ]]; then
        log_pass "Timestamp is ISO 8601: $TS"
    else
        log_fail "Bad timestamp format" "expected YYYY-MM-DDTHH:MM:SSZ, got: $TS"
    fi
else
    log_fail "No state file" "cannot check timestamp"
fi
kill -INT $SPM_PID 2>/dev/null; wait $SPM_PID 2>/dev/null || true

header "F14: Shared state children array must have correct child count"

rm -f /tmp/spm-state.json
./target/release/session-process-monitor run "sleep 10" "sleep 20" "sleep 30" --headless &
SPM_PID=$!
sleep 3

if [[ -f /tmp/spm-state.json ]]; then
    CHILD_COUNT=$(python3 -c "import json; print(len(json.load(open('/tmp/spm-state.json')).get('children',[])))" 2>/dev/null || echo "0")
    if [[ "$CHILD_COUNT" -eq 3 ]]; then
        log_pass "Shared state has 3 children"
    else
        log_fail "Wrong child count in state" "expected 3 children, got $CHILD_COUNT"
    fi
else
    log_fail "No state file" "cannot check children count"
fi
kill -INT $SPM_PID 2>/dev/null; wait $SPM_PID 2>/dev/null || true

header "F15: Shutdown event must be the LAST event in JSON output"

OUTPUT=$(timeout 8 ./target/release/session-process-monitor run "sleep 2" --headless 2>&1) || true
LAST_EVENT=$(echo "$OUTPUT" | tail -1 | grep -oP '"event":"[^"]*"' | head -1)
if [[ "$LAST_EVENT" == '"event":"shutdown"' ]]; then
    log_pass "Shutdown is the last event"
else
    log_fail "Shutdown not last" "expected shutdown as last event, got: $LAST_EVENT"
fi

echo ""
echo "============================================"
echo -e "  ${GREEN}PASS: $PASS${NC} | ${RED}FAIL: $FAIL${NC}"
TOTAL=$((PASS + FAIL))
echo "  Total: $TOTAL tests"
echo "============================================"

if [[ ${#ERRORS[@]} -gt 0 ]]; then
    echo ""
    echo -e "${RED}Failed tests:${NC}"
    for err in "${ERRORS[@]}"; do
        echo -e "  ${RED}• $err${NC}"
    done
fi

echo ""
echo "SUMMARY: PASS=$PASS FAIL=$FAIL" > "$SUMMARY_FILE"
for err in "${ERRORS[@]}"; do
    echo "  $err" >> "$SUMMARY_FILE"
done

if [[ "$FAIL" -eq 0 ]]; then
    echo -e "${GREEN}All tests passed!${NC}"
    exit 0
else
    echo -e "${RED}$FAIL test(s) failed — these are bugs to fix.${NC}"
    exit 1
fi
