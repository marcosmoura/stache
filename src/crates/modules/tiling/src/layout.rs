//! Pure layout algorithms, gap resolution and workspace ordering.
//!
//! Window identity is a generic type parameter so a later actor token plugs in
//! without this module knowing any native type.

mod dispatch;
mod dwindle;
mod gaps;
mod grid;
mod helpers;
mod master;
mod monocle;
mod ordering;
mod plan;
mod split;

pub use dispatch::{LayoutRequest, master_ratio, plan_layout};
pub use dwindle::{is_split_horizontal as is_dwindle_split_horizontal, layout as dwindle_layout};
pub use gaps::{GapDisplay, Gaps};
pub use grid::{
    MAX_GRID_TILES, layout as grid_layout, regular_dimensions as regular_grid_dimensions,
};
pub use master::clamp_ratio as clamp_master_ratio;
pub use ordering::{insert_window, insertion_index, swap_windows};
pub use plan::{
    Exclusion, GridOverflow, LayoutDiagnostic, LayoutPlan, PlanStatus, UntouchedReason,
    WindowEntry, WindowFlags,
};
pub use split::{
    has_valid_cumulative_ratios, is_horizontal as is_split_horizontal, layout as split_layout,
};
