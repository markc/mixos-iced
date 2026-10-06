// SPDX-License-Identifier: MIT OR Apache-2.0
//! Canonical location editor actions for apps with an editable path bar.

use crate::ActionId;

/// Focus and select a pane's location text (requires an available window).
pub const FOCUS: ActionId = ActionId::from_static("location.focus");
