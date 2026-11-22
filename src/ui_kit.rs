//! Design system: colours, typography, spacing and reusable widgets
//! (toolbar buttons, primary/secondary buttons, toggle chips, dialogs, toasts).

use egui::{
    vec2, Align, Color32, Context, CornerRadius, FontFamily, FontId, Frame, Layout, Margin, Response, RichText, Sense,
    Shadow, Stroke, TextStyle, Ui, Visuals,
};

use crate::settings::Theme;

/// Phosphor icon codepoints (font: assets/fonts/Phosphor.ttf, MIT).
#[allow(dead_code)]
pub mod icon {
    pub const FOLDER_OPEN: &str = "\u{E256}";
    pub const FLOPPY_DISK: &str = "\u{E248}";
    pub const UNDO: &str = "\u{E038}";
    pub const REDO: &str = "\u{E036}";
    pub const SEARCH: &str = "\u{E30C}";
    pub const FUNNEL: &str = "\u{E266}";
    pub const FUNNEL_X: &str = "\u{E26C}";
    pub const SORT_ASC: &str = "\u{E444}";
    pub const SORT_DESC: &str = "\u{E446}";
    pub const CHART_BAR: &str = "\u{E150}";
    pub const COMMAND: &str = "\u{E1C4}";
    pub const X: &str = "\u{E4F6}";
    pub const PLUS: &str = "\u{E3D4}";
    pub const FILE_PLUS: &str = "\u{E236}";
    pub const TABLE: &str = "\u{E476}";
    pub const GEAR: &str = "\u{E272}";
    pub const LOCK: &str = "\u{E308}";
    pub const WARNING: &str = "\u{E4E0}";
    pub const CHECK: &str = "\u{E182}";
    pub const INFO: &str = "\u{E2CE}";
    pub const CARET_UP: &str = "\u{E13C}";
    pub const CARET_DOWN: &str = "\u{E136}";
    pub const CASE: &str = "\u{E6EE}";
    pub const REGEX: &str = "\u{E0AA}";
    pub const BRACKETS: &str = "\u{E85E}";
    pub const SELECTION: &str = "\u{E69A}";
    pub const HIGHLIGHT: &str = "\u{E632}";
    pub const RELOAD: &str = "\u{E094}";
    pub const FILE_TEXT: &str = "\u{E23A}";
    pub const CLOCK: &str = "\u{E1A0}";
    pub const SIDEBAR: &str = "\u{EC24}";
    pub const PENCIL: &str = "\u{E3B4}";
    pub const TRASH: &str = "\u{E4A6}";
    pub const FOLDER: &str = "\u{E25A}";
    pub const MAGIC: &str = "\u{E6B6}";
    pub const PRINTER: &str = "\u{E3DC}";
    pub const DOTS: &str = "\u{E1FE}";
    pub const WORD: &str = "\u{E5BE}";
    pub const DOWNLOAD: &str = "\u{E20C}";
    pub const SPARKLE: &str = "\u{E6A2}";
}

/// Brand orange (from the app icon).
pub const BRAND: Color32 = Color32::from_rgb(0xF2, 0x6B, 0x0F);

pub fn icon_font(size: f32) -> FontId {
    FontId::new(size, FontFamily::Name("icons".into()))
}

/// An icon as rich text (uses the icon font family).
pub fn icon_text(icon: &str, size: f32) -> RichText {
    RichText::new(icon).font(icon_font(size))
}

/// Label with a leading icon.
pub fn icon_label(ui: &mut Ui, icon: &str, text: impl Into<RichText>, color: Option<Color32>) -> Response {
    ui.horizontal(|ui| {
        ui.spacing_mut().item_spacing.x = 5.0;
        let mut ic = icon_text(icon, 14.0);
        let mut t: RichText = text.into();
        if let Some(c) = color {
            ic = ic.color(c);
            t = t.color(c);
        }
        ui.label(ic);
        ui.label(t)
    })
    .inner
}

pub fn semibold(size: f32) -> FontId {
    FontId::new(size, FontFamily::Name("semibold".into()))
}

pub fn accent(ui: &Ui) -> Color32 {
    ui.visuals().selection.stroke.color
}

fn hex(s: u32) -> Color32 {
    Color32::from_rgb((s >> 16) as u8, (s >> 8) as u8, s as u8)
}

/// Palette per theme: (dark, canvas, panel, faint, stroke, text, weak, accent, selection).
struct Palette {
    dark: bool,
    canvas: Color32,
    panel: Color32,
    faint: Color32,
    stroke: Color32,
    text: Color32,
    weak: Color32,
    accent: Color32,
    widget: Color32,
}

fn palette(theme: Theme) -> Palette {
    match theme {
        Theme::Light => Palette {
            dark: false,
            canvas: hex(0xFFFFFF),
            panel: hex(0xF4F4F6),
            faint: hex(0xF8F8FA),
            stroke: hex(0xE2E2E7),
            text: hex(0x1D1D1F),
            weak: hex(0x6E6E76),
            accent: BRAND,
            widget: hex(0xE9E9EE),
        },
        Theme::Dark => Palette {
            dark: true,
            canvas: hex(0x141518),
            panel: hex(0x1C1D21),
            faint: hex(0x18191C),
            stroke: hex(0x2C2D33),
            text: hex(0xE7E7EA),
            weak: hex(0x8C8E96),
            accent: hex(0xFF7A21),
            widget: hex(0x2A2B31),
        },
        Theme::Dracula => Palette {
            dark: true,
            canvas: hex(0x22232E),
            panel: hex(0x282A36),
            faint: hex(0x262834),
            stroke: hex(0x383A4A),
            text: hex(0xF8F8F2),
            weak: hex(0x9EA2C0),
            accent: hex(0xBD93F9),
            widget: hex(0x343746),
        },
        Theme::SolarizedDark => Palette {
            dark: true,
            canvas: hex(0x002B36),
            panel: hex(0x073642),
            faint: hex(0x03313D),
            stroke: hex(0x0E4452),
            text: hex(0xC9D4D6),
            weak: hex(0x839496),
            accent: hex(0x2AA198),
            widget: hex(0x0B4250),
        },
        Theme::SolarizedLight => Palette {
            dark: false,
            canvas: hex(0xFDF6E3),
            panel: hex(0xEEE8D5),
            faint: hex(0xF7F0DC),
            stroke: hex(0xDCD5C0),
            text: hex(0x3B4D53),
            weak: hex(0x7A8A8C),
            accent: hex(0x268BD2),
            widget: hex(0xE4DDC8),
        },
    }
}

pub fn visuals(theme: Theme) -> Visuals {
    let p = palette(theme);
    let mut v = if p.dark { Visuals::dark() } else { Visuals::light() };
    let r6 = CornerRadius::same(6);
    v.override_text_color = Some(p.text);
    v.weak_text_color = Some(p.weak);
    v.extreme_bg_color = p.canvas;
    v.panel_fill = p.panel;
    v.window_fill = p.panel;
    v.faint_bg_color = p.faint;
    v.code_bg_color = p.widget;
    v.hyperlink_color = p.accent;
    v.warn_fg_color = if p.dark { hex(0xF5B53D) } else { hex(0xB7791F) };
    v.error_fg_color = if p.dark { hex(0xF87171) } else { hex(0xDC2626) };
    v.selection.bg_fill = p.accent.gamma_multiply(if p.dark { 0.45 } else { 0.30 });
    v.selection.stroke = Stroke::new(1.5, p.accent);
    v.window_corner_radius = CornerRadius::same(12);
    v.menu_corner_radius = CornerRadius::same(8);
    v.window_stroke = Stroke::new(1.0, p.stroke);
    v.window_shadow = Shadow { offset: [0, 8], blur: 28, spread: 0, color: Color32::from_black_alpha(if p.dark { 110 } else { 40 }) };
    v.popup_shadow = Shadow { offset: [0, 6], blur: 18, spread: 0, color: Color32::from_black_alpha(if p.dark { 90 } else { 30 }) };

    let w = &mut v.widgets;
    w.noninteractive.bg_fill = p.panel;
    w.noninteractive.weak_bg_fill = p.panel;
    w.noninteractive.bg_stroke = Stroke::new(1.0, p.stroke);
    w.noninteractive.fg_stroke = Stroke::new(1.0, p.text);
    w.noninteractive.corner_radius = r6;

    w.inactive.bg_fill = p.widget;
    w.inactive.weak_bg_fill = p.widget;
    w.inactive.bg_stroke = Stroke::NONE;
    w.inactive.fg_stroke = Stroke::new(1.0, p.text);
    w.inactive.corner_radius = r6;

    w.hovered.bg_fill = p.widget.lerp_to_gamma(p.accent, 0.12);
    w.hovered.weak_bg_fill = p.widget.lerp_to_gamma(p.text, 0.08);
    w.hovered.bg_stroke = Stroke::new(1.0, p.stroke.lerp_to_gamma(p.accent, 0.4));
    w.hovered.fg_stroke = Stroke::new(1.5, p.text);
    w.hovered.corner_radius = r6;
    w.hovered.expansion = 0.0;

    w.active.bg_fill = p.accent.gamma_multiply(0.85);
    w.active.weak_bg_fill = p.widget.lerp_to_gamma(p.accent, 0.25);
    w.active.bg_stroke = Stroke::new(1.0, p.accent);
    w.active.fg_stroke = Stroke::new(1.5, p.text);
    w.active.corner_radius = r6;
    w.active.expansion = 0.0;

    w.open.weak_bg_fill = p.widget.lerp_to_gamma(p.accent, 0.15);
    w.open.bg_stroke = Stroke::new(1.0, p.accent.gamma_multiply(0.6));
    w.open.corner_radius = r6;
    v
}

/// Apply theme, typography and spacing.
pub fn apply_style(ctx: &Context, theme: Theme, font_size: f32, animations: bool) {
    let v = visuals(theme);
    let egui_theme = if v.dark_mode { egui::Theme::Dark } else { egui::Theme::Light };
    ctx.set_theme(egui_theme);
    ctx.set_visuals_of(egui_theme, v);
    let body = (font_size + 0.5).clamp(11.0, 18.0);
    ctx.all_styles_mut(|s| {
        s.text_styles = [
            (TextStyle::Heading, semibold(body + 4.0)),
            (TextStyle::Body, FontId::proportional(body)),
            (TextStyle::Button, FontId::proportional(body)),
            (TextStyle::Small, FontId::proportional(body - 2.0)),
            (TextStyle::Monospace, FontId::monospace(body - 0.5)),
        ]
        .into();
        let sp = &mut s.spacing;
        sp.item_spacing = vec2(8.0, 6.0);
        sp.button_padding = vec2(10.0, 4.0);
        sp.interact_size = vec2(36.0, 26.0);
        sp.menu_margin = Margin::same(6);
        sp.window_margin = Margin::same(16);
        sp.combo_width = 140.0;
        sp.text_edit_width = 220.0;
        sp.icon_width = 15.0;
        sp.icon_spacing = 6.0;
        s.interaction.tooltip_delay = 0.35;
        s.animation_time = if animations { motion::BASE } else { 0.0 };
    });
}

// ---------------------------------------------------------------------------
// Motion
// ---------------------------------------------------------------------------

/// Fluid motion helpers. Everything here collapses to "no animation" when the
/// style's `animation_time` is zero (Preferences → Animations off).
pub mod motion {
    use egui::emath::TSTransform;
    use egui::{vec2, Color32, Context, Id, LayerId, Order, Response};

    /// Hover / press feedback.
    pub const FAST: f32 = 0.14;
    /// Dialogs, cards and toasts entering.
    pub const POP: f32 = 0.28;
    /// Default for egui's own animations (panels sliding, popups fading, …).
    pub const BASE: f32 = 0.2;
    /// Follow speed for `glide` (1/s). Settles in roughly 150 ms.
    const RATE: f64 = 22.0;

    pub fn enabled(ctx: &Context) -> bool {
        ctx.global_style().animation_time > 0.0
    }

    /// Fast start, long soft landing.
    pub fn ease_out(t: f32) -> f32 {
        1.0 - (1.0 - t.clamp(0.0, 1.0)).powi(4)
    }

    /// Eased 0..1 that follows a bool (hover, toggle state).
    pub fn toggle(ctx: &Context, id: Id, on: bool) -> f32 {
        if !enabled(ctx) {
            return if on { 1.0 } else { 0.0 };
        }
        ctx.animate_bool_with_time_and_easing(id, on, FAST, egui::emath::easing::cubic_out)
    }

    /// Linear 0..1 progress since `id` started being shown. Restarts whenever a
    /// frame goes by without this being called for `id`.
    pub fn appear(ctx: &Context, id: Id, duration: f32) -> f32 {
        if !enabled(ctx) {
            return 1.0;
        }
        let now = ctx.input(|i| i.time);
        let frame = ctx.cumulative_frame_nr();
        let start = ctx.data_mut(|d| {
            let e = d.get_temp_mut_or_insert_with(id.with("appear"), || (now, frame));
            if frame > e.1 + 1 {
                e.0 = now;
            }
            e.1 = frame;
            e.0
        });
        let t = (((now - start) as f32) / duration).clamp(0.0, 1.0);
        if t < 1.0 {
            ctx.request_repaint();
        }
        t
    }

    /// Smoothly follow `target` (in points). Interruptible: retargeting mid-flight
    /// just bends the path. Jumps larger than `snap` are not animated.
    pub fn glide(ctx: &Context, id: Id, target: f64, snap: f64) -> f64 {
        if !enabled(ctx) {
            return target;
        }
        let dt = ctx.input(|i| i.stable_dt).min(0.05) as f64;
        let cur = ctx.data_mut(|d| {
            let v = d.get_temp_mut_or_insert_with(id, || target);
            let diff = target - *v;
            if !v.is_finite() || diff.abs() > snap || diff.abs() < 0.25 {
                *v = target;
            } else {
                *v += diff * (1.0 - (-dt * RATE).exp());
            }
            *v
        });
        if cur != target {
            ctx.request_repaint();
        }
        cur
    }

    /// Scale-and-settle entrance for a floating layer (dialog, window).
    /// Call every frame the layer is shown, after showing it.
    pub fn pop_in(ctx: &Context, id: Id, resp: &Response) {
        let t = ease_out(appear(ctx, id, POP));
        let tf = if t >= 1.0 {
            TSTransform::IDENTITY
        } else {
            let s = 0.96 + 0.04 * t;
            TSTransform::new(resp.rect.center().to_vec2() * (1.0 - s) + vec2(0.0, (1.0 - t) * 8.0), s)
        };
        ctx.set_transform_layer(resp.layer_id, tf);
    }

    /// Dimmed backdrop behind a modal, fading in with it. Painted above windows
    /// and below the modal itself, so it stays put while the modal pops in.
    pub fn backdrop(ctx: &Context, id: Id, alpha: u8) {
        let t = ease_out(appear(ctx, id.with("backdrop"), FAST));
        let layer = LayerId::new(Order::Middle, Id::new("modal-backdrop"));
        ctx.layer_painter(layer).rect_filled(ctx.content_rect(), 0.0, Color32::from_black_alpha((alpha as f32 * t) as u8));
    }
}

/// Blend two colours (premultiplied), `t` in 0..1.
pub fn mix(a: Color32, b: Color32, t: f32) -> Color32 {
    a.lerp_to_gamma(b, t.clamp(0.0, 1.0))
}

// ---------------------------------------------------------------------------
// Widgets
// ---------------------------------------------------------------------------

/// Frameless square icon button used in toolbars.
pub fn tool_button(ui: &mut Ui, icon: &str, tooltip: &str, enabled: bool) -> Response {
    let size = vec2(30.0, 28.0);
    let (rect, resp) = ui.allocate_exact_size(size, if enabled { Sense::click() } else { Sense::hover() });
    let v = ui.visuals();
    let down = enabled && resp.is_pointer_button_down_on();
    let hover = motion::toggle(ui.ctx(), resp.id.with("hover"), enabled && (resp.hovered() || down));
    let press = motion::toggle(ui.ctx(), resp.id.with("press"), down);
    if hover > 0.0 {
        let fill = mix(v.widgets.hovered.weak_bg_fill, v.widgets.active.weak_bg_fill, press).gamma_multiply(hover);
        ui.painter().rect_filled(rect.shrink(press), CornerRadius::same(6), fill);
    }
    let color = if enabled { v.text_color() } else { v.weak_text_color().gamma_multiply(0.6) };
    ui.painter().text(rect.center(), egui::Align2::CENTER_CENTER, icon, icon_font(17.0), color);
    resp.on_hover_text(tooltip)
}

/// Thin vertical divider for toolbars.
pub fn divider(ui: &mut Ui) {
    let (rect, _) = ui.allocate_exact_size(vec2(9.0, 20.0), Sense::hover());
    let c = ui.visuals().widgets.noninteractive.bg_stroke.color;
    ui.painter().vline(rect.center().x, rect.y_range(), Stroke::new(1.0, c));
}

/// Filled accent button for the main action of a dialog.
pub fn primary(ui: &mut Ui, text: &str) -> Response {
    let a = accent(ui);
    ui.add(
        egui::Button::new(RichText::new(text).color(Color32::WHITE).font(semibold(ui.style().text_styles[&TextStyle::Button].size)))
            .fill(a)
            .corner_radius(6)
            .min_size(vec2(84.0, 30.0)),
    )
}

pub fn secondary(ui: &mut Ui, text: &str) -> Response {
    ui.add(egui::Button::new(text).corner_radius(6).min_size(vec2(84.0, 30.0)))
}

/// Destructive action button.
pub fn danger(ui: &mut Ui, text: &str) -> Response {
    let c = ui.visuals().error_fg_color;
    ui.add(egui::Button::new(RichText::new(text).color(c)).corner_radius(6).min_size(vec2(84.0, 30.0)))
}

/// Small pill toggle with an icon, e.g. the find-bar options.
pub fn chip(ui: &mut Ui, on: &mut bool, icon: &str, tooltip: &str) -> Response {
    let size = vec2(28.0, 24.0);
    let (rect, mut resp) = ui.allocate_exact_size(size, Sense::click());
    if resp.clicked() {
        *on = !*on;
        resp.mark_changed();
    }
    let v = ui.visuals();
    let a = v.selection.stroke.color;
    let on_t = motion::toggle(ui.ctx(), resp.id.with("on"), *on);
    let hover = motion::toggle(ui.ctx(), resp.id.with("hover"), resp.hovered());
    let idle_fill = v.widgets.hovered.weak_bg_fill.gamma_multiply(hover);
    let fill = mix(idle_fill, a.gamma_multiply(0.22), on_t);
    let fg = mix(mix(v.weak_text_color(), v.text_color(), hover), a, on_t);
    ui.painter().rect_filled(rect, CornerRadius::same(5), fill);
    if on_t > 0.0 {
        ui.painter().rect_stroke(rect, CornerRadius::same(5), Stroke::new(1.0, a.gamma_multiply(0.6 * on_t)), egui::StrokeKind::Inside);
    }
    ui.painter().text(rect.center(), egui::Align2::CENTER_CENTER, icon, icon_font(15.0), fg);
    resp.on_hover_text(tooltip)
}

/// Keyboard shortcut badge.
pub fn kbd(ui: &mut Ui, text: &str) {
    if text.is_empty() {
        return;
    }
    let v = ui.visuals();
    let (bg, fg) = (v.widgets.inactive.weak_bg_fill, v.weak_text_color());
    Frame::new().fill(bg).corner_radius(4).inner_margin(Margin::symmetric(5, 1)).show(ui, |ui| {
        ui.label(RichText::new(text).size(11.0).color(fg));
    });
}

/// Section title inside dialogs / preferences.
pub fn section(ui: &mut Ui, title: &str) {
    ui.add_space(6.0);
    ui.label(RichText::new(title.to_uppercase()).font(semibold(11.0)).color(ui.visuals().weak_text_color()));
    ui.add_space(2.0);
}

/// Standard dialog: title (with optional icon), body, and a right-aligned button row.
pub struct Dialog<'a> {
    id: &'a str,
    title: String,
    icon: Option<(&'static str, Color32)>,
    width: f32,
}

pub struct DialogResult<R> {
    pub inner: R,
    /// Esc pressed or backdrop clicked.
    pub dismissed: bool,
}

impl<'a> Dialog<'a> {
    pub fn new(id: &'a str, title: impl Into<String>) -> Self {
        Self { id, title: title.into(), icon: None, width: 420.0 }
    }
    pub fn icon(mut self, icon: &'static str, color: Color32) -> Self {
        self.icon = Some((icon, color));
        self
    }
    pub fn width(mut self, w: f32) -> Self {
        self.width = w;
        self
    }

    /// `body` draws the content; `buttons` draws the footer (laid out right-to-left,
    /// so add the primary button first).
    pub fn show<R>(self, ctx: &Context, body: impl FnOnce(&mut Ui) -> R, buttons: impl FnOnce(&mut Ui, &mut R)) -> DialogResult<R> {
        let fill = ctx.global_style().visuals.window_fill;
        let stroke = ctx.global_style().visuals.window_stroke;
        let shadow = ctx.global_style().visuals.window_shadow;
        let frame = Frame::new().fill(fill).stroke(stroke).shadow(shadow).corner_radius(12).inner_margin(Margin::same(20));
        let id = egui::Id::new(self.id);
        motion::backdrop(ctx, id, 90);
        let resp = egui::Modal::new(id)
            .frame(frame)
            .backdrop_color(Color32::TRANSPARENT)
            .show(ctx, |ui| {
                ui.set_width(self.width);
                ui.horizontal(|ui| {
                    if let Some((ic, color)) = self.icon {
                        ui.label(icon_text(ic, 22.0).color(color));
                        ui.add_space(2.0);
                    }
                    ui.label(RichText::new(&self.title).font(semibold(16.0)));
                });
                ui.add_space(8.0);
                let mut r = body(ui);
                ui.add_space(14.0);
                ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                    ui.spacing_mut().item_spacing.x = 8.0;
                    buttons(ui, &mut r);
                });
                r
            });
        motion::pop_in(ctx, id, &resp.response);
        let dismissed = resp.should_close();
        DialogResult { inner: resp.inner, dismissed }
    }
}

/// Floating notification in the bottom-right corner: slides up into place,
/// then fades and sinks away.
pub fn toast(ctx: &Context, text: &str, age_secs: f32, kind_warn: bool) {
    const IN: f32 = 0.3;
    const SHOW: f32 = 4.0;
    const FADE: f32 = 0.45;
    if age_secs > SHOW + FADE {
        return;
    }
    let slide = motion::enabled(ctx);
    let enter = if slide { motion::ease_out(age_secs / IN) } else { 1.0 };
    let exit = ((age_secs - SHOW) / FADE).clamp(0.0, 1.0);
    let alpha = enter * (1.0 - exit);
    let dy = if slide { (1.0 - enter) * 14.0 + exit * exit * 8.0 } else { 0.0 };
    let v = ctx.global_style().visuals.clone();
    let a = v.selection.stroke.color;
    let (icon, color) = if kind_warn { (icon::WARNING, v.warn_fg_color) } else { (icon::INFO, a) };
    egui::Area::new(egui::Id::new("toast"))
        .anchor(egui::Align2::RIGHT_BOTTOM, vec2(-16.0, -40.0 + dy))
        .order(egui::Order::Foreground)
        .interactable(false)
        .fade_in(false)
        .show(ctx, |ui| {
            ui.set_opacity(alpha);
            Frame::new()
                .fill(v.window_fill)
                .stroke(v.window_stroke)
                .shadow(v.popup_shadow)
                .corner_radius(10)
                .inner_margin(Margin::symmetric(14, 10))
                .show(ui, |ui| {
                    ui.set_max_width(420.0);
                    ui.horizontal(|ui| {
                        ui.label(icon_text(icon, 16.0).color(color));
                        ui.label(text);
                    });
                });
        });
    if age_secs < IN || age_secs >= SHOW {
        ctx.request_repaint();
    } else {
        ctx.request_repaint_after(std::time::Duration::from_secs_f32(SHOW - age_secs));
    }
}
