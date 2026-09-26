//! The compose wizard's pure core: turns a short, validated description of how
//! to build and run a project into a `docker-compose.yml` (plus the
//! Dockerfile it needs), for a project that ships without one.
//!
//! Three seams, built independently against the types in [`spec`]:
//! - [`catalog`]: the fixed set of runtimes, versions, build tools, databases
//!   and extra services LocalSync supports. The UI's dropdowns are generated
//!   from it, and both validation and generation look things up in it - one
//!   source of truth, so a value the UI offers is always one the rest accepts.
//! - [`validate`]: rejects bad input with a specific, actionable message
//!   *before* anything is generated.
//! - [`generate`]: the validated spec -> compose + Dockerfile.

pub mod catalog;
pub mod generate;
pub mod spec;
pub mod validate;

pub use generate::generate;
pub use spec::*;
pub use validate::validate;
