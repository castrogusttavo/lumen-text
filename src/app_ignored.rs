//! "Hide git-ignored files" on the `App`: applying the setting and the palette toggle.

use crate::app::App;

impl App {
    /// Show or hide the entries git ignores in the explorer.
    pub fn set_hide_ignored(&mut self, hide: bool) {
        self.config.explorer_hide_gitignored = hide;
        self.tree.set_hide_ignored(hide);
        self.file_index = None;
        self.request_redraw();
    }

    pub fn toggle_hide_ignored(&mut self) {
        let hide = !self.config.explorer_hide_gitignored;
        self.set_hide_ignored(hide);
        self.config.save();
        self.say(if hide { "Explorer hides ignored files" } else { "Explorer shows ignored files (dimmed)" });
    }
}
