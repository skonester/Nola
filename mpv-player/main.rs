//! A minimal mpv player: opens its own window through the app's bundled
//! libmpv, with mpv's on-screen controller, key bindings and the user's
//! mpv.conf. Used by "Open in external player" when mpv isn't installed.
//!
//! Usage mirrors mpv's for simple cases:
//!   mpv [--option=value | --flag | --no-flag]... <file-or-url>...

// No console window in release builds.
#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

use std::ffi::{c_char, c_int, c_void, CStr, CString};
use std::process::ExitCode;

const MPV_EVENT_SHUTDOWN: c_int = 1;

#[repr(C)]
#[allow(dead_code)] // only event_id is read; the other fields keep the C layout
struct MpvEvent {
    event_id: c_int,
    error: c_int,
    reply_userdata: u64,
    data: *mut c_void,
}

unsafe extern "C" {
    fn mpv_create() -> *mut c_void;
    fn mpv_initialize(ctx: *mut c_void) -> c_int;
    fn mpv_set_option_string(ctx: *mut c_void, name: *const c_char, data: *const c_char) -> c_int;
    fn mpv_command(ctx: *mut c_void, args: *const *const c_char) -> c_int;
    fn mpv_wait_event(ctx: *mut c_void, timeout: f64) -> *mut MpvEvent;
    fn mpv_terminate_destroy(ctx: *mut c_void);
    fn mpv_error_string(error: c_int) -> *const c_char;
}

fn error_text(code: c_int) -> String {
    unsafe { CStr::from_ptr(mpv_error_string(code)) }.to_string_lossy().into_owned()
}

fn cstring(value: &str) -> Result<CString, String> {
    CString::new(value).map_err(|_| format!("argument contains a NUL byte: {value:?}"))
}

/// Turn mpv-style `--name=value`, `--name` and `--no-name` into option pairs.
fn parse_option(arg: &str) -> (String, String) {
    let body = &arg[2..];
    match body.split_once('=') {
        Some((name, value)) => (name.to_string(), value.to_string()),
        None => match body.strip_prefix("no-") {
            Some(name) => (name.to_string(), "no".to_string()),
            None => (body.to_string(), "yes".to_string()),
        },
    }
}

fn run() -> Result<(), String> {
    let mut options = Vec::new();
    let mut files = Vec::new();
    let mut only_files = false;
    for arg in std::env::args().skip(1) {
        if only_files || !arg.starts_with("--") {
            files.push(arg);
        } else if arg == "--" {
            only_files = true;
        } else {
            options.push(parse_option(&arg));
        }
    }
    if files.is_empty() {
        return Err("usage: mpv [--option=value]... <file-or-url>...".into());
    }

    let ctx = unsafe { mpv_create() };
    if ctx.is_null() {
        return Err("failed to create the mpv context".into());
    }
    let set = |name: &str, value: &str| -> Result<(), String> {
        let (n, v) = (cstring(name)?, cstring(value)?);
        match unsafe { mpv_set_option_string(ctx, n.as_ptr(), v.as_ptr()) } {
            0.. => Ok(()),
            code => Err(format!("--{name}={value}: {}", error_text(code))),
        }
    };

    let result = (|| {
        // libmpv defaults to an embeddable, input-less, idle player; switch
        // back to the behaviour of the regular mpv executable.
        for (name, value) in [
            ("config", "yes"),
            ("idle", "no"),
            ("force-window", "yes"),
            ("osc", "yes"),
            ("input-default-bindings", "yes"),
            ("input-vo-keyboard", "yes"),
            ("terminal", "yes"),
        ] {
            set(name, value)?;
        }
        for (name, value) in &options {
            set(name, value)?;
        }

        let code = unsafe { mpv_initialize(ctx) };
        if code < 0 {
            return Err(format!("failed to start mpv: {}", error_text(code)));
        }

        let loadfile = cstring("loadfile")?;
        let append = cstring("append-play")?;
        for file in &files {
            let file = cstring(file)?;
            let args = [loadfile.as_ptr(), file.as_ptr(), append.as_ptr(), std::ptr::null()];
            let code = unsafe { mpv_command(ctx, args.as_ptr()) };
            if code < 0 {
                return Err(format!("failed to queue {:?}: {}", file, error_text(code)));
            }
        }

        // With idle=no the core shuts down once the playlist ends or the user
        // quits (q, or closing the window).
        loop {
            let event = unsafe { &*mpv_wait_event(ctx, -1.0) };
            if event.event_id == MPV_EVENT_SHUTDOWN {
                return Ok(());
            }
        }
    })();

    unsafe { mpv_terminate_destroy(ctx) };
    result
}

fn main() -> ExitCode {
    match run() {
        Ok(()) => ExitCode::SUCCESS,
        Err(message) => {
            eprintln!("mpv: {message}");
            ExitCode::FAILURE
        }
    }
}
