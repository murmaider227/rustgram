//! Widget components ported from `Telegram/lib_ui/ui/widgets/`.

pub mod buttons;
pub mod checkbox;
pub mod input_field;
pub mod labels;
pub mod menu;
pub mod rich_input;
pub mod scroll_area;
pub mod sliders;
pub mod tooltip;

pub use buttons::{FlatButton, IconButton, IconKind, LinkButton, RoundButton, paint_icon};
pub use checkbox::{
    Checkbox, Radio, Toggle, paint_check_view, paint_radio_view, paint_toggle_view,
};
pub use input_field::{InputField, InputFieldMode, estimate_wrapped_lines};
pub use labels::{FlatLabel, divider_bar};
pub use menu::{MenuItem, PopupMenuState};
pub use rich_input::{
    ActiveFormatFlags, EditLinkDialogAction, EditLinkDialogState, FormatAction,
    FormattingContextMenu, FormattingMenuEvent, RichEditState, RichInputPalette,
    build_rich_edit_layout_job, detect_format_shortcut, normalize_link_url, show_rich_text_edit,
};
pub use scroll_area::{TgScrollArea, compute_scroll_to, compute_scrollbar_thumb};
pub use sliders::{FilledSlider, MediaSlider, SettingsSlider};
pub use tooltip::{ImportantTooltipState, TooltipSide, paint_tooltip_at, show_tooltip_on_hover};
