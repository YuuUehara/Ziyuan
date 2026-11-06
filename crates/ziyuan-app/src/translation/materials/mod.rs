//! Unapproved local material DTOs and pure validation; no storage or production promotion.
mod model;
mod validation;

pub use model::*;
pub use validation::{MAX_MATERIAL_BYTES, check_material_editable, validate_material};
