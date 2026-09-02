//! The `native` namespace.

use clap::Args;

use super::{Exit, Failure};

/// `clickdoom native`.
#[derive(Args)]
#[command(
    about = "DOOM's own simulation and renderer, as SQL",
    // Hard-wrapped: clap only rewraps help text with its `wrap_help`
    // feature, which this binary does not enable.
    long_about = "\
DOOM's own simulation and renderer, expressed as ClickHouse SQL: the tic
loop, the game state and the frame, with no instruction-set emulator
underneath. native/README.md states what the SQL has to do, and
clickdoom_spec::native_state fixes the state it carries between tics.

The namespace has no subcommands. `clickdoom emulation --help` lists what
runs the DOOM ROM on the CPU in SQL."
)]
pub struct NativeCmd {}

/// Reports that the namespace has nothing to run, and says where to look
/// instead.
pub(super) fn run(_cmd: &NativeCmd) -> Result<Exit, Failure> {
    Err(Failure {
        exit: Exit::Usage,
        message: "native has no subcommands. `clickdoom native --help` describes the namespace; \
                  `clickdoom emulation --help` lists the subcommands that run the DOOM ROM."
            .into(),
    })
}
