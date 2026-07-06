//! All text handling lives here: the bitmap font renderer, word wrapping, the
//! keyboard/key-event plumbing, the editing state machine, and the generic
//! [`TextField`]/[`TextLayout`] widget that editable inputs and wrapped text
//! blocks across cozyui share.

pub mod center;
pub mod edit;
pub mod field;
pub mod input;

pub use center::{draw_text_centered, draw_text_centered_tight};
pub use edit::TextEditOutcome;
pub use field::TextField;
pub use input::{EditKey, KeyInput, edit_key};
pub use pixel_fonts::{BitmapFont, LinePlacement, TextLayout};
