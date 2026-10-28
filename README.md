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

## Requirements

- macOS, herdr 0.9.3 or later.
- The [`unity` CLI](https://docs.unity.com) on `PATH` or in `~/.unity/bin`.
- The project has the Pipeline package (`com.unity.pipeline`) installed and its Editor open.

## Install (local)

```sh
scripts/link.sh
```

That builds the release binary, links `bin/herdr-runner` to it, and runs `herdr plugin link`.
Then bind the toggle to a key in herdr's `config.toml`:

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
for herdr (`tests/fakes/herdr.py`), so they need `python3` but no Editor.
