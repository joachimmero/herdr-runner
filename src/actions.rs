//! The plugin's toggle, open, and close actions on the invoking workspace's runner pane.

use std::fs::File;
use std::path::Path;

use serde_json::Value;

use crate::herdr::{self, var};
use crate::project;

#[derive(Clone, Copy, PartialEq, Eq)]
enum Action {
    Toggle,
    Open,
    Close,
}

/// Run the action `name` and return the process exit code.
pub fn run(name: &str) -> u8 {
    let action = match name {
        "toggle" => Action::Toggle,
        "open" => Action::Open,
        "close" => Action::Close,
        _ => {
            eprintln!("herdr-runner: unknown action '{name}' (toggle | open | close)");
            return 1;
        }
    };
    match act(action) {
        Ok(line) => {
            println!("herdr-runner: {line}");
            0
        }
        Err(why) => {
            eprintln!("herdr-runner: {why}");
            1
        }
    }
}

fn act(action: Action) -> Result<String, String> {
    let context: Value = var("HERDR_PLUGIN_CONTEXT_JSON")
        .and_then(|json| serde_json::from_str(&json).ok())
        .unwrap_or_default();
    let ws = var("HERDR_WORKSPACE_ID")
        .or_else(|| text(&context, "/workspace_id"))
        .ok_or("no workspace context (invoke from inside herdr)")?;
    // Held through the close or open, so two quick toggles can't both open a pane.
    let _lock = lock(&ws)?;

    let panes = herdr::call(&["pane", "list", "--workspace", &ws])?;
    let panes = panes["panes"].as_array().cloned().unwrap_or_default();
    let ids: Vec<String> = panes.iter().filter_map(|pane| text(pane, "/pane_id")).collect();
    let open: Vec<&String> = ids.iter().filter(|pane| runs_runner(pane)).collect();

    match (action, open.is_empty()) {
        (Action::Toggle | Action::Close, false) => {
            open.iter().try_for_each(|pane| herdr::call(&["pane", "close", pane]).map(drop))?;
            Ok(format!(
                "closed {}",
                open.iter().map(|pane| pane.as_str()).collect::<Vec<_>>().join(" ")
            ))
        }
        (Action::Open, false) => Ok(format!("already open ({})", open[0])),
        (Action::Close, true) => Ok("nothing open".into()),
        (Action::Toggle | Action::Open, true) => open_pane(&ws, &context, &panes),
    }
}

fn open_pane(ws: &str, context: &Value, panes: &[Value]) -> Result<String, String> {
    let focused = text(context, "/focused_pane_id").or_else(|| var("HERDR_PANE_ID"));
    // The focused pane's live cwd wins over the workspace's, when it is inside a project.
    let live = focused.as_deref().and_then(|focused| {
        panes
            .iter()
            .find(|pane| text(pane, "/pane_id").as_deref() == Some(focused))
            .and_then(|pane| text(pane, "/foreground_cwd"))
    });
    let candidates: Vec<String> = live
        .into_iter()
        .chain(text(context, "/focused_pane_cwd"))
        .chain(text(context, "/workspace_cwd"))
        .collect();
    let root = candidates
        .iter()
        .find_map(|dir| project::find_root(Path::new(dir)))
        .ok_or_else(|| format!("no Unity project at or above {}", candidates.join(", ")))?;
    let target = focused
        .or_else(|| panes.first().and_then(|pane| text(pane, "/pane_id")))
        .ok_or_else(|| format!("no pane to split in {ws}"))?;
    let plugin = var("HERDR_PLUGIN_ID").unwrap_or_else(|| "joachimmero.herdr-runner".into());
    let root = root.to_string_lossy();

    let opened = herdr::call(&[
        "plugin",
        "pane",
        "open",
        "--plugin",
        &plugin,
        "--entrypoint",
        "pane",
        "--placement",
        "split",
        "--target-pane",
        &target,
        "--direction",
        "right",
        "--cwd",
        &root,
        "--focus",
    ])?;
    let pane = text(&opened, "/plugin_pane/pane/pane_id").unwrap_or_else(|| "a pane".into());
    Ok(format!("opened {pane} for {root}"))
}

/// Whether pane `pane`'s foreground is the runner's UI rather than one of its actions.
fn runs_runner(pane: &str) -> bool {
    let Ok(info) = herdr::call(&["pane", "process-info", "--pane", pane]) else {
        return false;
    };
    let processes = info.pointer("/process_info/foreground_processes").and_then(Value::as_array);
    processes.into_iter().flatten().any(|process| {
        let argv: Vec<&str> =
            process["argv"].as_array().into_iter().flatten().filter_map(Value::as_str).collect();
        let exe = text(process, "/argv0").or_else(|| argv.first().map(|exe| (*exe).to_owned()));
        let named =
            exe.is_some_and(|exe| Path::new(&exe).file_name().is_some_and(|name| name == BINARY));
        named && !argv.contains(&"--action")
    })
}

const BINARY: &str = env!("CARGO_PKG_NAME");

fn lock(ws: &str) -> Result<File, String> {
    let dir = var("HERDR_PLUGIN_STATE_DIR")
        .ok_or("no plugin state dir (invoke as a herdr plugin action)")?;
    std::fs::create_dir_all(&dir).map_err(|error| format!("cannot create {dir}: {error}"))?;
    // Workspace ids hold characters like `:`, so the file name keeps only the safe ones.
    let name: String =
        ws.chars().map(|ch| if ch.is_ascii_alphanumeric() { ch } else { '_' }).collect();
    let path = Path::new(&dir).join(format!("action-{name}.lock"));
    let file = File::options()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .open(&path)
        .map_err(|error| format!("cannot open {}: {error}", path.display()))?;
    file.lock().map_err(|error| format!("cannot lock {}: {error}", path.display()))?;
    Ok(file)
}

fn text(value: &Value, pointer: &str) -> Option<String> {
    value
        .pointer(pointer)
        .and_then(Value::as_str)
        .filter(|text| !text.is_empty())
        .map(str::to_owned)
}
