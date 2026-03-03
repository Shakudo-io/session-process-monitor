#!/usr/bin/env bash
set -uo pipefail

BINARY="./target/release/session-process-monitor"
PASS=0
FAIL=0
ERRORS=()
SUMMARY_FILE="/root/gitrepos/failing-test-5-summary.txt"

RED='\033[0;31m'
GREEN='\033[0;32m'
CYAN='\033[0;36m'
NC='\033[0m'

cleanup() {
    pkill -f "session-process-monitor run" 2>/dev/null || true
    rm -f /tmp/spm-state.json /tmp/spm-f5-* 2>/dev/null || true
}
trap cleanup EXIT

log_pass() { echo -e "  ${GREEN}✓ PASS${NC}: $1"; ((PASS++)); }
log_fail() { echo -e "  ${RED}✗ FAIL${NC}: $1"; echo -e "    ${RED}→ $2${NC}"; ((FAIL++)); ERRORS+=("$1: $2"); }
header() { echo ""; echo -e "${CYAN}━━━ $1 ━━━${NC}"; }

[[ -x "$BINARY" ]] || { echo "ERROR: cargo build --release first"; exit 1; }

echo "============================================"
echo "  Failing Tests Suite 5"
echo "  $(date -Iseconds)"
echo "============================================"

header "F5-01: grace-ticks=0 must kill immediately (no grace period)"
OUTPUT=$(timeout 10 $BINARY run "python3 -c 'import time; x=bytearray(10**7); time.sleep(10)'" --headless --grace-ticks 0 --kill-threshold 1 --max-restarts 0 2>&1) || true
if echo "$OUTPUT" | grep -q '"event":"guard_kill"'; then
    log_pass "Guard killed immediately with grace-ticks=0"
else
    if echo "$OUTPUT" | grep -q "guard_exhausted"; then
        log_fail "Guard exhausted instead of kill" "grace-ticks=0 should kill immediately, but guard says no eligible victims (10MB USS threshold too high for small processes)"
    else
        log_fail "No guard activity" "expected guard_kill with threshold=1 grace=0. Got: $(echo "$OUTPUT" | head -5)"
    fi
fi

header "F5-02: Stderr warning lines must not corrupt JSON event stream"
OUTPUT=$(timeout 15 $BINARY run "sh -c 'exit 1'" --headless --max-restarts 2 2>&1) || true
INVALID=0
TOTAL=0
while IFS= read -r line; do
    [[ -z "$line" ]] && continue
    ((TOTAL++))
    if ! python3 -c "import json,sys; json.loads(sys.stdin.read())" <<< "$line" 2>/dev/null; then
        ((INVALID++))
    fi
done <<< "$OUTPUT"
if [[ "$INVALID" -eq 0 && "$TOTAL" -gt 0 ]]; then
    log_pass "All $TOTAL stderr lines are valid JSON"
else
    NON_JSON=$(echo "$OUTPUT" | while IFS= read -r l; do python3 -c "import json,sys; json.loads(sys.stdin.read())" <<< "$l" 2>/dev/null || echo "BAD: $l"; done | grep "^BAD" | head -3)
    log_fail "Non-JSON lines in stderr" "$INVALID of $TOTAL lines are not JSON: $NON_JSON"
fi

header "F5-03: kill-threshold=0 must trigger guard on any memory usage"
OUTPUT=$(timeout 8 $BINARY run "python3 -c 'import time; x=bytearray(10**7); time.sleep(10)'" --headless --kill-threshold 0 --grace-ticks 1 --max-restarts 0 2>&1) || true
if echo "$OUTPUT" | grep -qE '"guard_kill"|"guard_warning"'; then
    log_pass "Guard triggered with kill-threshold=0"
else
    log_fail "No guard with threshold=0" "pod is using memory, threshold=0 should always trigger. Got: $(echo "$OUTPUT" | head -5)"
fi

header "F5-04: Restart spawn event must come BEFORE restart event"
OUTPUT=$(timeout 15 $BINARY run "sh -c 'exit 1'" --headless --max-restarts 1 2>&1) || true
EVENTS=$(echo "$OUTPUT" | python3 -c "
import json, sys
events = []
for line in sys.stdin:
    line = line.strip()
    if not line: continue
    try:
        events.append(json.loads(line))
    except: pass
spawn_idx = next((i for i,e in enumerate(events) if e.get('event')=='spawn' and e.get('restart_count', -1) != -1 or (e.get('event')=='spawn' and i > 0)), None)
restart_idx = next((i for i,e in enumerate(events) if e.get('event')=='restart'), None)
if spawn_idx is not None and restart_idx is not None:
    print(f'spawn@{spawn_idx} restart@{restart_idx}', 'ORDER_OK' if spawn_idx <= restart_idx else 'ORDER_BAD')
else:
    print(f'spawn@{spawn_idx} restart@{restart_idx} MISSING')
" 2>&1)
if echo "$EVENTS" | grep -q "ORDER_OK\|ORDER_BAD"; then
    if echo "$EVENTS" | grep -q "ORDER_OK"; then
        log_pass "Spawn event comes before/with restart event"
    else
        log_fail "Wrong event order" "spawn should come before restart: $EVENTS"
    fi
else
    log_fail "Missing events" "couldn't find both spawn and restart: $EVENTS"
fi

header "F5-05: All JSON event timestamps must be valid ISO 8601"
OUTPUT=$(timeout 10 $BINARY run "sleep 1" "sh -c 'exit 1'" --headless --max-restarts 1 2>&1) || true
INVALID_TS=$(echo "$OUTPUT" | python3 -c "
import json, sys, re
bad = 0
for line in sys.stdin:
    line = line.strip()
    if not line: continue
    try:
        d = json.loads(line)
        ts = d.get('ts', '')
        if ts and not re.match(r'\d{4}-\d{2}-\d{2}T\d{2}:\d{2}:\d{2}Z', ts):
            bad += 1
            print(f'BAD: {ts}', file=sys.stderr)
    except: pass
print(bad)
" 2>/dev/null)
if [[ "$INVALID_TS" == "0" ]]; then
    log_pass "All timestamps are valid ISO 8601"
else
    log_fail "Invalid timestamps" "$INVALID_TS events have malformed timestamps"
fi

header "F5-06: Event index must never exceed number of managed children"
OUTPUT=$(timeout 10 $BINARY run "sleep 1" "sleep 2" --headless 2>&1) || true
MAX_INDEX=$(echo "$OUTPUT" | python3 -c "
import json, sys
max_idx = -1
for line in sys.stdin:
    try:
        d = json.loads(line.strip())
        idx = d.get('index', -1)
        if idx > max_idx: max_idx = idx
    except: pass
print(max_idx)
" 2>/dev/null)
if [[ "$MAX_INDEX" -le 1 ]]; then
    log_pass "Max event index=$MAX_INDEX (correct for 2 children)"
else
    log_fail "Index out of bounds" "max index=$MAX_INDEX but only 2 children (0,1)"
fi

header "F5-07: Guard kill must include cmd matching the killed child"
OUTPUT=$(timeout 15 $BINARY run "python3 -c 'import time; x=bytearray(10**7); time.sleep(10)'" --headless --kill-threshold 1 --grace-ticks 1 --max-restarts 0 2>&1) || true
KILL_CMD=$(echo "$OUTPUT" | grep '"guard_kill"' | head -1 | python3 -c "import json,sys; d=json.loads(sys.stdin.readline()); print(d.get('cmd','MISSING'))" 2>/dev/null || echo "NO_KILL")
if [[ "$KILL_CMD" == *"python3"* ]]; then
    log_pass "Guard kill cmd contains python3"
elif [[ "$KILL_CMD" == "NO_KILL" ]]; then
    log_fail "No guard kill" "expected guard_kill event"
else
    log_fail "Wrong cmd in guard kill" "expected python3, got: $KILL_CMD"
fi

header "F5-08: Shutdown event on SIGINT must have reason=signal"
$BINARY run "sleep 300" --headless 2>/root/gitrepos/f5-sigint.txt &
P=$!; sleep 3
kill -INT $P 2>/dev/null; sleep 2; wait $P 2>/dev/null || true
SHUTDOWN=$(grep '"shutdown"' /root/gitrepos/f5-sigint.txt | head -1)
REASON=$(echo "$SHUTDOWN" | python3 -c "import json,sys; d=json.loads(sys.stdin.readline()); print(d.get('reason','NONE'))" 2>/dev/null || echo "NO_SHUTDOWN")
if [[ "$REASON" == "signal" ]]; then
    log_pass "SIGINT shutdown has reason=signal"
elif [[ "$REASON" == "all_terminal" ]]; then
    log_fail "Wrong shutdown reason" "expected 'signal' for SIGINT, got 'all_terminal'"
else
    log_fail "No/bad shutdown event" "reason=$REASON from: $SHUTDOWN"
fi

header "F5-09: Completed child must have exit_code=0 in preceding exit event"
OUTPUT=$(timeout 8 $BINARY run "sleep 2" --headless 2>&1) || true
EXIT_CODE=$(echo "$OUTPUT" | grep '"event":"exit"' | head -1 | python3 -c "import json,sys; d=json.loads(sys.stdin.readline()); print(d.get('exit_code','MISSING'))" 2>/dev/null || echo "NO_EXIT")
if [[ "$EXIT_CODE" == "0" ]]; then
    log_pass "Exit event has exit_code=0 before completed"
else
    log_fail "Wrong exit code" "expected 0 for clean exit, got: $EXIT_CODE"
fi

header "F5-10: Failed child must have non-zero exit in preceding exit event"
OUTPUT=$(timeout 15 $BINARY run "sh -c 'exit 42'" --headless --max-restarts 0 2>&1) || true
EXIT_CODE=$(echo "$OUTPUT" | grep '"event":"exit"' | head -1 | python3 -c "import json,sys; d=json.loads(sys.stdin.readline()); print(d.get('exit_code','MISSING'))" 2>/dev/null || echo "NO_EXIT")
if [[ -n "$EXIT_CODE" && "$EXIT_CODE" != "0" && "$EXIT_CODE" != "MISSING" ]]; then
    log_pass "Exit event has non-zero exit_code=$EXIT_CODE before failed"
else
    log_fail "Wrong exit code for crash" "expected non-zero, got: $EXIT_CODE"
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
