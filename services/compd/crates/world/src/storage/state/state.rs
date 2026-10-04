use std::collections::HashMap;
use slots::storage::token::base::Token;

/// KernelData token mirroring [`Storage::nested`], so input/draw SYSTEMS (which see
/// only `cx.kernel`, not the Orchestrator) can read whether the backend is nested
/// winit. Inserted once at init by the Orchestrator.
pub static NESTED: Token<bool> = Token::new();

pub struct Storage {
    pub nested: bool,
    // pub config: Config,
}
impl Storage  {
    pub fn new(nested: bool) -> Self {
        return Self {
            nested
        }
    }
}

