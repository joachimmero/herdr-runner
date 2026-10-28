mod actions;
mod app;
#[cfg(test)]
mod fake;
mod herdr;
mod project;
mod runner;
mod tree;
mod ui;
mod unity;

use std::process::ExitCode;

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    ExitCode::from(match args.as_slice() {
        [] => app::run(),
        [flag, name] if flag == "--action" => actions::run(name),
        _ => {
            eprintln!("usage: herdr-runner [--action toggle|open|close]");
            2
        }
    })
}
