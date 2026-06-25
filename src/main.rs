mod app;
mod config;
mod tmux;
mod ui;

use anyhow::{Result, bail};
use app::{App, Proc, ProcStatus};
use crossterm::{
    event::{self, DisableMouseCapture, EnableMouseCapture, Event, KeyCode, KeyModifiers},
    execute,
    terminal::{EnterAlternateScreen, LeaveAlternateScreen, disable_raw_mode, enable_raw_mode},
};
use ratatui::{Terminal, backend::CrosstermBackend};
use std::io;
use std::time::Duration;

fn main() -> Result<()> {
    // Wrapper subcommand: tmprocs wrap <status_file> <shell_cmd>
    let cli_args: Vec<String> = std::env::args().collect();
    if cli_args.get(1).map(|s| s.as_str()) == Some("wrap") {
        let status_file = cli_args
            .get(2)
            .ok_or_else(|| anyhow::anyhow!("wrap: missing status file"))?;
        let shell_cmd = cli_args
            .get(3)
            .ok_or_else(|| anyhow::anyhow!("wrap: missing command"))?;
        return run_wrapper(status_file, shell_cmd);
    }
    // Must be inside tmux.
    let (session, _window, my_pane_id) = tmux::current_pane()
        .map_err(|_| anyhow::anyhow!("tmprocs must be run inside a tmux session"))?;

    // Find and load config.
    let config_path = config::find_config()
        .ok_or_else(|| anyhow::anyhow!("no mprocs.yml / tmprocs.yml found in current directory"))?;
    let cfg = config::load(&config_path)?;

    if cfg.procs.is_empty() {
        bail!("no procs defined in config");
    }

    let bg_session = tmux::TmuxSession::new(session);

    // Start each process in the background session.
    let mut procs: Vec<Proc> = Vec::new();
    let mut names: Vec<String> = cfg.procs.keys().cloned().collect();
    names.sort();

    for name in &names {
        let proc_cfg = &cfg.procs[name];
        let cmd = proc_cfg
            .command()
            .unwrap_or_else(|| "echo 'no command'".to_string());
        bg_session.start_proc(name, &cmd)?;
        procs.push(Proc {
            name: name.clone(),
            cmd,
            status: ProcStatus::Running,
            is_shown: false,
        });
    }

    let mut app = App::new(procs, bg_session, my_pane_id);

    // Show the first process immediately.
    if !app.procs.is_empty() {
        app.show_selected()?;
    }

    run_tui(&mut app)?;

    // Kill child process groups before tearing down panes/windows.
    for p in &app.procs {
        let status_file = app.bg_session.status_file_for(&p.name);
        let _ = tmux::kill_child_in_wrapper(&status_file);
    }
    // Kill the right pane that's currently joined into our window.
    if let Some(right_id) = &app.right_pane_id {
        let _ = tmux::kill_pane(right_id);
    }
    // Kill remaining background windows.
    app.bg_session.cleanup()?;
    // Remove wrapper status files.
    for p in &app.procs {
        let _ = std::fs::remove_file(app.bg_session.status_file_for(&p.name));
    }

    Ok(())
}

fn run_tui(app: &mut App) -> Result<()> {
    enable_raw_mode()?;
    let mut stdout = io::stdout();
    execute!(stdout, EnterAlternateScreen, EnableMouseCapture)?;
    let backend = CrosstermBackend::new(stdout);
    let mut terminal = Terminal::new(backend)?;

    let result = tui_loop(app, &mut terminal);

    // Always restore the terminal, even if the loop errored.
    let _ = disable_raw_mode();
    let _ = execute!(
        terminal.backend_mut(),
        LeaveAlternateScreen,
        DisableMouseCapture
    );
    let _ = terminal.show_cursor();
    result
}

fn tui_loop(
    app: &mut App,
    terminal: &mut ratatui::Terminal<CrosstermBackend<io::Stdout>>,
) -> Result<()> {
    let status_interval = Duration::from_millis(1000);
    let swap_throttle = Duration::from_millis(250);
    let mut last_swap: Option<std::time::Instant> = None;
    let mut nav_pending = false;

    loop {
        terminal.draw(|f| ui::draw(f, app))?;

        let timeout = if nav_pending {
            last_swap
                .map(|t| swap_throttle.saturating_sub(t.elapsed()))
                .unwrap_or(Duration::ZERO)
        } else {
            status_interval
        };

        if event::poll(timeout)? {
            match event::read()? {
                Event::Key(key) => match (key.modifiers, key.code) {
                    (_, KeyCode::Char('q')) | (KeyModifiers::CONTROL, KeyCode::Char('c')) => {
                        app.should_quit = true;
                    }
                    (_, KeyCode::Up) | (_, KeyCode::Char('k')) => {
                        app.move_up();
                        let throttle_clear = last_swap.map_or(true, |t| t.elapsed() >= swap_throttle);
                        if throttle_clear {
                            if let Err(e) = app.show_selected() {
                                eprintln!("error showing proc: {e}");
                            }
                            last_swap = Some(std::time::Instant::now());
                            nav_pending = false;
                        } else {
                            nav_pending = true;
                        }
                    }
                    (_, KeyCode::Down) | (_, KeyCode::Char('j')) => {
                        app.move_down();
                        let throttle_clear = last_swap.map_or(true, |t| t.elapsed() >= swap_throttle);
                        if throttle_clear {
                            if let Err(e) = app.show_selected() {
                                eprintln!("error showing proc: {e}");
                            }
                            last_swap = Some(std::time::Instant::now());
                            nav_pending = false;
                        } else {
                            nav_pending = true;
                        }
                    }
                    (_, KeyCode::Enter) => {
                        if nav_pending {
                            if let Err(e) = app.show_selected() {
                                eprintln!("error showing proc: {e}");
                            }
                            last_swap = Some(std::time::Instant::now());
                            nav_pending = false;
                        }
                        if let Some(ref pane_id) = app.right_pane_id.clone() {
                            let _ = tmux::focus_pane(pane_id);
                        }
                    }
                    (_, KeyCode::Char('s')) => {
                        if let Err(e) = app.restart_selected() {
                            eprintln!("error restarting proc: {e}");
                        }
                    }
                    (_, KeyCode::Char('r')) => {
                        if let Err(e) = app.force_restart_selected() {
                            eprintln!("error restarting proc: {e}");
                        }
                        terminal.clear()?;
                    }
                    (_, KeyCode::Char('x')) => {
                        if let Err(e) = app.kill_selected() {
                            eprintln!("error killing proc: {e}");
                        }
                    }
                    _ => {}
                },
                Event::Resize(_, _) => {
                    terminal.clear()?;
                }
                _ => {}
            }
        } else if nav_pending {
            if let Err(e) = app.show_selected() {
                eprintln!("error showing proc: {e}");
            }
            last_swap = Some(std::time::Instant::now());
            nav_pending = false;
        } else {
            app.refresh_status();
        }

        if app.should_quit {
            break;
        }
    }
    Ok(())
}

/// Wrapper mode: run a shell command, keep the pane alive after exit, and
/// allow restarting by pressing 's'.
///
/// Sets SIGINT to SIG_IGN so the wrapper survives Ctrl+C, but resets it to
/// SIG_DFL in the child before exec so the child responds to Ctrl+C normally.
fn run_wrapper(status_file: &str, shell_cmd: &str) -> Result<()> {
    use crossterm::terminal::{disable_raw_mode, enable_raw_mode};
    use std::io::Read;
    use std::os::unix::process::CommandExt;

    unsafe { libc::signal(libc::SIGINT, libc::SIG_IGN) };

    loop {
        let mut child = unsafe {
            std::process::Command::new("sh")
                .arg("-c")
                .arg(shell_cmd)
                .pre_exec(|| {
                    libc::signal(libc::SIGINT, libc::SIG_DFL);
                    libc::setpgid(0, 0); // new process group; PGID == child PID
                    // Make child the terminal foreground group so interactive apps
                    // (emacs, psql, shells) can read stdin without getting SIGTTIN.
                    let pgid = libc::getpid();
                    let old_ttou = libc::signal(libc::SIGTTOU, libc::SIG_IGN);
                    libc::tcsetpgrp(libc::STDIN_FILENO, pgid);
                    libc::signal(libc::SIGTTOU, old_ttou);
                    Ok(())
                })
                .spawn()?
        };

        let child_pid = child.id();
        std::fs::write(status_file, format!("running:{child_pid}"))?;

        loop {
            match child.try_wait() {
                Ok(Some(status)) => {
                    let code = status.code().unwrap_or(-1);
                    let _ = std::fs::write(status_file, format!("dead:{code}"));
                    let msg = if code == 0 {
                        "success".to_string()
                    } else {
                        format!("code {code}")
                    };
                    println!("\n\x1b[31m[process exited: {msg}]\x1b[0m");
                    print!("\x1b[90mpress 's' to restart: \x1b[0m");
                    let _ = std::io::Write::flush(&mut std::io::stdout());
                    break;
                }
                Ok(None) => {}
                Err(e) => {
                    let _ = std::fs::write(status_file, "dead:-1");
                    eprintln!("[wrapper error: {e}]");
                    loop {
                        std::thread::sleep(std::time::Duration::from_secs(3600));
                    }
                }
            }
            std::thread::sleep(std::time::Duration::from_millis(100));
        }

        // Reclaim terminal foreground group so the wrapper can read the 's' key.
        unsafe {
            let wrapper_pgid = libc::getpgrp();
            let old_ttou = libc::signal(libc::SIGTTOU, libc::SIG_IGN);
            libc::tcsetpgrp(libc::STDIN_FILENO, wrapper_pgid);
            libc::signal(libc::SIGTTOU, old_ttou);
        }

        // Wait for 's' keypress to restart; ignore other keys.
        loop {
            let _ = enable_raw_mode();
            let mut buf = [0u8; 1];
            let n = std::io::stdin().read(&mut buf).unwrap_or(0);
            let _ = disable_raw_mode();
            if n > 0 && (buf[0] == b's' || buf[0] == b'S') {
                break;
            }
            if n == 0 {
                std::thread::sleep(std::time::Duration::from_millis(100));
            }
        }
        println!("\n[restarting...]");
    }
}
