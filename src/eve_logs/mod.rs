//! EVE chatlog / gamelog tailing.
//!
//! Pure parse + identify helpers are unconditional so Linux CI can
//! unit-test them. The poll loop lives in `tail` and is spawned from
//! the daemon; missing log dirs are non-fatal.

mod identify;
mod parse;
mod paths;
mod state;
mod tail;

pub use paths::resolve;
pub use state::LogLiveState;
pub use tail::spawn;
