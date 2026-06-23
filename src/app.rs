use crate::tmux::{self, TmuxSession};
use anyhow::Result;

#[derive(Debug, Clone, PartialEq)]
pub enum ProcStatus {
    Running,
    Dead,
}

pub struct Proc {
    pub name: String,
    pub cmd: String,
    pub status: ProcStatus,
    /// Whether this proc's pane is currently joined into the right slot.
    pub is_shown: bool,
}

pub struct App {
    pub procs: Vec<Proc>,
    pub selected: usize,
    pub bg_session: TmuxSession,
    pub left_pane_id: String,
    pub right_pane_id: Option<String>,
    pub should_quit: bool,
}

impl App {
    pub fn new(procs: Vec<Proc>, bg_session: TmuxSession, left_pane_id: String) -> Self {
        App {
            procs,
            selected: 0,
            bg_session,
            left_pane_id,
            right_pane_id: None,
            should_quit: false,
        }
    }

    pub fn move_up(&mut self) {
        if self.selected > 0 {
            self.selected -= 1;
        }
    }

    pub fn move_down(&mut self) {
        if self.selected + 1 < self.procs.len() {
            self.selected += 1;
        }
    }

    /// Show the currently selected process in the right pane.
    pub fn show_selected(&mut self) -> Result<()> {
        let name = self.procs[self.selected].name.clone();
        self.show_proc(&name)
    }

    /// Bring the named process into the right pane.
    pub fn show_proc(&mut self, name: &str) -> Result<()> {
        if self.procs.iter().any(|p| p.is_shown && p.name == name) {
            return Ok(());
        }
        let window = self.bg_session.window_id(name);

        let new_right_id = if let Some(shown_idx) = self.procs.iter().position(|p| p.is_shown) {
            self.procs[shown_idx].is_shown = false;
            let right_id = self.right_pane_id.take();
            let alive = right_id
                .as_deref()
                .map(tmux::is_pane_alive)
                .unwrap_or(false);
            if let (Some(right_id), true) = (right_id, alive) {
                let shown_name = self.procs[shown_idx].name.clone();
                tmux::swap_proc_pane(
                    &right_id,
                    &self.bg_session.session_name,
                    &self.bg_session.window_name_for(&shown_name),
                    &window,
                    &self.left_pane_id,
                    Some(50),
                )?
            } else {
                tmux::join_pane_right(&window, &self.left_pane_id, Some(50))?
            }
        } else {
            tmux::join_pane_right(&window, &self.left_pane_id, Some(50))?
        };

        self.right_pane_id = Some(new_right_id);
        if let Some(p) = self.procs.iter_mut().find(|p| p.name == name) {
            p.is_shown = true;
        }
        Ok(())
    }

    /// Restart the selected process if it is dead.
    pub fn restart_selected(&mut self) -> Result<()> {
        let idx = self.selected;
        if self.procs[idx].status != ProcStatus::Dead {
            return Ok(());
        }
        let name = self.procs[idx].name.clone();
        let cmd = self.procs[idx].cmd.clone();

        if self.procs[idx].is_shown {
            // Chain: kill dead pane → new-window → join → resize, all in one invocation.
            if let Some(right_id) = self.right_pane_id.take() {
                let window_name = self.bg_session.window_name_for(&name);
                let wrapper_cmd = self.bg_session.wrapper_cmd_for(&name, &cmd)?;
                match tmux::restart_shown_proc_pane(
                    &right_id,
                    &self.bg_session.session_name,
                    &window_name,
                    &wrapper_cmd,
                    &self.left_pane_id,
                    Some(50),
                ) {
                    Ok(new_right_id) => {
                        self.right_pane_id = Some(new_right_id);
                    }
                    Err(e) => {
                        self.procs[idx].is_shown = false;
                        return Err(e);
                    }
                }
            }
        } else {
            let window = self.bg_session.window_id(&name);
            let _ = tmux::kill_window(&window);
            self.bg_session.start_proc(&name, &cmd)?;
        }

        self.procs[idx].status = ProcStatus::Running;
        Ok(())
    }

    /// Kill the selected process's child, leaving the wrapper pane open.
    /// Status is not updated here; the next `refresh_status` tick will
    /// observe the status file change and update it accurately.
    pub fn kill_selected(&mut self) -> Result<()> {
        let name = self.procs[self.selected].name.clone();
        let status_file = self.bg_session.status_file_for(&name);
        tmux::kill_child_in_wrapper(&status_file);
        Ok(())
    }

    pub fn refresh_status(&mut self) {
        for i in 0..self.procs.len() {
            let status_file = self.bg_session.status_file_for(&self.procs[i].name);
            let content = std::fs::read_to_string(&status_file).unwrap_or_default();
            let alive = content.trim().starts_with("running");
            self.procs[i].status = if alive {
                ProcStatus::Running
            } else {
                ProcStatus::Dead
            };
        }
    }
}
