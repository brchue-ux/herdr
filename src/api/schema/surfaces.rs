use serde::{Deserialize, Serialize};

use super::panes::{PaneGraphicsFormat, PaneGraphicsPlacementParams};

/// A drawable region of the client viewport that is not a pane.
///
/// Panes carry an id, so `pane.graphics.*` addresses them directly. Every other
/// region a client can put an image on is named here instead, so one method
/// family covers all of them and adding a region is an enum variant rather than
/// a new method.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum GraphicsSurface {
    /// The desktop layout's left column, up to and including its divider.
    /// Absent from the mobile layout, where it has zero width and any placement
    /// on it is clipped away.
    Sidebar,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
pub struct SurfaceTarget {
    pub surface: GraphicsSurface,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
pub struct SurfaceGraphicsSetParams {
    pub surface: GraphicsSurface,
    pub format: PaneGraphicsFormat,
    pub image_width: u32,
    pub image_height: u32,
    #[serde(default)]
    pub data_base64: String,
    #[serde(default)]
    pub placement: PaneGraphicsPlacementParams,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
pub struct SurfaceGraphicsClearParams {
    pub surface: GraphicsSurface,
}
