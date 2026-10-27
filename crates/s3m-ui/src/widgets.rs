//! Small reusable widgets.

use egui::{Align2, Color32, CornerRadius, FontId, Response, RichText, Sense, Ui, Vec2};
use egui_phosphor::regular as icon;

use crate::theme;

/// Ribbon-style toolbar button: icon above a short label.
pub fn toolbar_button(
    ui: &mut Ui,
    glyph: &str,
    label: &str,
    enabled: bool,
    tooltip: &str,
) -> Response {
    let size = Vec2::new(64.0, 46.0);
    let sense = if enabled {
        Sense::click()
    } else {
        Sense::hover()
    };
    let (rect, response) = ui.allocate_exact_size(size, sense);
    let p = theme::palette(ui);
    let visuals = ui.visuals().clone();
    let painter = ui.painter();
    if enabled && response.hovered() {
        let fill = if response.is_pointer_button_down_on() {
            visuals.widgets.active.weak_bg_fill
        } else {
            visuals.widgets.hovered.weak_bg_fill
        };
        painter.rect_filled(rect, CornerRadius::same(4), fill);
    }
    let fg = if enabled { p.text } else { p.faint };
    painter.text(
        rect.center_top() + Vec2::new(0.0, 15.0),
        Align2::CENTER_CENTER,
        glyph,
        FontId::proportional(20.0),
        if enabled { p.accent } else { fg },
    );
    painter.text(
        rect.center_bottom() - Vec2::new(0.0, 10.0),
        Align2::CENTER_CENTER,
        label,
        FontId::proportional(11.5),
        fg,
    );
    if tooltip.is_empty() {
        response
    } else {
        response.on_hover_text(tooltip)
    }
}

/// Thin vertical separator for toolbars.
pub fn toolbar_separator(ui: &mut Ui) {
    let p = theme::palette(ui);
    let (rect, _) = ui.allocate_exact_size(Vec2::new(9.0, 40.0), Sense::hover());
    ui.painter().vline(
        rect.center().x,
        rect.y_range(),
        egui::Stroke::new(1.0, p.border),
    );
}

/// Icon and colour for a file list entry.
pub fn entry_icon(name: &str, is_folder: bool, p: theme::Palette) -> (&'static str, Color32) {
    if is_folder {
        return (icon::FOLDER, p.folder);
    }
    let ext = name
        .rsplit_once('.')
        .map(|(_, e)| e.to_ascii_lowercase())
        .unwrap_or_default();
    let glyph = match ext.as_str() {
        "png" | "jpg" | "jpeg" | "gif" | "bmp" | "webp" | "tif" | "tiff" | "ico" | "heic" => {
            icon::FILE_IMAGE
        }
        "svg" => icon::FILE_SVG,
        "pdf" => icon::FILE_PDF,
        "zip" | "7z" | "rar" | "gz" | "tgz" | "bz2" | "xz" | "tar" => icon::FILE_ZIP,
        "doc" | "docx" | "odt" | "rtf" => icon::FILE_DOC,
        "xls" | "xlsx" | "ods" => icon::FILE_XLS,
        "ppt" | "pptx" | "odp" => icon::FILE_PPT,
        "csv" | "tsv" => icon::FILE_CSV,
        "sql" => icon::FILE_SQL,
        "txt" | "log" | "md" | "ini" | "cfg" | "conf" => icon::FILE_TEXT,
        "json" | "xml" | "yaml" | "yml" | "toml" | "html" | "htm" | "css" | "js" | "ts" | "rs"
        | "py" | "cs" | "go" | "java" | "php" | "sh" | "ps1" => icon::FILE_CODE,
        "mp4" | "mkv" | "mov" | "avi" | "webm" | "wmv" => icon::FILE_VIDEO,
        "mp3" | "wav" | "flac" | "ogg" | "m4a" | "aac" => icon::FILE_AUDIO,
        _ => icon::FILE,
    };
    (glyph, p.dim)
}

/// Muted helper text.
pub fn muted(ui: &mut Ui, text: impl Into<String>) -> Response {
    let p = theme::palette(ui);
    ui.label(RichText::new(text.into()).color(p.dim).size(12.0))
}

/// Section title (bold, slightly larger).
pub fn section_title(ui: &mut Ui, text: &str) -> Response {
    ui.label(RichText::new(text).strong().size(13.5))
}

/// Full-width, left-aligned selectable list row with an icon (bucket list).
pub fn list_row(
    ui: &mut Ui,
    selected: bool,
    glyph: &str,
    glyph_color: Color32,
    text: &str,
    width: f32,
) -> Response {
    let (rect, response) = ui.allocate_exact_size(Vec2::new(width, 22.0), Sense::click());
    let visuals = ui.visuals().clone();
    let p = theme::palette(ui);
    let painter = ui.painter_at(rect);
    if selected {
        painter.rect_filled(rect, CornerRadius::same(3), visuals.selection.bg_fill);
    } else if response.hovered() {
        painter.rect_filled(
            rect,
            CornerRadius::same(3),
            visuals.widgets.hovered.weak_bg_fill,
        );
    }
    let y = rect.center().y;
    painter.text(
        egui::pos2(rect.left() + 6.0, y),
        Align2::LEFT_CENTER,
        glyph,
        FontId::proportional(15.0),
        glyph_color,
    );
    painter.text(
        egui::pos2(rect.left() + 28.0, y),
        Align2::LEFT_CENTER,
        text,
        FontId::proportional(13.5),
        p.text,
    );
    response
}
