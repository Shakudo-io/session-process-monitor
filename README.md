# Session Process Monitor

A terminal UI for monitoring per-process memory, CPU, and disk I/O usage inside Kubernetes session pods. Designed for Shakudo's data platform to help users identify resource-heavy processes and debug memory issues.

![Demo](docs/demo.gif)

## Features

- **Memory Metrics**: USS, PSS, RSS per process with growth rate tracking
- **CPU Usage**: Per-process CPU percentage with pod-level gauge
- **Disk I/O**: Read/write throughput per process (MB/s)
- **Cmdline Column**: Distinguish processes with same name (e.g., multiple `node` processes)
- **Pod Resource Gauges**: Visual memory and CPU usage against cgroup limits
- **Process Management**: Kill processes directly from the UI with confirmation
- **System Process Protection**: Warns before killing critical session processes
- **Sorting & Filtering**: Sort by any column, filter by process name/cmdline

## Installation

### Pre-built Binary

Download from [Releases](https://github.com/Shakudo-io/session-process-monitor/releases).

### Build from Source

```bash
git clone https://github.com/Shakudo-io/session-process-monitor.git
cd session-process-monitor
cargo build --release
```

The binary will be at `target/release/session-process-monitor`.

### Static MUSL Binary (for Alpine/minimal containers)

```bash
./scripts/build-musl.sh
```

## Usage

```bash
./session-process-monitor
```

### Keybindings

| Key | Action |
|-----|--------|
| `q` | Quit |
| `k` | Kill selected process |
| `s` | Cycle sort column |
| `S` / `r` | Toggle sort direction |
| `/` | Enter filter mode |
| `Esc` | Clear filter / Cancel |
| `↑` / `↓` | Move selection |
| `y` / `n` | Confirm/Cancel kill |

## Metrics Explained

| Metric | Description |
|--------|-------------|
| **PID** | Process ID |
| **Name** | Process name from `/proc/[pid]/stat` |
| **Cmdline** | Full command line (truncated) |
| **CPU%** | CPU usage as percentage of a single core |
| **USS** | Unique Set Size — private memory unique to the process |
| **PSS** | Proportional Set Size — shared memory proportionally attributed |
| **RSS** | Resident Set Size — total resident memory |
| **Growth** | USS change rate in MB/minute |
| **Read** | Disk read rate in MB/s |
| **Write** | Disk write rate in MB/s |

## Configuration

### Environment Variables

| Variable | Default | Description |
|----------|---------|-------------|
| `HYPERPLANE_SESSION_PROCESS_TERMINATOR_THRESHOLD_PERCENT` | `80` | Memory percentage shown as danger marker on gauge |

## How It Works

- Reads process information from `/proc/[pid]/` filesystem
- Memory limits detected from cgroups v1/v2 (`/sys/fs/cgroup/`)
- CPU quota read from cgroup cpu controller
- Disk I/O from `/proc/[pid]/io`
- Updates every second

## System Process Protection

The following processes are marked as system processes (dimmed in UI):
- `pilot-agent`, `envoy` (Istio sidecar)
- `ttyd` (Terminal daemon)
- `jupyter-lab`, `code-server` (Session entrypoints)
- `timeout.py`, `listener.py` (Session management)

Killing these processes may break the session.

## Requirements

- Linux with `/proc` filesystem
- cgroups v1 or v2 for resource limit detection
- No special privileges required (reads only)

## License

Apache License 2.0

## Contributing

Contributions welcome! Please open an issue or PR.

---

Built with [ratatui](https://github.com/ratatui-org/ratatui) 🐀

## Keybindings

| Key | Action |
| --- | ------ |
| q | Quit |
| k | Kill selected process |
| s | Cycle sort column |
| S / r | Toggle sort direction |
| / | Enter filter mode |
| Esc | Clear filter (when filtering) |
| ↑ / ↓ | Move selection |

## Metrics

- **USS**: Unique Set Size — private memory unique to the process.
- **PSS**: Proportional Set Size — shared memory proportionally attributed.
- **RSS**: Resident Set Size — total resident memory for the process.
- **Growth Rate**: Change in USS over time, shown as MB per minute.
- **CPU%**: Process CPU usage as a percentage of a core.

## System Process Warning

System processes are dimmed in the table. If you attempt to kill a system process, the status bar shows a red warning because terminating these can break the session.

## Memory Gauge

The gauge compares cgroup memory usage against the pod’s memory limit. It also shows the sum of process RSS for context. The vertical danger marker corresponds to the configured terminator threshold (via `HYPERPLANE_SESSION_PROCESS_TERMINATOR_THRESHOLD_PERCENT`).

## Build

```bash
cargo build --release
```

For a static MUSL binary, use:

```bash
./scripts/build-musl.sh
```
