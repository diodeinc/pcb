/// Location in 2D Cartesian coordinates
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Location {
    pub x: f64,
    pub y: f64,
}

/// Transformation characteristics (rotation, mirror, scale, offset)
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Xform {
    /// X offset (default: 0.0)
    pub x_offset: f64,
    /// Y offset (default: 0.0)
    pub y_offset: f64,
    /// Rotation in degrees, counter-clockwise (default: 0.0)
    pub rotation: f64,
    /// Mirror across y-axis (default: false)
    pub mirror: bool,
    /// Scale factor (default: 1.0)
    pub scale: f64,
}

impl Default for Xform {
    fn default() -> Self {
        Self {
            x_offset: 0.0,
            y_offset: 0.0,
            rotation: 0.0,
            mirror: false,
            scale: 1.0,
        }
    }
}
