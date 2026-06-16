//! All text handling lives here: the bitmap font renderer, word wrapping, the
//! keyboard/key-event plumbing, the editing state machine, and the generic
//! [`TextField`]/[`TextLayout`] widget that editable inputs and wrapped text
//! blocks across cozyui share.

pub mod edit;
pub mod field;
pub mod font;
pub mod input;
mod wrap;

pub use edit::TextEditOutcome;
pub use field::{LinePlacement, TextField, TextLayout};
pub use font::{BitmapFont, FontSpec};
pub use input::{EditKey, KeyInput, edit_key};
