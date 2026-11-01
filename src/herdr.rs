//! The herdr CLI, which is the whole plugin API.

use std::ffi::OsString;
use std::path::PathBuf;
use std::process::{Command, Stdio};

use serde_json::Value;

/// `HERDR_BIN_PATH` reaches the running herdr over its socket or pipe on every platform.
fn bin() -> OsString {
    std::env::var_os("HERDR_BIN_PATH").unwrap_or_else(|| "herdr".into())
}

/// The `result` of a herdr subcommand's JSON answer.
pub fn call(args: &[&str]) -> Result<Value, String> {
    let output = Command::new(bin())
        .args(args)
        .stdin(Stdio::null())
        .output()
        .map_err(|error| format!("could not run herdr: {error}"))?;
    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        return Err(format!("herdr {} failed: {}", args.join(" "), stderr.trim()));
    }
    let answer: Value = serde_json::from_slice(&output.stdout).map_err(|error| {
        format!("herdr {} answered in an unknown shape: {error}", args.join(" "))
    })?;
    Ok(answer.get("result").cloned().unwrap_or(Value::Null))
}

pub fn var(name: &str) -> Option<String> {
    std::env::var(name).ok().filter(|value| !value.is_empty())
}

pub fn plugin_id() -> String {
    var("HERDR_PLUGIN_ID").unwrap_or_else(|| "joachimmero.herdr-runner".into())
}

/// The plugin's state dir. herdr hands it to the pane and to actions but not to a shell running
/// the binary directly, so there it is rebuilt from herdr's layout.
pub fn state_dir() -> Result<PathBuf, String> {
    if let Some(dir) = var("HERDR_PLUGIN_STATE_DIR") {
        return Ok(PathBuf::from(dir));
    }
    let state = var("XDG_STATE_HOME")
        .map(PathBuf::from)
        .or_else(|| home().map(|home| home.join(".local/state")))
        .ok_or("no plugin state dir: neither HERDR_PLUGIN_STATE_DIR nor a home directory is set")?;
    Ok(state.join("herdr/plugins").join(plugin_id()))
}

pub fn home() -> Option<PathBuf> {
    var("HOME").or_else(|| var("USERPROFILE")).map(PathBuf::from)
}

/// `id` keeping only the characters a file name can safely hold; herdr ids contain `:`.
pub fn file_safe(id: &str) -> String {
    id.chars().map(|ch| if ch.is_ascii_alphanumeric() { ch } else { '_' }).collect()
}
