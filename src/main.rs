mod actions;
mod app;
mod client;
#[cfg(test)]
mod fake;
mod herdr;
mod mailbox;
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
        [verb, rest @ ..] if verb == "run" => client::run(rest),
        _ => {
            eprintln!("usage: herdr-runner [--action toggle|open|close]");
            eprintln!("       {}", client::USAGE);
            2
        }
    })
}
