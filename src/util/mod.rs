// ===========================================================================
// util - Utility Functions
// ===========================================================================

mod branch_name;
mod duration;

pub use branch_name::{generate_branch_name, generate_unique_branch_name};
pub use duration::{format_step, format_total};
