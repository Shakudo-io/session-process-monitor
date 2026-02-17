mod app;
mod cgroup;
mod proc;
mod process;
mod recording;
mod replay;
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
use crate::replay::{AppMode, PlaybackSpeed, RecordingListState, ReplayState};

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
                } else {
                    let mut recording_to_load: Option<String> = None;
                    let mut recording_to_delete: Option<String> = None;
                    let mut close_recording_list = false;
                    let mut list_selected = 0;
                    let mut was_recording_list = false;
                    let mut exit_replay = false;

                    match &mut app.mode {
                        AppMode::RecordingList(list_state) => {
                            was_recording_list = true;
                            match key_event.code {
                                KeyCode::Up => {
                                    if !list_state.recordings.is_empty() {
                                        list_state.selected = list_state.selected.saturating_sub(1);
                                    }
                                }
                                KeyCode::Down => {
                                    if !list_state.recordings.is_empty() {
                                        let max_index =
                                            list_state.recordings.len().saturating_sub(1);
                                        list_state.selected =
                                            (list_state.selected + 1).min(max_index);
                                    }
                                }
                                KeyCode::Enter => {
                                    if let Some(recording) =
                                        list_state.recordings.get(list_state.selected)
                                    {
                                        recording_to_load = Some(recording.id.clone());
                                    }
                                }
                                KeyCode::Char('d') => {
                                    if let Some(recording) =
                                        list_state.recordings.get(list_state.selected)
                                    {
                                        recording_to_delete = Some(recording.id.clone());
                                    }
                                }
                                KeyCode::Esc => {
                                    close_recording_list = true;
                                }
                                _ => {}
                            }
                            list_selected = list_state.selected;
                        }
                        AppMode::Replay(state) => match key_event.code {
                            KeyCode::Esc | KeyCode::Char('q') => {
                                exit_replay = true;
                            }
                            KeyCode::Left => {
                                state.current_index = state.current_index.saturating_sub(1);
                                state.last_advance_time = Instant::now();
                            }
                            KeyCode::Right => {
                                let max_index = state.recording.snapshots.len().saturating_sub(1);
                                if state.current_index < max_index {
                                    state.current_index += 1;
                                }
                                state.last_advance_time = Instant::now();
                            }
                            KeyCode::PageUp => {
                                state.current_index = state.current_index.saturating_sub(10);
                                state.last_advance_time = Instant::now();
                            }
                            KeyCode::PageDown => {
                                let max_index = state.recording.snapshots.len().saturating_sub(1);
                                state.current_index = (state.current_index + 10).min(max_index);
                                state.last_advance_time = Instant::now();
                            }
                            KeyCode::Home => {
                                state.current_index = 0;
                                state.last_advance_time = Instant::now();
                            }
                            KeyCode::End => {
                                state.current_index =
                                    state.recording.snapshots.len().saturating_sub(1);
                                state.last_advance_time = Instant::now();
                            }
                            KeyCode::Char(' ') => {
                                state.playing = !state.playing;
                                state.last_advance_time = Instant::now();
                            }
                            KeyCode::Char('+') => {
                                state.speed = state.speed.next();
                                state.last_advance_time = Instant::now();
                            }
                            KeyCode::Char('-') => {
                                state.speed = state.speed.prev();
                                state.last_advance_time = Instant::now();
                            }
                            _ => {}
                        },
                        AppMode::Live => {
                            if app.view_state.filter_active {
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
                                    KeyCode::Char('R') => {
                                        let recordings = app.recording_manager.list_recordings();
                                        app.mode = AppMode::RecordingList(RecordingListState {
                                            recordings,
                                            selected: 0,
                                        });
                                    }
                                    KeyCode::Up => {
                                        if !app.processes.is_empty() {
                                            app.view_state.selected =
                                                app.view_state.selected.saturating_sub(1);
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
                                            app.set_status_message(
                                                "No process selected".to_string(),
                                            );
                                        }
                                    }
                                    KeyCode::Char('s') => {
                                        app.view_state.sort_column =
                                            next_sort_column(app.view_state.sort_column);
                                    }
                                    KeyCode::Char('S') | KeyCode::Char('r') => {
                                        app.view_state.sort_ascending =
                                            !app.view_state.sort_ascending;
                                    }
                                    _ => {}
                                }
                            }
                        }
                    }

                    if exit_replay {
                        app.mode = AppMode::Live;
                    }

                    if let Some(recording_id) = recording_to_load {
                        match app.recording_manager.load_recording(&recording_id) {
                            Ok(recording) => {
                                if recording.snapshots.is_empty() {
                                    app.set_status_message(
                                        "Recording has no snapshots".to_string(),
                                    );
                                } else {
                                    app.mode = AppMode::Replay(ReplayState {
                                        recording,
                                        current_index: 0,
                                        speed: PlaybackSpeed::Normal,
                                        playing: false,
                                        last_advance_time: Instant::now(),
                                    });
                                }
                            }
                            Err(error) => {
                                app.set_status_message(format!(
                                    "Failed to load recording: {}",
                                    error
                                ));
                            }
                        }
                    } else if let Some(recording_id) = recording_to_delete {
                        if let Err(error) = app.recording_manager.delete_recording(&recording_id) {
                            app.set_status_message(format!(
                                "Failed to delete recording: {}",
                                error
                            ));
                        }
                        if was_recording_list {
                            let recordings = app.recording_manager.list_recordings();
                            let selected = if recordings.is_empty() {
                                0
                            } else {
                                list_selected.min(recordings.len().saturating_sub(1))
                            };
                            app.mode = AppMode::RecordingList(RecordingListState {
                                recordings,
                                selected,
                            });
                        }
                    } else if close_recording_list {
                        app.mode = AppMode::Live;
                    }
                }
            }
        }

        if last_tick.elapsed() >= tick_rate {
            app.tick();
            last_tick = Instant::now();
        }

        if let AppMode::Replay(state) = &mut app.mode {
            if state.playing {
                let elapsed = state.last_advance_time.elapsed();
                if elapsed >= Duration::from_millis(state.speed.interval_ms()) {
                    if state.recording.snapshots.is_empty() {
                        state.playing = false;
                    } else {
                        let max_index = state.recording.snapshots.len().saturating_sub(1);
                        if state.current_index < max_index {
                            state.current_index += 1;
                            state.last_advance_time = Instant::now();
                        } else {
                            state.playing = false;
                        }
                    }
                }
            }
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
