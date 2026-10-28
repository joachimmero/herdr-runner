#!/usr/bin/env python3
"""Stands in for the unity CLI's `command` verb, answering as a scripted Editor would.

Usage: unity.py <dir> command [--option value ...] <name> [--arg value ...] --json ...

<dir>/scenario.json scripts the Editor, <dir>/state.json carries it from one call to the next,
and <dir>/calls.jsonl records every call for the test to read back. Answers copy the live
Pipeline package's shapes, PascalCase fields and string-encoded results included.

Scenario keys, all optional:
  project         the projectPath editor_status reports
  unreachable     every call fails as if no Editor were open
  flaky           {command: n}: that command's first n calls fail the same way
  tests           {"editor": [...], "playmode": [...]}, each test
                  {"FullName", "Assembly", "Explicit", "outcome": {"Status", "Message", "StackTrace"}}
  compile         {"start": "up_to_date" | "compiling" | "failed", "polls": n, "errors": [...]}
  run_polls       how many test_status polls a run reads as running before it completes
  foreign_polls   how many test_status polls read as another client's run before any of ours
  refuse_run      run_tests answers success false with this error
  run_error       a run ends in test_status error with this message
"""
import json
import os
import sys

FLAGS = {"--json", "--no-banner", "--non-interactive"}
UNREACHABLE = "No Unity Editor instances found with reachable Pipeline servers."


def parse(argv):
    if not argv or argv[0] != "command":
        sys.exit("fake unity only serves the command verb")
    rest = [token for token in argv[1:] if token not in FLAGS]
    options, args, name = {}, {}, None
    index = 0
    while index < len(rest):
        token = rest[index]
        if token.startswith("--"):
            (args if name else options)[token[2:]] = rest[index + 1]
            index += 2
        else:
            name = token
            index += 1
    return options, name, args


def load(path, default):
    try:
        with open(path) as handle:
            return json.load(handle)
    except FileNotFoundError:
        return default


def answer(name, result):
    print(json.dumps({"success": True, "command": "command " + name,
                      "data": {"command": name, "result": result}, "errors": [], "warnings": []}))


def fail(name, message):
    print(json.dumps({"success": False, "command": "unity command " + name, "data": None,
                      "errors": [{"code": "COMMAND_FAILED", "message": message}], "warnings": []}))


def selected(tests, args):
    """Unity's selection: a case-insensitive substring match, [Explicit] only when included."""
    needle = args.get("filter", "").lower()
    by_assembly = args.get("filter_type", "testName").lower() == "assembly"
    include_explicit = args.get("include_explicit") == "true"

    def keep(test):
        if test.get("Explicit") and not include_explicit:
            return False
        haystack = test.get("Assembly", "") if by_assembly else test["FullName"]
        return needle in haystack.lower()

    return [test for test in tests if keep(test)]


def outcome(test):
    scripted = test.get("outcome", {})
    return {"FullName": test["FullName"], "Status": scripted.get("Status", "Passed"),
            "Duration": scripted.get("Duration", 0.01), "Message": scripted.get("Message"),
            "StackTrace": scripted.get("StackTrace")}


def serve(scenario, state, name, args):
    """The command's result, or a string to fail with."""
    if name == "editor_status":
        return {"status": "ready", "compiling": False, "domainReloadInProgress": False,
                "playMode": "stopped", "projectPath": scenario["project"], "unityVersion": "6000.3.6f1"}

    if name == "list_tests":
        tests = [{key: value for key, value in test.items() if key != "outcome"}
                 for test in scenario.get("tests", {}).get(args.get("mode", "editor"), [])]
        for test in tests:
            test.setdefault("Assembly", "Fake.Tests")
            test.setdefault("Explicit", False)
            test.update({"Mode": "EditMode", "Categories": ["Uncategorized"]})
        return {"Mode": "EditMode", "Count": len(tests), "Tests": tests, "success": True,
                "result": None, "message": "Found %d test(s)." % len(tests)}

    compile_ = scenario.get("compile", {})
    if name == "recompile":
        start = compile_.get("start", "up_to_date")
        state["compile_polls"] = compile_.get("polls", 1)
        return {"status": start, "message": "fake " + start}

    if name == "recompile_status":
        start = compile_.get("start", "up_to_date")
        if start == "compiling" and state.get("compile_polls", 0) > 0:
            state["compile_polls"] -= 1
            return json.dumps({"status": "compiling", "failed": False, "errors": []})
        errors = compile_.get("errors", [])
        status = "up_to_date" if start == "up_to_date" else "completed"
        return json.dumps({"status": status, "failed": bool(errors) or start == "failed",
                           "errors": errors, "compilationFailed": bool(errors)})

    if name == "run_tests":
        if scenario.get("refuse_run"):
            return {"success": False, "error": scenario["refuse_run"], "Results": [], "Summary": {}}
        tests = scenario.get("tests", {}).get(args.get("mode", "editor"), [])
        state["run"] = {"polls": scenario.get("run_polls", 1),
                        "results": [outcome(test) for test in selected(tests, args)]}
        state["status"] = None
        return {"success": True, "result": "running", "StatusPath": "Temp/pipeline_test_status.json",
                "Mode": "EditMode", "message": "Tests started in async mode."}

    if name == "test_status":
        foreign = state.setdefault("foreign", scenario.get("foreign_polls", 0))
        if foreign > 0:
            state["foreign"] -= 1
            return json.dumps({"status": "running"})
        run = state.get("run")
        if run and run["polls"] > 0:
            run["polls"] -= 1
            return json.dumps({"status": "running"})
        if run:
            results = run["results"]
            state["run"] = None
            if scenario.get("run_error"):
                state["status"] = {"status": "error", "message": scenario["run_error"]}
            else:
                failed = sum(1 for result in results if result["Status"] == "Failed")
                state["status"] = {"status": "completed", "duration": 0.1, "results": results,
                                   "summary": {"total": len(results), "passed": len(results) - failed,
                                               "failed": failed, "skipped": 0, "inconclusive": 0}}
        status = state.get("status") or {"status": "no_tests", "message": "No test run in progress"}
        return json.dumps(status, indent=2)

    if name == "cancel_tests":
        state["run"] = None
        state["status"] = {"status": "cancelled", "message": "Test run cancelled."}
        return {"status": "cancelled", "message": "Test run cancelled."}

    return "Unknown command: " + name


def main():
    directory = sys.argv[1]
    options, name, args = parse(sys.argv[2:])
    scenario = load(os.path.join(directory, "scenario.json"), {})
    state_path = os.path.join(directory, "state.json")
    state = load(state_path, {})
    with open(os.path.join(directory, "calls.jsonl"), "a") as log:
        log.write(json.dumps({"command": name, "options": options, "args": args}) + "\n")

    counts = state.setdefault("calls", {})
    counts[name] = counts.get(name, 0) + 1
    flaky = scenario.get("flaky", {}).get(name, 0)
    if scenario.get("unreachable") or counts[name] <= flaky:
        result = UNREACHABLE
    else:
        result = serve(scenario, state, name, args)
    with open(state_path, "w") as handle:
        json.dump(state, handle)

    # A plain string is a failure; the string-encoded results above are JSON text, so they parse.
    if isinstance(result, str):
        try:
            json.loads(result)
        except ValueError:
            fail(name, result)
            return
    answer(name, result)


if __name__ == "__main__":
    main()
