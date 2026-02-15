mod app;
mod cgroup;
mod proc;
mod process;
mod ui;

use std::io;
use std::time::{Duration, Instant};

use crossterm::event::{self, Event, KeyCode};
use crossterm::execute;
use crossterm::terminal::{
    disable_raw_mode, enable_raw_mode, EnterAlternateScreen, LeaveAlternateScreen,
};
use ratatui::backend::CrosstermBackend;
use ratatui::Terminal;

use crate::app::{App, KillConfirmation, SortColumn};

fn setup_terminal() -> io::Result<Terminal<CrosstermBackend<io::Stdout>>> {
    enable_raw_mode()?;
    let mut stdout = io::stdout();
    execute!(stdout, EnterAlternateScreen)?;
    let backend = CrosstermBackend::new(stdout);
    let mut terminal = Terminal::new(backend)?;
    terminal.clear()?;
    Ok(terminal)
}

fn restore_terminal(terminal: &mut Terminal<CrosstermBackend<io::Stdout>>) -> io::Result<()> {
    disable_raw_mode()?;
    execute!(terminal.backend_mut(), LeaveAlternateScreen)?;
    terminal.show_cursor()?;
    Ok(())
}

fn run_app(terminal: &mut Terminal<CrosstermBackend<io::Stdout>>) -> io::Result<()> {
    let mut app = App::new();
    let mut last_tick = Instant::now();
    let tick_rate = Duration::from_secs(1);

    while app.running {
        let timeout = Duration::from_millis(100);
        if event::poll(timeout)? {
            if let Event::Key(key_event) = event::read()? {
                if let Some(confirm) = app.confirm_kill.clone() {
                    match key_event.code {
                        KeyCode::Char('y') => {
                            let outcome = match process::terminate_process(confirm.pid) {
                                Ok(message) => message,
                                Err(message) => message,
                            };
                            app.set_status_message(outcome);
                            app.confirm_kill = None;
                        }
                        KeyCode::Char('n') | KeyCode::Esc => {
                            app.confirm_kill = None;
                        }
                        _ => {}
                    }
                } else if app.view_state.filter_active {
                    let previous_filter = app.view_state.filter.clone();
                    match key_event.code {
                        KeyCode::Char(ch) => {
                            app.view_state.filter.push(ch);
                        }
                        KeyCode::Backspace => {
                            app.view_state.filter.pop();
                        }
                        KeyCode::Esc => {
                            app.view_state.filter.clear();
                            app.view_state.filter_active = false;
                        }
                        KeyCode::Enter => {
                            app.view_state.filter_active = false;
                        }
                        _ => {}
                    }
                    if app.view_state.filter != previous_filter {
                        app.view_state.selected = 0;
                    }
                } else {
                    match key_event.code {
                        KeyCode::Char('q') | KeyCode::Esc => {
                            app.running = false;
                        }
                        KeyCode::Char('/') => {
                            app.view_state.filter_active = true;
                        }
                        KeyCode::Up => {
                            if !app.processes.is_empty() {
                                app.view_state.selected = app.view_state.selected.saturating_sub(1);
                            }
                        }
                        KeyCode::Down => {
                            if !app.processes.is_empty() {
                                let max_index = app.processes.len().saturating_sub(1);
                                app.view_state.selected =
                                    (app.view_state.selected + 1).min(max_index);
                            }
                        }
                        KeyCode::Char('k') => {
                            if let Some(process) = app.selected_process() {
                                app.confirm_kill = Some(KillConfirmation {
                                    pid: process.pid,
                                    name: process.name.clone(),
                                    is_system: process.is_system,
                                });
                            } else {
                                app.set_status_message("No process selected".to_string());
                            }
                        }
                        KeyCode::Char('s') => {
                            app.view_state.sort_column =
                                next_sort_column(app.view_state.sort_column);
                        }
                        KeyCode::Char('S') | KeyCode::Char('r') => {
                            app.view_state.sort_ascending = !app.view_state.sort_ascending;
                        }
                        _ => {}
                    }
                }
            }
        }

        if last_tick.elapsed() >= tick_rate {
            app.tick();
            last_tick = Instant::now();
        }

        terminal.draw(|frame| ui::draw(frame, &app))?;
    }

    Ok(())
}

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let mut terminal = setup_terminal()?;
    let run_result = run_app(&mut terminal);
    let restore_result = restore_terminal(&mut terminal);

    if let Err(error) = &run_result {
        eprintln!("Application error: {error}");
    }
    if let Err(error) = &restore_result {
        eprintln!("Terminal restore error: {error}");
    }

    run_result?;
    restore_result?;
    Ok(())
}

fn next_sort_column(current: SortColumn) -> SortColumn {
    match current {
        SortColumn::Uss => SortColumn::Pss,
        SortColumn::Pss => SortColumn::Rss,
        SortColumn::Rss => SortColumn::Cpu,
        SortColumn::Cpu => SortColumn::GrowthRate,
        SortColumn::GrowthRate => SortColumn::Name,
        SortColumn::Name => SortColumn::Cmdline,
        SortColumn::Cmdline => SortColumn::Pid,
        SortColumn::Pid => SortColumn::DiskRead,
        SortColumn::DiskRead => SortColumn::DiskWrite,
        SortColumn::DiskWrite => SortColumn::Uss,
    }
}
