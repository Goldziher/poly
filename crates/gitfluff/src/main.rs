//! Thin binary entry point for the standalone `gitfluff` command.
//!
//! All orchestration lives in the library (`gitfluff::main_entry`); this binary
//! only translates the resolved exit code into a process exit.

fn main() {
    std::process::exit(gitfluff::main_entry());
}
