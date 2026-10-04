use slots::storage::token::base::{Token, TokenMut};
use smithay::reexports::calloop::RegistrationToken;

/// Resume/vblank driver data (TTY-switch + resume timing): the "vblank seen
/// since resume" flag and the resume-watchdog timer live in kernel/driver
/// storage by token, not as Orchestrator fields.
pub static VBLANK_SEEN: Token<bool> = Token::new();
pub static VBLANK_SEEN_MUT: TokenMut<bool> = TokenMut::new(&VBLANK_SEEN);
pub static RESUME_WATCHDOG: Token<Option<RegistrationToken>> = Token::new();
pub static RESUME_WATCHDOG_MUT: TokenMut<Option<RegistrationToken>> = TokenMut::new(&RESUME_WATCHDOG);
