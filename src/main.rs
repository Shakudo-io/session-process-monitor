mod app;
mod cgroup;
mod guard;
mod health;
mod monitor;
mod policy;
mod proc;
mod process;
mod recording;
mod replay;
mod supervisor;
mod ui;

use std::io;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use clap::{Parser, Subcommand};
use crossterm::event::{self, Event, KeyCode};
use crossterm::execute;
use crossterm::terminal::{
    disable_raw_mode, enable_raw_mode, EnterAlternateScreen, LeaveAlternateScreen,
};
use ratatui::backend::CrosstermBackend;
use ratatui::Terminal;

use crate::app::{App, GuardAlert, KillConfirmation, KillTarget, SortColumn};
use crate::replay::{AppMode, PlaybackSpeed, RecordingListState, ReplayState};

#[derive(Parser, Debug, Clone)]
#[command(name = "spm", about = "Session Process Monitor")]
struct Cli {
    #[command(subcommand)]
    command: Option<Commands>,

    /// Start with recordings list open
    #[arg(long, conflicts_with = "replay", global = true)]
    record: bool,

    /// Replay a recording by id
    #[arg(long, global = true)]
    replay: Option<String>,

    /// Enable dark mode theme
    #[arg(long, global = true)]
    dark: bool,
}

#[derive(Subcommand, Debug, Clone)]
enum Commands {
    /// Launch and supervise processes
    Run {
        /// Commands to run (quoted strings)
        #[arg(required = true)]
        commands: Vec<String>,

        /// Run without TUI, emit JSON events
        #[arg(long)]
        headless: bool,

        /// Pod memory % to trigger kill (default: 75)
        #[arg(long, env = "SPM_GUARD_KILL_THRESHOLD", default_value = "75")]
        kill_threshold: u8,

        /// Consecutive seconds above threshold before kill
        #[arg(long, env = "SPM_GUARD_GRACE_TICKS", default_value = "3")]
        grace_ticks: u8,

        /// Max restarts before marking Failed
        #[arg(long, env = "SPM_GUARD_MAX_RESTARTS", default_value = "10")]
        max_restarts: u32,

        /// Path for JSON event log file
        #[arg(long, env = "SPM_GUARD_LOG")]
        log: Option<PathBuf>,
    },
}

#[derive(Debug, Clone)]
struct TuiConfig {
    record: bool,
    replay: Option<String>,
    dark_mode: bool,
}

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

fn run_app(
    terminal: &mut Terminal<CrosstermBackend<io::Stdout>>,
    config: &TuiConfig,
) -> io::Result<()> {
    let mut app = App::new();
    app.dark_mode = config.dark_mode;

    if let Some(recording_id) = &config.replay {
        match app.recording_manager.load_recording(recording_id) {
            Ok(recording) => {
                if recording.snapshots.is_empty() {
                    app.set_status_message("Recording has no snapshots".to_string());
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
                app.set_status_message(format!("Failed to load recording: {}", error));
            }
        }
    } else if config.record {
        let recordings = app.recording_manager.list_recordings();
        app.mode = AppMode::RecordingList(RecordingListState {
            recordings,
            selected: 0,
        });
    }
    let mut last_tick = Instant::now();
    let tick_rate = Duration::from_secs(1);

    while app.running {
        let timeout = Duration::from_millis(100);
        if event::poll(timeout)? {
            if let Event::Key(key_event) = event::read()? {
                if let Some(confirm) = app.confirm_kill.clone() {
                    match key_event.code {
                        KeyCode::Char('y') => {
                            let outcome = match confirm.target {
                                KillTarget::Process { pid, .. } => {
                                    match process::terminate_process(pid) {
                                        Ok(message) => message,
                                        Err(message) => message,
                                    }
                                }
                                KillTarget::Managed { pgid, .. } => match pgid {
                                    Some(pgid) => match process::kill_process_group(pgid, false) {
                                        Ok(message) => message,
                                        Err(message) => message,
                                    },
                                    None => "Managed process missing pgid".to_string(),
                                },
                            };
                            app.set_status_message(outcome);
                            app.confirm_kill = None;
                        }
                        KeyCode::Char('n') | KeyCode::Esc => {
                            app.confirm_kill = None;
                        }
                        _ => {}
                    }
                } else if app.show_cmdline.is_some() {
                    app.show_cmdline = None;
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
                                    KeyCode::Char('q') => {
                                        app.running = false;
                                    }
                                    KeyCode::Esc => {
                                        if !app.view_state.filter.is_empty() {
                                            app.view_state.filter.clear();
                                            app.view_state.selected = 0;
                                        } else {
                                            app.running = false;
                                        }
                                    }
                                    KeyCode::Char('/') => {
                                        app.view_state.filter_active = true;
                                    }
                                    KeyCode::Enter => {
                                        if let Some(process) = app.selected_process() {
                                            app.show_cmdline = Some((
                                                process.pid,
                                                process.name.clone(),
                                                process.cmdline.clone(),
                                            ));
                                        }
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
                                                target: KillTarget::Process {
                                                    pid: process.pid,
                                                    name: process.name.clone(),
                                                    is_system: process.is_system,
                                                },
                                            });
                                        } else {
                                            app.set_status_message(
                                                "No process selected".to_string(),
                                            );
                                        }
                                    }
                                    KeyCode::Char('w') => {
                                        app.toggle_watch();
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
    let cli = Cli::parse();
    let config = TuiConfig {
        record: cli.record,
        replay: cli.replay.clone(),
        dark_mode: cli.dark,
    };
    match cli.command {
        None => run_tui(config),
        Some(Commands::Run {
            commands,
            headless,
            kill_threshold,
            grace_ticks,
            max_restarts,
            log,
        }) => run_supervisor(
            commands,
            headless,
            kill_threshold,
            grace_ticks,
            max_restarts,
            log,
            config.dark_mode,
        ),
    }
}

fn run_tui(config: TuiConfig) -> Result<(), Box<dyn std::error::Error>> {
    let mut terminal = setup_terminal()?;
    let run_result = run_app(&mut terminal, &config);
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

fn run_supervisor(
    commands: Vec<String>,
    headless: bool,
    kill_threshold: u8,
    grace_ticks: u8,
    max_restarts: u32,
    log_path: Option<PathBuf>,
    dark_mode: bool,
) -> Result<(), Box<dyn std::error::Error>> {
    let is_tty = unsafe { libc::isatty(libc::STDOUT_FILENO) } == 1;
    let effective_headless = headless || !is_tty;
    if !effective_headless {
        println!("Supervisor mode: {} commands", commands.len());
    }

    let config = guard::GuardConfig {
        kill_threshold_percent: kill_threshold,
        emergency_threshold_percent: 78,
        grace_ticks,
        max_restarts,
        enabled: true,
        post_kill_cooldown: std::time::Duration::from_secs(5),
    };
    let guard = Arc::new(Mutex::new(guard::Guard::new(config)));
    let policy = policy::ProtectionPolicy::new();

    let commands: Vec<String> = commands
        .into_iter()
        .filter(|cmd| {
            let trimmed = cmd.trim();
            if trimmed.is_empty() {
                eprintln!("[spm] Warning: skipping empty command");
                false
            } else {
                true
            }
        })
        .collect();

    if commands.is_empty() {
        eprintln!("[spm] Error: no valid commands to run");
        return Ok(());
    }

    let mut children: Vec<supervisor::ManagedChild> = commands
        .iter()
        .enumerate()
        .map(|(index, cmd)| supervisor::ManagedChild::new(index, cmd.clone()))
        .collect();

    let (tx, rx) = std::sync::mpsc::channel();

    for child in children.iter_mut() {
        match supervisor::spawn_child(child, effective_headless) {
            Ok(spawned) => {
                if !effective_headless {
                    eprintln!("[spm] Spawned '{}' (PID {})", child.command, spawned.pid);
                } else {
                    let name = supervisor::command_name(&child.command);
                    if let Some(stdout) = spawned.stdout {
                        supervisor::spawn_output_reader(name.clone(), stdout, false);
                    }
                    if let Some(stderr) = spawned.stderr {
                        supervisor::spawn_output_reader(name, stderr, true);
                    }
                }
            }
            Err(error) => {
                if !effective_headless {
                    eprintln!("[spm] Failed to spawn '{}': {}", child.command, error);
                }
                child.state = supervisor::ChildState::Failed;
            }
        }
    }

    for child in children.iter() {
        if let Some(pid) = child.pid {
            let _ = tx.send(monitor::MonitorEvent::Spawn {
                index: child.index,
                cmd: child.command.clone(),
                pid,
                log_path: child.log_path.clone(),
            });
        }
    }

    let managed = Arc::new(Mutex::new(children));

    let _monitor = monitor::spawn_monitor_thread(
        Arc::clone(&managed),
        Arc::clone(&guard),
        policy,
        effective_headless,
        tx,
    );

    if effective_headless {
        run_supervisor_headless(rx, managed, log_path);
        return Ok(());
    }

    run_supervisor_tui(managed, guard, rx, dark_mode)
}

fn run_supervisor_tui(
    managed: Arc<Mutex<Vec<supervisor::ManagedChild>>>,
    guard: Arc<Mutex<guard::Guard>>,
    rx: std::sync::mpsc::Receiver<monitor::MonitorEvent>,
    dark_mode: bool,
) -> Result<(), Box<dyn std::error::Error>> {
    let mut terminal = setup_terminal()?;
    let run_result = run_supervisor_app(&mut terminal, managed, guard, rx, dark_mode);
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

fn run_supervisor_app(
    terminal: &mut Terminal<CrosstermBackend<io::Stdout>>,
    managed: Arc<Mutex<Vec<supervisor::ManagedChild>>>,
    guard: Arc<Mutex<guard::Guard>>,
    rx: std::sync::mpsc::Receiver<monitor::MonitorEvent>,
    dark_mode: bool,
) -> io::Result<()> {
    let mut app = App::new();
    app.dark_mode = dark_mode;
    app.supervisor_mode = true;
    app.local_supervisor = true;
    if let Ok(children) = managed.lock() {
        app.managed_children = children.clone();
    }
    if let Ok(guard) = guard.lock() {
        app.guard = Some(guard.clone());
    }

    let mut last_tick = Instant::now();
    let tick_rate = Duration::from_secs(1);

    while app.running {
        let timeout = Duration::from_millis(100);
        if event::poll(timeout)? {
            if let Event::Key(key_event) = event::read()? {
                if let Some(confirm) = app.confirm_kill.clone() {
                    match key_event.code {
                        KeyCode::Char('y') => {
                            let outcome = match confirm.target {
                                KillTarget::Process { pid, .. } => {
                                    match process::terminate_process(pid) {
                                        Ok(message) => message,
                                        Err(message) => message,
                                    }
                                }
                                KillTarget::Managed { pgid, .. } => match pgid {
                                    Some(pgid) => match process::kill_process_group(pgid, false) {
                                        Ok(message) => message,
                                        Err(message) => message,
                                    },
                                    None => "Managed process missing pgid".to_string(),
                                },
                            };
                            app.set_status_message(outcome);
                            app.confirm_kill = None;
                        }
                        KeyCode::Char('n') | KeyCode::Esc => {
                            app.confirm_kill = None;
                        }
                        _ => {}
                    }
                } else if app.show_cmdline.is_some() {
                    app.show_cmdline = None;
                } else if app.show_log.is_some() {
                    app.show_log = None;
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
                                    KeyCode::Char('q') => {
                                        monitor::request_shutdown();
                                        app.running = false;
                                    }
                                    KeyCode::Esc => {
                                        if !app.view_state.filter.is_empty() {
                                            app.view_state.filter.clear();
                                            app.view_state.selected = 0;
                                        } else {
                                            monitor::request_shutdown();
                                            app.running = false;
                                        }
                                    }
                                    KeyCode::Char('/') => {
                                        app.view_state.filter_active = true;
                                    }
                                    KeyCode::Enter => {
                                        if let Some(process) = app.selected_process() {
                                            app.show_cmdline = Some((
                                                process.pid,
                                                process.name.clone(),
                                                process.cmdline.clone(),
                                            ));
                                        }
                                    }
                                    KeyCode::Char('R') => {
                                        let recordings = app.recording_manager.list_recordings();
                                        app.mode = AppMode::RecordingList(RecordingListState {
                                            recordings,
                                            selected: 0,
                                        });
                                    }
                                    KeyCode::Tab | KeyCode::BackTab => {
                                        if app.supervisor_mode && !app.managed_children.is_empty() {
                                            app.focus = match app.focus {
                                                app::FocusPane::Processes => {
                                                    app::FocusPane::Managed
                                                }
                                                app::FocusPane::Managed => {
                                                    app::FocusPane::Processes
                                                }
                                            };
                                        }
                                    }
                                    KeyCode::Up => match app.focus {
                                        app::FocusPane::Processes => {
                                            if !app.processes.is_empty() {
                                                app.view_state.selected =
                                                    app.view_state.selected.saturating_sub(1);
                                            }
                                        }
                                        app::FocusPane::Managed => {
                                            app.selected_managed =
                                                app.selected_managed.saturating_sub(1);
                                        }
                                    },
                                    KeyCode::Down => match app.focus {
                                        app::FocusPane::Processes => {
                                            if !app.processes.is_empty() {
                                                let max_index =
                                                    app.processes.len().saturating_sub(1);
                                                app.view_state.selected =
                                                    (app.view_state.selected + 1).min(max_index);
                                            }
                                        }
                                        app::FocusPane::Managed => {
                                            if !app.managed_children.is_empty() {
                                                let max_index =
                                                    app.managed_children.len().saturating_sub(1);
                                                app.selected_managed =
                                                    (app.selected_managed + 1).min(max_index);
                                            }
                                        }
                                    },
                                    KeyCode::Char('k') => {
                                        if app.focus == app::FocusPane::Managed {
                                            if let Some(child) =
                                                app.managed_children.get(app.selected_managed)
                                            {
                                                app.confirm_kill = Some(KillConfirmation {
                                                    target: KillTarget::Managed {
                                                        index: child.index,
                                                        command: child.command.clone(),
                                                        pid: child.pid,
                                                        pgid: child.pgid,
                                                    },
                                                });
                                            }
                                        } else if let Some(process) = app.selected_process() {
                                            let managed_target = app
                                                .managed_children
                                                .iter()
                                                .find(|child| child.pid == Some(process.pid));

                                            if let Some(child) = managed_target {
                                                app.confirm_kill = Some(KillConfirmation {
                                                    target: KillTarget::Managed {
                                                        index: child.index,
                                                        command: child.command.clone(),
                                                        pid: child.pid,
                                                        pgid: child.pgid,
                                                    },
                                                });
                                            } else {
                                                app.confirm_kill = Some(KillConfirmation {
                                                    target: KillTarget::Process {
                                                        pid: process.pid,
                                                        name: process.name.clone(),
                                                        is_system: process.is_system,
                                                    },
                                                });
                                            }
                                        } else {
                                            app.set_status_message(
                                                "No process selected".to_string(),
                                            );
                                        }
                                    }
                                    KeyCode::Char('w') => {
                                        app.toggle_watch();
                                    }
                                    KeyCode::Char('s') => {
                                        app.view_state.sort_column =
                                            next_sort_column(app.view_state.sort_column);
                                    }
                                    KeyCode::Char('S') => {
                                        app.view_state.sort_ascending =
                                            !app.view_state.sort_ascending;
                                    }
                                    KeyCode::Char('r') => {
                                        if app.focus == app::FocusPane::Managed {
                                            if let Some(child) =
                                                app.managed_children.get(app.selected_managed)
                                            {
                                                if child.pid.is_some() {
                                                    app.restart_requested = Some(child.index);
                                                    app.set_status_message(format!(
                                                        "Restarting '{}'...",
                                                        child.command
                                                    ));
                                                } else {
                                                    app.set_status_message(
                                                        "Process not running".to_string(),
                                                    );
                                                }
                                            }
                                        } else {
                                            app.view_state.sort_ascending =
                                                !app.view_state.sort_ascending;
                                        }
                                    }
                                    KeyCode::Char('l') => {
                                        if app.focus == app::FocusPane::Managed {
                                            if let Some(child) =
                                                app.managed_children.get(app.selected_managed)
                                            {
                                                if let Some(ref log_path) = child.log_path {
                                                    match std::fs::read_to_string(log_path) {
                                                        Ok(content) => {
                                                            let lines: Vec<&str> =
                                                                content.lines().collect();
                                                            let start =
                                                                lines.len().saturating_sub(50);
                                                            let tail = lines[start..].join("\n");
                                                            app.show_log = Some((
                                                                format!(
                                                                    "{} — {}",
                                                                    child.command,
                                                                    log_path.display()
                                                                ),
                                                                tail,
                                                            ));
                                                        }
                                                        Err(e) => app.set_status_message(format!(
                                                            "Cannot read log: {e}"
                                                        )),
                                                    }
                                                } else {
                                                    app.set_status_message(
                                                        "No log file (headless mode)".to_string(),
                                                    );
                                                }
                                            }
                                        }
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

        let mut saw_state_update = false;
        while let Ok(event) = rx.try_recv() {
            match event {
                monitor::MonitorEvent::GuardWarning {
                    pod_percent,
                    ticks_remaining,
                } => {
                    app.guard_alert = Some(GuardAlert::Triggered {
                        percent: pod_percent,
                        ticks_remaining,
                    });
                }
                monitor::MonitorEvent::GuardKill {
                    cmd,
                    pid,
                    uss,
                    pod_percent,
                    ..
                } => {
                    app.guard_alert = None;
                    let message = format!(
                        "⚡ Killed {} (PID {}) — pod at {:.0}%, freed {}",
                        cmd,
                        pid,
                        pod_percent.round(),
                        format_bytes(uss)
                    );
                    app.set_status_message_with_duration(message, Duration::from_secs(5));
                }
                monitor::MonitorEvent::GuardExhausted { pod_percent } => {
                    app.guard_alert = Some(GuardAlert::Exhausted {
                        percent: pod_percent,
                    });
                }
                monitor::MonitorEvent::HealthKill {
                    cmd, pid, endpoint, ..
                } => {
                    let message = format!(
                        "⚡ Killed {} (PID {}) — health check failed {}",
                        cmd, pid, endpoint
                    );
                    app.set_status_message_with_duration(message, Duration::from_secs(5));
                }
                monitor::MonitorEvent::StateUpdate => {
                    saw_state_update = true;
                }
                _ => {}
            }
        }

        if let Some(restart_idx) = app.restart_requested.take() {
            if let Ok(mut children) = managed.lock() {
                if let Some(child) = children.get_mut(restart_idx) {
                    if let Some(pgid) = child.pgid {
                        let _ = crate::process::kill_process_group(pgid, false);
                        child.state = crate::supervisor::ChildState::Stopping { emergency: false };
                    }
                }
            }
        }

        if saw_state_update {
            if let Ok(children) = managed.lock() {
                app.managed_children = children.clone();
            }
            if let Ok(guard) = guard.lock() {
                app.guard = Some(guard.clone());
            }

            if !app.managed_children.is_empty()
                && app.managed_children.iter().all(|child| {
                    matches!(
                        child.state,
                        supervisor::ChildState::Completed | supervisor::ChildState::Failed
                    )
                })
            {
                app.running = false;
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

        if let Some(GuardAlert::Triggered { .. }) = app.guard_alert {
            if let (Some(guard), Some(percent)) = (&app.guard, pod_memory_percent(&app)) {
                if percent < guard.config.kill_threshold_percent as f64 {
                    app.guard_alert = None;
                }
            }
        }

        terminal.draw(|frame| ui::draw(frame, &app))?;
    }

    Ok(())
}

fn run_supervisor_headless(
    rx: std::sync::mpsc::Receiver<monitor::MonitorEvent>,
    managed: Arc<Mutex<Vec<supervisor::ManagedChild>>>,
    log_path: Option<PathBuf>,
) {
    let mut log_file = log_path.and_then(|p| std::fs::File::create(p).ok());

    loop {
        let mut events = Vec::new();
        match rx.recv() {
            Ok(event) => events.push(event),
            Err(_) => break,
        }

        while let Ok(event) = rx.try_recv() {
            events.push(event);
        }

        let mut signal_shutdown = false;
        for event in &events {
            if matches!(event, monitor::MonitorEvent::SignalShutdown) {
                signal_shutdown = true;
            }
            if let Some(json) = monitor::event_to_json(event) {
                eprintln!("{}", json);
                if let Some(ref mut file) = log_file {
                    use std::io::Write;
                    let _ = writeln!(file, "{}", json);
                }
            }
        }

        if signal_shutdown {
            monitor::remove_shared_state();
            break;
        }

        if let Ok(children) = managed.lock() {
            if !children.is_empty()
                && children.iter().all(|child| {
                    matches!(
                        child.state,
                        supervisor::ChildState::Completed | supervisor::ChildState::Failed
                    )
                })
            {
                let reason = if monitor::is_shutdown_requested() {
                    "signal"
                } else {
                    "all_terminal"
                };
                let shutdown = format!(
                    "{{\"ts\":\"{}\",\"event\":\"shutdown\",\"reason\":\"{}\"}}",
                    monitor::chrono_like_timestamp(),
                    reason,
                );
                eprintln!("{}", shutdown);
                if let Some(ref mut file) = log_file {
                    use std::io::Write;
                    let _ = writeln!(file, "{}", shutdown);
                }
                monitor::remove_shared_state();
                break;
            }
        }
    }
}

fn pod_memory_percent(app: &App) -> Option<f64> {
    let limit = app.pod_memory.cgroup_limit?;
    if limit == 0 {
        return None;
    }
    Some((app.pod_memory.cgroup_usage as f64 / limit as f64) * 100.0)
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
