use bitflags::bitflags;

bitflags! {
    #[derive(Clone, Copy, Debug, PartialEq, Eq)]
    pub struct Layer: u64 {
        const SCENE       = 1 << 0;
        const LOCK_SCENE               = 1 << 1; // <-- Lock scene is not rendering scene and vice-verse
        const GLOBAL_SCREEN               = 1 << 2; // <-- Playback controls, etc.
        const SCENE_SURFACE_GROUP       = 1 << 3; // <-- Gropu surface
        const CAPTURE_DIM               = 1 << 4; // <-- Capture region dim, drawn BELOW windows
        const CAPTURE_PASSTHROUGH       = 1 << 5; // <-- Hit-test-transparent (capture border/dim); pointer falls through to windows
        const PICKER_SCENE              = 1 << 6; // <-- World-selection screen (own overlay world, like LOCK_SCENE)
    }
}
