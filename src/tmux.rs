use anyhow::{Result, bail};
use std::process::Command;

/// Manages process windows as hidden windows in the user's own tmux session,
/// using a per-instance prefix so they don't pollute the session chooser.
pub struct TmuxSession {
    pub session_name: String,
    prefix: String,
}

impl TmuxSession {
    /// Use the current tmux session; process windows get a `_tp<pid>_` prefix.
    pub fn new(session_name: String) -> Self {
        TmuxSession {
            prefix: format!("_tp{}_", std::process::id()),
            session_name,
        }
    }

    /// Start a process as a new (detached) window in the current session,
    /// wrapped by the tmprocs wrapper so the pane stays alive after exit.
    pub fn start_proc(&self, name: &str, shell_cmd: &str) -> Result<String> {
        let window_name = format!("{}{}", self.prefix, name);
        let window_target = format!("{}:{}", self.session_name, window_name);
        let wrapper_cmd = self.wrapper_cmd_for(name, shell_cmd)?;
        let output = Command::new("tmux")
            .args([
                "new-window",
                "-d",
                "-t",
                &self.session_name,
                "-n",
                &window_name,
                &wrapper_cmd,
                ";",
                "set-window-option",
                "-t",
                &window_target,
                "remain-on-exit",
                "on",
            ])
            .output()?;
        if !output.status.success() {
            bail!(
                "tmux new-window failed: {}",
                String::from_utf8_lossy(&output.stderr)
            );
        }
        Ok(format!("{}:{}", self.session_name, window_name))
    }

    pub fn window_id(&self, name: &str) -> String {
        format!("{}:{}{}", self.session_name, self.prefix, name)
    }

    /// Kill all windows owned by this instance.
    pub fn cleanup(&self) -> Result<()> {
        // list-windows, filter by prefix, kill each
        let out = Command::new("tmux")
            .args([
                "list-windows",
                "-t",
                &self.session_name,
                "-F",
                "#{window_name}",
            ])
            .output()?;
        if !out.status.success() {
            return Ok(()); // session may already be gone
        }
        for wname in String::from_utf8(out.stdout)?.lines() {
            if wname.starts_with(&self.prefix) {
                let _ = Command::new("tmux")
                    .args([
                        "kill-window",
                        "-t",
                        &format!("{}:{}", self.session_name, wname),
                    ])
                    .status();
            }
        }
        Ok(())
    }

    pub fn window_name_for(&self, name: &str) -> String {
        format!("{}{}", self.prefix, name)
    }

    pub fn status_file_for(&self, name: &str) -> String {
        format!("/tmp/tmprocs_{}{}", self.prefix, name)
    }

    pub fn wrapper_cmd_for(&self, name: &str, shell_cmd: &str) -> Result<String> {
        let exe = std::env::current_exe()?;
        let status_file = self.status_file_for(name);
        Ok(format!(
            "{} wrap {} {}",
            exe.display(),
            status_file,
            shell_quote(shell_cmd)
        ))
    }
}

fn shell_quote(s: &str) -> String {
    format!("'{}'", s.replace('\'', "'\\''"))
}

/// Kill the child process tracked by a wrapper status file.
/// Sends SIGTERM to the child PID recorded as `running:<pid>`.
/// Returns an error if the status file is missing/unreadable, the proc
/// is not running, or the `kill` command fails.
pub fn kill_child_in_wrapper(status_file: &str) -> Result<()> {
    let content = std::fs::read_to_string(status_file)
        .map_err(|e| anyhow::anyhow!("cannot read status file: {e}"))?;
    let pid_str = content
        .trim()
        .strip_prefix("running:")
        .ok_or_else(|| anyhow::anyhow!("process is not running"))?;
    let pgid = format!("-{pid_str}"); // negative = kill whole process group
    let status = Command::new("kill")
        .args(["-TERM", &pgid])
        .status()
        .map_err(|e| anyhow::anyhow!("kill failed: {e}"))?;
    if !status.success() {
        bail!("kill exited with {status}");
    }
    Ok(())
}

/// Get the current tmux session and window/pane we're running in.
pub fn current_pane() -> Result<(String, String, String)> {
    let out = Command::new("tmux")
        .args([
            "display-message",
            "-p",
            "#S\t#W\t#D", // session, window name, pane id
        ])
        .output()?;
    if !out.status.success() {
        bail!("not running inside tmux");
    }
    let s = String::from_utf8(out.stdout)?;
    let parts: Vec<&str> = s.trim().splitn(3, '\t').collect();
    if parts.len() != 3 {
        bail!("unexpected tmux display-message output: {s}");
    }
    Ok((
        parts[0].to_string(),
        parts[1].to_string(),
        parts[2].to_string(),
    ))
}

/// Join a process pane to the right of `left_pane_id`, returning the new right pane's ID.
/// `max_left_cols`: if Some, resize the left pane to at most that width in the same chain.
pub fn join_pane_right(
    src_window: &str,
    left_pane_id: &str,
    max_left_cols: Option<u16>,
) -> Result<String> {
    let mut args = vec![
        "join-pane",
        "-h",
        "-d",
        "-s",
        src_window,
        "-t",
        left_pane_id,
    ];
    let cols_str;
    if let Some(cols) = max_left_cols {
        cols_str = cols.to_string();
        args.extend_from_slice(&[";", "resize-pane", "-t", left_pane_id, "-x", &cols_str]);
    }
    let out = Command::new("tmux").args(&args).output()?;
    if !out.status.success() {
        bail!(
            "tmux join-pane failed: {}",
            String::from_utf8_lossy(&out.stderr)
        );
    }
    let right_pane_id = rightmost_pane_in_window(left_pane_id)?;
    set_pane_remain_on_exit(&right_pane_id);
    Ok(right_pane_id)
}

/// Set remain-on-exit at the pane level so the pane survives regardless of the
/// window option on whichever window it currently lives in.
fn set_pane_remain_on_exit(pane_id: &str) {
    let _ = Command::new("tmux")
        .args(["set-option", "-p", "-t", pane_id, "remain-on-exit", "on"])
        .status();
}

/// Return the pane ID with the largest left-offset within the same window as `pane_id`.
fn rightmost_pane_in_window(pane_id: &str) -> Result<String> {
    let out = Command::new("tmux")
        .args(["list-panes", "-t", pane_id, "-F", "#{pane_left} #{pane_id}"])
        .output()?;
    if !out.status.success() {
        bail!(
            "tmux list-panes failed: {}",
            String::from_utf8_lossy(&out.stderr)
        );
    }
    String::from_utf8(out.stdout)?
        .lines()
        .filter_map(|line| {
            let mut parts = line.split_whitespace();
            let left: u32 = parts.next()?.parse().ok()?;
            let id = parts.next()?.to_string();
            Some((left, id))
        })
        .max_by_key(|(left, _)| *left)
        .map(|(_, id)| id)
        .ok_or_else(|| anyhow::anyhow!("no panes found in window containing {pane_id}"))
}

/// Kill a dead shown pane, start a fresh window, join it, and resize — all in
/// one tmux invocation to avoid intermediate repaints.
pub fn restart_shown_proc_pane(
    right_pane_id: &str,
    session: &str,
    window_name: &str,
    shell_cmd: &str,
    left_pane_id: &str,
    max_left_cols: Option<u16>,
) -> Result<String> {
    let window_target = format!("{session}:{window_name}");
    let mut args = vec![
        "kill-pane",
        "-t",
        right_pane_id,
        ";",
        "new-window",
        "-d",
        "-t",
        session,
        "-n",
        window_name,
        shell_cmd,
        ";",
        "set-window-option",
        "-t",
        &window_target,
        "remain-on-exit",
        "on",
        ";",
        "join-pane",
        "-h",
        "-d",
        "-s",
        &window_target,
        "-t",
        left_pane_id,
    ];
    let cols_str;
    if let Some(cols) = max_left_cols {
        cols_str = cols.to_string();
        args.extend_from_slice(&[";", "resize-pane", "-t", left_pane_id, "-x", &cols_str]);
    }
    let out = Command::new("tmux").args(&args).output()?;
    if !out.status.success() {
        bail!(
            "tmux restart failed: {}",
            String::from_utf8_lossy(&out.stderr)
        );
    }
    let right_pane_id = rightmost_pane_in_window(left_pane_id)?;
    set_pane_remain_on_exit(&right_pane_id);
    Ok(right_pane_id)
}

/// Swap the currently-shown pane out and bring in a new one in a single tmux
/// server round-trip, so the server can process both commands before repainting.
/// `max_left_cols`: if Some, resize the left pane in the same chain.
/// Returns the new right pane's ID.
pub fn swap_proc_pane(
    right_pane_id: &str,
    session: &str,
    old_window_name: &str,
    new_window: &str,
    left_pane_id: &str,
    max_left_cols: Option<u16>,
) -> Result<String> {
    let mut args = vec![
        "break-pane",
        "-d",
        "-s",
        right_pane_id,
        "-t",
        session,
        "-n",
        old_window_name,
        ";",
        "join-pane",
        "-h",
        "-d",
        "-s",
        new_window,
        "-t",
        left_pane_id,
    ];
    let cols_str;
    if let Some(cols) = max_left_cols {
        cols_str = cols.to_string();
        args.extend_from_slice(&[";", "resize-pane", "-t", left_pane_id, "-x", &cols_str]);
    }
    let out = Command::new("tmux").args(&args).output()?;
    if !out.status.success() {
        bail!("tmux swap failed: {}", String::from_utf8_lossy(&out.stderr));
    }
    let right_pane_id = rightmost_pane_in_window(left_pane_id)?;
    set_pane_remain_on_exit(&right_pane_id);
    Ok(right_pane_id)
}

/// Check whether a pane (by ID, e.g. `%42`) has a live process.
/// Only use this for pane IDs — not window targets, which can fall back
/// to the current pane on some tmux versions.
pub fn is_pane_alive(pane_id: &str) -> bool {
    let out = Command::new("tmux")
        .args(["display-message", "-t", pane_id, "-p", "#{pane_dead}"])
        .output();
    match out {
        Ok(o) if o.status.success() => String::from_utf8_lossy(&o.stdout).trim() == "0",
        _ => false,
    }
}

/// Move tmux focus to the given pane.
pub fn focus_pane(pane_id: &str) -> Result<()> {
    Command::new("tmux")
        .args(["select-pane", "-t", pane_id])
        .status()?;
    Ok(())
}

/// Kill a pane by ID.
pub fn kill_pane(pane_id: &str) -> Result<()> {
    Command::new("tmux")
        .args(["kill-pane", "-t", pane_id])
        .status()?;
    Ok(())
}

/// Kill a window in the background session.
pub fn kill_window(window: &str) -> Result<()> {
    Command::new("tmux")
        .args(["kill-window", "-t", window])
        .status()?;
    Ok(())
}
