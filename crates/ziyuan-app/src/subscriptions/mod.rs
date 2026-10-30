//! Independent metadata-only subscriptions. No Court, model, body download or background task.
//! Callers supply Unix seconds and invoke `check` themselves. One call claims at most 50 works;
//! production requests have a 30 second total timeout, comfortably below the 90 second lease.
mod check;
mod model;
mod store;
mod syosetu;

pub use model::*;
pub use store::Subscriptions;
pub use syosetu::SyosetuSource;
