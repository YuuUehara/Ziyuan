//! Unapproved local material DTOs, pure validation and independent draft storage;
//! no production promotion.
mod diff;
mod model;
mod preview;
mod store;
mod validation;

pub use diff::*;
pub use model::*;
pub use validation::{MAX_MATERIAL_BYTES, check_material_editable, validate_material};
