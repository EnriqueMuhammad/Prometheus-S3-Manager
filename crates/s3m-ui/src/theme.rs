//! Light and dark themes: palettes, typography, spacing and widget visuals.
//!
//! Text colours meet WCAG AA (4.5:1) on panels and lists in both themes, secondary text reaches
//! about 7:1, and borders are strong enough to separate panels without a glaring white or a
//! pure black background.

use egui::{
    Color32, CornerRadius, FontFamily, FontId, Margin, Stroke, TextStyle, Theme, ThemePreference,
    Vec2,
};

/// Colours used by custom-painted parts of the UI.
#[derive(Debug, Clone, Copy)]
pub struct Palette {
    /// Panel background.
    pub panel: Color32,
    /// List / table background.
    pub list: Color32,
    /// Raised surface (inputs, inactive buttons).
    pub surface: Color32,
    /// Hovered surface.
    pub surface_hover: Color32,
    /// Pressed surface.
    pub surface_active: Color32,
    /// Hairline borders.
    pub border: Color32,
    /// Primary text.
    pub text: Color32,
    /// Emphasised text (hovered widgets).
    pub text_strong: Color32,
    /// Secondary text.
    pub dim: Color32,
    /// Placeholder text.
    pub faint: Color32,
    /// Accent (icons, links).
    pub accent: Color32,
    /// Fill of primary buttons, readable under white text.
    pub accent_fill: Color32,
    /// Selected rows and text selection.
    pub selection: Color32,
    /// Folder icon.
    pub folder: Color32,
    /// Success.
    pub success: Color32,
    /// Warning.
    pub warning: Color32,
    /// Error.
    pub danger: Color32,
    /// Fill of destructive buttons, readable under white text.
    pub danger_fill: Color32,
    /// Banner background for warnings/errors.
    pub banner: Color32,
    /// Striped rows.
    pub faint_bg: Color32,
}

const LIGHT: Palette = Palette {
    panel: Color32::from_rgb(0xEF, 0xF1, 0xF4),
    list: Color32::from_rgb(0xFC, 0xFC, 0xFD),
    surface: Color32::from_rgb(0xE2, 0xE6, 0xEB),
    surface_hover: Color32::from_rgb(0xD6, 0xDC, 0xE4),
    surface_active: Color32::from_rgb(0xC8, 0xD0, 0xDB),
    border: Color32::from_rgb(0xBF, 0xC5, 0xCE),
    text: Color32::from_rgb(0x14, 0x17, 0x1B),
    text_strong: Color32::from_rgb(0x05, 0x06, 0x08),
    dim: Color32::from_rgb(0x47, 0x4F, 0x59),
    faint: Color32::from_rgb(0x64, 0x6C, 0x77),
    accent: Color32::from_rgb(0x0A, 0x58, 0xBF),
    accent_fill: Color32::from_rgb(0x0A, 0x58, 0xBF),
    selection: Color32::from_rgb(0xC4, 0xDC, 0xF7),
    folder: Color32::from_rgb(0xB9, 0x81, 0x0A),
    success: Color32::from_rgb(0x15, 0x73, 0x3A),
    warning: Color32::from_rgb(0x8A, 0x56, 0x00),
    danger: Color32::from_rgb(0xB3, 0x20, 0x1D),
    danger_fill: Color32::from_rgb(0xB3, 0x20, 0x1D),
    banner: Color32::from_rgb(0xFF, 0xF0, 0xD4),
    faint_bg: Color32::from_rgb(0xF2, 0xF4, 0xF7),
};

const DARK: Palette = Palette {
    panel: Color32::from_rgb(0x1B, 0x1E, 0x24),
    list: Color32::from_rgb(0x14, 0x16, 0x1B),
    surface: Color32::from_rgb(0x27, 0x2B, 0x33),
    surface_hover: Color32::from_rgb(0x31, 0x36, 0x40),
    surface_active: Color32::from_rgb(0x3B, 0x41, 0x4D),
    border: Color32::from_rgb(0x3A, 0x40, 0x4B),
    text: Color32::from_rgb(0xE4, 0xE7, 0xEC),
    text_strong: Color32::from_rgb(0xF6, 0xF8, 0xFA),
    dim: Color32::from_rgb(0xA8, 0xB0, 0xBD),
    faint: Color32::from_rgb(0x82, 0x8B, 0x99),
    accent: Color32::from_rgb(0x62, 0xA8, 0xFF),
    accent_fill: Color32::from_rgb(0x2F, 0x6F, 0xD0),
    selection: Color32::from_rgb(0x22, 0x4A, 0x84),
    folder: Color32::from_rgb(0xE8, 0xB9, 0x49),
    success: Color32::from_rgb(0x4C, 0xC7, 0x64),
    warning: Color32::from_rgb(0xE5, 0xAC, 0x3A),
    danger: Color32::from_rgb(0xFF, 0x6B, 0x63),
    danger_fill: Color32::from_rgb(0xC4, 0x2B, 0x26),
    banner: Color32::from_rgb(0x3B, 0x30, 0x18),
    faint_bg: Color32::from_rgb(0x19, 0x1C, 0x21),
};

/// Palette for the current visuals.
pub fn palette(ui: &egui::Ui) -> Palette {
    if ui.visuals().dark_mode { DARK } else { LIGHT }
}

/// Installs fonts (with Phosphor icons) once.
pub fn install_fonts(ctx: &egui::Context) {
    let mut fonts = egui::FontDefinitions::default();
    egui_phosphor::add_to_fonts(&mut fonts, egui_phosphor::Variant::Regular);
    ctx.set_fonts(fonts);
}

/// Configures both themes and selects the light or dark one.
pub fn apply(ctx: &egui::Context, dark: bool) {
    ctx.style_mut_of(Theme::Dark, |style| configure(style, &DARK, true));
    ctx.style_mut_of(Theme::Light, |style| configure(style, &LIGHT, false));
    ctx.set_theme(if dark {
        ThemePreference::Dark
    } else {
        ThemePreference::Light
    });
}

/// Typography, spacing and widget visuals of one theme.
fn configure(style: &mut egui::Style, p: &Palette, dark: bool) {
    use FontFamily::{Monospace, Proportional};
    style.text_styles = [
        (TextStyle::Small, FontId::new(11.0, Proportional)),
        (TextStyle::Body, FontId::new(13.5, Proportional)),
        (TextStyle::Button, FontId::new(13.5, Proportional)),
        (TextStyle::Heading, FontId::new(18.0, Proportional)),
        (TextStyle::Monospace, FontId::new(12.5, Monospace)),
    ]
    .into();

    let s = &mut style.spacing;
    s.item_spacing = Vec2::new(6.0, 4.0);
    s.button_padding = Vec2::new(8.0, 3.0);
    s.interact_size.y = 22.0;
    s.menu_margin = Margin::same(6);
    s.window_margin = Margin::same(12);

    let v = &mut style.visuals;
    v.dark_mode = dark;
    v.panel_fill = p.panel;
    v.window_fill = p.panel;
    v.window_stroke = Stroke::new(1.0, p.border);
    v.window_corner_radius = CornerRadius::same(8);
    v.menu_corner_radius = CornerRadius::same(6);
    v.extreme_bg_color = p.list;
    v.faint_bg_color = p.faint_bg;
    v.code_bg_color = p.list;
    v.text_edit_bg_color = Some(p.list);
    v.hyperlink_color = p.accent;
    v.warn_fg_color = p.warning;
    v.error_fg_color = p.danger;
    v.selection.bg_fill = p.selection;
    v.selection.stroke = Stroke::new(1.0, p.text_strong);
    v.striped = true;

    let radius = CornerRadius::same(4);
    let w = &mut v.widgets;
    w.noninteractive.bg_fill = p.panel;
    w.noninteractive.weak_bg_fill = p.panel;
    w.noninteractive.bg_stroke = Stroke::new(1.0, p.border);
    w.noninteractive.fg_stroke = Stroke::new(1.0, p.text);
    w.noninteractive.corner_radius = radius;
    for (state, fill) in [
        (&mut w.inactive, p.surface),
        (&mut w.hovered, p.surface_hover),
        (&mut w.active, p.surface_active),
        (&mut w.open, p.surface_hover),
    ] {
        state.bg_fill = fill;
        state.weak_bg_fill = fill;
        state.corner_radius = radius;
        // Widgets keep their size on hover, so rows and toolbars do not twitch.
        state.expansion = 0.0;
    }
    w.inactive.bg_stroke = Stroke::new(1.0, p.border);
    w.inactive.fg_stroke = Stroke::new(1.0, p.text);
    w.hovered.bg_stroke = Stroke::new(1.0, p.border);
    w.hovered.fg_stroke = Stroke::new(1.5, p.text_strong);
    w.active.bg_stroke = Stroke::new(1.0, p.accent);
    w.active.fg_stroke = Stroke::new(1.5, p.text_strong);
    w.open.bg_stroke = Stroke::new(1.0, p.border);
    w.open.fg_stroke = Stroke::new(1.0, p.text_strong);
}
