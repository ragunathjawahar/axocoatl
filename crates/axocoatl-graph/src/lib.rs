//! Experimental graph-validation crate.
//!
//! Provides workflow-graph types and validation (cycle detection, dependency
//! checks). It is **not yet wired into the runtime** — no daemon, actor, or CLI
//! path imports it. Kept as a standalone, self-contained module for future
//! static validation of agent workflow graphs. Treat its API as unstable.

pub mod error;
pub mod types;
pub mod validation;
pub mod workflow;

pub use error::*;
pub use types::*;
pub use validation::*;
pub use workflow::*;
