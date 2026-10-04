//! The host's mount reservations: edge pages by page id, and the one dialog
//! seat.
//!
//! Quoin keeps these beside its panel carousel; compd has no carousel, so
//! the registry here is the whole page namespace and the renderer places
//! what it holds.

use std::collections::BTreeMap;
use std::fmt::{Display, Formatter};

/// The screen edge an edge scene mounts on.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Hash, Ord, PartialOrd)]
pub enum Edge {
    Left,
    Right,
    Top,
    Bottom,
}

impl Edge {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Left => "left",
            Self::Right => "right",
            Self::Top => "top",
            Self::Bottom => "bottom",
        }
    }
}

/// Where one page id lives and which citizen owns it.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct PageSeat {
    /// Output the page maps on.
    pub output: String,
    pub edge: Edge,
    /// Broker-attested sender (qualified for remote callers), never authored
    /// scene metadata.
    pub owner: String,
    /// Host receipt sequence at acceptance.
    pub accepted_at: u64,
}

/// A page id is already live (on any output, edge or owner), or empty.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum PageError {
    EmptyName,
    Duplicate(String),
}

impl Display for PageError {
    fn fmt(&self, formatter: &mut Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::EmptyName => formatter.write_str("sub-panel name must not be empty"),
            Self::Duplicate(name) => write!(formatter, "sub-panel name '{name}' is already registered"),
        }
    }
}

/// Live page ids with their seats.
#[derive(Clone, Debug, Default)]
pub struct PageRegistry {
    seats: BTreeMap<String, PageSeat>,
}

impl PageRegistry {
    pub fn seat(&self, name: &str) -> Option<&PageSeat> {
        self.seats.get(name)
    }

    pub fn iter(&self) -> impl Iterator<Item = (&str, &PageSeat)> {
        self.seats.iter().map(|(name, seat)| (name.as_str(), seat))
    }

    /// Reserve a mount before accepting its content. Only the same owner on
    /// the same output and edge may update a live name; conflicts change
    /// nothing.
    pub fn mount(
        &mut self,
        name: &str,
        output: &str,
        edge: Edge,
        owner: &str,
        accepted_at: u64,
    ) -> Result<(), PageError> {
        if name.trim().is_empty() {
            return Err(PageError::EmptyName);
        }
        if let Some(seat) = self.seats.get_mut(name) {
            if seat.output != output || seat.edge != edge || seat.owner != owner {
                return Err(PageError::Duplicate(name.to_owned()));
            }
            seat.accepted_at = accepted_at;
            return Ok(());
        }
        self.seats.insert(
            name.to_owned(),
            PageSeat { output: output.to_owned(), edge, owner: owner.to_owned(), accepted_at },
        );
        Ok(())
    }

    /// Drop a page's seat.
    pub fn forget(&mut self, name: &str) {
        self.seats.remove(name);
    }
}

/// The live dialog: which scene holds the seat, whose it is, and how big.
#[derive(Clone, Debug, PartialEq)]
pub struct DialogSeat {
    pub scene: String,
    pub owner: String,
    pub accepted_at: u64,
    /// Output the dialog maps on.
    pub output: String,
    /// Authored logical size, 240..=2048.
    pub w: f32,
    pub h: f32,
    pub title: Option<String>,
    /// Frame chrome: title bar and close button. Defaults to true.
    pub chrome: bool,
}

#[derive(Clone, Debug, PartialEq)]
pub enum DialogError {
    EmptyName,
    /// Another scene holds the seat; the load did not ask to pre-empt.
    Busy { scene: String, owner: String },
    /// Release named a scene that does not hold the seat.
    Unknown(String),
}

impl Display for DialogError {
    fn fmt(&self, formatter: &mut Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::EmptyName => formatter.write_str("dialog scene name must not be empty"),
            Self::Busy { scene, owner } => write!(formatter, "the dialog seat is held by {scene} ({owner})"),
            Self::Unknown(scene) => write!(formatter, "{scene} does not hold the dialog seat"),
        }
    }
}

/// At most one dialog per host.
#[derive(Clone, Debug, Default)]
pub struct DialogSlot {
    seat: Option<DialogSeat>,
}

impl DialogSlot {
    pub fn seat(&self) -> Option<&DialogSeat> {
        self.seat.as_ref()
    }

    /// Reserve or update the seat. The same scene and owner may reload;
    /// any other holder refuses `Busy`.
    pub fn register(&mut self, seat: DialogSeat) -> Result<(), DialogError> {
        if seat.scene.trim().is_empty() {
            return Err(DialogError::EmptyName);
        }
        if let Some(held) = &self.seat
            && (held.scene != seat.scene || held.owner != seat.owner)
        {
            return Err(DialogError::Busy { scene: held.scene.clone(), owner: held.owner.clone() });
        }
        self.seat = Some(seat);
        Ok(())
    }

    /// Free the seat held by `scene`.
    pub fn release(&mut self, scene: &str) -> Result<DialogSeat, DialogError> {
        self.seat
            .take_if(|held| held.scene == scene)
            .ok_or_else(|| DialogError::Unknown(scene.to_owned()))
    }

    /// Take the seat whoever holds it. Returns the displaced seat when a
    /// different scene or owner held it; `None` when it was free or already
    /// this scene's.
    pub fn preempt(&mut self, seat: DialogSeat) -> Result<Option<DialogSeat>, DialogError> {
        if seat.scene.trim().is_empty() {
            return Err(DialogError::EmptyName);
        }
        let displaced = self
            .seat
            .take()
            .filter(|held| held.scene != seat.scene || held.owner != seat.owner);
        self.seat = Some(seat);
        Ok(displaced)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn seat(scene: &str, owner: &str) -> DialogSeat {
        DialogSeat {
            scene: scene.into(),
            owner: owner.into(),
            accepted_at: 1,
            output: "DP-1".into(),
            w: 880.0,
            h: 620.0,
            title: None,
            chrome: true,
        }
    }

    #[test]
    fn page_mount_refuses_cross_output_edge_and_owner_collisions_atomically() {
        let mut registry = PageRegistry::default();
        registry.mount("page", "DP-1", Edge::Right, "a", 1).unwrap();
        for (output, edge, owner) in [("DP-2", Edge::Right, "a"), ("DP-1", Edge::Left, "a"), ("DP-1", Edge::Right, "b")] {
            assert_eq!(registry.mount("page", output, edge, owner, 9), Err(PageError::Duplicate("page".into())));
            assert_eq!(registry.seat("page").unwrap().accepted_at, 1);
        }
        registry.mount("page", "DP-1", Edge::Right, "a", 2).unwrap();
        assert_eq!(registry.seat("page").unwrap().accepted_at, 2);
        assert_eq!(registry.mount(" ", "DP-1", Edge::Right, "a", 1), Err(PageError::EmptyName));
    }

    #[test]
    fn dialog_slot_busy_release_and_preempt() {
        let mut slot = DialogSlot::default();
        slot.register(seat("editor", "scenes")).unwrap();
        slot.register(seat("editor", "scenes")).unwrap();
        assert_eq!(
            slot.register(seat("other", "scenes")),
            Err(DialogError::Busy { scene: "editor".into(), owner: "scenes".into() })
        );
        assert_eq!(slot.release("other"), Err(DialogError::Unknown("other".into())));
        assert_eq!(slot.preempt(seat("other", "x")).unwrap().unwrap().scene, "editor");
        assert_eq!(slot.preempt(seat("other", "x")).unwrap(), None);
        assert_eq!(slot.release("other").unwrap().owner, "x");
        assert!(slot.seat().is_none());
    }
}
