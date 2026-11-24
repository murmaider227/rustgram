//! `tg-app-core`: Layer-0 shared traits, DTOs, stand-in primitives, and `AppSession`
//! contract shared across all `tg-app-*` crates.

pub mod clock;
pub mod core_types;
pub mod session_api;
pub mod stand_in;
pub mod ui_icons;

pub mod bots {
    pub mod backend;
}
pub mod business {
    pub mod backend;
    pub mod model;
}
pub mod calls {
    pub mod backend;
}
pub mod chat_helpers {
    pub mod backend;
}
pub mod dialogs {
    pub mod backend;
}
pub mod history {
    pub mod backend;
    pub mod sections_model;
    pub mod theme;
}
pub mod info {
    pub mod backend;
    pub mod values;
}
pub mod media {
    pub mod backend;
}
pub mod notifications {
    pub mod backend;
    pub mod settings;
}
pub mod payments {
    pub mod backend;
    pub mod marketplace_backend;
    pub mod verification;
}
pub mod settings {
    pub mod backend;
}
pub mod statistics {
    pub mod backend;
}
pub mod stories {
    pub mod backend;
}
pub mod themes {
    pub mod backend;
}
pub mod wallet {
    pub mod backend;
}
pub mod webview {
    pub mod backend;
}
