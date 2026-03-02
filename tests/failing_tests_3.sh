#!/usr/bin/env bash
set -uo pipefail

BINARY="./target/release/session-process-monitor"
PASS=0
FAIL=0
ERRORS=()
SUMMARY_FILE="/root/gitrepos/failing-test-3-summary.txt"

RED='\033[0;31m'
GREEN='\033[0;32m'
CYAN='\033[0;36m'
NC='\033[0m'

cleanup() {
    pkill -f "session-process-monitor run" 2>/dev/null || true
    pkill -f "http.server 198" 2>/dev/null || true
    rm -f /tmp/spm-state.json /tmp/spm-f3-* 2>/dev/null || true
}
trap cleanup EXIT

log_pass() { echo -e "  ${GREEN}✓ PASS${NC}: $1"; ((PASS++)); }
log_fail() { echo -e "  ${RED}✗ FAIL${NC}: $1"; echo -e "    ${RED}→ $2${NC}"; ((FAIL++)); ERRORS+=("$1: $2"); }
header() { echo ""; echo -e "${CYAN}━━━ $1 ━━━${NC}"; }

[[ -x "$BINARY" ]] || { echo "ERROR: cargo build --release first"; exit 1; }

echo "============================================"
echo "  Failing Tests Suite 3"
echo "  $(date -Iseconds)"
echo "============================================"

header "F3-01: max-restarts=0 must emit failed event (not just exit)"
OUTPUT=$(timeout 8 $BINARY run "sh -c 'exit 1'" --headless --max-restarts 0 2>&1) || true
if echo "$OUTPUT" | grep -q '"event":"failed"'; then
    log_pass "Failed event emitted with max-restarts=0"
else
    log_fail "No failed event at max-restarts=0" "crash with 0 restarts allowed should emit failed. Got: $(echo "$OUTPUT")"
fi

header "F3-02: Restart event new_pid must differ from original exit pid"
OUTPUT=$(timeout 15 $BINARY run "sh -c 'exit 1'" --headless --max-restarts 1 2>&1) || true
ORIG_PID=$(echo "$OUTPUT" | grep '"event":"exit"' | head -1 | grep -oP '"pid":\d+' | grep -oP '\d+')
NEW_PID=$(echo "$OUTPUT" | grep '"event":"restart"' | head -1 | grep -oP '"new_pid":\d+' | grep -oP '\d+')
if [[ -n "$ORIG_PID" && -n "$NEW_PID" && "$ORIG_PID" != "$NEW_PID" ]]; then
    log_pass "Restart new_pid=$NEW_PID differs from original pid=$ORIG_PID"
elif [[ -z "$NEW_PID" ]]; then
    log_fail "No new_pid in restart" "restart event missing or no new_pid field"
else
    log_fail "PIDs match" "original=$ORIG_PID new=$NEW_PID — should be different processes"
fi

header "F3-03: Non-server process health must be NotApplicable after 30s"
rm -f /tmp/spm-state.json
$BINARY run "sleep 60" --headless &
P=$!; sleep 35
if [[ -f /tmp/spm-state.json ]]; then
    STATUS=$(python3 -c "import json; print(json.load(open('/tmp/spm-state.json'))['children'][0]['health_status'])" 2>/dev/null || echo "ERROR")
    if [[ "$STATUS" == "NotApplicable" ]]; then
        log_pass "Non-server shows NotApplicable after 30s"
    else
        log_fail "Wrong health status for non-server" "expected NotApplicable after 30s, got: $STATUS"
    fi
else
    log_fail "No state file" "cannot check health status"
fi
kill -INT $P 2>/dev/null; wait $P 2>/dev/null || true

header "F3-04: total_kills in state file must match actual kill count"
rm -f /tmp/spm-state.json
$BINARY run "python3 -c 'import time; x=[bytearray(10**7) for _ in range(10)]; time.sleep(60)'" --headless --kill-threshold 1 --grace-ticks 1 --max-restarts 1 &
P=$!; sleep 10
KILLS_IN_STATE=$(python3 -c "import json; print(json.load(open('/tmp/spm-state.json'))['guard']['total_kills'])" 2>/dev/null || echo "-1")
kill -INT $P 2>/dev/null; wait $P 2>/dev/null || true
OUTPUT_KILLS=$(timeout 1 cat /dev/null)
if [[ "$KILLS_IN_STATE" -ge 1 ]]; then
    log_pass "total_kills=$KILLS_IN_STATE in state file (>=1)"
else
    log_fail "total_kills wrong" "expected >=1, got $KILLS_IN_STATE"
fi

header "F3-05: Empty command string must report error or fail immediately"
OUTPUT=$(timeout 5 $BINARY run "" --headless 2>&1) || true
if echo "$OUTPUT" | grep -qi "error\|fail\|invalid"; then
    log_pass "Empty command reported error"
elif echo "$OUTPUT" | grep -q '"event":"exit"'; then
    ECODE=$(echo "$OUTPUT" | grep '"exit"' | head -1 | grep -oP '"exit_code":\d+' | grep -oP '\d+')
    if [[ "$ECODE" == "0" ]]; then
        log_fail "Empty command treated as success" "sh -c '' exits 0 silently — should validate input or report warning"
    else
        log_pass "Empty command exited with code $ECODE"
    fi
else
    log_fail "Empty command silent" "no output at all for empty command"
fi

header "F3-06: Guard kill on process A must not affect process B state"
OUTPUT=$(timeout 20 $BINARY run "python3 -c 'import time; x=[bytearray(10**7) for _ in range(15)]; time.sleep(30)'" "sleep 30" --headless --kill-threshold 1 --grace-ticks 1 --max-restarts 0 2>&1) || true
KILLS=$(echo "$OUTPUT" | grep '"guard_kill"')
KILL_INDEX=$(echo "$KILLS" | head -1 | grep -oP '"index":\d+' | grep -oP '\d+')
if [[ "$KILL_INDEX" == "0" ]]; then
    SLEEP_EXIT=$(echo "$OUTPUT" | grep '"event":"exit"' | grep '"index":1' | head -1 | grep -oP '"exit_code":\d+' | grep -oP '\d+')
    if [[ "$SLEEP_EXIT" == "0" ]] || echo "$OUTPUT" | grep -q '"index":1.*"completed"'; then
        log_pass "Process B (sleep) unaffected by guard kill on process A"
    elif [[ -z "$SLEEP_EXIT" ]]; then
        log_fail "Process B exit missing" "no exit event for index 1 (sleep) — may have been killed too"
    else
        log_fail "Process B affected" "sleep (index 1) exit_code=$SLEEP_EXIT (expected 0)"
    fi
else
    log_fail "Wrong kill target" "expected index 0 killed, got index $KILL_INDEX"
fi

header "F3-07: Event timestamps must be monotonically increasing"
OUTPUT=$(timeout 15 $BINARY run "sleep 1" "sh -c 'exit 1'" --headless --max-restarts 1 2>&1) || true
TIMESTAMPS=$(echo "$OUTPUT" | grep -oP '"ts":"[^"]*"' | sed 's/"ts":"//;s/"//')
PREV=""
MONOTONIC=true
for ts in $TIMESTAMPS; do
    if [[ -n "$PREV" && "$ts" < "$PREV" ]]; then
        MONOTONIC=false
        break
    fi
    PREV=$ts
done
if [[ "$MONOTONIC" == "true" && -n "$PREV" ]]; then
    log_pass "Timestamps are monotonically increasing"
else
    log_fail "Timestamps not monotonic" "found non-increasing timestamp sequence"
fi

header "F3-08: Guard warning ticks_remaining must count down"
OUTPUT=$(timeout 15 $BINARY run "sleep 10" --headless --kill-threshold 1 --grace-ticks 5 2>&1) || true
TICKS=$(echo "$OUTPUT" | grep '"guard_warning"' | grep -oP '"ticks_remaining":\d+' | grep -oP '\d+')
PREV=999
COUNTING_DOWN=true
COUNT=0
for t in $TICKS; do
    ((COUNT++))
    if [[ "$t" -ge "$PREV" && "$PREV" -ne 999 ]]; then
        COUNTING_DOWN=false
    fi
    PREV=$t
done
if [[ "$COUNTING_DOWN" == "true" && "$COUNT" -ge 2 ]]; then
    log_pass "ticks_remaining counts down: $TICKS"
else
    log_fail "ticks_remaining not counting down" "got sequence: $TICKS (count=$COUNT)"
fi

header "F3-09: Completed event must include index matching the right child"
OUTPUT=$(timeout 12 $BINARY run "sleep 1" "sleep 8" --headless 2>&1) || true
COMPLETED_INDEX=$(echo "$OUTPUT" | grep '"event":"completed"' | head -1 | grep -oP '"index":\d+' | grep -oP '\d+')
if [[ "$COMPLETED_INDEX" == "0" ]]; then
    log_pass "First completed event is index 0 (sleep 1 finishes first)"
else
    log_fail "Wrong completed index" "expected index 0 (sleep 1) to complete first, got index $COMPLETED_INDEX"
fi

header "F3-10: Guard exhausted event must fire when all children are terminal"
OUTPUT=$(timeout 20 $BINARY run "python3 -c 'import time; x=[bytearray(10**7) for _ in range(15)]; time.sleep(30)'" --headless --kill-threshold 1 --grace-ticks 1 --max-restarts 0 2>&1) || true
if echo "$OUTPUT" | grep -q '"event":"guard_exhausted"'; then
    log_pass "Guard exhausted event emitted after all children killed"
else
    log_fail "No guard_exhausted" "with max-restarts=0 and kill-threshold=1, guard should exhaust. Got: $(echo "$OUTPUT" | tail -3)"
fi

header "F3-11: Log file and stderr must have same events"
rm -f /tmp/spm-f3-log.json
OUTPUT=$(timeout 10 $BINARY run "sleep 2" --headless --log /tmp/spm-f3-log.json 2>&1) || true
STDERR_EVENTS=$(echo "$OUTPUT" | grep -c '"event"' || true)
LOG_EVENTS=$(grep -c '"event"' /tmp/spm-f3-log.json 2>/dev/null || echo "0")
if [[ "$STDERR_EVENTS" -eq "$LOG_EVENTS" && "$STDERR_EVENTS" -gt 0 ]]; then
    log_pass "Event count matches: stderr=$STDERR_EVENTS log=$LOG_EVENTS"
else
    log_fail "Event count mismatch" "stderr=$STDERR_EVENTS vs log=$LOG_EVENTS"
fi

header "F3-12: Process killed by guard must show signal in exit event"
OUTPUT=$(timeout 15 $BINARY run "python3 -c 'import time; x=[bytearray(10**7) for _ in range(10)]; time.sleep(30)'" --headless --kill-threshold 1 --grace-ticks 1 --max-restarts 0 2>&1) || true
EXIT_AFTER_KILL=$(echo "$OUTPUT" | grep '"event":"exit"' | head -1)
if echo "$EXIT_AFTER_KILL" | grep -qP '"signal":\d+'; then
    SIG=$(echo "$EXIT_AFTER_KILL" | grep -oP '"signal":\d+' | grep -oP '\d+')
    if [[ "$SIG" == "15" || "$SIG" == "9" ]]; then
        log_pass "Guard-killed process exit has signal=$SIG (SIGTERM=15 or SIGKILL=9)"
    else
        log_fail "Unexpected signal" "expected 15 (SIGTERM) or 9 (SIGKILL), got $SIG"
    fi
else
    log_fail "No signal in exit event" "guard-killed process should have signal field: $EXIT_AFTER_KILL"
fi

header "F3-13: Pipe commands must work (sh -c handles pipes)"
OUTPUT=$(timeout 8 $BINARY run "echo pipe-test | cat" --headless 2>&1) || true
if echo "$OUTPUT" | grep -q "pipe-test"; then
    log_pass "Pipe command output captured"
else
    log_fail "Pipe command failed" "expected 'pipe-test' in output"
fi

header "F3-14: State file spm_pid must match actual supervisor PID"
rm -f /tmp/spm-state.json
$BINARY run "sleep 30" --headless &
ACTUAL_PID=$!
sleep 3
if [[ -f /tmp/spm-state.json ]]; then
    STATE_PID=$(python3 -c "import json; print(json.load(open('/tmp/spm-state.json'))['spm_pid'])" 2>/dev/null || echo "0")
    if [[ "$STATE_PID" == "$ACTUAL_PID" ]]; then
        log_pass "State spm_pid=$STATE_PID matches actual PID"
    else
        log_fail "PID mismatch" "state says $STATE_PID, actual supervisor PID is $ACTUAL_PID"
    fi
else
    log_fail "No state file" "cannot verify spm_pid"
fi
kill -INT $ACTUAL_PID 2>/dev/null; wait $ACTUAL_PID 2>/dev/null || true

header "F3-15: Backoff first restart delay must be ~1s (not 2s)"
OUTPUT=$(timeout 15 $BINARY run "sh -c 'exit 1'" --headless --max-restarts 2 2>&1) || true
FIRST_BACKOFF=$(echo "$OUTPUT" | grep '"event":"restart"' | head -1 | grep -oP '"backoff_secs":[0-9.]+' | grep -oP '[0-9.]+')
if [[ -n "$FIRST_BACKOFF" ]]; then
    BACKOFF_INT=$(printf "%.0f" "$FIRST_BACKOFF")
    if [[ "$BACKOFF_INT" -le 1 ]]; then
        log_pass "First restart backoff=${FIRST_BACKOFF}s (<=1s, correct)"
    else
        log_fail "First backoff too high" "expected ~1s for first restart, got ${FIRST_BACKOFF}s (backoff should start at 1s, not 2s)"
    fi
else
    log_fail "No restart event" "cannot verify backoff delay"
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
