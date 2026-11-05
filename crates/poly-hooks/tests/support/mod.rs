//! Helpers shared by the test binaries that run a hook shim they just wrote.

use std::io::ErrorKind;
use std::process::{Child, Command, Output, Stdio};
use std::thread;
use std::time::Duration;

const TEXT_BUSY_RETRIES: u32 = 50;
const TEXT_BUSY_DELAY: Duration = Duration::from_millis(20);

/// Spawn `command`, retrying while Linux reports the executable busy (`ETXTBSY`).
///
/// A test thread forking between another thread's write and exec of a script inherits the
/// writable descriptor until its own exec, so a freshly written shim can briefly refuse to run.
/// The race is within one test process; `poly hooks install` and git's later run never meet it.
pub fn spawn(command: &mut Command) -> Child {
    let mut attempts = 0;
    loop {
        match command.spawn() {
            Err(error) if error.kind() == ErrorKind::ExecutableFileBusy && attempts < TEXT_BUSY_RETRIES => {
                attempts += 1;
                thread::sleep(TEXT_BUSY_DELAY);
            }
            spawned => return spawned.expect("spawn the hook shim"),
        }
    }
}

/// [`spawn`], then wait for the output; like [`Command::output`], stdout and stderr are captured.
#[allow(dead_code)] // Not every test binary that includes this module collects output.
pub fn output(command: &mut Command) -> Output {
    command.stdout(Stdio::piped()).stderr(Stdio::piped());
    spawn(command).wait_with_output().expect("wait for the hook shim")
}
