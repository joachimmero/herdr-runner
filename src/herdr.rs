//! The herdr CLI, which is the whole plugin API.

use std::ffi::OsString;
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
