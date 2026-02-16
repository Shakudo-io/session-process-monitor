use ratatui::layout::{Constraint, Direction, Layout, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::widgets::{Block, Borders, Gauge, Paragraph, Row, Table, TableState};
use ratatui::Frame;

use crate::app::{App, SortColumn};

pub fn draw(frame: &mut Frame, app: &App) {
    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(3),
            Constraint::Min(1),
            Constraint::Length(1),
        ])
        .split(frame.area());

    let gauge_chunks = Layout::default()
        .direction(Direction::Horizontal)
        .constraints([Constraint::Percentage(60), Constraint::Percentage(40)])
        .split(chunks[0]);

    let mem_state = memory_gauge_state(app);
    let mem_block = Block::default().borders(Borders::ALL).title("Pod Memory");
    let mem_gauge = Gauge::default()
        .block(mem_block.clone())
        .ratio(mem_state.ratio)
        .label(mem_state.label)
        .gauge_style(mem_state.gauge_style);
    frame.render_widget(mem_gauge, gauge_chunks[0]);
    if let Some(danger_percent) = mem_state.danger_percent {
        render_danger_marker(frame, gauge_chunks[0], &mem_block, danger_percent);
    }

    let cpu_state = cpu_gauge_state(app);
    let cpu_block = Block::default().borders(Borders::ALL).title("CPU Usage");
    let cpu_gauge = Gauge::default()
        .block(cpu_block)
        .ratio(cpu_state.ratio)
        .label(cpu_state.label)
        .gauge_style(cpu_state.gauge_style);
    frame.render_widget(cpu_gauge, gauge_chunks[1]);

    let header = Row::new(vec![
        header_label("PID", SortColumn::Pid, app),
        header_label("Name", SortColumn::Name, app),
        header_label("Cmdline", SortColumn::Cmdline, app),
        header_label("CPU%", SortColumn::Cpu, app),
        header_label("USS", SortColumn::Uss, app),
        header_label("PSS", SortColumn::Pss, app),
        header_label("RSS", SortColumn::Rss, app),
        header_label("Growth", SortColumn::GrowthRate, app),
        header_label("Read", SortColumn::DiskRead, app),
        header_label("Write", SortColumn::DiskWrite, app),
    ])
    .style(Style::default().add_modifier(Modifier::BOLD));

    let rows = app.processes.iter().map(|process| {
        let style = if process.is_system {
            Style::default()
                .fg(Color::DarkGray)
                .add_modifier(Modifier::DIM)
        } else {
            Style::default()
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

        const CMDLINE_MAX_LEN: usize = 40;
        let cmdline_display = if process.cmdline.len() > CMDLINE_MAX_LEN {
            format!("{}...", &process.cmdline[..CMDLINE_MAX_LEN - 3])
        } else {
            process.cmdline.clone()
        };

        Row::new(vec![
            process.pid.to_string(),
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
            Constraint::Length(8),
            Constraint::Min(12),
            Constraint::Min(30),
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
    .block(Block::default().borders(Borders::ALL).title("Processes"))
    .column_spacing(1)
    .row_highlight_style(Style::default().bg(Color::DarkGray));

    let mut table_state = TableState::default();
    if !app.processes.is_empty() {
        table_state.select(Some(app.view_state.selected));
    }
    frame.render_stateful_widget(table, chunks[1], &mut table_state);

    let (status_text, status_style) = status_line(app);
    let status = Paragraph::new(status_text)
        .style(status_style)
        .block(Block::default().borders(Borders::ALL));
    frame.render_widget(status, chunks[2]);
}

struct MemoryGaugeState {
    ratio: f64,
    label: String,
    gauge_style: Style,
    danger_percent: Option<u8>,
}

fn memory_gauge_state(app: &App) -> MemoryGaugeState {
    let usage = app.pod_memory.cgroup_usage;
    let limit = app.pod_memory.cgroup_limit;
    let rss_sum = app.pod_memory.rss_sum;
    let threshold = app.pod_memory.terminator_threshold_percent.min(100);

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

fn cpu_gauge_state(app: &App) -> CpuGaugeState {
    let total_cpu: f64 = app.processes.iter().map(|p| p.cpu_percent).sum();
    let process_count = app.processes.len();

    let (ratio, label, color) = match app.cpu_cores {
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

    if !app.view_state.filter.trim().is_empty() {
        return (
            format!(
                "Filter: {} | q: quit | k: kill | s: sort | S/r: dir | /: filter | ↑/↓: select",
                app.view_state.filter
            ),
            Style::default().fg(Color::Gray),
        );
    }

    (
        "q: quit | k: kill | s: sort | S/r: dir | /: filter | ↑/↓: select".to_string(),
        Style::default().fg(Color::Gray),
    )
}

fn header_label(label: &str, column: SortColumn, app: &App) -> String {
    if app.view_state.sort_column == column {
        let arrow = if app.view_state.sort_ascending {
            "▲"
        } else {
            "▼"
        };
        format!("{} {}", label, arrow)
    } else {
        label.to_string()
    }
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
