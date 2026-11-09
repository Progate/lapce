use serde::{Deserialize, Serialize};
use structdesc::FieldNames;

#[derive(FieldNames, Debug, Clone, Deserialize, Serialize, Default)]
#[serde(rename_all = "kebab-case")]
pub struct CoreConfig {
    #[field_names(desc = "Enable modal editing (Vim like)")]
    pub modal: bool,
    #[field_names(
        desc = "The language of Lapce's menus and panels. \"system\" follows the language of the system (the LANG environment variable). Takes effect the next time Lapce starts"
    )]
    pub language: String,
    #[field_names(desc = "Set the color theme of Lapce")]
    pub color_theme: String,
    #[field_names(desc = "Set the icon theme of Lapce")]
    pub icon_theme: String,
    #[field_names(
        desc = "Enable customised titlebar and disable OS native one (Linux, BSD, Windows)"
    )]
    pub custom_titlebar: bool,
    #[field_names(
        desc = "Only allow double-click to open files in the file explorer"
    )]
    pub file_explorer_double_click: bool,
    #[field_names(
        desc = "Enable auto-reload for the plugin when its configuration changes."
    )]
    pub auto_reload_plugin: bool,
}
