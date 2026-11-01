# herdr-runner

A [herdr](https://herdr.dev) plugin that opens a pane for browsing and running Unity Test Runner
tests in the Editor you already have open.

- Browse tests as assembly → fixture → test, and run one test, a fixture, an assembly, or
  everything. Each test shows pass/fail inline; a selected failure shows its message and stack
  trace.
- Every run recompiles first, so it never tests stale assemblies. Compile errors show in the pane
  and the run is skipped.
- Switch between EditMode and PlayMode.
- `[Explicit]` tests are listed and marked, skipped by broader runs, and run when selected directly.
- Cancel a run in progress. If another client's async run holds the Editor, the pane waits for it.
- Hand the pane a run from the command line, so a coding agent's test runs show where you can
  watch them (see [Running from the command line](#running-from-the-command-line)).

## Requirements

- macOS or Windows, herdr 0.9.3 or later.
- The [`unity` CLI](https://docs.unity.com) on `PATH` or in `~/.unity/bin`.
- The project has the Pipeline package (`com.unity.pipeline`) installed and its Editor open.
- `cargo`, to build the plugin. Install it with [rustup](https://rustup.rs); the first build
  fetches the Rust version pinned in `rust-toolchain.toml`.

## Install (local)

```sh
scripts/link.sh
```

On Windows, run it from Git Bash.

That builds the release binary with `cargo`, puts it at `bin/herdr-runner` (`.exe` on Windows),
puts `herdr-runner` on `PATH` through `~/.local/bin`, and runs `herdr plugin link`. On Windows
it also adds that folder to the user PATH. Then bind the toggle to a key in herdr's `config.toml`:

```toml
[[keys.command]]
key = "prefix+u"
type = "plugin_action"
command = "joachimmero.herdr-runner.toggle"
description = "toggle Unity tests"
```

The pane opens as a split beside the focused pane, for the Unity project at or above that pane's
directory.

## Keys

| Key | Does |
| --- | --- |
| `j` `k` / `↓` `↑` | Move |
| `Ctrl+d` `Ctrl+u` | Move half a page down, up |
| `l` `h` / `→` `←`, `space` | Open, close, toggle a branch |
| `⏎` | Run the selection |
| `a` | Run everything (explicit tests excluded) |
| `f` | Re-run the current mode's failures |
| `m` | Switch EditMode / PlayMode |
| `c` | Cancel the run |
| `/` | Filter by name (`⏎` keeps it, `Esc` clears it, `⌫` on an empty filter leaves it) |
| `r` | Reload the test list |
| `J` `K` | Scroll the failure detail |
| `Esc` | Dismiss a notice |
| `q` | Close the pane |

## Running from the command line

`herdr-runner run` hands a run to the pane, so a coding agent's test runs show in the pane as
your own do:

```sh
bin/herdr-runner run [<name>] [--mode edit|play] [--timeout <seconds>]
```

- `<name>` is a test's full name, a fixture's, or an assembly's, matched exactly. Leave it out
  to run everything. An unknown name fails and lists names it might have meant. Naming an
  `[Explicit]` test runs it.
- Run it from a herdr pane inside the project. It uses the workspace's runner pane, or opens one
  beside the calling pane without taking focus.
- The pane recompiles and relists before it looks up the name, so a test written moments ago
  is found. Then it filters the tree to the target, selects it, and runs it. A run you or
  another client already started goes first.
- It blocks until the run ends and prints a summary line, then every failure with its full
  message and stack trace. Exit codes: `0` all passed, `5` a test failed, `1` compile errors,
  a cancelled run, or any other problem, `2` bad arguments.
- `--timeout` defaults to 300 seconds. When it runs out, the pane is asked to cancel the run.

## Limits

These come from the Pipeline package's test commands:

- Unity selects tests by case-insensitive substring of the full name, so running one test also
  runs any test whose name contains it.
- No per-test progress is reported while a run is going, only when it finishes.
- Only async runs are visible to other clients. A synchronous run another client started (for
  example `run_tests` without `--async_tests`) can't be seen, and starting a run from the pane
  replaces it.

## Development

```sh
cargo test
```

The tests run the plugin against scripted stand-ins for the Editor (`tests/fakes/unity.py`) and
for herdr (`tests/fakes/herdr.py`), so they need `python3` (or `python`) but no Editor.
