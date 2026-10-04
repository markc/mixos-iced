use std::collections::HashMap;
use uuid::Uuid;

#[derive(Default)]
pub struct CameraZone {
    pub zone: HashMap<String, Zone>,
}


pub struct Zone {
    pub specifier: ZoneSpecifier
}


pub enum ZoneSpecifier {
    Element {
        UUID: Vec<Uuid>
    },
    Camera{
        position: (f64, f64),
        zoom: f64,
    }
}