//! Stand-in traits, types, and mocks shared across `tg-app-*` crates.

pub mod backend;
pub mod settings;
pub mod strings;
pub mod theme;
pub mod types;

pub use backend::{Backend, MockBackend};
pub use settings::{SavedWindowColumns, ShellSettings, WindowPosition};
pub use strings::{EnglishStrings, Strings, TgLangStrings};
pub use theme::{Palette, ThemeMode};
pub use types::{
    AuthUser, CodeSentInfo, CodeVerifyOutcome, ConnectionStatus, DialogEntry, FilterId,
    FolderFilter, FullMsgId, MessageItem, MsgId, PeerId, PeerKind, PeerSummary,
};
