use crate::input::layer::base::InputLayer;

/// Registration-order input routing with priority layers. The bus only owns
/// the ORDER; the world drives the actual traversal (it holds the systems and
/// stops on `InputFlow::Consume`). Dispatch is synchronous — "bus" refers to
/// the decoupled registration shape, not to deferral.
#[derive(Default)]
pub struct InputBus {
    entries: Vec<Entry>,
    sorted: bool,
}

struct Entry {
    layer: InputLayer,
    seq: usize,
    system_index: usize,
}

impl InputBus {
    pub fn new() -> Self {
        Self::default()
    }

    /// Register a system (by its index in the world) at a priority layer.
    pub fn register(&mut self, layer: InputLayer, system_index: usize) {
        let seq = self.entries.len();
        self.entries.push(Entry { layer, seq, system_index });
        self.sorted = false;
    }

    /// System indices in traversal order: layer descending, then registration
    /// order ascending within a layer.
    pub fn order(&mut self) -> impl Iterator<Item = usize> + '_ {
        if !self.sorted {
            self.entries.sort_by(|a, b| b.layer.cmp(&a.layer).then(a.seq.cmp(&b.seq)));
            self.sorted = true;
        }
        self.entries.iter().map(|e| e.system_index)
    }
}
