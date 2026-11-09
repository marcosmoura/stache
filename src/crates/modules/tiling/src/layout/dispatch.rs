//! Layout dispatch: classification, Grid cap and finite-frame validation.

use super::plan::{
    Exclusion, GridOverflow, LayoutDiagnostic, LayoutPlan, PlanStatus, UntouchedReason, WindowEntry,
};
use super::{Gaps, MAX_GRID_TILES, dwindle, grid, master, monocle, split};
use crate::config::{LayoutType, MasterConfig, MasterPosition};
use crate::geometry::{GeometryError, Rect};

/// Everything needed to plan one workspace on one display.
#[derive(Clone, Copy, Debug)]
pub struct LayoutRequest<'a, K> {
    /// The workspace layout.
    pub layout: LayoutType,
    /// Tracked windows in stable workspace order.
    pub windows: &'a [WindowEntry<K>],
    /// The usable area from [`crate::usable_area`], outer gaps already applied.
    pub usable: Rect,
    /// Gaps; only the inner values are used here.
    pub gaps: Gaps,
    /// Master share in `0.0..=1.0` (see [`master_ratio`]).
    pub master_ratio: f64,
    /// Master position.
    pub master_position: MasterPosition,
    /// Runtime ratios: cumulative for the split layouts, per-node for Dwindle,
    /// layout-specific for Grid.
    pub ratios: &'a [f64],
}

/// The effective master ratio: a user override, else the configured percentage.
#[must_use]
pub fn master_ratio(config: &MasterConfig, user_override: Option<f64>) -> f64 {
    user_override.unwrap_or_else(|| f64::from(config.ratio) / 100.0)
}

/// Plans a workspace.
///
/// Every window of `request.windows` appears exactly once in the result. If any
/// input is non-finite, the usable area has no positive area or a computed
/// frame degenerates, the plan is [`PlanStatus::Infeasible`], holds no frames
/// and lists the eligible windows as untouched.
#[must_use]
pub fn plan_layout<K: Copy + PartialEq>(request: &LayoutRequest<'_, K>) -> LayoutPlan<K> {
    let mut plan = LayoutPlan {
        status: PlanStatus::Ready,
        frames: Vec::new(),
        overflow: Vec::new(),
        untouched: Vec::new(),
        excluded: Vec::new(),
        diagnostics: Vec::new(),
    };

    let mut eligible: Vec<K> = Vec::with_capacity(request.windows.len());
    for entry in request.windows {
        let flags = entry.flags;
        if flags.explicit_floating {
            plan.untouched.push((entry.id, UntouchedReason::ExplicitFloating));
        } else if flags.inactive_tab {
            plan.excluded.push((entry.id, Exclusion::InactiveTab));
        } else if flags.fullscreen {
            plan.excluded.push((entry.id, Exclusion::Fullscreen));
        } else if flags.minimized {
            plan.excluded.push((entry.id, Exclusion::Minimized));
        } else if flags.hidden {
            plan.excluded.push((entry.id, Exclusion::Hidden));
        } else {
            eligible.push(entry.id);
        }
    }

    if request.layout == LayoutType::Floating {
        plan.untouched
            .extend(eligible.into_iter().map(|id| (id, UntouchedReason::FloatingLayout)));
        return plan;
    }

    let (tiled_ids, overflow_ids) = if request.layout == LayoutType::Grid {
        let split_at = eligible.len().min(MAX_GRID_TILES);
        let overflow = eligible.split_off(split_at);
        (eligible, overflow)
    } else {
        (eligible, Vec::new())
    };

    match compute_frames(request, &tiled_ids) {
        Ok(frames) => plan.frames = frames,
        Err(error) => {
            plan.status = PlanStatus::Infeasible(error);
            plan.untouched
                .extend(tiled_ids.iter().map(|id| (*id, UntouchedReason::Infeasible)));
        }
    }
    // Overflow is reported whether or not geometry was feasible: the cap is
    // independent of frame math and these windows never receive a frame.
    if !overflow_ids.is_empty() {
        plan.diagnostics.push(LayoutDiagnostic::GridCapacityExceeded {
            tiled: tiled_ids.len(),
            overflow: overflow_ids.len(),
        });
    }
    plan.overflow = overflow_ids
        .into_iter()
        .enumerate()
        .map(|(offset, id)| GridOverflow {
            id,
            eligible_index: MAX_GRID_TILES + offset,
        })
        .collect();
    plan
}

fn compute_frames<K: Copy>(
    request: &LayoutRequest<'_, K>,
    ids: &[K],
) -> Result<Vec<(K, Rect)>, GeometryError> {
    if ids.is_empty() {
        return Ok(Vec::new());
    }
    if !request.usable.is_finite() {
        return Err(GeometryError::NonFinite { what: "usable area" });
    }
    request.gaps.validate()?;
    if !request.usable.has_area() {
        return Err(GeometryError::NoUsableArea);
    }
    let needs_master = request.layout == LayoutType::Master;
    if needs_master && !request.master_ratio.is_finite() {
        return Err(GeometryError::NonFinite { what: "master ratio" });
    }
    let per_node_ratios = matches!(request.layout, LayoutType::Dwindle | LayoutType::Grid);
    if per_node_ratios && request.ratios.iter().any(|ratio| !ratio.is_finite()) {
        return Err(GeometryError::NonFinite { what: "split ratios" });
    }

    let (usable, gaps) = (&request.usable, &request.gaps);
    let frames = match request.layout {
        LayoutType::Floating => Vec::new(),
        LayoutType::Monocle => monocle::layout(ids, usable),
        LayoutType::Dwindle => dwindle::layout(ids, usable, gaps, request.ratios),
        LayoutType::Split | LayoutType::SplitVertical | LayoutType::SplitHorizontal => {
            split::layout(request.layout, ids, usable, gaps, request.ratios)
        }
        LayoutType::Master => {
            master::layout(ids, usable, request.master_ratio, gaps, request.master_position)
        }
        LayoutType::Grid => grid::layout(ids, usable, gaps, request.ratios),
    };
    if frames.iter().all(|(_, frame)| frame.has_area()) {
        Ok(frames)
    } else {
        Err(GeometryError::DegenerateFrame)
    }
}
