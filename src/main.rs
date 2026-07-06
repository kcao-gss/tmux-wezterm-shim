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
    /// WezTerm integer pane id -> tmux "remain-on-exit" value ("off"/"on"/
    /// "failed"), set via `set-option -p -t <pane> remain-on-exit <value>`.
    /// Absent entries default to "off" (tmux's own default), matching the
    /// close-always behavior respawn-pane used before this field existed.
    #[serde(default)]
    remain_on_exit: HashMap<u64, String>,
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

    /// Force tmux_id to map to wez_id, overriding any stale mapping on either
    /// side. Used to anchor WEZTERM_PANE as the authoritative "current pane":
    /// if tmux_id was previously bound to a different wezterm pane, or wez_id
    /// was previously known under a different tmux id, both stale entries are
    /// removed before the new pair is inserted.
    fn bind_pane(&mut self, tmux_id: &str, wez_id: u64) {
        if let Some(old_wez) = self.tmux_to_wez.get(tmux_id).copied() {
            if old_wez != wez_id {
                self.wez_to_tmux.remove(&old_wez);
            }
        }
        if let Some(old_tid) = self.wez_to_tmux.get(&wez_id).cloned() {
            if old_tid != tmux_id {
                self.tmux_to_wez.remove(&old_tid);
            }
        }
        self.tmux_to_wez.insert(tmux_id.to_string(), wez_id);
        self.wez_to_tmux.insert(wez_id, tmux_id.to_string());
    }

    /// Remove all bookkeeping for a WezTerm pane: its tmux id mapping (both
    /// directions) and any remain-on-exit policy. Used when a pane is
    /// actually destroyed (kill-pane/kill-window) so stale ids do not
    /// accumulate in state.json.
    fn forget_pane(&mut self, wez_id: u64) {
        if let Some(tid) = self.wez_to_tmux.remove(&wez_id) {
            self.tmux_to_wez.remove(&tid);
        }
        self.remain_on_exit.remove(&wez_id);
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
/// Fail-soft: if the lock file cannot be opened or locked (e.g. a read-only
/// %LOCALAPPDATA%), logs the failure and returns loaded-or-default state with
/// no lock rather than panicking. The caller then proceeds without state
/// persistence guarantees for that invocation.
fn load_state_locked() -> (State, Option<fs::File>) {
    let dir = state_dir();
    let _ = fs::create_dir_all(&dir);
    let lock_file = match OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .open(dir.join("state.lock"))
    {
        Ok(f) => match f.lock_exclusive() {
            Ok(()) => Some(f),
            Err(e) => {
                log_line(&format!("  failed to acquire exclusive lock on state.lock: {}", e));
                None
            }
        },
        Err(e) => {
            log_line(&format!("  failed to open state lock file: {}", e));
            None
        }
    };

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

// ----- bash binary resolution (for respawn-pane launcher execution) -----

/// Locate a Git-for-Windows bash.exe to execute the POSIX command strings CC
/// builds for respawn-pane. CC assumes a Unix /bin/sh is available; on Windows
/// the closest equivalent is Git bash. Preference order:
///   1. $SHELL, if it names an existing file (CC or the user may set this).
///   2. The default Git-for-Windows install location (64-bit layout).
///   3. The default Git-for-Windows install location (usr/bin layout).
///   4. Bare "bash" resolved via PATH, as a last resort.
/// Returns None if nothing is found; callers must fail soft.
fn bash_bin() -> Option<String> {
    if let Ok(shell) = std::env::var("SHELL") {
        if !shell.is_empty() && PathBuf::from(&shell).is_file() {
            return Some(shell);
        }
    }
    let candidates = [
        r"C:\Program Files\Git\bin\bash.exe",
        r"C:\Program Files\Git\usr\bin\bash.exe",
    ];
    for c in candidates {
        if PathBuf::from(c).is_file() {
            return Some(c.to_string());
        }
    }
    let ok = Command::new("bash")
        .arg("--version")
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .is_ok();
    if ok {
        return Some("bash".to_string());
    }
    None
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

/// Resolve the "current" pane tmux id from environment. WEZTERM_PANE is
/// authoritative when present; TMUX_PANE is rebound to it if the two disagree.
/// Allocates a new id if unseen.
fn resolve_current_pane(state: &mut State) -> String {
    // WEZTERM_PANE is authoritative: it is the actual pane wezterm launched
    // this process in, so it always names the real "current" pane. CC also
    // reconstructs TMUX_PANE from a previous display-message reply, which can
    // go stale (e.g. still pointing at an old session's pane after a restart).
    // When both are present, force the tmux id CC already knows as TMUX_PANE
    // to point at the real WEZTERM_PANE rather than trusting a stale binding.
    if let Ok(wp) = std::env::var("WEZTERM_PANE") {
        if let Ok(wid) = wp.parse::<u64>() {
            if let Ok(tp) = std::env::var("TMUX_PANE") {
                if !tp.is_empty() {
                    state.bind_pane(&tp, wid);
                    return tp;
                }
            }
            return state.tmux_id_for_wez(wid);
        }
    }
    if let Ok(tp) = std::env::var("TMUX_PANE") {
        if !tp.is_empty() {
            return tp;
        }
    }
    // Fallback: allocate %0 mapped to wezterm pane 0.
    state.tmux_id_for_wez(0)
}

// ----- target resolution -----

/// Resolve a tmux target string to a WezTerm pane id.
/// Handles: tmux pane id ("%N"), bare integer (wezterm id), or session/window
/// name (best-effort: prefer a pane in the current pane's own window).
fn resolve_target(target: &str, state: &mut State) -> Option<u64> {
    if target.starts_with('%') {
        return state.wez_id_for_tmux(target);
    }
    if let Ok(n) = target.parse::<u64>() {
        return Some(n);
    }
    // Session or window name - best-effort: prefer a pane in the dispatching
    // session's own window over the first pane found across all windows.
    // Without this, name/window-name targets always resolved into whichever
    // window happened to list first (window 0), so a teammate spawned from a
    // claude session in window 1 could split off a pane in window 0 instead.
    let panes = list_wez_panes();
    let cur_tmux = resolve_current_pane(state);
    if let Some(cur_wez) = state.wez_id_for_tmux(&cur_tmux) {
        if let Some(cur_window) = panes.iter().find(|p| p.pane_id == cur_wez).map(|p| p.window_id) {
            if let Some(p) = panes.iter().find(|p| p.window_id == cur_window) {
                return Some(p.pane_id);
            }
        }
    }
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
    //
    // -t can name a window ("@N", tmux window id - our apply_format already
    // sets this equal to the wezterm window_id) or a pane ("%N"). When it
    // names a window, only panes in that window must be listed - CC relies on
    // this to enumerate panes to split/target within its own session, and an
    // unfiltered list previously let a later split-window fall back to a pane
    // in an unrelated window.
    let mut fmt = String::new();
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
    let window_filter: Option<u64> = target.strip_prefix('@').and_then(|n| n.parse::<u64>().ok());
    let cur_tmux = resolve_current_pane(state);
    let cur_wez = state.wez_id_for_tmux(&cur_tmux);
    for wp in &panes {
        if let Some(wid) = window_filter {
            if wp.window_id != wid {
                continue;
            }
        }
        let tmux_id = state.tmux_id_for_wez(wp.pane_id);
        let window_id = format!("@{}", wp.window_id);
        let mut line = apply_format(&fmt, Some(&tmux_id), Some(&window_id));
        // Mark the pane bound to WEZTERM_PANE as active/current so CC treats
        // it (not an arbitrary first pane) as its own when scanning this list.
        let is_current = cur_wez == Some(wp.pane_id);
        line = line.replace("#{pane_active}", if is_current { "1" } else { "0" });
        line = line.replace("#{pane_current}", if is_current { "1" } else { "0" });
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
        // Look up window_id for the *resolved current pane*, not the first
        // pane in the whole list - the caller could be in any window, and
        // reporting window 0 unconditionally was the root cause of teammates
        // landing in the wrong window (see resolve_target/cmd_list_panes).
        let panes = list_wez_panes();
        let cur_wez = state.wez_id_for_tmux(&cur_pane);
        let window_id = cur_wez
            .and_then(|wid| panes.iter().find(|p| p.pane_id == wid))
            .or_else(|| panes.first())
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

fn cmd_set_option(args: &[String], state: &mut State) -> i32 {
    // set-option -p -t <target> <name> [value]
    //
    // Only "remain-on-exit" is tracked (see cmd_respawn_pane's auto-close
    // teardown); every other option name stays a best-effort no-op, matching
    // the pre-existing behavior for pane-border-style, window-style, etc.
    let mut target = String::new();
    let mut positional: Vec<String> = Vec::new();
    let mut i = 0;
    while i < args.len() {
        match args[i].as_str() {
            "-t" => {
                i += 1;
                if i < args.len() {
                    target = args[i].clone();
                }
            }
            "-p" | "-g" | "-a" | "-u" | "-o" | "-q" | "-s" | "-w" => {}
            s if s.starts_with('-') => {}
            _ => positional.push(args[i].clone()),
        }
        i += 1;
    }
    if positional.first().map(|s| s.as_str()) == Some("remain-on-exit") {
        let value = positional.get(1).cloned().unwrap_or_else(|| "on".to_string());
        if let Some(wez_target) = resolve_target(&target, state) {
            log_line(&format!(
                "  set-option: remain-on-exit target={} wez_target={} value={}",
                target, wez_target, value
            ));
            state.remain_on_exit.insert(wez_target, value);
            save_state_locked(state);
        } else {
            log_line(&format!("  set-option: could not resolve target={}", target));
        }
    }
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
    //   2. Writing a generated bash launcher (.sh) that exports stored env vars
    //      and then runs <cmd> (the tail of args after --) as-is. CC's native
    //      agent-teams path builds <cmd> as a POSIX/bash command string (e.g.
    //      `cd '...' && env VAR=val '...\claude.exe' ...`), so it must be
    //      interpreted by a real POSIX shell rather than cmd.exe.
    //   3. Appending an auto-close teardown after <cmd> that honors the
    //      "remain-on-exit" option CC sets via set-option before each
    //      respawn-pane (see cmd_set_option): the pane is destroyed with
    //      `wezterm cli kill-pane` on exit code 0 when the policy is "off"
    //      (unconditional close, tmux's own default and our default when the
    //      pane was never given a policy) or "failed" (close only on a clean
    //      exit; a failing teammate leaves its pane open by design so the
    //      user can read the error), and never destroyed when "on".
    //   4. Writing a thin .cmd wrapper that just invokes that .sh under a
    //      discovered Git-for-Windows bash.exe.
    //   5. Sending the .cmd path + CR to the pane via:
    //        wezterm cli send-text --pane-id <id> --no-paste "<launcher>"
    //      A .cmd is always run by cmd.exe regardless of the shell the target
    //      pane happens to be sitting in, so this delivery step stays
    //      shell-agnostic even though the payload it launches is now bash.
    //
    // Limitation: the target pane must have an idle shell accepting input.
    // WezTerm has no API to kill and restart a pane process. For CC agent-teams,
    // panes are typically idle shells between invocations, so this works in
    // practice. If the pane is occupied, send-text deposits keystrokes into the
    // running process instead.
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
    // The command is typically a single POSIX/bash command string; if CC ever
    // passes it as multiple argv entries, join with spaces and let bash parse
    // the result, matching the previous joining behavior.
    let posix_cmd = cmd_parts.join(" ");

    let dir = state_dir();
    let _ = fs::create_dir_all(&dir);

    // Write the bash launcher (.sh). LF line endings only - CRLF in a shebang
    // script confuses bash's #! handling and can break here-doc-free scripts.
    let sh_path = dir.join(format!("respawn_{}.sh", wez_target));
    let mut sh_lines: Vec<String> = vec!["#!/usr/bin/env bash".to_string()];
    for (k, v) in &state.env_vars {
        // Strip CR/LF to prevent script injection via a stored env var, then
        // single-quote the value using the standard bash escaping idiom:
        // close the quote, emit an escaped literal quote, reopen the quote.
        let clean_v = v.replace('\r', "").replace('\n', "");
        let escaped_v = clean_v.replace('\'', r"'\''");
        sh_lines.push(format!("export {}='{}'", k, escaped_v));
    }
    sh_lines.push(posix_cmd);

    // Auto-close teardown: honor the "remain-on-exit" policy CC set for this
    // pane via set-option (default "off" - close always - when unset, which
    // matches tmux's own default).
    let policy = state
        .remain_on_exit
        .get(&wez_target)
        .map(|s| s.as_str())
        .unwrap_or("off");
    sh_lines.push("rc=$?".to_string());
    let kill_cmd = format!("wezterm cli kill-pane --pane-id {}", wez_target);
    match policy {
        "on" => {
            // Never auto-close; emit nothing.
        }
        "failed" => {
            sh_lines.push(format!("if [ \"$rc\" -eq 0 ]; then {}; fi", kill_cmd));
        }
        _ => {
            // "off" or any unrecognized value - close unconditionally.
            sh_lines.push(kill_cmd);
        }
    }

    let sh_content = sh_lines.join("\n") + "\n";
    if let Err(e) = fs::write(&sh_path, sh_content.as_bytes()) {
        log_line(&format!("  respawn-pane: failed to write bash launcher: {}", e));
        return 0;
    }

    // Write the .cmd wrapper that hands the .sh off to bash.
    let bash = match bash_bin() {
        Some(b) => {
            log_line(&format!("  respawn-pane: using bash={}", b));
            b
        }
        None => {
            log_line("  respawn-pane: no bash.exe found (Git for Windows required); launcher will fail");
            "bash".to_string()
        }
    };
    let sh_str = sh_path.to_string_lossy().to_string();
    let cmd_path = dir.join(format!("respawn_{}.cmd", wez_target));
    let cmd_content = format!("@echo off\r\n\"{}\" \"{}\"\r\n", bash, sh_str);
    if let Err(e) = fs::write(&cmd_path, cmd_content.as_bytes()) {
        log_line(&format!("  respawn-pane: failed to write cmd wrapper: {}", e));
        return 0;
    }

    let cmd_str = cmd_path.to_string_lossy().to_string();
    let send_text_arg = format!("{}\r", cmd_str);
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
// ----- send-keys key translation -----

/// Translate a single non-literal send-keys token into the string that
/// should actually be sent to the pane. tmux key names we recognize are
/// mapped to their control sequence; anything else (including symbolic key
/// names we do not know about) is passed through as literal text, matching
/// tmux's own fallback of treating an unrecognized token as literal input.
fn translate_key_token(token: &str) -> String {
    match token {
        "Enter" | "C-m" => "\r".to_string(),
        "Tab" => "\t".to_string(),
        "Space" => " ".to_string(),
        _ => {
            if let Some(rest) = token.strip_prefix("C-") {
                let mut chars = rest.chars();
                if let (Some(c), None) = (chars.next(), chars.next()) {
                    if c.is_ascii_alphabetic() {
                        let ctrl = c.to_ascii_uppercase() as u8 - b'A' + 1;
                        return (ctrl as char).to_string();
                    }
                }
            }
            token.to_string()
        }
    }
}

/// Build the text to hand to `wezterm cli send-text` for a send-keys
/// invocation. In literal mode (-l) tokens are simply space-joined and sent
/// verbatim - tmux's -l treats all remaining arguments as a single literal
/// string. Otherwise each token is translated individually (recognized key
/// name -> control sequence, else literal text) and concatenated with no
/// separator, since tmux never inserts implied whitespace between separate
/// key/text arguments.
fn build_send_text(tokens: &[String], literal: bool) -> String {
    if literal {
        tokens.join(" ")
    } else {
        tokens.iter().map(|t| translate_key_token(t.as_str())).collect()
    }
}

fn cmd_send_keys(args: &[String], state: &mut State) -> i32 {
    // send-keys [-l] -t <target> <key/text> [<key/text> ...]
    let mut target = String::new();
    let mut literal = false;
    let mut tokens: Vec<String> = Vec::new();
    let mut i = 0;
    while i < args.len() {
        match args[i].as_str() {
            "-t" => {
                i += 1;
                if i < args.len() {
                    target = args[i].clone();
                }
            }
            "-l" => literal = true,
            _ => tokens.push(args[i].clone()),
        }
        i += 1;
    }
    // resolve_current_pane already prefers WEZTERM_PANE (see its own doc
    // comment), so no separate WEZTERM_PANE check is needed here.
    let target = if target.is_empty() {
        resolve_current_pane(state)
    } else {
        target
    };
    let wez_target = match resolve_target(&target, state) {
        Some(id) => id,
        None => {
            log_line(&format!("  send-keys: could not resolve target={}", target));
            return 0;
        }
    };
    let text = build_send_text(&tokens, literal);
    let pane_id_str = wez_target.to_string();
    log_line(&format!(
        "  send-keys: target={} wez_target={} literal={} text={:?}",
        target, wez_target, literal, text
    ));
    run_wezterm(&[
        "cli",
        "send-text",
        "--pane-id",
        pane_id_str.as_str(),
        "--no-paste",
        text.as_str(),
    ]);
    0
}

// ----- capture-pane -----

/// Parsed capture-pane flags. `start`/`end` are kept as the raw string tmux
/// gave us (they can be negative, meaning "into the scrollback") since
/// `wezterm cli get-text --start-line/--end-line` accepts the same signed
/// line-number convention directly - no reinterpretation needed.
struct CapturePaneOpts {
    target: String,
    print: bool,
    start: Option<String>,
    end: Option<String>,
    escapes: bool,
}

fn parse_capture_pane_args(args: &[String]) -> CapturePaneOpts {
    let mut opts = CapturePaneOpts {
        target: String::new(),
        print: false,
        start: None,
        end: None,
        escapes: false,
    };
    let mut i = 0;
    while i < args.len() {
        match args[i].as_str() {
            "-t" => {
                i += 1;
                if i < args.len() {
                    opts.target = args[i].clone();
                }
            }
            "-p" => opts.print = true,
            "-e" => opts.escapes = true,
            "-S" => {
                i += 1;
                if i < args.len() {
                    opts.start = Some(args[i].clone());
                }
            }
            "-E" => {
                i += 1;
                if i < args.len() {
                    opts.end = Some(args[i].clone());
                }
            }
            _ => {}
        }
        i += 1;
    }
    opts
}

fn cmd_capture_pane(args: &[String], state: &mut State) -> i32 {
    // capture-pane -t <target> [-p] [-S <start>] [-E <end>] [-e]
    //
    // Maps directly onto `wezterm cli get-text`, which (unlike a fallback
    // full-text-then-slice approach) supports --start-line/--end-line with
    // the same signed line-number convention tmux uses (0 = top of screen,
    // negative = into scrollback). -p is effectively the only supported
    // mode here - there is no tmux paste-buffer equivalent in WezTerm, so
    // capturing without -p is a fail-soft no-op (nothing to do with the
    // text otherwise).
    let opts = parse_capture_pane_args(args);
    let target = if opts.target.is_empty() {
        resolve_current_pane(state)
    } else {
        opts.target
    };
    let wez_target = match resolve_target(&target, state) {
        Some(id) => id,
        None => {
            log_line(&format!("  capture-pane: could not resolve target={}", target));
            return 0;
        }
    };
    let pane_id_str = wez_target.to_string();
    let mut wez_args = vec!["cli", "get-text", "--pane-id", pane_id_str.as_str()];
    if let Some(s) = opts.start.as_deref() {
        wez_args.push("--start-line");
        wez_args.push(s);
    }
    if let Some(e) = opts.end.as_deref() {
        wez_args.push("--end-line");
        wez_args.push(e);
    }
    if opts.escapes {
        wez_args.push("--escapes");
    }
    let (stdout, _, _) = run_wezterm(&wez_args);
    if opts.print {
        print!("{}", stdout);
    } else {
        log_line("  capture-pane: -p not set; nothing to output (no tmux paste-buffer equivalent)");
    }
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

fn cmd_kill_pane(args: &[String], state: &mut State) -> i32 {
    // kill-pane [-t <target>]
    //
    // This is the real teammate-pane close path: CC issues kill-pane when a
    // teammate is dismissed or its task finishes, but the teammate claude
    // process itself stays alive/idle rather than exiting, so the
    // remain-on-exit teardown appended in cmd_respawn_pane never fires for
    // it. Without honoring kill-pane, the WezTerm pane lingers forever.
    //
    // If -t is absent, tmux kills the current pane; we mirror that via
    // resolve_current_pane.
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
            _ => {}
        }
        i += 1;
    }
    let target = if target.is_empty() {
        resolve_current_pane(state)
    } else {
        target
    };
    let wez_target = match resolve_target(&target, state) {
        Some(id) => id,
        None => {
            log_line(&format!("  kill-pane: could not resolve target={}", target));
            return 0;
        }
    };
    log_line(&format!("  kill-pane: target={} wez_target={}", target, wez_target));
    let pane_id_str = wez_target.to_string();
    let (_, _, exit) = run_wezterm(&["cli", "kill-pane", "--pane-id", pane_id_str.as_str()]);
    log_line(&format!("  kill-pane: wezterm exit={}", exit));
    // Best-effort cleanup regardless of the wezterm call's outcome, so a
    // pane we believe is gone does not linger in state and get reused.
    state.forget_pane(wez_target);
    save_state_locked(state);
    0
}

fn cmd_kill_window(args: &[String], state: &mut State) -> i32 {
    // kill-window [-t <target>]
    //
    // Best-effort: <target> may be a window id ("@N"), or a pane id/name that
    // resolves to a pane whose window we then kill. Every live pane in that
    // wezterm window is closed individually via wezterm cli kill-pane,
    // since WezTerm has no single "kill window" call.
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
            _ => {}
        }
        i += 1;
    }
    let panes = list_wez_panes();
    let window_id: Option<u64> = if let Some(n) = target.strip_prefix('@') {
        n.parse::<u64>().ok()
    } else {
        let pane_target = if target.is_empty() {
            resolve_current_pane(state)
        } else {
            target.clone()
        };
        resolve_target(&pane_target, state).and_then(|wid| {
            panes.iter().find(|p| p.pane_id == wid).map(|p| p.window_id)
        })
    };
    let window_id = match window_id {
        Some(w) => w,
        None => {
            log_line(&format!("  kill-window: could not resolve target={}", target));
            return 0;
        }
    };
    log_line(&format!("  kill-window: window_id={}", window_id));
    for p in panes.iter().filter(|p| p.window_id == window_id) {
        let pane_id_str = p.pane_id.to_string();
        let (_, _, exit) = run_wezterm(&["cli", "kill-pane", "--pane-id", pane_id_str.as_str()]);
        log_line(&format!(
            "  kill-window: killed pane_id={} exit={}",
            p.pane_id, exit
        ));
        state.forget_pane(p.pane_id);
    }
    save_state_locked(state);
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
        "kill-pane" => cmd_kill_pane(args, state),
        "kill-window" => cmd_kill_window(args, state),
        "send-keys" => cmd_send_keys(args, state),
        "capture-pane" => cmd_capture_pane(args, state),
        // kill-session intentionally stays on the UNHANDLED no-op path below:
        // it would otherwise mean mass-killing every pane the shim knows
        // about, which could tear down unrelated WezTerm panes/windows the
        // user still cares about. Only pane- and window-scoped kills are
        // honored.

        // Known-accepted no-ops (GitHub #6): subcommands CC may emit that
        // this shim intentionally does not act on. Exit 0, make no wezterm
        // call, and log a distinct NOOP line so a future shim.log audit can
        // tell a deliberate no-op apart from a never-seen UNHANDLED
        // subcommand.
        "select-layout" | "refresh-client" | "set-window-option" | "setw" | "rename-window"
        | "rename-session" | "move-window" | "swap-pane" => {
            log_line(&format!("NOOP: {} (known-accepted no-op)", subcommand));
            0
        }
        other => {
            // Fail-soft: log and exit 0 so CC does not crash on version drift,
            // but also tell ad-hoc/manual users on stderr that this
            // subcommand is not implemented (GitHub #1), so they can tell
            // "not implemented" apart from silent success.
            log_line(&format!("UNHANDLED: subcommand={:?} args={:?}", other, args));
            eprintln!(
                "tmux-wezterm-shim: unhandled subcommand '{}' (not implemented - see shim.log)",
                other
            );
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

    // Skip tmux's global options (e.g. `-S <socket>`, reconstructed by CC
    // from the TMUX env var) to find the actual subcommand.
    let mut i = 1;
    while i < argv.len() {
        match argv[i].as_str() {
            "-S" | "-L" | "-f" | "-c" | "-T" => i += 2,
            "-2" | "-C" | "-CC" | "-D" | "-l" | "-q" | "-u" | "-v" | "-V" => i += 1,
            _ => break,
        }
    }

    if i >= argv.len() {
        log_line("UNHANDLED: no subcommand after global flags");
        std::process::exit(0);
    }

    let subcommand = argv[i].clone();
    let args = argv[i + 1..].to_vec();

    let (mut state, _lock) = load_state_locked();
    let exit_code = dispatch(&subcommand, &args, &mut state);

    log_line(&format!("  -> exit {}", exit_code));
    std::process::exit(exit_code);
}

// ----- tests -----

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn translate_key_token_maps_enter_and_c_m() {
        assert_eq!(translate_key_token("Enter"), "\r");
        assert_eq!(translate_key_token("C-m"), "\r");
    }

    #[test]
    fn translate_key_token_maps_tab_and_space() {
        assert_eq!(translate_key_token("Tab"), "\t");
        assert_eq!(translate_key_token("Space"), " ");
    }

    #[test]
    fn translate_key_token_maps_control_letter() {
        assert_eq!(translate_key_token("C-c"), "\u{3}");
        assert_eq!(translate_key_token("C-a"), "\u{1}");
    }

    #[test]
    fn translate_key_token_passes_through_literal_text() {
        assert_eq!(translate_key_token("hello"), "hello");
        // Non-letter after "C-" is not a recognized control sequence.
        assert_eq!(translate_key_token("C-1"), "C-1");
    }

    #[test]
    fn build_send_text_literal_mode_joins_with_space() {
        let tokens = vec!["hello".to_string(), "world".to_string()];
        assert_eq!(build_send_text(&tokens, true), "hello world");
    }

    #[test]
    fn build_send_text_translates_and_concatenates_without_separator() {
        let tokens = vec!["ls".to_string(), "Enter".to_string()];
        assert_eq!(build_send_text(&tokens, false), "ls\r");
    }

    #[test]
    fn parse_capture_pane_args_parses_all_flags() {
        let args: Vec<String> = ["-t", "%3", "-p", "-S", "-10", "-E", "-1", "-e"]
            .iter()
            .map(|s| s.to_string())
            .collect();
        let opts = parse_capture_pane_args(&args);
        assert_eq!(opts.target, "%3");
        assert!(opts.print);
        assert_eq!(opts.start, Some("-10".to_string()));
        assert_eq!(opts.end, Some("-1".to_string()));
        assert!(opts.escapes);
    }

    #[test]
    fn parse_capture_pane_args_defaults_when_no_flags_given() {
        let opts = parse_capture_pane_args(&[]);
        assert_eq!(opts.target, "");
        assert!(!opts.print);
        assert_eq!(opts.start, None);
        assert_eq!(opts.end, None);
        assert!(!opts.escapes);
    }
}
