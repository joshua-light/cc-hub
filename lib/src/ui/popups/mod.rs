//! Overlay views drawn over the tab body.
//!
//! - `folder_picker`: new-session / assign folder picker and gh-create input
//! - `fuzzy_pickers`: type-to-filter model, task-link and session-finder pickers
//! - `list_pickers`: j/k agent, respawn and task-kind pickers
//! - `task_input`: add/rename-task popup with its context box
//! - `line_inputs`: attach, tags and rename-session editors
//! - `confirm_close`: close-terminal confirmation
//! - `tmux_pane`: embedded tmux pane
//! - `live_tail`: live transcript tail
//! - `transcript`: preview tokenizer and bullet renderers for the live tail
//! - `widgets`: building blocks shared by the popups above

mod confirm_close;
mod folder_picker;
mod fuzzy_pickers;
mod line_inputs;
mod list_pickers;
mod live_tail;
mod task_input;
mod tmux_pane;
mod transcript;
mod widgets;

pub(crate) use confirm_close::render_confirm_close;
pub(crate) use folder_picker::{render_folder_picker, render_gh_create_input};
pub(crate) use fuzzy_pickers::{
    render_model_picker, render_session_finder, render_task_link_picker,
};
pub(crate) use line_inputs::{render_rename_session, render_task_attach_input, render_task_tags};
pub(crate) use list_pickers::{
    render_agent_picker, render_respawn_picker, render_task_kind_picker,
};
pub(crate) use live_tail::render_live_tail;
pub(crate) use task_input::render_task_input;
pub(crate) use tmux_pane::render_tmux_pane;
