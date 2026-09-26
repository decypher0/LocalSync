//! Validation: rejects a bad [`ComposeSpec`] with a specific, actionable
//! message per field *before* anything is generated.
//!
//! (Stub - implemented by the validation work; the signature is the contract.)

use crate::spec::{ComposeSpec, FieldError};

/// Every problem with `spec`, all at once (not just the first), each naming
/// the field and saying what would fix it. `Ok(())` only if the spec is
/// complete and every value is one the catalog supports.
pub fn validate(_spec: &ComposeSpec) -> Result<(), Vec<FieldError>> {
    unimplemented!("validation is built against the contract in spec.rs")
}
