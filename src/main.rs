//! wezterm-tmux-shim: native Windows tmux.exe shim for Claude Code agent-teams.
//!
//! Translates the tmux subcommands that CC TmuxBackend emits into wezterm cli
//! calls. Verified against claude 2.1.196. Unknown subcommands are logged and
//! silently succeed so CC does not crash on version drift.

use fs2::FileExt;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::fs::{self, OpenOptions};
use std::io::Write;
use std::path::PathBuf;
use std::process::{Command, Stdio};
use std::time::{SystemTime, UNIX_EPOCH};

// ----- version sentinel -----

/// The CC version this shim was verified against. Printed by --version/-V and
/// embedded in the log for drift tracking.
const VERIFIED_AGAINST: &str = "claude 2.1.196";

// ----- state file -----

#[derive(Debug, Default, Serialize, Deserialize)]
struct State {
    /// tmux pane id (e.g. "%3") -> WezTerm integer pane id
    tmux_to_wez: HashMap<String, u64>,
    /// WezTerm integer pane id -> tmux pane id
    wez_to_tmux: HashMap<u64, String>,
    /// next tmux pane counter
    next_pane: u64,
    /// stored environment variables (set-environment / show-environment)
    env_vars: HashMap<String, String>,
}

impl State {
    fn alloc_pane(&mut self, wez_id: u64) -> String {
        // If we already mapped this wezterm id, return the existing tmux id.
        if let Some(tid) = self.wez_to_tmux.get(&wez_id) {
            return tid.clone();
        }
        let tid = format!("%{}", self.next_pane);
        self.next_pane += 1;
        self.tmux_to_wez.insert(tid.clone(), wez_id);
        self.wez_to_tmux.insert(wez_id, tid.clone());
        tid
    }

    fn tmux_id_for_wez(&mut self, wez_id: u64) -> String {
        self.alloc_pane(wez_id)
    }

    fn wez_id_for_tmux(&self, tmux_id: &str) -> Option<u64> {
        self.tmux_to_wez.get(tmux_id).copied()
    }
}

// ----- paths -----

fn state_dir() -> PathBuf {
    let local_app = std::env::var("LOCALAPPDATA")
        .unwrap_or_else(|_| r"C:\Users\Default\AppData\Local".to_string());
    PathBuf::from(local_app).join("wezterm-tmux-shim")
}

fn state_path() -> PathBuf {
    state_dir().join("state.json")
}

fn log_path() -> PathBuf {
    state_dir().join("shim.log")
}

/// Load state while holding an exclusive OS file lock. Returns the lock file
/// handle so the caller keeps the lock across the load-mutate-save cycle.
fn load_state_locked() -> (State, fs::File) {
    let dir = state_dir();
    let _ = fs::create_dir_all(&dir);
    let lock_file = OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .open(dir.join("state.lock"))
        .expect("failed to open state lock file");
    lock_file
        .lock_exclusive()
        .expect("failed to acquire exclusive lock on state.lock");

    let p = state_path();
    let state = if p.exists() {
        match fs::read_to_string(&p) {
            Ok(raw) => serde_json::from_str(&raw).unwrap_or_default(),
            Err(_) => State::default(),
        }
    } else {
        State::default()
    };
    (state, lock_file)
}

/// Write state atomically: serialize to a temp file, then rename onto state.json.
/// The caller must hold the lock file returned by load_state_locked.
fn save_state_locked(state: &State) {
    let dir = state_dir();
    let _ = fs::create_dir_all(&dir);
    let json = serde_json::to_string_pretty(state).unwrap_or_default();
    let tmp = dir.join("state.json.tmp");
    if fs::write(&tmp, json.as_bytes()).is_ok() {
        let _ = fs::rename(&tmp, state_path());
    }
}

// ----- logging -----

fn log_line(msg: &str) {
    let dir = state_dir();
    let _ = fs::create_dir_all(&dir);
    let ts = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    let line = format!("[{}] {}
", ts, msg);
    if let Ok(mut f) = OpenOptions::new().create(true).append(true).open(log_path()) {
        let _ = f.write_all(line.as_bytes());
    }
}
// ----- wezterm binary resolution -----

fn wezterm_bin() -> String {
    // Try PATH first; fall back to the default WezTerm install location.
    let candidate = "wezterm";
    let ok = Command::new(candidate)
        .args(["--version"])
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .is_ok();
    if ok {
        return candidate.to_string();
    }
    let prog_files = std::env::var("ProgramFiles")
        .unwrap_or_else(|_| r"C:\Program Files".to_string());
    format!(r"{}\WezTerm\wezterm.exe", prog_files)
}

fn run_wezterm(args: &[&str]) -> (String, String, i32) {
    let bin = wezterm_bin();
    log_line(&format!("  wezterm cmd: {} {}", bin, args.join(" ")));
    let out = Command::new(&bin).args(args).output();
    match out {
        Ok(o) => {
            let stdout = String::from_utf8_lossy(&o.stdout).to_string();
            let stderr = String::from_utf8_lossy(&o.stderr).to_string();
            let code = o.status.code().unwrap_or(-1);
            log_line(&format!(
                "  wezterm exit={} stdout={:?} stderr={:?}",
                code,
                stdout.trim(),
                stderr.trim()
            ));
            (stdout, stderr, code)
        }
        Err(e) => {
            log_line(&format!("  wezterm exec error: {}", e));
            (String::new(), e.to_string(), -1)
        }
    }
}

// ----- WezTerm JSON pane list -----

#[derive(Debug, Deserialize)]
struct WezPane {
    pane_id: u64,
    window_id: u64,
    #[allow(dead_code)]
    tab_id: u64,
}

fn list_wez_panes() -> Vec<WezPane> {
    let (stdout, _, _) = run_wezterm(&["cli", "list", "--format", "json"]);
    serde_json::from_str(&stdout).unwrap_or_default()
}

// ----- format token substitution -----

/// Substitute #{token} occurrences in a format string.
/// pane_id is the tmux pane id for the context pane (e.g. "%3").
/// window_id is e.g. "@0".
fn apply_format(fmt: &str, pane_id: Option<&str>, window_id: Option<&str>) -> String {
    let mut out = fmt.to_string();
    if let Some(pid) = pane_id {
        out = out.replace("#{pane_id}", pid);
    }
    if let Some(wid) = window_id {
        out = out.replace("#{window_id}", wid);
    }
    out = out.replace("#{client_termtype}", "tmux-256color");
    out = out.replace("#{client_control_mode}", "0");
    out
}

/// Resolve the "current" pane tmux id from environment. Prefers TMUX_PANE,
/// then maps WEZTERM_PANE (integer) through state. Allocates a new id if unseen.
fn resolve_current_pane(state: &mut State) -> String {
    if let Ok(tp) = std::env::var("TMUX_PANE") {
        if !tp.is_empty() {
            return tp;
        }
    }
    if let Ok(wp) = std::env::var("WEZTERM_PANE") {
        if let Ok(wid) = wp.parse::<u64>() {
            return state.tmux_id_for_wez(wid);
        }
    }
    // Fallback: allocate %0 mapped to wezterm pane 0.
    state.tmux_id_for_wez(0)
}

// ----- target resolution -----

/// Resolve a tmux target string to a WezTerm pane id.
/// Handles: tmux pane id ("%N"), bare integer (wezterm id), or session/window
/// name (best-effort: return first live pane).
fn resolve_target(target: &str, state: &State) -> Option<u64> {
    if target.starts_with('%') {
        return state.wez_id_for_tmux(target);
    }
    if let Ok(n) = target.parse::<u64>() {
        return Some(n);
    }
    // Session or window name - pick first live pane as best-effort.
    let panes = list_wez_panes();
    panes.first().map(|p| p.pane_id)
}
// ----- subcommand handlers -----

fn cmd_has_session(_args: &[String], _state: &mut State) -> i32 {
    // Always report "session exists" (exit 0).
    0
}

fn cmd_new_session(args: &[String], state: &mut State) -> i32 {
    // new-session -d -s <name> [-x W] [-y H] [-c cwd] [-n win] [-P -F <fmt>] [-- <cmd>]
    let mut print_pane = false;
    let mut fmt = String::new();
    let mut i = 0;
    while i < args.len() {
        match args[i].as_str() {
            "-P" => print_pane = true,
            "-F" => {
                i += 1;
                if i < args.len() {
                    fmt = args[i].clone();
                }
            }
            "--" => break,
            _ => {}
        }
        i += 1;
    }
    if print_pane {
        let panes = list_wez_panes();
        let wez_id = panes.first().map(|p| p.pane_id).unwrap_or(0);
        let tmux_id = state.alloc_pane(wez_id);
        let window_id = panes
            .first()
            .map(|p| format!("@{}", p.window_id))
            .unwrap_or_else(|| "@0".into());
        let resolved = apply_format(&fmt, Some(&tmux_id), Some(&window_id));
        println!("{}", resolved);
        save_state_locked(state);
    }
    0
}

fn cmd_new_window(args: &[String], state: &mut State) -> i32 {
    // new-window -t <s> -n <name> -P -F <fmt> -- <cmd>
    let mut print_pane = false;
    let mut fmt = String::new();
    let mut i = 0;
    while i < args.len() {
        match args[i].as_str() {
            "-P" => print_pane = true,
            "-F" => {
                i += 1;
                if i < args.len() {
                    fmt = args[i].clone();
                }
            }
            "--" => break,
            _ => {}
        }
        i += 1;
    }
    if print_pane {
        let panes = list_wez_panes();
        let wez_id = panes.first().map(|p| p.pane_id).unwrap_or(0);
        let tmux_id = state.alloc_pane(wez_id);
        let window_id = panes
            .first()
            .map(|p| format!("@{}", p.window_id))
            .unwrap_or_else(|| "@0".into());
        let resolved = apply_format(&fmt, Some(&tmux_id), Some(&window_id));
        println!("{}", resolved);
        save_state_locked(state);
    }
    0
}

fn cmd_split_window(args: &[String], state: &mut State) -> i32 {
    // split-window -d -t <target> (-v|-h) [-l 70%] -P -F <fmt> -- <cmd>
    let mut target = String::new();
    let mut horizontal = false;
    let mut print_pane = false;
    let mut fmt = String::new();
    let mut i = 0;
    while i < args.len() {
        match args[i].as_str() {
            "-t" => {
                i += 1;
                if i < args.len() {
                    target = args[i].clone();
                }
            }
            "-h" => horizontal = true,
            "-v" => horizontal = false,
            "-P" => print_pane = true,
            "-F" => {
                i += 1;
                if i < args.len() {
                    fmt = args[i].clone();
                }
            }
            "--" => break,
            _ => {}
        }
        i += 1;
    }
    let wez_target = resolve_target(&target, state).unwrap_or(0);
    let wez_target_str = wez_target.to_string();
    // -h means left/right split -> --horizontal; -v (default) -> --bottom.
    let mut wez_args = vec!["cli", "split-pane", "--pane-id", wez_target_str.as_str()];
    if horizontal {
        wez_args.push("--horizontal");
    } else {
        wez_args.push("--bottom");
    }
    let (stdout, _, exit) = run_wezterm(&wez_args);
    if exit != 0 {
        log_line(&format!(
            "  split-pane failed (exit={}); falling back to synthetic pane id",
            exit
        ));
    }
    // wezterm cli split-pane prints the new integer pane id on stdout.
    let new_wez_id: Option<u64> = stdout.trim().parse().ok();
    let tmux_id = match new_wez_id {
        Some(wid) => state.alloc_pane(wid),
        None => {
            log_line("  could not parse split-pane stdout; allocating synthetic id");
            let tid = format!("%{}", state.next_pane);
            state.next_pane += 1;
            tid
        }
    };
    if print_pane {
        let panes = list_wez_panes();
        let window_id = panes
            .first()
            .map(|p| format!("@{}", p.window_id))
            .unwrap_or_else(|| "@0".into());
        let resolved = apply_format(&fmt, Some(&tmux_id), Some(&window_id));
        println!("{}", resolved);
    }
    save_state_locked(state);
    0
}

fn cmd_list_panes(args: &[String], state: &mut State) -> i32 {
    // list-panes [-t <target>] -F <fmt>
    let mut fmt = String::new();
    let mut i = 0;
    while i < args.len() {
        match args[i].as_str() {
            "-F" => {
                i += 1;
                if i < args.len() {
                    fmt = args[i].clone();
                }
            }
            _ => {}
        }
        i += 1;
    }
    let panes = list_wez_panes();
    for wp in &panes {
        let tmux_id = state.tmux_id_for_wez(wp.pane_id);
        let window_id = format!("@{}", wp.window_id);
        let line = apply_format(&fmt, Some(&tmux_id), Some(&window_id));
        println!("{}", line);
    }
    save_state_locked(state);
    0
}
fn cmd_display_message(args: &[String], state: &mut State) -> i32 {
    // display-message -p <fmt>
    // CC uses: display-message -p #{token}
    // Without -p it is a no-op for us.
    let mut print_mode = false;
    let mut fmt = String::new();
    let mut i = 0;
    while i < args.len() {
        match args[i].as_str() {
            "-p" => {
                print_mode = true;
                // Format string follows as next arg if it is not a flag.
                i += 1;
                if i < args.len() && !args[i].starts_with('-') {
                    fmt = args[i].clone();
                } else {
                    // Next arg is a flag; step back so the loop processes it.
                    i -= 1;
                }
            }
            s if print_mode && fmt.is_empty() && !s.starts_with('-') => {
                fmt = s.to_string();
            }
            _ => {}
        }
        i += 1;
    }
    if print_mode && !fmt.is_empty() {
        let cur_pane = resolve_current_pane(state);
        let panes = list_wez_panes();
        let window_id = panes
            .first()
            .map(|p| format!("@{}", p.window_id))
            .unwrap_or_else(|| "@0".into());
        let resolved = apply_format(&fmt, Some(&cur_pane), Some(&window_id));
        println!("{}", resolved);
        save_state_locked(state);
    }
    0
}

fn cmd_select_pane(args: &[String], _state: &mut State) -> i32 {
    // select-pane -t <t> [-T title]
    // Best-effort: attempt wezterm cli set-tab-title if -T is given.
    let mut title = String::new();
    let mut target = String::new();
    let mut i = 0;
    while i < args.len() {
        match args[i].as_str() {
            "-t" => {
                i += 1;
                if i < args.len() {
                    target = args[i].clone();
                }
            }
            "-T" => {
                i += 1;
                if i < args.len() {
                    title = args[i].clone();
                }
            }
            _ => {}
        }
        i += 1;
    }
    if !title.is_empty() {
        log_line(&format!(
            "  select-pane: set-tab-title target={} title={}",
            target, title
        ));
        run_wezterm(&["cli", "set-tab-title", &title]);
    }
    0
}

fn cmd_set_option(_args: &[String], _state: &mut State) -> i32 {
    // Best-effort no-op.
    0
}

fn cmd_resize_pane(_args: &[String], _state: &mut State) -> i32 {
    // Best-effort no-op.
    0
}
fn cmd_respawn_pane(args: &[String], state: &mut State) -> i32 {
    // respawn-pane -k -t <target> -- <cmd>
    //
    // Approach: WezTerm has no direct respawn-pane API. We implement this by:
    //   1. Resolving <target> to a WezTerm pane id.
    //   2. Writing a generated .cmd launcher script to the state dir that:
    //        a. Exports any stored CLAUDE_CODE_* env vars via SET commands.
    //        b. Invokes <cmd> (the tail of args after --).
    //   3. Sending the launcher path + CR to the pane via:
    //        wezterm cli send-text --pane-id <id> --no-paste "<launcher>"
    //
    // Limitation: the target pane must have an idle shell accepting input.
    // WezTerm has no API to kill and restart a pane process. For CC agent-teams,
    // panes are typically idle shells between invocations, so this works in
    // practice for the spike. If the pane is occupied, send-text deposits
    // keystrokes into the running process instead.
    let mut target = String::new();
    let mut cmd_parts: Vec<String> = Vec::new();
    let mut after_dashdash = false;
    let mut i = 0;
    while i < args.len() {
        if after_dashdash {
            cmd_parts.push(args[i].clone());
        } else {
            match args[i].as_str() {
                "-t" => {
                    i += 1;
                    if i < args.len() {
                        target = args[i].clone();
                    }
                }
                "--" => after_dashdash = true,
                _ => {}
            }
        }
        i += 1;
    }
    let wez_target = match resolve_target(&target, state) {
        Some(id) => id,
        None => {
            log_line(&format!("  respawn-pane: could not resolve target={}", target));
            return 0;
        }
    };
    if cmd_parts.is_empty() {
        log_line("  respawn-pane: no cmd after --; nothing to do");
        return 0;
    }
    // Build .cmd launcher script with stored env vars then the command.
    let dir = state_dir();
    let _ = fs::create_dir_all(&dir);
    let script_path = dir.join(format!("respawn_{}.cmd", wez_target));
    let mut script_lines: Vec<String> = vec!["@echo off".to_string()];
    for (k, v) in &state.env_vars {
        // Escape % in values by doubling them (CMD batch syntax).
        // Also strip CR/LF to prevent batch command injection.
        let escaped_v = v.replace('%', "%%").replace('\r', "").replace('\n', "");
        script_lines.push(format!("SET {}={}", k, escaped_v));
    }
    if cmd_parts.len() == 1 {
        script_lines.push(cmd_parts[0].clone());
    } else {
        let exe = &cmd_parts[0];
        let rest = cmd_parts[1..].join(" ");
        script_lines.push(format!("\"{}\" {}", exe, rest));
    }
    let script_content = script_lines.join("\r\n");
    if let Err(e) = fs::write(&script_path, script_content.as_bytes()) {
        log_line(&format!("  respawn-pane: failed to write launcher: {}", e));
        return 0;
    }
    let script_str = script_path.to_string_lossy().to_string();
    let send_text_arg = format!("{}\r", script_str);
    let pane_id_str = wez_target.to_string();
    run_wezterm(&[
        "cli",
        "send-text",
        "--pane-id",
        pane_id_str.as_str(),
        "--no-paste",
        send_text_arg.as_str(),
    ]);
    0
}
fn cmd_set_environment(args: &[String], state: &mut State) -> i32 {
    // set-environment -g <NAME> <VALUE>
    // Also: set -as <...> accepted as no-op-ok.
    let filtered: Vec<&String> = args.iter().filter(|a| !a.starts_with('-')).collect();
    if filtered.len() >= 2 {
        let name = filtered[0].to_string();
        let value = filtered[1].to_string();
        log_line(&format!("  set-environment: {}={}", name, value));
        state.env_vars.insert(name, value);
        save_state_locked(state);
    }
    0
}

fn cmd_show_environment(args: &[String], state: &mut State) -> i32 {
    // show-environment -g <NAME>
    let filtered: Vec<&String> = args.iter().filter(|a| !a.starts_with('-')).collect();
    if let Some(name) = filtered.first() {
        match state.env_vars.get(*name) {
            Some(val) => println!("{}={}", name, val),
            None => return 1, // not found - match real tmux exit code
        }
    }
    0
}

fn cmd_break_pane(_args: &[String], _state: &mut State) -> i32 {
    // Best-effort no-op.
    0
}

// ----- dispatch -----

fn dispatch(subcommand: &str, args: &[String], state: &mut State) -> i32 {
    match subcommand {
        "has-session" => cmd_has_session(args, state),
        "new-session" => cmd_new_session(args, state),
        "new-window" => cmd_new_window(args, state),
        "split-window" => cmd_split_window(args, state),
        "list-panes" => cmd_list_panes(args, state),
        "display-message" => cmd_display_message(args, state),
        "select-pane" => cmd_select_pane(args, state),
        "set-option" => cmd_set_option(args, state),
        "resize-pane" => cmd_resize_pane(args, state),
        "respawn-pane" => cmd_respawn_pane(args, state),
        "set-environment" | "set" => cmd_set_environment(args, state),
        "show-environment" => cmd_show_environment(args, state),
        "break-pane" => cmd_break_pane(args, state),
        other => {
            // Fail-soft: log and exit 0 so CC does not crash on version drift.
            log_line(&format!("UNHANDLED: subcommand={:?} args={:?}", other, args));
            0
        }
    }
}

fn main() {
    let argv: Vec<String> = std::env::args().collect();
    log_line(&format!("INVOKE: {:?}", argv));

    // Handle --version / -V before subcommand dispatch.
    if argv.len() >= 2 && (argv[1] == "--version" || argv[1] == "-V") {
        println!("tmux-wezterm-shim ({})", VERIFIED_AGAINST);
        log_line("  -> version printed");
        std::process::exit(0);
    }

    if argv.len() < 2 {
        log_line("UNHANDLED: no subcommand (argc < 2)");
        std::process::exit(0);
    }

    let subcommand = argv[1].clone();
    let args = argv[2..].to_vec();

    let (mut state, _lock) = load_state_locked();
    let exit_code = dispatch(&subcommand, &args, &mut state);

    log_line(&format!("  -> exit {}", exit_code));
    std::process::exit(exit_code);
}
