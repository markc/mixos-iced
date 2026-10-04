// Where persisted storage lives on disk. This is runtime STATE, not user config,
// so it goes under $XDG_STATE_HOME (~/.local/state) per the XDG basedir spec —
// separate from settings.json under ~/.config. Dependency-free; never panics
// (absence is normal first-run, handled by the engine).
pub mod base;
