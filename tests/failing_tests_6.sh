#!/usr/bin/env bash
set -uo pipefail

BINARY="./target/release/session-process-monitor"
PASS=0
FAIL=0
ERRORS=()
SUMMARY_FILE="/root/gitrepos/failing-test-6-summary.txt"

RED='\033[0;31m'
GREEN='\033[0;32m'
CYAN='\033[0;36m'
NC='\033[0m'

cleanup() {
    pkill -f "session-process-monitor run" 2>/dev/null || true
    pkill -f "http.server 181" 2>/dev/null || true
    rm -f /tmp/spm-state.json /tmp/spm-f6-* 2>/dev/null || true
}
trap cleanup EXIT

log_pass() { echo -e "  ${GREEN}✓ PASS${NC}: $1"; ((PASS++)); }
log_fail() { echo -e "  ${RED}✗ FAIL${NC}: $1"; echo -e "    ${RED}→ $2${NC}"; ((FAIL++)); ERRORS+=("$1: $2"); }
header() { echo ""; echo -e "${CYAN}━━━ $1 ━━━${NC}"; }

[[ -x "$BINARY" ]] || { echo "ERROR: cargo build --release first"; exit 1; }

echo "============================================"
echo "  Failing Tests Suite 6"
echo "  $(date -Iseconds)"
echo "============================================"

header "F6-01: max-restarts=2 must allow exactly 2 restarts (not 1)"
OUTPUT=$(timeout 25 $BINARY run "sh -c 'exit 1'" --headless --max-restarts 2 2>&1) || true
RESTART_COUNT=$(echo "$OUTPUT" | grep -c '"event":"restart"' || true)
if [[ "$RESTART_COUNT" -eq 2 ]]; then
    log_pass "max-restarts=2 produced 2 restarts"
else
    log_fail "Off-by-one in max-restarts" "expected 2 restart events, got $RESTART_COUNT (should_restart uses < instead of <=)"
fi

header "F6-02: State file must be deleted after natural completion"
rm -f /tmp/spm-state.json
timeout 8 $BINARY run "sleep 2" --headless 2>/dev/null
if [[ ! -f /tmp/spm-state.json ]]; then
    log_pass "State file deleted after natural completion"
else
    log_fail "State file not cleaned" "/tmp/spm-state.json still exists after all children completed"
fi

header "F6-03: Exit event for guard-killed process must indicate kill source"
OUTPUT=$(timeout 15 $BINARY run "python3 -c 'import time; x=bytearray(20*10**6); time.sleep(60)'" --headless --kill-threshold 1 --grace-ticks 1 --max-restarts 0 2>&1) || true
EXIT_LINE=$(echo "$OUTPUT" | grep '"event":"exit"' | head -1)
if echo "$EXIT_LINE" | grep -qP '"killed_by":"guard"|"killed_by_guard":true'; then
    log_pass "Exit event indicates guard kill"
else
    log_fail "Exit missing kill source" "after guard kill, exit should indicate who killed it: $EXIT_LINE"
fi

header "F6-04: Restart event must have new_pid different from previous exit pid"
OUTPUT=$(timeout 15 $BINARY run "sh -c 'exit 1'" --headless --max-restarts 1 2>&1) || true
FIRST_EXIT_PID=$(echo "$OUTPUT" | grep '"event":"exit"' | head -1 | python3 -c "import json,sys; print(json.loads(sys.stdin.readline()).get('pid',0))" 2>/dev/null)
RESTART_PID=$(echo "$OUTPUT" | grep '"event":"restart"' | head -1 | python3 -c "import json,sys; print(json.loads(sys.stdin.readline()).get('new_pid',0))" 2>/dev/null)
if [[ -n "$FIRST_EXIT_PID" && -n "$RESTART_PID" && "$FIRST_EXIT_PID" != "$RESTART_PID" && "$RESTART_PID" != "0" ]]; then
    log_pass "Restart PID ($RESTART_PID) differs from exit PID ($FIRST_EXIT_PID)"
elif [[ -z "$RESTART_PID" || "$RESTART_PID" == "0" ]]; then
    log_fail "No restart PID" "restart event missing or new_pid=0"
else
    log_fail "PID reuse" "exit=$FIRST_EXIT_PID restart=$RESTART_PID"
fi

header "F6-05: Health probes at ~5s intervals (not every tick)"
pkill -f "http.server 181" 2>/dev/null; sleep 1
$BINARY run "python3 -m http.server 18114" --headless --max-restarts 0 --log /root/gitrepos/f6-health.json 2>/dev/null &
P=$!; sleep 20
kill -INT $P 2>/dev/null; wait $P 2>/dev/null || true
pkill -f "http.server 18114" 2>/dev/null || true
HEALTH_EVENTS=$(grep -c "health" /root/gitrepos/f6-health.json 2>/dev/null || echo 0)
if [[ "$HEALTH_EVENTS" -ge 1 && "$HEALTH_EVENTS" -le 5 ]]; then
    log_pass "Health events in 20s: $HEALTH_EVENTS (reasonable for 5s interval)"
elif [[ "$HEALTH_EVENTS" -gt 10 ]]; then
    log_fail "Too many health events" "$HEALTH_EVENTS events in 20s — probing every tick instead of every 5s"
elif [[ "$HEALTH_EVENTS" -eq 0 ]]; then
    log_fail "No health events" "health detection didn't find port 18114"
else
    log_pass "Health events: $HEALTH_EVENTS"
fi

header "F6-06: Duplicate commands get separate indices"
OUTPUT=$(timeout 8 $BINARY run "sleep 2" "sleep 2" --headless 2>&1) || true
IDX0=$(echo "$OUTPUT" | grep '"event":"spawn"' | grep '"index":0')
IDX1=$(echo "$OUTPUT" | grep '"event":"spawn"' | grep '"index":1')
if [[ -n "$IDX0" && -n "$IDX1" ]]; then
    log_pass "Duplicate commands got index 0 and 1"
else
    log_fail "Duplicate commands not tracked separately" "expected spawn events with index 0 and 1"
fi

header "F6-07: Guard kill event uss must reflect actual process memory"
OUTPUT=$(timeout 15 $BINARY run "python3 -c 'import time; x=bytearray(20*10**6); time.sleep(60)'" --headless --kill-threshold 1 --grace-ticks 1 --max-restarts 0 2>&1) || true
KILL_USS=$(echo "$OUTPUT" | grep '"guard_kill"' | head -1 | python3 -c "import json,sys; print(json.loads(sys.stdin.readline()).get('uss',0))" 2>/dev/null)
if [[ -n "$KILL_USS" && "$KILL_USS" -gt 15000000 ]]; then
    log_pass "Guard kill USS=$KILL_USS (>15MB, reflects 20MB allocation)"
else
    log_fail "Guard kill USS too low" "expected >15MB for 20MB allocation, got $KILL_USS"
fi

header "F6-08: Natural completion shutdown must have reason=all_terminal"
OUTPUT=$(timeout 8 $BINARY run "sleep 2" --headless 2>&1) || true
REASON=$(echo "$OUTPUT" | grep '"shutdown"' | head -1 | python3 -c "import json,sys; print(json.loads(sys.stdin.readline()).get('reason','NONE'))" 2>/dev/null)
if [[ "$REASON" == "all_terminal" ]]; then
    log_pass "Natural completion shutdown has reason=all_terminal"
else
    log_fail "Wrong natural shutdown reason" "expected all_terminal, got $REASON"
fi

header "F6-09: Spawn event must include log_path in TUI mode (non-headless)"
OUTPUT=$(timeout 5 $BINARY run "sleep 1" 2>&1) || true
if echo "$OUTPUT" | grep '"spawn"' | grep -q "log_path"; then
    log_pass "Spawn event has log_path"
else
    log_fail "Spawn missing log_path" "TUI mode spawn should include log_path for child output file"
fi

header "F6-10: SIGTERM shutdown must also have reason=signal"
$BINARY run "sleep 300" --headless 2>/root/gitrepos/f6-sigterm.txt &
P=$!; sleep 2; kill -TERM $P; sleep 3; wait $P 2>/dev/null || true
REASON=$(grep "shutdown" /root/gitrepos/f6-sigterm.txt | head -1 | python3 -c "import json,sys; print(json.loads(sys.stdin.readline()).get('reason','NONE'))" 2>/dev/null)
if [[ "$REASON" == "signal" ]]; then
    log_pass "SIGTERM shutdown has reason=signal"
else
    log_fail "SIGTERM wrong reason" "expected signal, got $REASON"
fi

header "F6-11: Completed event must appear AFTER exit event for same index"
OUTPUT=$(timeout 8 $BINARY run "sleep 2" --headless 2>&1) || true
EXIT_POS=$(echo "$OUTPUT" | grep -n '"event":"exit".*"index":0' | head -1 | cut -d: -f1)
COMP_POS=$(echo "$OUTPUT" | grep -n '"event":"completed".*"index":0' | head -1 | cut -d: -f1)
if [[ -n "$EXIT_POS" && -n "$COMP_POS" && "$EXIT_POS" -lt "$COMP_POS" ]]; then
    log_pass "Exit (line $EXIT_POS) before completed (line $COMP_POS)"
elif [[ -z "$COMP_POS" ]]; then
    log_fail "No completed event" "completed event missing for index 0"
else
    log_fail "Wrong order" "exit@$EXIT_POS completed@$COMP_POS"
fi

header "F6-12: Failed event restart_count must match actual restart count"
OUTPUT=$(timeout 25 $BINARY run "sh -c 'exit 1'" --headless --max-restarts 2 2>&1) || true
ACTUAL_RESTARTS=$(echo "$OUTPUT" | grep -c '"event":"restart"' || true)
FAILED_RC=$(echo "$OUTPUT" | grep '"event":"failed"' | head -1 | python3 -c "import json,sys; print(json.loads(sys.stdin.readline()).get('restart_count',-1))" 2>/dev/null)
if [[ -n "$FAILED_RC" && "$ACTUAL_RESTARTS" -gt 0 ]]; then
    if [[ "$FAILED_RC" -eq "$ACTUAL_RESTARTS" ]]; then
        log_fail "restart_count equals restart events" "failed.restart_count=$FAILED_RC but this counts ALL exits including initial — should match max-restarts value"
    elif [[ "$FAILED_RC" -eq 2 ]]; then
        log_pass "Failed restart_count=$FAILED_RC matches max-restarts=2"
    else
        log_fail "restart_count mismatch" "actual restarts=$ACTUAL_RESTARTS failed.restart_count=$FAILED_RC"
    fi
else
    log_fail "No failed event" "FAILED_RC=$FAILED_RC ACTUAL=$ACTUAL_RESTARTS"
fi

header "F6-13: Log file must not contain child process output (only JSON events)"
pkill -f "http.server 181" 2>/dev/null; sleep 1
rm -f /root/gitrepos/f6-logclean.json
$BINARY run "python3 -m http.server 18115" --headless --max-restarts 0 --log /root/gitrepos/f6-logclean.json 2>/dev/null &
P=$!; sleep 15
kill -INT $P 2>/dev/null; wait $P 2>/dev/null || true
pkill -f "http.server 18115" 2>/dev/null || true
if [[ -f /root/gitrepos/f6-logclean.json ]]; then
    INVALID=0
    while IFS= read -r line; do
        [[ -z "$line" ]] && continue
        if ! python3 -c "import json,sys; json.loads(sys.stdin.read())" <<< "$line" 2>/dev/null; then
            ((INVALID++))
        fi
    done < /root/gitrepos/f6-logclean.json
    if [[ "$INVALID" -eq 0 ]]; then
        log_pass "Log file is pure JSON"
    else
        log_fail "Log file has non-JSON" "$INVALID non-JSON lines (child output leaked into log)"
    fi
else
    log_fail "No log file" "log file not created"
fi

header "F6-14: Event stream must end with exactly one shutdown event"
OUTPUT=$(timeout 8 $BINARY run "sleep 2" --headless 2>&1) || true
SHUTDOWN_COUNT=$(echo "$OUTPUT" | grep -c '"event":"shutdown"' || true)
if [[ "$SHUTDOWN_COUNT" -eq 1 ]]; then
    log_pass "Exactly 1 shutdown event"
else
    log_fail "Wrong shutdown count" "expected 1, got $SHUTDOWN_COUNT"
fi

header "F6-15: State file guard section must reflect actual config values"
rm -f /tmp/spm-state.json
$BINARY run "sleep 30" --headless --kill-threshold 65 --grace-ticks 7 &
P=$!; sleep 3
if [[ -f /tmp/spm-state.json ]]; then
    RESULT=$(python3 -c "
import json
d = json.load(open('/tmp/spm-state.json'))
g = d['guard']
kt = g['kill_threshold_percent']
ok = kt == 65
print(f'threshold={kt} (expected 65)', 'PASS' if ok else 'FAIL')
" 2>/dev/null)
    if echo "$RESULT" | grep -q "PASS"; then
        log_pass "State file reflects --kill-threshold 65"
    else
        log_fail "State file wrong config" "$RESULT"
    fi
else
    log_fail "No state file" "cannot verify guard config"
fi
kill -INT $P 2>/dev/null; wait $P 2>/dev/null || true

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
