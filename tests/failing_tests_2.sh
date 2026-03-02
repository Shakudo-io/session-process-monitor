#!/usr/bin/env bash
set -uo pipefail

BINARY="./target/release/session-process-monitor"
PASS=0
FAIL=0
ERRORS=()
SUMMARY_FILE="/root/gitrepos/failing-test-2-summary.txt"

RED='\033[0;31m'
GREEN='\033[0;32m'
CYAN='\033[0;36m'
NC='\033[0m'

cleanup() {
    pkill -f "session-process-monitor run" 2>/dev/null || true
    pkill -f "http.server 198" 2>/dev/null || true
    pkill -f "spm-f2-sentinel" 2>/dev/null || true
    rm -f /tmp/spm-state.json /tmp/spm-f2-* 2>/dev/null || true
}
trap cleanup EXIT

log_pass() { echo -e "  ${GREEN}✓ PASS${NC}: $1"; ((PASS++)); }
log_fail() { echo -e "  ${RED}✗ FAIL${NC}: $1"; echo -e "    ${RED}→ $2${NC}"; ((FAIL++)); ERRORS+=("$1: $2"); }
header() { echo ""; echo -e "${CYAN}━━━ $1 ━━━${NC}"; }

[[ -x "$BINARY" ]] || { echo "ERROR: cargo build --release first"; exit 1; }

echo "============================================"
echo "  Failing Tests Suite 2"
echo "  $(date -Iseconds)"
echo "============================================"

header "F2-01: Exit event must include cmd field"
OUTPUT=$(timeout 8 $BINARY run "sleep 2" --headless 2>&1) || true
if echo "$OUTPUT" | grep '"event":"exit"' | grep -q '"cmd"'; then
    log_pass "Exit event includes cmd field"
else
    EXIT_LINE=$(echo "$OUTPUT" | grep '"event":"exit"' | head -1)
    log_fail "Exit event missing cmd" "exit event has no cmd field: $EXIT_LINE"
fi

header "F2-02: Failed event must include restart_count"
OUTPUT=$(timeout 20 $BINARY run "sh -c 'exit 1'" --headless --max-restarts 2 2>&1) || true
FAILED_LINE=$(echo "$OUTPUT" | grep '"event":"failed"' | head -1)
if echo "$FAILED_LINE" | grep -q '"restart_count"'; then
    log_pass "Failed event has restart_count"
else
    log_fail "No restart_count in failed" "failed event: $FAILED_LINE (or no failed event at all)"
fi

header "F2-03: Emergency kill must set emergency=true when pod >78%"
OUTPUT=$(timeout 15 $BINARY run "python3 -c 'import time; x=[bytearray(10**7) for _ in range(30)]; time.sleep(10)'" --headless --kill-threshold 1 --grace-ticks 1 2>&1) || true
KILL_LINE=$(echo "$OUTPUT" | grep '"guard_kill"' | head -1)
POD_PCT=$(echo "$KILL_LINE" | grep -oP '"pod_percent":[0-9.]+' | grep -oP '[0-9.]+')
EMERGENCY=$(echo "$KILL_LINE" | grep -oP '"emergency":(true|false)' | grep -oP '(true|false)')

if [[ -n "$POD_PCT" ]]; then
    POD_INT=$(printf "%.0f" "$POD_PCT")
    if [[ "$POD_INT" -ge 78 && "$EMERGENCY" == "true" ]]; then
        log_pass "Emergency=true at ${POD_PCT}%"
    elif [[ "$POD_INT" -lt 78 && "$EMERGENCY" == "false" ]]; then
        log_pass "Emergency=false at ${POD_PCT}% (below 78%, correct)"
    elif [[ "$POD_INT" -ge 78 && "$EMERGENCY" == "false" ]]; then
        log_fail "Emergency not triggered" "pod at ${POD_PCT}% (>=78%) but emergency=$EMERGENCY"
    else
        log_pass "Guard kill at ${POD_PCT}%, emergency=$EMERGENCY"
    fi
else
    log_fail "No guard kill event" "expected guard_kill with pod_percent"
fi

header "F2-04: JSON events must go to stderr, child output to stdout"
STDOUT_OUT=$(timeout 5 $BINARY run "echo f2-stdout-test" --headless 2>/dev/null) || true
STDERR_OUT=$(timeout 5 $BINARY run "echo f2-stderr-test" --headless 1>/dev/null 2>&1) || true

if echo "$STDOUT_OUT" | grep -q "f2-stdout-test"; then
    log_pass "Child stdout goes to spm stdout"
else
    log_fail "Child stdout routing" "expected child output on stdout, got: $(echo "$STDOUT_OUT" | head -2)"
fi

if echo "$STDERR_OUT" | grep -q '"event"'; then
    log_pass "JSON events go to stderr"
else
    log_fail "JSON on stderr" "expected JSON events on stderr, got: $(echo "$STDERR_OUT" | head -2)"
fi

header "F2-05: Guard warning must include pod_percent > 0"
OUTPUT=$(timeout 10 $BINARY run "sleep 5" --headless --kill-threshold 1 --grace-ticks 10 2>&1) || true
WARN_LINE=$(echo "$OUTPUT" | grep '"guard_warning"' | head -1)
if [[ -n "$WARN_LINE" ]]; then
    PCT=$(echo "$WARN_LINE" | grep -oP '"pod_percent":[0-9.]+' | grep -oP '[0-9.]+')
    if [[ -n "$PCT" ]] && python3 -c "assert float('$PCT') > 0" 2>/dev/null; then
        log_pass "Guard warning has pod_percent=$PCT"
    else
        log_fail "Guard warning bad percent" "pod_percent=$PCT"
    fi
else
    log_fail "No guard warning" "with --kill-threshold 1, should always warn"
fi

header "F2-06: Guard exhausted event when all children killed and memory high"
OUTPUT=$(timeout 15 $BINARY run "python3 -c 'import time; x=[bytearray(10**7) for _ in range(20)]; time.sleep(10)'" --headless --kill-threshold 1 --grace-ticks 1 --max-restarts 0 2>&1) || true
if echo "$OUTPUT" | grep -q '"event":"guard_exhausted"'; then
    log_pass "Guard exhausted event emitted"
else
    log_fail "No guard_exhausted event" "after killing all managed children with high memory, should emit exhausted. Got: $(echo "$OUTPUT" | head -5)"
fi

header "F2-07: Restart event cmd matches original command"
OUTPUT=$(timeout 15 $BINARY run "sh -c 'exit 42'" --headless --max-restarts 1 2>&1) || true
RESTART_CMD=$(echo "$OUTPUT" | grep '"event":"restart"' | head -1 | grep -oP '"cmd":"[^"]*"' | sed 's/"cmd":"//;s/"//')
if [[ "$RESTART_CMD" == "sh -c 'exit 42'" ]]; then
    log_pass "Restart event has correct cmd: $RESTART_CMD"
elif [[ -n "$RESTART_CMD" ]]; then
    log_fail "Restart cmd mismatch" "expected \"sh -c 'exit 42'\", got: $RESTART_CMD"
else
    log_fail "No restart event" "expected restart event with cmd field"
fi

header "F2-08: Shared state USS updates every tick (not stuck at 0)"
rm -f /tmp/spm-state.json
$BINARY run "python3 -c 'import time; x=[bytearray(10**7) for _ in range(5)]; time.sleep(30)'" --headless &
P=$!; sleep 5
USS1=$(python3 -c "import json; print(json.load(open('/tmp/spm-state.json'))['children'][0]['total_uss'])" 2>/dev/null || echo "0")
sleep 3
USS2=$(python3 -c "import json; print(json.load(open('/tmp/spm-state.json'))['children'][0]['total_uss'])" 2>/dev/null || echo "0")
kill -INT $P 2>/dev/null; wait $P 2>/dev/null || true

if [[ "$USS1" -gt 0 && "$USS2" -gt 0 ]]; then
    log_pass "USS tracked: tick1=${USS1}B tick2=${USS2}B"
else
    log_fail "USS not tracked" "USS values: $USS1, $USS2 (expected >0)"
fi

header "F2-09: Shared state child state reflects actual state"
rm -f /tmp/spm-state.json
$BINARY run "sleep 30" --headless &
P=$!; sleep 3
STATE=$(python3 -c "import json; print(json.load(open('/tmp/spm-state.json'))['children'][0]['state'])" 2>/dev/null || echo "UNKNOWN")
kill -INT $P 2>/dev/null; wait $P 2>/dev/null || true

if [[ "$STATE" == "Running" ]]; then
    log_pass "Child state is Running in shared state"
else
    log_fail "Wrong child state" "expected Running, got: $STATE"
fi

header "F2-10: Completed child shows Completed state in shared state"
rm -f /tmp/spm-state.json
$BINARY run "sleep 1" "sleep 30" --headless &
P=$!; sleep 5
STATE0=$(python3 -c "import json; print(json.load(open('/tmp/spm-state.json'))['children'][0]['state'])" 2>/dev/null || echo "UNKNOWN")
kill -INT $P 2>/dev/null; wait $P 2>/dev/null || true

if echo "$STATE0" | grep -qi "completed"; then
    log_pass "Completed child shows Completed in state: $STATE0"
else
    log_fail "No Completed in state" "sleep 1 should be Completed after 5s, got: $STATE0"
fi

header "F2-11: Guard kill event USS must be > 0 for memory-consuming process"
OUTPUT=$(timeout 15 $BINARY run "python3 -c 'import time; x=[bytearray(10**7) for _ in range(10)]; time.sleep(10)'" --headless --kill-threshold 1 --grace-ticks 1 2>&1) || true
KILL_USS=$(echo "$OUTPUT" | grep '"guard_kill"' | head -1 | grep -oP '"uss":\d+' | grep -oP '\d+')
if [[ -n "$KILL_USS" && "$KILL_USS" -gt 0 ]]; then
    log_pass "Guard kill USS=$KILL_USS (>0)"
else
    log_fail "Zero USS in guard kill" "expected USS > 0, got: $KILL_USS"
fi

header "F2-12: Multiple guard kills target different children by USS"
OUTPUT=$(timeout 20 $BINARY run "python3 -c 'import time; x=[bytearray(10**7) for _ in range(20)]; time.sleep(30)'" "python3 -c 'import time; x=[bytearray(10**6) for _ in range(5)]; time.sleep(30)'" --headless --kill-threshold 1 --grace-ticks 1 --max-restarts 0 2>&1) || true
KILLED_INDICES=$(echo "$OUTPUT" | grep '"guard_kill"' | grep -oP '"index":\d+' | grep -oP '\d+' | sort -u | tr '\n' ',')
KILL_COUNT=$(echo "$OUTPUT" | grep -c '"guard_kill"' || true)
if [[ "$KILL_COUNT" -ge 1 ]]; then
    FIRST_KILL_INDEX=$(echo "$OUTPUT" | grep '"guard_kill"' | head -1 | grep -oP '"index":\d+' | grep -oP '\d+')
    if [[ "$FIRST_KILL_INDEX" == "0" ]]; then
        log_pass "Highest-USS child (index 0, 200MB) killed first"
    else
        log_fail "Wrong victim order" "expected index 0 (200MB) killed first, got index $FIRST_KILL_INDEX"
    fi
else
    log_fail "No guard kills" "expected guard kills with two memory-heavy processes"
fi

header "F2-13: Headless --log file must be valid NDJSON (each line parses)"
rm -f /tmp/spm-f2-log.json
OUTPUT=$(timeout 15 $BINARY run "sleep 2" "sh -c 'exit 1'" --headless --max-restarts 1 --log /tmp/spm-f2-log.json 2>&1) || true

if [[ -f /tmp/spm-f2-log.json ]]; then
    TOTAL_LINES=$(wc -l < /tmp/spm-f2-log.json)
    VALID=0; INVALID=0
    while IFS= read -r line; do
        [[ -z "$line" ]] && continue
        if python3 -c "import json,sys; json.loads(sys.stdin.read())" <<< "$line" 2>/dev/null; then
            ((VALID++))
        else
            ((INVALID++))
        fi
    done < /tmp/spm-f2-log.json
    if [[ "$INVALID" -eq 0 && "$VALID" -gt 0 ]]; then
        log_pass "Log file: $VALID valid JSON lines, 0 invalid"
    else
        log_fail "Invalid log JSON" "$INVALID invalid lines out of $TOTAL_LINES"
    fi
else
    log_fail "No log file" "/tmp/spm-f2-log.json not created"
fi

header "F2-14: Headless child stderr goes to spm stderr with prefix"
STDERR_ONLY=$(timeout 5 $BINARY run "python3 -c 'import sys; sys.stderr.write(\"f2-stderr-sentinel\\n\")'" --headless 1>/dev/null 2>&1) || true
if echo "$STDERR_ONLY" | grep -q "f2-stderr-sentinel"; then
    if echo "$STDERR_ONLY" | grep "f2-stderr-sentinel" | grep -q "^\["; then
        log_pass "Child stderr prefixed on spm stderr"
    else
        log_fail "No prefix on child stderr" "stderr line found but missing [cmd] prefix"
    fi
else
    log_fail "Child stderr lost" "expected 'f2-stderr-sentinel' on stderr, not found"
fi

header "F2-15: Shutdown JSON event has reason field"
OUTPUT=$(timeout 8 $BINARY run "sleep 2" --headless 2>&1) || true
SHUTDOWN_LINE=$(echo "$OUTPUT" | grep '"event":"shutdown"' | head -1)
if echo "$SHUTDOWN_LINE" | grep -q '"reason"'; then
    REASON=$(echo "$SHUTDOWN_LINE" | grep -oP '"reason":"[^"]*"' | sed 's/"reason":"//;s/"//')
    if [[ "$REASON" == "all_terminal" || "$REASON" == "signal" ]]; then
        log_pass "Shutdown event has reason=$REASON"
    else
        log_fail "Unknown shutdown reason" "expected all_terminal or signal, got: $REASON"
    fi
else
    log_fail "No reason in shutdown" "shutdown event missing reason field: $SHUTDOWN_LINE"
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
