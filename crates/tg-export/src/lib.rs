//! Telegram account and chat data export ("Export Telegram data").
//!
//! Port of `Telegram/SourceFiles/export/` including:
//! - [`settings`]: export flags, media filters, validation, and Qt account-settings serialization.
//! - [`data`]: plain Rust export data model, entity/text/date/size formatting, and thumbnail helpers.
//! - [`output`]: byte-compatible JSON writer (`result.json`), HTML writer (`export_results.html`,
//!   `lists/*.html`, `chats/chat_*/messages*.html`, embedded GPL static assets), combined
//!   HTML+JSON writer, incremental file writer, and stats counters.
//! - [`controller`]: step-planning state machine and async [`controller::ExportSource`] trait with
//!   an in-memory fake source for testing.

pub mod api_wrap;
pub mod controller;
pub mod data;
pub mod output;
pub mod settings;
pub mod stand_in;
pub mod tl_parse;

pub use api_wrap::{
    CUSTOM_EMOJI_BATCH_LIMIT, DIALOGS_SLICE_LIMIT, ExportRpc, FILE_CHUNK_SIZE,
    MESSAGES_SLICE_LIMIT, PROFILE_MUSIC_SLICE_LIMIT, RpcCallError, STORIES_SLICE_LIMIT,
    TOP_PEERS_SLICE_LIMIT, TlExportSource, USERPICS_SLICE_LIMIT,
};

pub use controller::{
    ApiError, DownloadProgress, EntityType, ExportControlHandle, ExportController, ExportSource,
    ExportState, FinishedState, InMemoryExportSource, PasswordCheckState, ProcessingState,
    StartInfo, Step, StreamEvent, compute_substeps, normalize_settings, plan_export_steps,
};
pub use output::{AbstractWriter, OutputFormat, create_writer, normalize_path};
pub use settings::{Environment, ExportTypes, MediaSettings, MediaTypes, Settings};
