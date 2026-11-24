//! Stand-in traits, types, and mocks for crates being implemented in parallel.

pub use tg_app_window::mock_session;

pub use mock_session::{
    MockSession, PendingChatOpen, SharedSessionHandle, session_folders, session_peers,
};
pub use tg_app_core::stand_in::{
    AuthUser, Backend, CodeSentInfo, CodeVerifyOutcome, ConnectionStatus, DialogEntry,
    EnglishStrings, FilterId, FolderFilter, FullMsgId, MessageItem, MockBackend, MsgId, Palette,
    PeerId, PeerKind, PeerSummary, SavedWindowColumns, ShellSettings, Strings, TgLangStrings,
    ThemeMode, WindowPosition,
};
pub use tg_app_core::stand_in::{backend, settings, strings, theme, types};
