use ratatui::layout::{Constraint, Direction, Layout, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::widgets::{Block, Borders, Clear, Gauge, Paragraph, Row, Table, TableState};
use ratatui::Frame;

use crate::app::{App, PodMemorySnapshot, ProcessSnapshot, SortColumn};
use crate::replay::{AppMode, RecordingListState, ReplayState};

const BG: Color = Color::Rgb(24, 24, 32);
const BG_ALT: Color = Color::Rgb(32, 32, 42);
const FG: Color = Color::Rgb(200, 200, 210);
const FG_DIM: Color = Color::Rgb(100, 100, 120);
const BORDER: Color = Color::Rgb(60, 60, 80);
const ACCENT: Color = Color::Rgb(100, 160, 255);
const HIGHLIGHT_BG: Color = Color::Rgb(50, 50, 70);

pub fn draw(frame: &mut Frame, app: &App) {
    match &app.mode {
        AppMode::Replay(state) => draw_replay(frame, app, state),
        _ => draw_live(frame, app),
    }

    if let AppMode::RecordingList(list_state) = &app.mode {
        draw_recording_list_modal(frame, list_state);
    }

    if let Some((pid, name, cmdline)) = &app.show_cmdline {
        draw_cmdline_modal(frame, *pid, name, cmdline);
    }
}

fn draw_live(frame: &mut Frame, app: &App) {
    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(3),
            Constraint::Min(1),
            Constraint::Length(3),
        ])
        .split(frame.area());

    render_gauges(
        frame,
        chunks[0],
        &app.pod_memory,
        &app.processes,
        app.cpu_cores,
    );
    render_process_table(
        frame,
        chunks[1],
        &app.processes,
        app.view_state.sort_column,
        app.view_state.sort_ascending,
        Some(app.view_state.selected),
        &app.watched_pids,
    );

    let (status_text, status_style) = status_line(app);
    let status = Paragraph::new(status_text).style(status_style).block(
        Block::default()
            .borders(Borders::ALL)
            .border_style(Style::default().fg(BORDER))
            .style(Style::default().bg(BG)),
    );
    frame.render_widget(status, chunks[2]);
}

fn draw_replay(frame: &mut Frame, app: &App, state: &ReplayState) {
    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(1),
            Constraint::Length(3),
            Constraint::Min(1),
            Constraint::Length(3),
        ])
        .split(frame.area());

    let total_snapshots = state.recording.snapshots.len();
    let snapshot_index = if total_snapshots == 0 {
        0
    } else {
        state.current_index.min(total_snapshots - 1) + 1
    };
    let (timestamp_label, snapshot) = match state.recording.snapshots.get(state.current_index) {
        Some(snapshot) => (format_timestamp(snapshot.timestamp), Some(snapshot)),
        None => ("—".to_string(), None),
    };
    let play_label = if state.playing {
        "▶ PLAYING"
    } else {
        "⏸ PAUSED"
    };
    let header_text = format!(
        "▶ REPLAY | {} | Snapshot {}/{} | {} | Speed: {} | [Space] Play/Pause [←→] Step [Esc] Exit",
        play_label,
        snapshot_index,
        total_snapshots,
        timestamp_label,
        state.speed.label()
    );
    let header = Paragraph::new(header_text).style(
        Style::default()
            .fg(ACCENT)
            .bg(BG_ALT)
            .add_modifier(Modifier::BOLD),
    );
    frame.render_widget(header, chunks[0]);

    if let Some(snapshot) = snapshot {
        render_gauges(
            frame,
            chunks[1],
            &snapshot.pod_memory,
            &snapshot.processes,
            snapshot.cpu_cores,
        );
        render_process_table(
            frame,
            chunks[2],
            &snapshot.processes,
            app.view_state.sort_column,
            app.view_state.sort_ascending,
            Some(app.view_state.selected),
            &app.watched_pids,
        );
    } else {
        let placeholder = Paragraph::new("Recording has no snapshots.")
            .style(Style::default().fg(Color::Yellow).bg(BG))
            .block(
                Block::default()
                    .borders(Borders::ALL)
                    .border_style(Style::default().fg(BORDER))
                    .title("Replay")
                    .style(Style::default().bg(BG)),
            );
        frame.render_widget(placeholder, chunks[2]);
        frame.render_widget(
            Block::default()
                .borders(Borders::ALL)
                .border_style(Style::default().fg(BORDER))
                .style(Style::default().bg(BG)),
            chunks[1],
        );
    }

    let (status_text, status_style) = status_line(app);
    let status = Paragraph::new(status_text).style(status_style).block(
        Block::default()
            .borders(Borders::ALL)
            .border_style(Style::default().fg(BORDER))
            .style(Style::default().bg(BG)),
    );
    frame.render_widget(status, chunks[3]);
}

fn draw_recording_list_modal(frame: &mut Frame, list_state: &RecordingListState) {
    let area = centered_rect(80, 60, frame.area());
    frame.render_widget(Clear, area);

    let block = Block::default()
        .borders(Borders::ALL)
        .border_style(Style::default().fg(ACCENT))
        .title("Recordings (Enter: select, d: delete, Esc: close)")
        .title_style(
            Style::default()
                .fg(Color::White)
                .add_modifier(Modifier::BOLD),
        )
        .style(Style::default().bg(BG));

    if list_state.recordings.is_empty() {
        let empty =
            Paragraph::new("No recordings available. Recordings are saved when processes exit.")
                .style(Style::default().fg(FG_DIM))
                .block(block);
        frame.render_widget(empty, area);
        return;
    }

    let header = Row::new(vec!["Time", "Process", "Snapshots"]).style(
        Style::default()
            .fg(ACCENT)
            .bg(BG_ALT)
            .add_modifier(Modifier::BOLD),
    );
    let rows = list_state.recordings.iter().map(|recording| {
        Row::new(vec![
            format_timestamp(recording.end_time),
            recording.trigger_name.clone(),
            recording.snapshot_count.to_string(),
        ])
    });

    let table = Table::new(
        rows,
        [
            Constraint::Length(12),
            Constraint::Min(20),
            Constraint::Length(10),
        ],
    )
    .header(header)
    .block(block)
    .column_spacing(1)
    .row_highlight_style(Style::default().bg(HIGHLIGHT_BG).fg(Color::White));

    let mut table_state = TableState::default();
    if !list_state.recordings.is_empty() {
        let selected = list_state
            .selected
            .min(list_state.recordings.len().saturating_sub(1));
        table_state.select(Some(selected));
    }
    frame.render_stateful_widget(table, area, &mut table_state);
}

fn draw_cmdline_modal(frame: &mut Frame, pid: u32, name: &str, cmdline: &str) {
    let area = centered_rect(80, 40, frame.area());
    frame.render_widget(Clear, area);

    let title = format!("PID {} — {} (any key to close)", pid, name);
    let block = Block::default()
        .borders(Borders::ALL)
        .border_style(Style::default().fg(ACCENT))
        .title(title)
        .title_style(
            Style::default()
                .fg(Color::White)
                .add_modifier(Modifier::BOLD),
        )
        .style(Style::default().bg(BG));

    let content = Paragraph::new(cmdline.to_string())
        .style(Style::default().fg(FG))
        .wrap(ratatui::widgets::Wrap { trim: false })
        .block(block);
    frame.render_widget(content, area);
}

fn render_gauges(
    frame: &mut Frame,
    area: Rect,
    pod_memory: &PodMemorySnapshot,
    processes: &[ProcessSnapshot],
    cpu_cores: Option<f64>,
) {
    let gauge_chunks = Layout::default()
        .direction(Direction::Horizontal)
        .constraints([Constraint::Percentage(60), Constraint::Percentage(40)])
        .split(area);

    let mem_state = memory_gauge_state(pod_memory);
    let mem_block = Block::default()
        .borders(Borders::ALL)
        .border_style(Style::default().fg(BORDER))
        .title("Pod Memory")
        .title_style(Style::default().fg(ACCENT))
        .style(Style::default().bg(BG));
    let mem_gauge = Gauge::default()
        .block(mem_block.clone())
        .ratio(mem_state.ratio)
        .label(mem_state.label)
        .gauge_style(mem_state.gauge_style);
    frame.render_widget(mem_gauge, gauge_chunks[0]);
    if let Some(danger_percent) = mem_state.danger_percent {
        render_danger_marker(frame, gauge_chunks[0], &mem_block, danger_percent);
    }

    let cpu_state = cpu_gauge_state(processes, cpu_cores);
    let cpu_block = Block::default()
        .borders(Borders::ALL)
        .border_style(Style::default().fg(BORDER))
        .title("CPU Usage")
        .title_style(Style::default().fg(ACCENT))
        .style(Style::default().bg(BG));
    let cpu_gauge = Gauge::default()
        .block(cpu_block)
        .ratio(cpu_state.ratio)
        .label(cpu_state.label)
        .gauge_style(cpu_state.gauge_style);
    frame.render_widget(cpu_gauge, gauge_chunks[1]);
}

fn render_process_table(
    frame: &mut Frame,
    area: Rect,
    processes: &[ProcessSnapshot],
    sort_column: SortColumn,
    sort_ascending: bool,
    selected: Option<usize>,
    watched_pids: &std::collections::HashSet<u32>,
) {
    let header = Row::new(vec![
        header_label("PID", SortColumn::Pid, sort_column, sort_ascending),
        header_label("Name", SortColumn::Name, sort_column, sort_ascending),
        header_label("Cmdline", SortColumn::Cmdline, sort_column, sort_ascending),
        header_label("CPU%", SortColumn::Cpu, sort_column, sort_ascending),
        header_label("USS", SortColumn::Uss, sort_column, sort_ascending),
        header_label("PSS", SortColumn::Pss, sort_column, sort_ascending),
        header_label("RSS", SortColumn::Rss, sort_column, sort_ascending),
        header_label(
            "Growth",
            SortColumn::GrowthRate,
            sort_column,
            sort_ascending,
        ),
        header_label("Read", SortColumn::DiskRead, sort_column, sort_ascending),
        header_label("Write", SortColumn::DiskWrite, sort_column, sort_ascending),
    ])
    .style(
        Style::default()
            .fg(ACCENT)
            .bg(BG_ALT)
            .add_modifier(Modifier::BOLD),
    );

    let rows = processes.iter().map(|process| {
        let style = if process.is_system {
            Style::default()
                .fg(FG_DIM)
                .bg(BG)
                .add_modifier(Modifier::DIM)
        } else {
            Style::default().fg(FG).bg(BG)
        };

        let growth_text = match process.growth_rate {
            Some(rate) => format!("{:.1} MB/m", rate),
            None => "—".to_string(),
        };

        let read_text = match process.disk_read_rate {
            Some(rate) => format!("{:.1}", rate),
            None => "—".to_string(),
        };

        let write_text = match process.disk_write_rate {
            Some(rate) => format!("{:.1}", rate),
            None => "—".to_string(),
        };

        const CMDLINE_MAX_LEN: usize = 80;
        let cmdline_display = if process.cmdline.len() > CMDLINE_MAX_LEN {
            format!("{}...", &process.cmdline[..CMDLINE_MAX_LEN - 3])
        } else {
            process.cmdline.clone()
        };

        let pid_label = if watched_pids.contains(&process.pid) {
            format!("● {}", process.pid)
        } else {
            process.pid.to_string()
        };

        Row::new(vec![
            pid_label,
            process.name.clone(),
            cmdline_display,
            format!("{:.1}", process.cpu_percent),
            format_bytes(process.uss),
            format_bytes(process.pss),
            format_bytes(process.rss),
            growth_text,
            read_text,
            write_text,
        ])
        .style(style)
    });

    let table = Table::new(
        rows,
        [
            Constraint::Length(10),
            Constraint::Length(18),
            Constraint::Min(40),
            Constraint::Length(7),
            Constraint::Length(10),
            Constraint::Length(10),
            Constraint::Length(10),
            Constraint::Length(10),
            Constraint::Length(7),
            Constraint::Length(7),
        ],
    )
    .header(header)
    .block(
        Block::default()
            .borders(Borders::ALL)
            .border_style(Style::default().fg(BORDER))
            .title("Processes")
            .title_style(Style::default().fg(ACCENT))
            .style(Style::default().bg(BG)),
    )
    .column_spacing(1)
    .row_highlight_style(Style::default().bg(HIGHLIGHT_BG).fg(Color::White));

    let mut table_state = TableState::default();
    if !processes.is_empty() {
        if let Some(selected) = selected {
            let selected = selected.min(processes.len().saturating_sub(1));
            table_state.select(Some(selected));
        }
    }
    frame.render_stateful_widget(table, area, &mut table_state);
}

struct MemoryGaugeState {
    ratio: f64,
    label: String,
    gauge_style: Style,
    danger_percent: Option<u8>,
}

fn memory_gauge_state(pod_memory: &PodMemorySnapshot) -> MemoryGaugeState {
    let usage = pod_memory.cgroup_usage;
    let limit = pod_memory.cgroup_limit;
    let rss_sum = pod_memory.rss_sum;
    let threshold = pod_memory.terminator_threshold_percent.min(100);

    let ratio = match limit {
        Some(limit) if limit > 0 => (usage as f64 / limit as f64).min(1.0),
        _ => 0.0,
    };

    let (label, danger_percent, gauge_style) = match limit {
        Some(limit) if limit > 0 => {
            let percent = (ratio * 100.0).round() as u64;
            let available = limit.saturating_sub(usage);
            let color = if percent >= 80 {
                Color::Red
            } else if percent >= 60 {
                Color::Yellow
            } else {
                Color::Green
            };
            let label = format!(
                "{} / {} | Avail: {} | {}%",
                format_bytes(usage),
                format_bytes(limit),
                format_bytes(available),
                percent
            );
            (label, Some(threshold), Style::default().fg(color))
        }
        _ => {
            let label = format!(
                "{} / unlimited | RSS Sum: {}",
                format_bytes(usage),
                format_bytes(rss_sum)
            );
            (label, None, Style::default().fg(Color::Gray))
        }
    };

    MemoryGaugeState {
        ratio,
        label,
        gauge_style,
        danger_percent,
    }
}

struct CpuGaugeState {
    ratio: f64,
    label: String,
    gauge_style: Style,
}

fn cpu_gauge_state(processes: &[ProcessSnapshot], cpu_cores: Option<f64>) -> CpuGaugeState {
    let total_cpu: f64 = processes.iter().map(|p| p.cpu_percent).sum();
    let process_count = processes.len();

    let (ratio, label, color) = match cpu_cores {
        Some(cores) if cores > 0.0 => {
            let cpu_percent = total_cpu / cores;
            let ratio = (cpu_percent / 100.0).min(1.0);
            let available = (cores * 100.0 - total_cpu).max(0.0);
            let color = if cpu_percent >= 80.0 {
                Color::Red
            } else if cpu_percent >= 50.0 {
                Color::Yellow
            } else {
                Color::Green
            };
            let label = format!(
                "{:.1}% / {:.1} cores | Avail: {:.1} cores",
                cpu_percent,
                cores,
                available / 100.0
            );
            (ratio, label, color)
        }
        _ => {
            let ratio = (total_cpu / 100.0).min(1.0);
            let color = if total_cpu >= 80.0 {
                Color::Red
            } else if total_cpu >= 50.0 {
                Color::Yellow
            } else {
                Color::Green
            };
            let label = format!("{:.1}% | {} procs", total_cpu, process_count);
            (ratio, label, color)
        }
    };

    CpuGaugeState {
        ratio,
        label,
        gauge_style: Style::default().fg(color),
    }
}

fn render_danger_marker(frame: &mut Frame, area: Rect, block: &Block, percent: u8) {
    let inner = block.inner(area);
    if inner.width == 0 || inner.height == 0 {
        return;
    }

    let width = inner.width as usize;
    let position = ((percent as f64 / 100.0) * (width.saturating_sub(1) as f64)).round() as usize;
    let mut marker_line = vec![' '; width];
    let index = position.min(width.saturating_sub(1));
    marker_line[index] = '│';
    let marker: String = marker_line.into_iter().collect();
    let marker_widget =
        Paragraph::new(marker).style(Style::default().fg(Color::Red).add_modifier(Modifier::BOLD));
    frame.render_widget(marker_widget, inner);
}

fn status_line(app: &App) -> (String, Style) {
    if let Some(confirm) = &app.confirm_kill {
        if confirm.is_system {
            return (
                format!(
                    "⚠ SYSTEM PROCESS — Kill {} {}? This may break the session. (y/n)",
                    confirm.pid, confirm.name
                ),
                Style::default().fg(Color::Red).add_modifier(Modifier::BOLD),
            );
        }
        return (
            format!("Kill process {} {}? (y/n)", confirm.pid, confirm.name),
            Style::default().fg(Color::Yellow),
        );
    }

    if app.view_state.filter_active {
        return (
            format!("Filter: {}_", app.view_state.filter),
            Style::default().fg(Color::Yellow),
        );
    }

    if let Some(message) = &app.status_message {
        return (message.text.clone(), Style::default().fg(Color::Cyan));
    }

    let watched = app.watched_count();
    let recording_label = if watched > 0 {
        format!(
            "REC ● {}/{} W:{} | ",
            app.recording_manager.snapshot_count(),
            app.recording_manager.max_snapshots(),
            watched
        )
    } else {
        format!(
            "REC ● {}/{} | ",
            app.recording_manager.snapshot_count(),
            app.recording_manager.max_snapshots()
        )
    };

    let keys = "q: quit | k: kill | w: watch | R: recordings | s: sort | /: filter | ↑/↓: select";

    if !app.view_state.filter.trim().is_empty() {
        return (
            format!(
                "{}Filter: {} | {}",
                recording_label, app.view_state.filter, keys
            ),
            Style::default().fg(Color::Gray),
        );
    }

    (
        format!("{}{}", recording_label, keys),
        Style::default().fg(Color::Gray),
    )
}

fn header_label(
    label: &str,
    column: SortColumn,
    sort_column: SortColumn,
    sort_ascending: bool,
) -> String {
    if sort_column == column {
        let arrow = if sort_ascending { "▲" } else { "▼" };
        format!("{} {}", label, arrow)
    } else {
        label.to_string()
    }
}

fn centered_rect(percent_x: u16, percent_y: u16, area: Rect) -> Rect {
    let popup_layout = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Percentage((100 - percent_y) / 2),
            Constraint::Percentage(percent_y),
            Constraint::Percentage((100 - percent_y) / 2),
        ])
        .split(area);
    Layout::default()
        .direction(Direction::Horizontal)
        .constraints([
            Constraint::Percentage((100 - percent_x) / 2),
            Constraint::Percentage(percent_x),
            Constraint::Percentage((100 - percent_x) / 2),
        ])
        .split(popup_layout[1])[1]
}

fn format_timestamp(ts: u64) -> String {
    let secs = ts % 60;
    let mins = (ts / 60) % 60;
    let hours = (ts / 3600) % 24;
    format!("{:02}:{:02}:{:02}", hours, mins, secs)
}

fn format_bytes(value: u64) -> String {
    const KB: f64 = 1024.0;
    const MB: f64 = 1024.0 * 1024.0;
    const GB: f64 = 1024.0 * 1024.0 * 1024.0;

    let value_f = value as f64;
    if value_f >= GB {
        format!("{:.1} GB", value_f / GB)
    } else if value_f >= MB {
        format!("{:.1} MB", value_f / MB)
    } else if value_f >= KB {
        format!("{:.1} KB", value_f / KB)
    } else {
        format!("{} B", value)
    }
}
