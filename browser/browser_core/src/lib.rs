mod navigation;
mod tab;

pub use browser_studio::{Accent, Settings, ToolbarPosition};
pub use navigation::{Example, History, Location, Visit, resolve_link};
pub use tab::{Tab, TabId, TabManager};
