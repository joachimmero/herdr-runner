#!/usr/bin/env python3
"""Stands in for the herdr CLI calls the plugin's actions make, over a scripted workspace.

$FAKE_HERDR_DIR/state.json holds {"panes": [{"pane_id", "foreground_cwd", "argv0", "argv"}]},
which pane close and plugin pane open change; calls.jsonl records every call.
"""
import json
import os
import sys


def option(args, name):
    return args[args.index(name) + 1] if name in args else None


def main():
    directory = os.environ["FAKE_HERDR_DIR"]
    args = sys.argv[1:]
    with open(os.path.join(directory, "calls.jsonl"), "a") as log:
        log.write(json.dumps(args) + "\n")
    state_path = os.path.join(directory, "state.json")
    with open(state_path) as handle:
        state = json.load(handle)
    panes = state["panes"]
    find = lambda pane_id: next((pane for pane in panes if pane["pane_id"] == pane_id), None)

    if args[:2] == ["pane", "list"]:
        result = {"panes": [{"pane_id": pane["pane_id"], "foreground_cwd": pane.get("foreground_cwd")}
                            for pane in panes]}
    elif args[:2] == ["pane", "process-info"]:
        pane = find(option(args, "--pane"))
        if pane is None:
            print(json.dumps({"error": {"code": "pane_not_found"}}), file=sys.stderr)
            sys.exit(1)
        process = {"pid": 1, "name": pane.get("argv0"), "argv0": pane.get("argv0"),
                   "argv": pane.get("argv", []), "cwd": pane.get("foreground_cwd")}
        result = {"process_info": {"pane_id": pane["pane_id"], "foreground_processes": [process]}}
    elif args[:2] == ["pane", "close"]:
        state["panes"] = [pane for pane in panes if pane["pane_id"] != args[2]]
        result = {}
    elif args[:3] == ["plugin", "pane", "open"]:
        pane_id = "w1:p%d" % (len(panes) + 10)
        panes.append({"pane_id": pane_id, "foreground_cwd": option(args, "--cwd"),
                      "argv0": "herdr-runner", "argv": ["/plugin/bin/herdr-runner"]})
        result = {"plugin_pane": {"pane": {"pane_id": pane_id, "tab_id": "w1:t1"}}}
    else:
        print(json.dumps({"error": {"code": "unknown_command"}}), file=sys.stderr)
        sys.exit(1)

    with open(state_path, "w") as handle:
        json.dump(state, handle)
    print(json.dumps({"id": "1", "result": result}))


if __name__ == "__main__":
    main()
