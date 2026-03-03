#!/usr/bin/env bash
B="./target/release/session-process-monitor"
R="/root/gitrepos/all-results.txt"
P=0; F=0
> "$R"

t() {
    local name="$1" result="$2"
    if [[ "$result" == "1" || "$result" == "PASS" ]]; then
        ((P++)); echo "PASS $name" >> "$R"
    else
        ((F++)); echo "FAIL $name ($result)" >> "$R"
    fi
}

O=$(timeout 8 $B run "sleep 2" --headless 2>&1)
t "spawn" "$(echo "$O" | grep -c '"spawn"')"
t "exit_cmd" "$(echo "$O" | grep '"exit"' | grep -c '"cmd"')"
t "completed" "$(echo "$O" | grep -c '"completed"')"
t "shutdown" "$(echo "$O" | grep -c '"shutdown"')"

O=$(timeout 15 $B run "sleep 1" "sleep 2" "sleep 3" --headless 2>&1)
C=$(echo "$O" | grep -c '"completed"' || echo 0)
[[ "$C" -ge 3 ]] && t "multi_complete" "PASS" || t "multi_complete" "$C"
S=$(echo "$O" | grep -c '"spawn"' || echo 0)
[[ "$S" -ge 3 ]] && t "multi_spawn" "PASS" || t "multi_spawn" "$S"

O=$(timeout 10 $B run "sh -c 'exit 1'" --headless --max-restarts 0 2>&1)
t "failed_event" "$(echo "$O" | grep -c '"failed"')"

O=$(timeout 15 $B run "sh -c 'exit 1'" --headless --max-restarts 1 2>&1)
t "restart_event" "$(echo "$O" | grep -c '"restart"')"
t "backoff_1s" "$(echo "$O" | grep '"restart"' | grep -c '"backoff_secs":1')"

O=$(timeout 30 $B run "sh -c 'exit 1'" --headless --max-restarts 2 2>&1)
RC=$(echo "$O" | grep -c '"restart"' || echo 0)
[[ "$RC" -eq 2 ]] && t "max_restarts_2" "PASS" || t "max_restarts_2" "$RC"

O=$(timeout 15 $B run "python3 -c 'import time; x=bytearray(20*10**6); time.sleep(60)'" --headless --kill-threshold 1 --grace-ticks 1 --max-restarts 0 2>&1)
t "guard_kill" "$(echo "$O" | grep -c '"guard_kill"')"
t "killed_by" "$(echo "$O" | grep -c '"killed_by":"guard"')"
USS=$(echo "$O" | grep '"guard_kill"' | head -1 | grep -oP '"uss":\d+' | grep -oP '\d+' || echo 0)
[[ "$USS" -gt 10000000 ]] && t "kill_uss_gt10mb" "PASS" || t "kill_uss_gt10mb" "$USS"

O=$(timeout 10 $B run "sh -c 'exit 1'" --headless --max-restarts 1 2>&1)
BAD=0
while IFS= read -r l; do
    [[ -z "$l" ]] && continue
    echo "$l" | python3 -c "import json,sys; json.loads(sys.stdin.read())" 2>/dev/null || ((BAD++))
done <<< "$O"
[[ "$BAD" -eq 0 ]] && t "json_purity" "PASS" || t "json_purity" "$BAD bad"

O=$(timeout 5 $B run "" --headless 2>&1)
echo "$O" | grep -qi "error\|warning" && t "empty_cmd" "PASS" || t "empty_cmd" "no error"

rm -f /tmp/spm-state.json
timeout 8 $B run "sleep 2" --headless 2>/dev/null
[[ ! -f /tmp/spm-state.json ]] && t "state_cleanup" "PASS" || t "state_cleanup" "file exists"

O=$(timeout 5 $B run "sleep 1" --headless --kill-threshold 70 --grace-ticks 5 --max-restarts 3 2>&1)
t "cli_flags" "$(echo "$O" | grep -c '"spawn"')"

O=$(timeout 10 $B run "sleep 5" --headless --kill-threshold 1 --grace-ticks 1 --max-restarts 0 2>&1)
t "guard_small_uss" "$(echo "$O" | grep -c '"guard_kill"')"

O=$(timeout 10 $B run "python3 -c 'import time; x=bytearray(10**7); time.sleep(10)'" --headless --grace-ticks 0 --kill-threshold 1 --max-restarts 0 2>&1)
t "grace_zero" "$(echo "$O" | grep -c '"guard_kill"')"

O=$(timeout 8 $B run "sleep 2" --headless 2>&1)
t "exit_before_completed" "$(echo "$O" | grep -nE '"exit"|"completed"' | head -1 | grep -c '"exit"')"

echo "" >> "$R"
echo "TOTAL: PASS=$P FAIL=$F" >> "$R"
cat "$R"
