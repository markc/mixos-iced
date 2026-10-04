use smithay::utils::{Logical, Point};
use std::collections::HashMap;

#[derive(Clone)]
pub struct Transform {
    // Right now, no concept of layers, and no concepts of collection of layers.
    // that is why, zones exist on the camera state.
    pub position: smithay::utils::Point<f64, smithay::utils::Logical>,
    pub zoom: f64,
}

impl Transform {
    pub fn zoom(&self) -> &f64 {
        return &self.zoom;
    }
    pub fn position(&self) -> &Point<f64, Logical> {
        return &self.position;
    }
}

impl Default for Transform {
    fn default() -> Self {
        Self {
            zoom: 1.0,
            position: (0.0, 0.0).into(),
        }
    }
}