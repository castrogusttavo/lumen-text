//! Modal with two pages: SETTINGS (searchable list of config keys with toggles, choosers,
//! steppers and text fields) and KEYBINDINGS (filterable command table with key-cap chips and a
//! record mode with conflict detection).
//!
//! Everything here is plain state + drawing: input methods return an [`Event`] that the app
//! applies (write config.json / keybindings.json, reload the keymap, ...). Nothing touches `App`.
//!
//! Hit-testing uses the rectangles recorded by the last `draw`, so `mouse_down` is only
//! meaningful after a frame was drawn (hover is not tracked).
#![allow(dead_code)]

use crate::{
    field::TextField,
    keymap,
    theme::{Rgba, Theme},
    ui::{Canvas, Rect, TextCtx},
};
use serde_json::{Map, Value, json};
use std::collections::{BTreeMap, HashMap};
use winit::keyboard::{Key, NamedKey};

mod draw;
mod keybinds;
mod modal;
mod model;
mod settings_page;
#[cfg(test)]
mod tests;

use draw::*;
pub use keybinds::*;
pub use modal::*;
pub use model::*;
pub use settings_page::*;

// ───────────────────────────── events ─────────────────────────────

/// What the app must do after an input call.
#[derive(Debug, Clone, PartialEq)]
pub enum Event {
    None,
    /// Close the modal.
    Close,
    /// A setting changed: store `value` under `key` in config.json and apply it.
    Setting {
        key: String,
        value: Value,
    },
    /// Set `keybindings.json[combo] = command_id` (`command_id == "none"` disables a default).
    Bind {
        combo: String,
        command_id: String,
    },
    /// Remove `combo` from keybindings.json (the built-in default, if any, applies again).
    Unbind {
        combo: String,
    },
    /// Clear keybindings.json entirely (every shortcut back to its default).
    ResetAll,
    /// Several events produced by one action, to be applied in order.
    Batch(Vec<Event>),
}

impl Event {
    /// Flatten `Batch` recursively into a plain list (never contains `None` or `Batch`).
    pub fn flatten(self) -> Vec<Event> {
        match self {
            Event::None => vec![],
            Event::Batch(v) => v.into_iter().flat_map(Event::flatten).collect(),
            e => vec![e],
        }
    }

    fn batch(mut v: Vec<Event>) -> Event {
        match v.len() {
            0 => Event::None,
            1 => v.remove(0),
            _ => Event::Batch(v),
        }
    }
}
