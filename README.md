# Session Process Monitor

A terminal UI for monitoring per-process memory, CPU, and disk I/O usage inside Kubernetes session pods — plus a **process supervisor** that preemptively kills memory hogs before the platform OOM-kills your entire session.

![Demo](docs/demo.gif)

## Features

- **Process Supervisor**: Launch and manage multiple processes with automatic OOM protection
- **OOM Guard**: Preemptively kills the biggest memory consumer when pod memory exceeds threshold
- **Health Checking**: Auto-detects server ports and probes health endpoints
- **Memory Metrics**: USS, PSS, RSS per process with growth rate tracking
- **CPU Usage**: Per-process CPU percentage with pod-level gauge
- **Disk I/O**: Read/write throughput per process (MB/s)
- **Recording & Replay**: Rolling buffer recording with VCR-style playback
- **Dark Mode**: Default dark theme for terminal environments
- **Sorting & Filtering**: Sort by any column, filter by process name/cmdline
- **Process Management**: Kill processes directly from the UI with confirmation

## Installation

### Cargo

```bash
cargo install session-process-monitor
```

### Build from Source

```bash
git clone https://github.com/Shakudo-io/session-process-monitor.git
cd session-process-monitor
cargo build --release
cp target/release/session-process-monitor /usr/local/bin/spm
```

## Quick Start

```bash
# Monitor processes (read-only TUI)
spm

# Supervise processes with OOM guard
spm run "python3 -m http.server 8080" "python train.py" --kill-threshold 75
```

## Supervisor Mode

Launch and supervise one or more commands. The guard monitors pod memory and kills the highest-USS process when pressure rises, then restarts it with exponential backoff.

```bash
# Basic
spm run "python3 -m http.server 8080" "sleep 300"

# Custom thresholds
spm run "python train.py" --kill-threshold 70 --grace-ticks 5 --max-restarts 5

# Headless (JSON events, no TUI)
spm run "python train.py" --headless --log /tmp/events.json
```

### How the Guard Works

1. Every second, reads pod memory from cgroups
2. If usage exceeds `--kill-threshold` (default 75%) for `--grace-ticks` consecutive seconds (default 3), kills the managed process with the highest USS
3. Normal kill: SIGTERM → 3s grace → SIGKILL
4. Emergency kill (>78%): immediate SIGKILL, no grace period
5. 5-second cooldown after each kill to let the kernel reclaim memory
6. Killed processes restart with exponential backoff (1s → 2s → 4s → ... → 30s cap)
7. After `--max-restarts` (default 10), the process is marked Failed

### Health Checking

When a managed process binds a TCP port, spm auto-detects it and probes health endpoints: `/healthz`, `/health`, `/ready`, `/`. Three consecutive failures trigger a kill + restart.

### Headless JSON Events

```bash
spm run "python train.py" --headless 2>events.jsonl
```

Events: `spawn`, `exit`, `completed`, `failed`, `restart`, `guard_warning`, `guard_kill`, `guard_exhausted`, `health_ok`, `health_fail`, `health_kill`, `shutdown`

### Supervisor Flags

| Flag | Env Var | Default | Description |
|------|---------|---------|-------------|
| `--kill-threshold` | `SPM_GUARD_KILL_THRESHOLD` | 75 | Pod memory % to trigger kill |
| `--grace-ticks` | `SPM_GUARD_GRACE_TICKS` | 3 | Seconds above threshold before kill |
| `--max-restarts` | `SPM_GUARD_MAX_RESTARTS` | 10 | Max restarts before marking Failed |
| `--log` | `SPM_GUARD_LOG` | — | Path for JSON event log file |
| `--headless` | — | auto | Run without TUI, JSON to stderr |

### Exit Behavior

- Exit code 0 → **Completed** (not restarted)
- Non-zero exit → restarted with backoff until max-restarts, then **Failed**
- Supervisor exits when all processes are Completed or Failed
- SIGINT/SIGTERM → forwarded to all managed process groups

## Recording & Replay

- Press `w` to watch a process — when it exits, the recording is saved
- Press `R` to browse saved recordings
- Replay with VCR controls: Space (play/pause), ←→ (step), +/- (speed)

| Env Var | Default | Description |
|---------|---------|-------------|
| `SPM_RECORDING_WINDOW` | 300 | Snapshots in rolling buffer |
| `SPM_RECORDING_MAX_SIZE_MB` | 50 | Max total storage |
| `SPM_RECORDING_MAX_AGE_DAYS` | 7 | Auto-delete old recordings |

## Keybindings

### Live / Supervisor Mode

| Key | Action |
|-----|--------|
| `q` | Quit |
| `k` | Kill selected process (confirm with `y`) |
| `Tab` | Switch focus between Managed and Process panes |
| `↑`/`↓` | Navigate in focused pane |
| `s` | Cycle sort column |
| `/` | Filter by name/cmdline |
| `Enter` | Show full cmdline |
| `w` | Toggle watch on process |
| `R` | Browse recordings |

### Replay Mode

| Key | Action |
|-----|--------|
| `Space` | Play / Pause |
| `←`/`→` | Step backward / forward |
| `+`/`-` | Speed up / slow down |
| `Esc` | Exit replay |

## Shared State

Supervisor writes `/tmp/spm-state.json` every second. A read-only `spm` in another terminal reads it and shows the managed pane automatically.

## Requirements

- Linux with `/proc` filesystem
- cgroups v1 or v2
- No special privileges required

## License

Apache License 2.0

---

Built with [ratatui](https://github.com/ratatui-org/ratatui) 🐀
