//! Generation: a validated [`ComposeSpec`] -> `docker-compose.yml` + the
//! Dockerfile it builds from.
//!
//! (Stub - implemented by the generation work; the signature is the contract.)

use crate::spec::{ComposeSpec, GenerateContext, GenerateError, Generated};

/// Validates `spec` first (returning [`GenerateError::Invalid`] if it fails -
/// nothing is generated from bad input), then generates.
pub fn generate(_spec: &ComposeSpec, _ctx: &GenerateContext) -> Result<Generated, GenerateError> {
    unimplemented!("generation is built against the contract in spec.rs")
}
