use crate::backend::AccountMode;
use crate::i18n::{t, tf};
use crate::icons::{icon, Icon};
use crate::{App, Field, MainAction, MaintenanceKind, Message, Page};
use iced::widget::{
    button, checkbox, column, container, image, mouse_area, opaque, pick_list, progress_bar, row,
    scrollable, stack, text, text_input, Space,
};
use iced::{
    gradient, Alignment, Background, Border, Color, ContentFit, Element, Font, Length, Shadow,
    Theme, Vector,
};
use std::collections::HashMap;

const DEEP: Color = Color::from_rgb8(20, 33, 45);
const LINE: Color = Color::from_rgb8(42, 59, 75);
const INK: Color = Color::from_rgb8(233, 240, 246);
const MUTED: Color = Color::from_rgb8(146, 166, 183);
const CRYSTAL: Color = Color::from_rgb8(157, 213, 234);
const GOLD_BRIGHT: Color = Color::from_rgb8(98, 161, 183);
pub(crate) const BODY: Font = Font::with_name("Segoe UI");
const MONO: Font = Font::with_name("Consolas");
#[derive(Clone, Copy)]
pub(crate) struct Colors {
    pub(crate) bg: Color,
    pub(crate) panel: Color,
    pub(crate) card: Color,
    pub(crate) field: Color,
    pub(crate) line: Color,
    pub(crate) ink: Color,
    pub(crate) sub: Color,
    pub(crate) accent: Color,
    pub(crate) select: Color,
    pub(crate) play: Color,
    pub(crate) play_text: Color,
    /// Bottom stop of the window surface; `bg` is the top stop. See `surface`.
    pub(crate) bg_low: Color,
    /// Card fill that floats on the surface: translucent, so the illustration and
    /// the surface gradient read through it. Use for cards, not menus/dialogs
    /// (those stay on the opaque `panel`).
    pub(crate) raised: Color,
    /// 1 px low-contrast card/section border. `line` stays for input borders.
    pub(crate) hairline: Color,
    /// Inset field fill inside a card (search, dropdowns): darker than the
    /// card, no heavy outline.
    pub(crate) well: Color,
    /// Accent wash for a selected/emphasised block inside a card.
    pub(crate) tint: Color,
    /// Soft card shadow colour (use with `card_shadow`).
    pub(crate) shadow: Color,
}
// ---- Shared layout tokens: one 8-pt scale, one radius, one type scale. ----
/// Every card, panel, news card and the Play button use this radius.
pub(crate) const RADIUS: f32 = 12.0;
/// Fields and blocks nested inside a card.
pub(crate) const RADIUS_INNER: f32 = 8.0;
/// Spacing steps (multiply by `scale(app)`): 8, 16, 24, 32.
pub(crate) const SP1: f32 = 8.0;
pub(crate) const SP2: f32 = 16.0;
pub(crate) const SP3: f32 = 24.0;
pub(crate) const SP4: f32 = 32.0;
/// Column gutter and the gap between Home's sections.
pub(crate) const COL_GUTTER: f32 = SP4;
/// Gutter between cards on the Accounts and Downloads pages.
pub(crate) const GUTTER: f32 = SP3;
/// Width of Home's right column (profile card, community cards, Play button).
pub(crate) const SIDE_W: f32 = 336.0;
/// Type scale (multiply by `scale(app)`): caption, small, body, title, heading.
pub(crate) const T_XS: f32 = 11.0;
pub(crate) const T_SM: f32 = 12.0;
pub(crate) const T_MD: f32 = 14.0;
pub(crate) const T_LG: f32 = 17.0;
fn rgb(value: u32) -> Color {
    Color::from_rgb8((value >> 16) as u8, (value >> 8) as u8, value as u8)
}
fn rgba(value: u32, a: f32) -> Color {
    Color { a, ..rgb(value) }
}
pub(crate) fn colors(light: bool) -> Colors {
    if light {
        Colors {
            bg: rgb(0xF4F7FA),
            panel: Color::WHITE,
            card: Color::WHITE,
            field: rgb(0xF5F8FA),
            line: rgb(0xD2DEE7),
            ink: rgb(0x1B3446),
            sub: rgb(0x526D81),
            accent: rgb(0x24667E),
            select: rgb(0xE5EFF4),
            play: rgb(0x205D79),
            play_text: Color::WHITE,
            bg_low: rgb(0xE6EEF4),
            raised: rgba(0xFFFFFF, 0.80),
            hairline: rgba(0x1B3446, 0.10),
            well: rgba(0x1B3446, 0.045),
            tint: rgba(0x24667E, 0.10),
            shadow: rgba(0x1B3446, 0.07),
        }
    } else {
        Colors {
            bg: rgb(0x0E1721),
            panel: rgb(0x17232F),
            card: DEEP,
            field: rgb(0x0F1B26),
            line: LINE,
            ink: INK,
            sub: rgb(0xA4B6C5),
            accent: CRYSTAL,
            select: rgb(0x213B4B),
            play: rgb(0xAFDDED),
            play_text: rgb(0x132A37),
            bg_low: rgb(0x0A1118),
            raised: rgba(0x182532, 0.80),
            hairline: rgba(0x9DD5EA, 0.10),
            well: rgba(0x050B11, 0.32),
            tint: rgba(0x9DD5EA, 0.09),
            shadow: rgba(0x000000, 0.28),
        }
    }
}
/// The one window surface: a subtle top-to-bottom gradient from `bg` to
/// `bg_low`. Paint it once behind the whole window; regions sit on it.
pub(crate) fn surface(light: bool) -> Background {
    let c = colors(light);
    gradient::Linear::new(std::f32::consts::PI)
        .add_stop(0.0, c.bg)
        .add_stop(1.0, c.bg_low)
        .into()
}
/// The soft shadow under a floating card.
pub(crate) fn card_shadow(c: Colors) -> Shadow {
    Shadow {
        color: c.shadow,
        offset: Vector::new(0.0, 6.0),
        blur_radius: 24.0,
    }
}
fn tc(theme: &Theme) -> Colors {
    colors(theme.palette().background.r > 0.5)
}
pub fn theme(light: bool) -> Theme {
    let c = colors(light);
    Theme::custom(
        "Crystal Launcher",
        iced::theme::Palette {
            background: c.bg,
            text: c.ink,
            primary: c.accent,
            success: rgb(0x4F947C),
            danger: rgb(0xCB6262),
            warning: rgb(0xBC8D40),
        },
    )
}
/// Builds an embedded image's handle once. `Handle::from_bytes` mints a new id
/// per call, so a handle made inside `view` is re-decoded on every redraw.
macro_rules! cached_image {
    ($path:literal) => {{
        static HANDLE: std::sync::OnceLock<image::Handle> = std::sync::OnceLock::new();
        HANDLE
            .get_or_init(|| image::Handle::from_bytes(include_bytes!($path).as_slice()))
            .clone()
    }};
}
pub(crate) fn panel(theme: &Theme) -> container::Style {
    let c = tc(theme);
    container::Style {
        background: Some(c.raised.into()),
        text_color: Some(c.ink),
        border: Border {
            color: c.hairline,
            width: 1.0,
            radius: RADIUS.into(),
        },
        shadow: card_shadow(c),
        ..Default::default()
    }
}
pub(crate) fn quiet(theme: &Theme, status: button::Status) -> button::Style {
    let c = tc(theme);
    button::Style {
        background: matches!(status, button::Status::Hovered | button::Status::Pressed)
            .then(|| c.select.into()),
        text_color: if status == button::Status::Disabled {
            c.sub
        } else {
            c.accent
        },
        border: Border {
            radius: 5.0.into(),
            ..Default::default()
        },
        ..Default::default()
    }
}
pub(crate) fn primary(theme: &Theme, status: button::Status) -> button::Style {
    let c = tc(theme);
    button::Style {
        background: Some(
            if status == button::Status::Disabled {
                c.select
            } else {
                c.play
            }
            .into(),
        ),
        text_color: if status == button::Status::Disabled {
            c.sub
        } else {
            c.play_text
        },
        border: Border {
            radius: 7.0.into(),
            ..Default::default()
        },
        ..Default::default()
    }
}
fn action<'a>(label: &'a str, message: Message) -> crate::controller_nav::Button<'a, Message> {
    crate::controller_nav::button(text(label).font(BODY).size(14))
        .padding([8, 12])
        .style(quiet)
        .on_press(message)
}
// ---- top bar and play bar ----
pub(crate) const SEMI: Font = Font {
    weight: iced::font::Weight::Semibold,
    ..BODY
};
/// Height of the integrated title bar, above its hairline.
const BAR_H: f32 = 56.0;
/// Width of a window caption button, as on Windows 11.
const CAPTION_W: f32 = 46.0;
/// A title-bar tab: the label sits in a fixed-width slot and the active one
/// carries an underline exactly as wide as its label, resting on the bar's
/// hairline.
fn nav<'a>(label: &'a str, page: Page, app: &App) -> Element<'a, Message> {
    let c = colors(app.settings.light_theme);
    let active = app.page == page;
    let underline = column![
        Space::new().height(Length::Fill),
        container(Space::new())
            .width(Length::Fill)
            .height(if active { 2.5 } else { 0.0 })
            .style(move |_| container::Style {
                background: Some(c.accent.into()),
                border: Border {
                    radius: iced::border::top(1.5),
                    ..Default::default()
                },
                ..Default::default()
            })
    ];
    // In a row so the fixed-width button's min width does not stretch the
    // stack (and with it the underline) past the label.
    let tab = row![stack![
        container(text(label).font(if active { SEMI } else { BODY }).size(13.5))
            .height(BAR_H)
            .align_y(Alignment::Center),
        underline
    ]];
    crate::controller_nav::button(tab)
        .padding([0, 14])
        .style(move |theme: &Theme, status| {
            let c = tc(theme);
            button::Style {
                background: None,
                text_color: if active {
                    c.accent
                } else if matches!(status, button::Status::Hovered | button::Status::Pressed) {
                    c.ink
                } else {
                    c.sub
                },
                ..Default::default()
            }
        })
        .on_press(Message::Navigate(page))
        .into()
}
/// Transparent 36 px square icon button for the window chrome.
fn chrome_icon<'a>(which: Icon, size: f32, color: Color, message: Message) -> Element<'a, Message> {
    crate::controller_nav::button(container(icon(which, size, color)).center(36))
        .padding(0)
        .style(|theme: &Theme, status| {
            let c = tc(theme);
            button::Style {
                background: matches!(status, button::Status::Hovered | button::Status::Pressed)
                    .then(|| c.select.into()),
                text_color: c.sub,
                border: Border {
                    radius: 8.0.into(),
                    ..Default::default()
                },
                ..Default::default()
            }
        })
        .on_press(message)
        .into()
}
/// Everything on screen that identifies the user or a contact: account
/// labels and IDs, friend and character names, mail handles. Privacy mode
/// blurs these wherever they appear inside other text.
pub(crate) fn identities(app: &App) -> Vec<String> {
    let mut names: Vec<String> = Vec::new();
    for row in &app.accounts {
        names.extend([row.account.label.clone(), row.account.pol_id.clone(), row.account.se_id.clone()]);
    }
    if let Some(snapshot) = &app.friends.snapshot {
        for friend in &snapshot.entries {
            names.push(friend.name.clone());
            names.extend(friend.character.clone());
        }
        names.extend(snapshot.social.headers.iter().map(|h| h.sender.clone()));
    }
    names.retain(|n| n.trim().chars().count() > 1);
    names
}
/// Free text (a status, an error, a result) with whatever identifies someone
/// blurred while privacy mode is on.
pub(crate) fn private_text<'a>(app: &App, value: &str, size: f32, font: Font, color: Color) -> Element<'a, Message> {
    if !crate::privacy::on() {
        return text(value.to_owned()).font(font).size(size).color(color).into();
    }
    let names = identities(app);
    let names: Vec<&str> = names.iter().map(String::as_str).collect();
    crate::privacy::line(crate::privacy::split(value, &names), size, font, color)
}
/// A name (account label, friend, character): blurred in privacy mode.
pub(crate) fn name_text<'a>(value: &str, size: f32, font: Font, color: Color) -> Element<'a, Message> {
    crate::privacy::name(text(value.to_owned()).font(font).size(size).color(color), value, size, color)
}
/// A small labelled hint shown when the pointer rests on `content`.
pub(crate) fn hint<'a>(
    content: impl Into<Element<'a, Message>>,
    label: impl iced::widget::text::IntoFragment<'a>,
    position: iced::widget::tooltip::Position,
) -> Element<'a, Message> {
    iced::widget::tooltip(
        content,
        container(text(label).font(BODY).size(12)).padding([6, 10]),
        position,
    )
    .gap(6)
    .delay(std::time::Duration::from_millis(350))
    .style(|theme: &Theme| {
        let c = tc(theme);
        container::Style {
            background: Some(c.panel.into()),
            text_color: Some(c.ink),
            border: Border {
                color: c.line,
                width: 1.0,
                radius: 6.0.into(),
            },
            shadow: card_shadow(c),
            ..Default::default()
        }
    })
    .into()
}
/// The title-bar eye: screenshot privacy on (accent, eye crossed out) or off.
fn privacy_toggle(app: &App) -> Element<'_, Message> {
    let c = colors(app.settings.light_theme);
    let on = app.settings.privacy;
    hint(
        chrome_icon(
            if on { Icon::EyeOff } else { Icon::Eye },
            18.0,
            if on { c.accent } else { c.sub },
            Message::SetPrivacy(!on),
        ),
        if on {
            t("Privacy mode is on: names, IDs and messages are blurred. Click to show them.")
        } else {
            t("Privacy mode: blur names, IDs and messages for screenshots")
        },
        iced::widget::tooltip::Position::Bottom,
    )
}
/// The caption buttons' thin Windows 11 style glyphs, built once. Each canvas
/// is the whole 46 x BAR_H button with the 10 px glyph drawn at its centre, so
/// the svg's own hover state is the button's.
#[derive(Clone, Copy)]
enum Caption {
    Minimize,
    Maximize,
    Restore,
    Close,
}
fn caption_glyph(which: Caption) -> iced::widget::svg::Handle {
    use std::sync::OnceLock;
    static HANDLES: OnceLock<[iced::widget::svg::Handle; 4]> = OnceLock::new();
    let handles = HANDLES.get_or_init(|| {
        let doc = |body: &str| {
            let (w, h) = (CAPTION_W, BAR_H);
            let (x, y) = ((w - 16.0) / 2.0, (h - 16.0) / 2.0);
            iced::widget::svg::Handle::from_memory(format!(
                r##"<svg xmlns="http://www.w3.org/2000/svg" width="{w}" height="{h}" viewBox="0 0 {w} {h}" fill="none" stroke="#ffffff" stroke-width="1" stroke-linecap="round" stroke-linejoin="round"><g transform="translate({x} {y})">{body}</g></svg>"##
            ).into_bytes())
        };
        [
            doc(r#"<path d="M3 8.5h10"/>"#),
            doc(r#"<rect x="3.5" y="3.5" width="9" height="9" rx="1.5"/>"#),
            doc(r#"<rect x="3.5" y="5.5" width="7" height="7" rx="1.25"/><path d="M5.5 3.5h5.5a1.5 1.5 0 0 1 1.5 1.5v5.5"/>"#),
            doc(r#"<path d="M3.5 3.5l9 9M12.5 3.5l-9 9"/>"#),
        ]
    });
    handles[which as usize].clone()
}
/// A full-height Windows caption button flush with the window's top edge.
/// Close turns red on hover, like the system's own.
fn caption_button<'a>(which: Caption, message: Message) -> Element<'a, Message> {
    let close = matches!(which, Caption::Close);
    let glyph = iced::widget::svg(caption_glyph(which))
        .width(CAPTION_W)
        .height(BAR_H)
        .style(move |theme: &Theme, status| {
            let c = tc(theme);
            iced::widget::svg::Style {
                color: Some(match status {
                    iced::widget::svg::Status::Hovered if close => Color::WHITE,
                    iced::widget::svg::Status::Hovered => c.ink,
                    _ => c.sub,
                }),
            }
        });
    crate::controller_nav::button(glyph)
        .padding(0)
        .width(CAPTION_W)
        .height(BAR_H)
        .style(move |theme: &Theme, status| {
            let c = tc(theme);
            let hot = matches!(status, button::Status::Hovered | button::Status::Pressed);
            button::Style {
                background: hot.then(|| {
                    if close {
                        rgb(if status == button::Status::Pressed { 0x94_1E_13 } else { 0xC4_2B_1C })
                    } else {
                        c.select
                    }
                    .into()
                }),
                ..Default::default()
            }
        })
        .on_press(message)
        .into()
}
/// Exact-shape glyphs the shared icon set does not carry (filled play
/// triangle, bare check mark), tinted like `icon`.
fn shape<'a>(body: &str, w: f32, h: f32, color: Color) -> Element<'a, Message> {
    let doc = format!(
        r##"<svg xmlns="http://www.w3.org/2000/svg" width="{w}" height="{h}" viewBox="0 0 {w} {h}" fill="none" stroke="#ffffff" stroke-linecap="round" stroke-linejoin="round">{body}</svg>"##
    );
    iced::widget::svg(iced::widget::svg::Handle::from_memory(doc.into_bytes()))
        .width(w)
        .height(h)
        .style(move |_, _| iced::widget::svg::Style { color: Some(color) })
        .into()
}
/// The window's own title bar: no box of its own, it sits on the page
/// background and ends in a soft full-width hairline. Every empty part of it
/// drags the window, and a double click there maximises or restores.
fn chrome_bar(app: &App) -> Element<'_, Message> {
    let c = colors(app.settings.light_theme);
    let light = app.settings.light_theme;
    let s = scale(app);
    // Pre-downscaled 12x34 mark (glow stripped) shown at 6x17: pixel-exact at
    // 2x and a gentle bilinear 0.75 step at 1.5x, never a sliver.
    let mark = cached_image!("../assets/crystal-mark.png");
    let brand = row![
        image(mark).width(6).height(17),
        Space::new().width(12),
        text("Crystal Launcher").font(SEMI).size(16).color(c.ink)
    ]
    .align_y(Alignment::Center);
    let bar = row![
        // The brand lines up with the page content's left edge.
        Space::new().width(48.0 * s),
        brand,
        Space::new().width(26),
        nav(t("Home"), Page::Home, app),
        nav(t("Accounts"), Page::Accounts, app),
        nav(t("Friends"), Page::Friends, app),
        nav(t("Downloads"), Page::Maintenance, app),
        nav(t("Settings"), Page::Settings, app),
        Space::new().width(Length::Fill),
        privacy_toggle(app),
        Space::new().width(2),
        chrome_icon(Icon::Sun, 18.0, if light { c.accent } else { c.sub }, Message::SetLightTheme(true)),
        Space::new().width(2),
        chrome_icon(Icon::Moon, 17.0, if light { c.sub } else { c.accent }, Message::SetLightTheme(false)),
        Space::new().width(2),
        chrome_icon(Icon::Settings, 18.0, c.sub, Message::ToggleSettings),
        Space::new().width(14),
        caption_button(Caption::Minimize, Message::MinimizeWindow),
        caption_button(
            if app.maximized { Caption::Restore } else { Caption::Maximize },
            Message::MaximizeWindow
        ),
        caption_button(Caption::Close, Message::CloseRequested)
    ]
    .height(BAR_H)
    .align_y(Alignment::Center);
    let hairline = container(Space::new())
        .width(Length::Fill)
        .height(1)
        .style(move |_| container::Style {
            background: Some(Color { a: 0.55, ..c.line }.into()),
            ..Default::default()
        });
    // Buttons capture their own presses; whatever reaches this area is the
    // title bar itself. The window drag starts on the first move with the
    // button held, never on the press: a system move loop entered after a
    // quick click's release would swallow the second click of a double click.
    let area = mouse_area(column![bar, hairline])
        .on_press(Message::TitlePressed)
        .on_release(Message::TitleReleased)
        .on_exit(Message::TitleReleased);
    if app.title_held {
        area.on_move(|_| Message::TitleMoved).into()
    } else {
        area.into()
    }
}
/// Layout scale. 1440 px is the design width: at and above it everything
/// keeps its natural size and the layout reflows into the extra room instead
/// of zooming; below it the layout shrinks in step with the width, to a floor
/// at the 900 px minimum window.
pub(crate) fn scale(app: &App) -> f32 {
    (app.viewport.width / 1440.0).clamp(0.68, 1.0)
}
/// Invisible grab strips along the borderless window's edges and corners that
/// resize it, with the matching cursors. Not shown while maximised.
fn resize_edges<'a>(app: &App, content: Element<'a, Message>) -> Element<'a, Message> {
    use iced::mouse::Interaction;
    use iced::window::Direction;
    if app.maximized {
        return content;
    }
    const EDGE: f32 = 5.0;
    const CORNER: f32 = 14.0;
    let zone = |direction: Direction, width: Length, height: Length| -> Element<'a, Message> {
        let cursor = match direction {
            Direction::North | Direction::South => Interaction::ResizingVertically,
            Direction::East | Direction::West => Interaction::ResizingHorizontally,
            Direction::NorthWest | Direction::SouthEast => Interaction::ResizingDiagonallyDown,
            Direction::NorthEast | Direction::SouthWest => Interaction::ResizingDiagonallyUp,
        };
        mouse_area(Space::new().width(width).height(height))
            .interaction(cursor)
            .on_press(Message::ResizeWindow(direction))
            .into()
    };
    let (edge, corner, fill) = (Length::Fixed(EDGE), Length::Fixed(CORNER), Length::Fill);
    let side = |top: Direction, mid: Direction, bottom: Direction| {
        column![
            zone(top, edge, Length::Fixed(CORNER - EDGE)),
            zone(mid, edge, fill),
            zone(bottom, edge, Length::Fixed(CORNER - EDGE))
        ]
        .height(fill)
    };
    let edges = column![
        row![
            zone(Direction::NorthWest, corner, edge),
            zone(Direction::North, fill, edge),
            zone(Direction::NorthEast, corner, edge)
        ],
        row![
            side(Direction::NorthWest, Direction::West, Direction::SouthWest),
            Space::new().width(fill).height(fill),
            side(Direction::NorthEast, Direction::East, Direction::SouthEast)
        ]
        .height(fill),
        row![
            zone(Direction::SouthWest, corner, edge),
            zone(Direction::South, fill, edge),
            zone(Direction::SouthEast, corner, edge)
        ]
    ];
    stack![content, edges].into()
}

pub fn view(app: &App) -> Element<'_, Message> {
    let c = colors(app.settings.light_theme);
    let s = scale(app);
    let topbar = chrome_bar(app);
    let content:Element<'_,Message>=match app.page {
  Page::Home=>portal_home(app),
  Page::Accounts=>accounts(app),
  Page::Friends=>crate::friends::view(app),
  Page::Maintenance=>inset_scroll(maintenance(app)).height(Length::Fill).into(),
  Page::Settings=>crate::settings_page::view(app),
 };
    let base = container(
        column![
            topbar,
            crate::self_update::banner(app).unwrap_or_else(|| Space::new().height(0).into()),
            container(content)
                .padding(
                    iced::Padding::ZERO
                        .top(26.0 * s)
                        .left(48.0 * s)
                        .right(48.0 * s)
                        .bottom(0)
                )
                .height(Length::Fill),
            play_bar(app)
        ]
        .spacing(0)
        .height(Length::Fill),
    )
    .width(Length::Fill)
    .height(Length::Fill)
    .style(move |_| container::Style {
        background: Some(surface(app.settings.light_theme)),
        text_color: Some(c.ink),
        ..Default::default()
    })
    .into();
    let base = if app.confirm_close || app.pending_delete.is_some() || app.editor.is_some() || app.account_import.open || app.show_settings || app.show_details || app.ah_lookup.open { crate::controller_nav::exclude(base) } else { base };
    let screen = if app.confirm_close {
        close_confirm(app, base)
    } else if let Some(key) = &app.pending_delete {
        delete_confirm(app, key, base)
    } else if let Some(editor) = &app.editor {
        account_editor(app, editor, base)
    } else if app.account_import.open {
        import_accounts(app, base)
    } else if app.show_settings {
        settings_panel(app, base)
    } else if app.show_details {
        details(app, base)
    } else if app.ah_lookup.open {
        ah_lookup_panel(app, base)
    } else {
        base
    };
    crate::controller_nav::view(&app.controller_nav, resize_edges(app, screen))
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct Profile {
    key: String,
    label: String,
}
impl std::fmt::Display for Profile {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.label)
    }
}
/// The game front end the launch profile starts. Native remains the serialized
/// private-server route and is implemented by xiloader; retail maps legacy Native to default.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub(crate) enum LaunchWith {
    Native,
    #[default]
    Windower,
    Ashita,
    Vanilla,
}
impl LaunchWith {
    pub(crate) const ALL: [LaunchWith; 3] = [LaunchWith::Vanilla, LaunchWith::Windower, LaunchWith::Ashita];
    pub(crate) const PRIVATE: [LaunchWith; 1] = [LaunchWith::Native];

    pub(crate) fn retail(self) -> Self {
        if self == Self::Native { Self::default() } else { self }
    }
}
impl std::fmt::Display for LaunchWith {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(match self {
            LaunchWith::Native => "xiloader",
            LaunchWith::Windower => "Windower",
            LaunchWith::Ashita => "Ashita",
            LaunchWith::Vanilla => t("Vanilla"),
        })
    }
}
/// The line under the account name: the server for private accounts, else the
/// retail region.
fn profile_detail(account: &crate::backend::Account) -> String {
    match account.mode {
        AccountMode::Private if !account.server.trim().is_empty() => account.server.trim().into(),
        AccountMode::Private => t("Private server").into(),
        AccountMode::Retail => tf(
            "Retail · {region}",
            &[(
                "region",
                ["US", "EU", "JP"].get(account.region as usize).unwrap_or(&"US"),
            )],
        ),
    }
}
/// Profile-card palette entries that the shared `Colors` has no slot for.
fn profile_avatar_fill(light: bool) -> Color {
    if light {
        rgb(0xCFDFE9)
    } else {
        rgb(0x2B4B5F)
    }
}
/// Styles a pick_list as a flat filled box; its own label and handle are hidden
/// when `hide_label` so a richer row can be stacked on top of it.
pub(crate) fn profile_pick_style(
    fill: Color,
    border: Border,
    hide_label: bool,
    light: bool,
) -> impl Fn(&Theme, pick_list::Status) -> pick_list::Style {
    move |_, status| {
        let c = colors(light);
        let label = if hide_label { Color::TRANSPARENT } else { c.ink };
        let mut border = border;
        if matches!(status, pick_list::Status::Hovered | pick_list::Status::Opened { .. }) {
            border.color = if border.width > 0.0 {
                with_alpha(c.accent, 0.40)
            } else {
                border.color
            };
        }
        pick_list::Style {
            text_color: label,
            placeholder_color: if hide_label { Color::TRANSPARENT } else { c.sub },
            handle_color: Color::TRANSPARENT,
            background: fill.into(),
            border,
        }
    }
}
pub(crate) fn profile_menu_style(light: bool) -> impl Fn(&Theme) -> iced::overlay::menu::Style {
    move |_| {
        let c = colors(light);
        iced::overlay::menu::Style {
            background: c.panel.into(),
            border: Border {
                color: c.line,
                width: 1.0,
                radius: RADIUS_INNER.into(),
            },
            text_color: c.ink,
            selected_text_color: c.ink,
            selected_background: c.select.into(),
            shadow: Shadow {
                color: Color::from_rgba8(0, 0, 0, if light { 0.12 } else { 0.35 }),
                offset: Vector::new(0.0, 6.0),
                blur_radius: 18.0,
            },
        }
    }
}
/// Lays a widget out at a fixed `height` regardless of its intrinsic height.
/// A pick_list draws its box, centres its label and drops its menu from these
/// bounds, so the whole tall profile block opens the account menu while the
/// menu rows keep the pick_list's normal padding.
struct ProfileTall<'a> {
    content: Element<'a, Message>,
    height: f32,
}
pub(crate) fn profile_tall<'a>(content: impl Into<Element<'a, Message>>, height: f32) -> Element<'a, Message> {
    Element::new(ProfileTall {
        content: content.into(),
        height,
    })
}
impl<'a> iced::advanced::Widget<Message, Theme, iced::Renderer> for ProfileTall<'a> {
    fn size(&self) -> iced::Size<Length> {
        iced::Size::new(Length::Fill, Length::Fixed(self.height))
    }
    fn children(&self) -> Vec<iced::advanced::widget::Tree> {
        vec![iced::advanced::widget::Tree::new(&self.content)]
    }
    fn diff(&self, tree: &mut iced::advanced::widget::Tree) {
        tree.diff_children(std::slice::from_ref(&self.content));
    }
    fn layout(
        &mut self,
        tree: &mut iced::advanced::widget::Tree,
        renderer: &iced::Renderer,
        limits: &iced::advanced::layout::Limits,
    ) -> iced::advanced::layout::Node {
        let inner = self
            .content
            .as_widget_mut()
            .layout(&mut tree.children[0], renderer, limits);
        let size = iced::Size::new(inner.size().width, self.height);
        iced::advanced::layout::Node::with_children(
            size,
            vec![iced::advanced::layout::Node::new(size)],
        )
    }
    fn draw(
        &self,
        tree: &iced::advanced::widget::Tree,
        renderer: &mut iced::Renderer,
        theme: &Theme,
        style: &iced::advanced::renderer::Style,
        layout: iced::advanced::Layout<'_>,
        cursor: iced::mouse::Cursor,
        viewport: &iced::Rectangle,
    ) {
        if let Some(child) = layout.children().next() {
            self.content
                .as_widget()
                .draw(&tree.children[0], renderer, theme, style, child, cursor, viewport);
        }
    }
    fn operate(
        &mut self,
        tree: &mut iced::advanced::widget::Tree,
        layout: iced::advanced::Layout<'_>,
        renderer: &iced::Renderer,
        operation: &mut dyn iced::advanced::widget::Operation,
    ) {
        if let Some(child) = layout.children().next() {
            self.content
                .as_widget_mut()
                .operate(&mut tree.children[0], child, renderer, operation);
        }
    }
    fn update(
        &mut self,
        tree: &mut iced::advanced::widget::Tree,
        event: &iced::Event,
        layout: iced::advanced::Layout<'_>,
        cursor: iced::mouse::Cursor,
        renderer: &iced::Renderer,
        clipboard: &mut dyn iced::advanced::Clipboard,
        shell: &mut iced::advanced::Shell<'_, Message>,
        viewport: &iced::Rectangle,
    ) {
        if let Some(child) = layout.children().next() {
            self.content.as_widget_mut().update(
                &mut tree.children[0],
                event,
                child,
                cursor,
                renderer,
                clipboard,
                shell,
                viewport,
            );
        }
    }
    fn mouse_interaction(
        &self,
        tree: &iced::advanced::widget::Tree,
        layout: iced::advanced::Layout<'_>,
        cursor: iced::mouse::Cursor,
        viewport: &iced::Rectangle,
        renderer: &iced::Renderer,
    ) -> iced::mouse::Interaction {
        layout
            .children()
            .next()
            .map(|child| {
                self.content.as_widget().mouse_interaction(
                    &tree.children[0],
                    child,
                    cursor,
                    viewport,
                    renderer,
                )
            })
            .unwrap_or_default()
    }
    fn overlay<'b>(
        &'b mut self,
        tree: &'b mut iced::advanced::widget::Tree,
        layout: iced::advanced::Layout<'b>,
        renderer: &iced::Renderer,
        viewport: &iced::Rectangle,
        translation: Vector,
    ) -> Option<iced::advanced::overlay::Element<'b, Message, Theme, iced::Renderer>> {
        let child = layout.children().next()?;
        self.content.as_widget_mut().overlay(
            &mut tree.children[0],
            child,
            renderer,
            viewport,
            translation,
        )
    }
}
fn profile(app: &App) -> Option<&crate::AccountRow> {
    app.accounts
        .iter()
        .find(|a| a.selected)
        .or_else(|| app.accounts.first())
}
// Home keeps its hero, profile and news sections independently sized.
/// Minimum height of Home's top band (wordmark + launch profile), on the
/// 8-pt grid; the band grows when a larger wordmark (or the banner) needs it.
const HERO_H: f32 = 288.0;
/// The wordmark PNG's aspect (747x154) and the cover art's (1400x544).
const WORDMARK_ASPECT: f32 = 747.0 / 154.0;
const ART_ASPECT: f32 = 1400.0 / 544.0;
/// Hero geometry for a hero column `hw` logical px wide. The wordmark takes
/// a proportional share of the column (373.5 of 976) within sensible bounds, so
/// it grows and shrinks with the window instead of being pinned at one size;
/// the greeting and the gap under the wordmark follow it.
#[derive(Clone, Copy)]
struct HeroCopy {
    word_w: f32,
    word_h: f32,
    gap: f32,
    welcome: f32,
}
impl HeroCopy {
    fn for_width(hw: f32) -> Self {
        Self::for_size(hw, f32::INFINITY)
    }
    /// As `for_width`, but the whole block also fits in `room` px of height.
    fn for_size(hw: f32, room: f32) -> Self {
        // The block is about 0.368 x the wordmark's width tall.
        let by_height = (room / 0.368).max(160.0);
        let word_w = (hw * 0.383)
            .clamp(220.0, 640.0)
            .min(by_height)
            .min(hw.max(0.0));
        HeroCopy {
            word_w,
            word_h: word_w / WORDMARK_ASPECT,
            gap: (word_w * 0.0857).clamp(16.0, 40.0),
            welcome: (word_w * 0.0585).clamp(15.0, 26.0),
        }
    }
    /// Wordmark + gap + one line of greeting.
    fn height(&self) -> f32 {
        self.word_h + self.gap + self.welcome * 1.3
    }
}
/// The hero column width implied by the window: content width (the window
/// minus Home's side padding) minus the right column and its COL_GUTTER.
fn hero_width_estimate(app: &App) -> f32 {
    let s = scale(app);
    app.viewport.width - 96.0 * s - (SIDE_W + COL_GUTTER) * s
}
/// The band's minimum height: 288, or what the wordmark, the greeting, 32
/// above and below them and (when shown) the components banner need.
fn hero_band_min(app: &App, banner: bool) -> f32 {
    let s = scale(app);
    let copy = HeroCopy::for_width(hero_width_estimate(app));
    let banner_h = if banner { 56.0 * s } else { 0.0 };
    (copy.height() + 2.0 * SP4 * s + banner_h).max(HERO_H * s)
}
/// Height of everything on Home below the band: the art's bleed COL_GUTTER, the
/// news block (header 36 + 16 + cards + 18 + link 16) and a last 8.
fn home_below_band(app: &App) -> f32 {
    let s = scale(app);
    COL_GUTTER * s + (36.0 + 16.0 + 18.0 + 16.0 + SP1) * s + news_grid(app).height()
}
/// Home's own height in the window: the viewport less the title bar and its
/// hairline, the page's top padding and the play bar with its hairline.
fn home_room(app: &App) -> f32 {
    // The update banner can wrap as the window narrows. Keep Home scrollable
    // while it is present so it cannot crowd out the hero or news controls.
    if crate::self_update::banner(app).is_some() { return 0.0; }
    let s = scale(app);
    app.viewport.height - (BAR_H + 1.0) - 26.0 * s - (96.0 * s + 1.0)
}
fn portal_home(app: &App) -> Element<'_, Message> {
    let s = scale(app);
    let min_band = hero_band_min(app, crate::prereqs::banner(app).is_some());
    let bleed = COL_GUTTER * s;
    // When the page fits, the band takes every spare pixel (Fill), so a tall
    // or maximised window has no empty strip above the play bar: the hero,
    // its art and the wordmark grow into it while the news stays anchored
    // just above the play bar. When it does not fit, the band keeps its
    // minimum and the page scrolls.
    let fits = home_room(app) >= min_band + home_below_band(app);
    let band_len = if fits {
        Length::Fill
    } else {
        Length::Fixed(min_band + bleed)
    };
    // The illustration is part of the surface: it sits behind the
    // whole top band (wordmark and launch profile alike) and bleeds into the
    // COL_GUTTER below, fading out on every edge. The translucent profile card
    // lets it read through.
    let top = stack![
        hero_art(app, band_len),
        column![
            row![hero(app), profile_panel(app)]
                .spacing(COL_GUTTER * s)
                .height(Length::Fill),
            Space::new().height(bleed)
        ]
        .height(Length::Fill)
    ]
    .height(band_len);
    let page = column![
        top,
        row![news_section(app), community(app)].spacing(COL_GUTTER * s)
    ]
    .padding(iced::Padding::ZERO.bottom(SP1 * s));
    if fits {
        return page.height(Length::Fill).into();
    }
    // A thin scrollbar that reserves its own lane (only while it is shown), so
    // it never covers the right column's cards or the banner's button.
    let bar = scrollable::Scrollbar::new()
        .width(4)
        .scroller_width(4)
        .margin(0)
        .spacing(SP1 * s);
    scrollable(page)
        .direction(scrollable::Direction::Vertical(bar))
        .style(thin_scrollbar)
        .height(Length::Fill)
        .into()
}
/// Quiet rail-less scrollbar in the hairline/sub tones.
pub(crate) fn thin_scrollbar(theme: &Theme, status: scrollable::Status) -> scrollable::Style {
    let c = tc(theme);
    let mut style = scrollable::default(theme, status);
    let hot = matches!(
        status,
        scrollable::Status::Hovered { is_vertical_scrollbar_hovered: true, .. }
            | scrollable::Status::Dragged { is_vertical_scrollbar_dragged: true, .. }
    );
    let rail = scrollable::Rail {
        background: None,
        border: Border::default(),
        scroller: scrollable::Scroller {
            background: with_alpha(c.sub, if hot { 0.55 } else { 0.28 }).into(),
            border: Border {
                radius: 2.0.into(),
                ..Default::default()
            },
        },
    };
    style.vertical_rail = rail;
    style.horizontal_rail = rail;
    style.container = container::Style::default();
    style
}
/// Cover artwork behind the top band.
fn hero_art(app: &App, len: Length) -> Element<'_, Message> {
    let s = scale(app);
    let light = app.settings.light_theme;
    let source = if light {
        cached_image!("../assets/portal-art-light.png")
    } else {
        cached_image!("../assets/portal-art-dark.png")
    };
    // Keep the figure group between the title and profile card; let it extend
    // behind the card on narrow windows without clipping the source artwork.
    let art = iced::widget::responsive(move |size| {
        let card_left = size.width - SIDE_W * s;
        let word_right = HeroCopy::for_size(card_left - COL_GUTTER * s, size.height).word_w;
        let min_left = 0.75 * word_right;
        let w = (size.height.min(560.0) * ART_ASPECT)
            .min(size.width)
            .min((size.width - min_left).max(0.0));
        let focus = word_right + 0.62 * (card_left - word_right);
        let left = (focus - 0.55 * w)
            .max(min_left)
            .min((size.width - w).max(0.0));
        column![
            Space::new().height(Length::Fill),
            row![
                Space::new().width(left.max(0.0)),
                image(source.clone())
                    .width(w)
                    .height(w / ART_ASPECT)
                    .content_fit(ContentFit::Contain)
                    .opacity(if light { 0.85_f32 } else { 0.68_f32 }),
            ]
        ]
        .height(Length::Fill)
        .into()
    });
    let h = len;
    let art = container(art).width(Length::Fill).height(h);
    // Dissolve the art's lowest rows into the surface so the band has no
    // bottom edge. The surface colour at this height is roughly 40% of the
    // way down its gradient.
    let c = colors(light);
    let mix = |a: f32, b: f32| a + (b - a) * 0.4;
    let here = Color::from_rgb(
        mix(c.bg.r, c.bg_low.r),
        mix(c.bg.g, c.bg_low.g),
        mix(c.bg.b, c.bg_low.b),
    );
    let fade = container(Space::new())
        .width(Length::Fill)
        .height(64.0 * s)
        .style(move |_| container::Style {
            background: Some(
                gradient::Linear::new(std::f32::consts::PI)
                    .add_stop(0.0, with_alpha(here, 0.0))
                    .add_stop(1.0, here)
                    .into(),
            ),
            ..Default::default()
        });
    stack![art, column![Space::new().height(Length::Fill), fade].height(h)].into()
}
fn hero(app: &App) -> Element<'_, Message> {
    let c = colors(app.settings.light_theme);
    let s = scale(app);
    let word = if app.settings.light_theme {
        cached_image!("../assets/portal-wordmark-light.png")
    } else {
        cached_image!("../assets/portal-wordmark-dark.png")
    };
    // A Windows-components warning, when there is one, heads the hero column
    // inside the band (top-aligned with the profile card, right edge on the
    // COL_GUTTER), so it neither pushes the page into scrolling nor spans under
    // the card. A near-opaque surface backing keeps it legible over the art.
    let banner = crate::prereqs::banner(app).map(|b| {

        column![
            container(b).style(move |_| container::Style {
                background: Some(with_alpha(c.bg, 0.92).into()),
                border: Border {
                    radius: RADIUS_INNER.into(),
                    ..Default::default()
                },
                ..Default::default()
            }),
            Space::new().height(SP2 * s)
        ]
    });
    // Wordmark and greeting, sized from the column's real width, anchored to
    // the content's left edge and centred in the space under the banner (a
    // hair above true centre, which reads as centred).
    let copy = iced::widget::responsive(move |size| {
        let g = HeroCopy::for_size(size.width, size.height - 2.0 * SP3 * s);
        column![
            Space::new().height(Length::FillPortion(5)),
            image(word.clone())
                .width(g.word_w)
                .height(g.word_h)
                .content_fit(ContentFit::Fill),
            Space::new().height(g.gap),
            text(t("Welcome back to Vana’diel."))
                .font(Font::with_name("Constantia"))
                .size(g.welcome)
                .color(c.ink),
            Space::new().height(Length::FillPortion(6))
        ]
        .width(Length::Fill)
        .height(Length::Fill)
        .into()
    });
    column![banner, copy]
        .width(Length::Fill)
        .height(Length::Fill)
        .into()
}
fn profile_panel(app: &App) -> Element<'_, Message> {
    let c = colors(app.settings.light_theme);
    let s = scale(app);
    let profiles: Vec<Profile> = app
        .accounts
        .iter()
        .map(|a| Profile {
            key: a.key.clone(),
            label: crate::privacy::mask(&a.account.label),
        })
        .collect();
    let selected = profile(app).map(|a| Profile {
        key: a.key.clone(),
        label: crate::privacy::mask(&a.account.label),
    });
    use crate::icons::{icon, Icon};
    let light = app.settings.light_theme;
    let semibold = Font {
        weight: iced::font::Weight::Semibold,
        ..BODY
    };
    let plain = move |_: &Theme, _: button::Status| button::Style::default();
    // Header: muted title, plus button that adds an account.
    let header = row![
        text(t("Launch profile")).font(BODY).size(T_SM * s).color(c.sub),
        Space::new().width(Length::Fill),
        crate::controller_nav::button(icon(Icon::Plus, (16.0 * s).max(14.0), c.accent))
            .padding(0)
            .style(plain)
            .on_press(Message::NewAccount)
    ]
    .align_y(Alignment::Center);
    // Selected account: a tinted block (a pick_list underneath, so a click
    // anywhere on it opens the account menu) with avatar, name and chevron on top.
    let block_height = 72.0 * s;
    let (name, detail) = match profile(app) {
        Some(row) => (row.account.label.clone(), profile_detail(&row.account)),
        None => (t("No accounts").to_string(), t("Add an account to play").to_string()),
    };
    let hide_name = profile(app).is_some();
    let avatar = container(icon(Icon::User, 19.5 * s, c.accent))
        .width(40.0 * s)
        .height(40.0 * s)
        .center_x(40.0 * s)
        .center_y(40.0 * s)
        .style(move |_| container::Style {
            background: Some(profile_avatar_fill(light).into()),
            border: Border {
                radius: (20.0 * s).into(),
                ..Default::default()
            },
            ..Default::default()
        });
    // Avatar, name/detail and chevron share one centre line.
    let face = container(
        row![
            avatar,
            Space::new().width(SP2 * s),
            column![
                if hide_name {
                    crate::privacy::name(text(name.clone()).font(semibold).size(T_LG * s).color(c.ink), &name, T_LG * s, c.ink)
                } else {
                    text(name).font(semibold).size(T_LG * s).color(c.ink).into()
                },
                text(detail).font(BODY).size(T_SM * s).color(c.sub)
            ]
            .spacing(2.0 * s),
            Space::new().width(Length::Fill),
            icon(Icon::ChevronDown, 14.0 * s, c.sub)
        ]
        .align_y(Alignment::Center),
    )
    .padding(iced::Padding::ZERO.left(SP2 * s).right(SP2 * s))
    .width(Length::Fill)
    .height(block_height)
    .center_y(block_height);
    let block_border = Border {
        color: c.hairline,
        width: 1.0,
        radius: RADIUS_INNER.into(),
    };
    let account_block: Element<'_, Message> = if profiles.is_empty() {
        crate::controller_nav::button(face)
            .padding(0)
            .width(Length::Fill)
            .style(move |_, _| button::Style {
                background: Some(c.tint.into()),
                border: block_border,
                ..Default::default()
            })
            .on_press(Message::NewAccount)
            .into()
    } else {
        let picker = crate::controller_nav::pick_list(profiles, selected, |p| Message::ChooseProfile(p.key))
            .padding([SP1 * s, SP2 * s])
            .text_size(T_MD * s)
            .font(BODY)
            .width(Length::Fill)
            .handle(pick_list::Handle::None)
            .style(profile_pick_style(c.tint, block_border, true, light))
            .menu_style(profile_menu_style(light));
        stack![profile_tall(picker, block_height), face].into()
    };
    // Private-server profiles have one fixed route: xiloader.
    let field_height = 40.0 * s;
    let shown = profile(app);
    let launchers: &'static [LaunchWith] = match shown {
        Some(row) if row.account.mode == AccountMode::Private => &LaunchWith::PRIVATE,
        _ => &LaunchWith::ALL,
    };
    let launcher = crate::controller_nav::pick_list(
        launchers,
        shown.map(|row| app.launcher_for(&row.account))
            .or_else(|| shown.is_none().then_some(app.settings.launch_with.retail())),
        Message::SetLaunchWith,
    )
    .placeholder(t("Client unavailable"))
    .padding([SP1 * s, SP2 * s])
    .text_size(T_MD * s)
    .font(BODY)
    .width(Length::Fill)
    .handle(pick_list::Handle::None)
    .style(profile_pick_style(
        c.well,
        Border {
            color: c.hairline,
            width: 1.0,
            radius: RADIUS_INNER.into(),
        },
        false,
        light,
    ))
    .menu_style(profile_menu_style(light));
    let launcher = stack![
        profile_tall(launcher, field_height),
        container(icon(Icon::ChevronDown, 14.0 * s, c.sub))
            .padding(iced::Padding::ZERO.right(SP2 * s))
            .width(Length::Fill)
            .height(field_height)
            .align_x(Alignment::End)
            .align_y(Alignment::Center)
    ];
    // Manage accounts: the whole row is the link.
    let manage = crate::controller_nav::button(
        row![
            text(t("Manage accounts"))
                .font(semibold)
                .size(T_SM * s)
                .color(c.accent),
            Space::new().width(Length::Fill),
            icon(Icon::ArrowRight, 16.0 * s, c.accent)
        ]
        .align_y(Alignment::Center),
    )
    .padding(0)
    .height(SP2 * s)
    .width(Length::Fill)
    .style(plain)
    .on_press(Message::Navigate(Page::Accounts));
    // 8-pt rhythm: 24 padding, 16 header, 16, block, 24, label, 8, field,
    // then the link pinned to the bottom edge.
    // A taller band spreads the slack over both gaps rather than one hole
    // above the link.
    let card = container(column![
        header,
        Space::new().height(SP2 * s),
        account_block,
        Space::new().height(SP3 * s),
        Space::new().height(Length::FillPortion(1)),
        container(text(t("Launch with")).font(BODY).size(T_SM * s).color(c.sub))
            .height(SP2 * s)
            .align_y(Alignment::Center),
        Space::new().height(SP1 * s),
        launcher,
        Space::new().height(SP3 * s),
        Space::new().height(Length::FillPortion(2)),
        manage
    ])
    .padding(SP3 * s)
    .width(SIDE_W * s)
    .height(Length::Fill)
    .max_height(PROFILE_MAX_H * s)
    .style(panel);
    // In a band taller than the card (a tall window), the card rides on the
    // band's centre line with the wordmark rather than stretching hollow.
    container(card)
        .width(SIDE_W * s)
        .height(Length::Fill)
        .align_y(Alignment::Center)
        .into()
}
/// The launch-profile card's tallest size; beyond it the card stays put and
/// centred while the band grows.
const PROFILE_MAX_H: f32 = 336.0;
fn news_section(app: &App) -> Element<'_, Message> {
    let c = colors(app.settings.light_theme);
    let s = scale(app);
    // Tabs: accent text over a short accent bar; the column is top-padded so
    // the label sits on the title's centre line while the bar hangs below it.
    let mut filters = row![].spacing(39.0 * s).align_y(Alignment::Start);
    for (id, label) in [
        (0, t("Latest")),
        (1, t("Updates")),
        (2, t("Events")),
        (3, t("Maintenance")),
    ] {
        filters = filters.push(news_tab(label, id, app.news_filter == id, s, c));
    }
    let news_header = row![
        container(text(t("PlayOnline news")).size(23.0 * s).color(c.ink)).height(33.0 * s),
        container(icon(Icon::Rss, 16.0 * s, c.sub))
            .height(30.0 * s)
            .center_y(30.0 * s),
        Space::new().width(Length::FillPortion(12)),
        filters,
        Space::new().width(Length::FillPortion(7)),
        container(
            crate::controller_nav::button(icon(Icon::Refresh, 16.0 * s, c.sub))
                .padding(2.0 * s)
                .on_press(Message::Refresh)
                .style(quiet)
        )
        .height(30.0 * s)
        .center_y(30.0 * s)
    ]
    .spacing(28.0 * s)
    .height(36.0 * s)
    .align_y(Alignment::Start);
    // The section's width is known from the window: the page's side padding,
    // the community column and the gap between them are fixed. Cards reflow
    // from three columns to two to one as the window narrows, never squashed.
    let grid = news_grid(app);
    let gap = NEWS_GAP * s;
    let cols = grid.cols;
    // Fill whole rows: 3 cards in three columns, 4 in two, 3 stacked.
    let want = cols * 3usize.div_ceil(cols);
    let mut picked: Vec<(&crate::services::NewsItem, &'static str, Icon)> = Vec::new();
    if let Some(snapshot) = &app.services {
        if app.news_filter == 3 {
            picked.extend(snapshot.notices.iter().take(want).map(portal_notice));
        } else {
            picked.extend(
                snapshot
                    .news
                    .iter()
                    .filter(|n| news_matches(n, app.news_filter))
                    .take(want)
                    .map(portal_article),
            );
        }
    }
    let mut cards = column![].spacing(gap).width(Length::Fill);
    for chunk in picked.chunks(cols) {
        // Reserve the same text blocks across every category and row.
        let text_w = grid.card_w - 2.0 - 2.0 * NEWS_PAD * s;
        let wrapped: Vec<(Vec<String>, Vec<String>)> = chunk
            .iter()
            .map(|(item, _, _)| {
                (
                    wrap_lines(&item.title, text_w, NEWS_TITLE_SIZE * s, NEWS_TITLE_LINES),
                    wrap_lines(&teaser(&item.summary), text_w, NEWS_TEASER_SIZE * s, NEWS_TEASER_LINES),
                )
            })
            .collect();
        let mut line = row![].spacing(gap).width(Length::Fill);
        for ((item, label, placeholder), (title, blurb)) in chunk.iter().zip(wrapped) {
            line = line.push(news_card(
                item,
                label,
                *placeholder,
                grid,
                title,
                blurb,
                c,
            ));
        }
        for _ in chunk.len()..cols {
            line = line.push(Space::new().width(Length::FillPortion(1)));
        }
        cards = cards.push(line);
    }
    if picked.is_empty() {
        cards = cards.push(
            container(
                column![
                    text(if app.news_loading {
                        t("Loading PlayOnline news…")
                    } else {
                        t("No articles in this category.")
                    })
                    .size(18),
                    text(&app.news_error).size(13),
                    action(t("Refresh news"), Message::Refresh)
                ]
                .spacing(15),
            )
            .padding(24)
            .width(Length::Fill)
            .height(grid.height())
            .style(panel),
        );
    }
    let view_all = crate::controller_nav::button(
        row![
            text(t("View all news")).size(12.0 * s),
            icon(Icon::ExternalLink, 13.0 * s, c.accent)
        ]
        .spacing(12.0 * s)
        .align_y(Alignment::Center),
    )
    .padding(0)
    .on_press(Message::OpenUrl(crate::services::sources(crate::i18n::current()).home.into()))
    .style(move |_, status| button::Style {
        text_color: if status == button::Status::Hovered {
            c.ink
        } else {
            c.accent
        },
        ..Default::default()
    });
    column![
        news_header,
        Space::new().height(16.0 * s),
        container(cards).height(grid.height()).width(Length::Fill),
        Space::new().height(18.0 * s),
        view_all
    ]
    .width(Length::Fill)
    .into()
}
/// One filter tab: accent label with a short underline when active.
fn news_tab<'a>(label: &'a str, id: u8, active: bool, s: f32, c: Colors) -> Element<'a, Message> {
    column![
        Space::new().height(8.5 * s),
        crate::controller_nav::button(text(label).size(12.0 * s))
            .padding([0.0, 2.0 * s])
            .on_press(Message::NewsFilter(id))
            .style(move |_, status| button::Style {
                text_color: if active || status == button::Status::Hovered {
                    c.accent
                } else {
                    c.sub
                },
                ..Default::default()
            }),
        Space::new().height(9.0 * s),
        container(Space::new().height(2.0 * s))
            .width(Length::Fill)
            .style(move |_| container::Style {
                background: active.then(|| c.accent.into()),
                border: Border {
                    radius: 1.0.into(),
                    ..Default::default()
                },
                ..Default::default()
            })
    ]
    .width(Length::Shrink)
    .align_x(Alignment::Center)
    .into()
}
fn community(app: &App) -> Element<'_, Message> {
    let c = colors(app.settings.light_theme);
    let s = scale(app);
    let faint = faint(app.settings.light_theme);
    // Mirrors the news column's rhythm: a 36-pt heading band (the heading
    // centred on the news title's centre line), 16 to the cards, the two cards
    // together exactly as tall as a news card, then the footnote on the
    // "View all news" line.
    let card_h = (news_grid(app).card_h - SP2 * s) / 2.0;
    column![
        container(text(t("Community")).font(BODY).size(T_LG * s).color(c.sub))
            .height(30.0 * s)
            .center_y(30.0 * s),
        Space::new().height(22.0 * s),
        resource_card(app, true, card_h, s, c),
        Space::new().height(SP2 * s),
        resource_card(app, false, card_h, s, c),
        Space::new().height(18.0 * s),
        container(
            text("FINAL FANTASY XI © SQUARE ENIX")
                .font(BODY)
                .size(T_XS * s)
                .color(faint)
        )
        .height(16.0 * s)
        .align_right(Length::Fill)
        .center_y(16.0 * s)
    ]
    .width(SIDE_W * s)
    .into()
}
/// The quiet grey used for placeholders, field icons and the copyright line.
fn faint(light: bool) -> Color {
    if light {
        rgb(0x5C7588)
    } else {
        MUTED
    }
}
/// The FFXIAH server picked when the launcher opens.
pub(crate) const DEFAULT_AH_SERVER: &str = "Asura";
/// The active retail worlds with FFXIAH's server ids (its `sid` select).
const AH_SERVERS: [(&str, u8); 16] = [
    ("Asura", 28),
    ("Bahamut", 1),
    ("Bismarck", 25),
    ("Carbuncle", 6),
    ("Cerberus", 23),
    ("Fenrir", 7),
    ("Lakshmi", 27),
    ("Leviathan", 11),
    ("Odin", 12),
    ("Phoenix", 5),
    ("Quetzalcoatl", 16),
    ("Ragnarok", 20),
    ("Shiva", 2),
    ("Siren", 17),
    ("Sylph", 8),
    ("Valefor", 9),
];
const AH_SERVER_NAMES: [&str; 16] = {
    let mut names = [""; 16];
    let mut i = 0;
    while i < 16 {
        names[i] = AH_SERVERS[i].0;
        i += 1;
    }
    names
};
struct WikiSite {
    title: &'static str,
    home: &'static str,
    search_placeholder: &'static str,
}
fn wiki_site() -> WikiSite {
    match crate::i18n::current() {
        crate::i18n::Lang::En => WikiSite {
            title: "BG Wiki", home: "https://www.bg-wiki.com/", search_placeholder: t("Search BG Wiki"),
        },
        crate::i18n::Lang::Ja => WikiSite {
            title: "FF11用語辞典", home: "https://wiki.ffo.jp/", search_placeholder: t("Search FF11 Dictionary"),
        },
    }
}
/// Open the search for the wiki matching the launcher's selected language.
pub(crate) fn wiki_search_url(query: &str) -> String {
    let url = match crate::i18n::current() {
        crate::i18n::Lang::En => {
            let mut url = reqwest::Url::parse("https://www.bg-wiki.com/index.php").expect("static URL");
            url.query_pairs_mut().append_pair("search", query.trim());
            url
        },
        crate::i18n::Lang::Ja => {
            let mut url = reqwest::Url::parse("https://wiki.ffo.jp/search.cgi").expect("static URL");
            url.query_pairs_mut().extend_pairs([
                ("CCC", "愛"), ("Command", "Search"), ("qf", query.trim()),
                ("order", "match"), ("ffotype", "title"), ("type", "title"),
            ]);
            url
        },
    };
    url.into()
}
pub(crate) fn ah_server_id(server: &str) -> Option<u8> {
    AH_SERVERS.iter().find(|(name, _)| *name == server).map(|(_, id)| *id)
}
/// Browser fallback. FFXIAH's saved world cookie overrides this URL's `sid`;
/// the native lookup sends its own isolated cookie instead.
pub(crate) fn ah_search_url(query: &str, server: &str) -> String {
    let mut url = reqwest::Url::parse("https://www.ffxiah.com/search/item").expect("static URL");
    {
        let mut pairs = url.query_pairs_mut();
        pairs.append_pair("q", query.trim());
        if let Some(sid) = ah_server_id(server) {
            pairs.append_pair("sid", &sid.to_string());
        }
    }
    url.into()
}
#[cfg(test)]
mod community_tests {
    use super::*;

    #[test]
    fn wiki_search_encodes_the_query() {
        crate::i18n::set(crate::i18n::Lang::En);
        assert_eq!(
            wiki_search_url("  Sortie gear & aug "),
            "https://www.bg-wiki.com/index.php?search=Sortie+gear+%26+aug"
        );
        assert_eq!(wiki_site().title, "BG Wiki");
        crate::i18n::set(crate::i18n::Lang::Ja);
        let site = wiki_site();
        assert_eq!(site.title, "FF11用語辞典");
        assert_eq!(site.search_placeholder, "FF11用語辞典を検索");
        assert!(crate::allowed_link(site.home));
        let search = wiki_search_url("  サルベージ & 装備  ");
        assert!(crate::allowed_link(&search));
        let url = reqwest::Url::parse(&search).unwrap();
        assert_eq!(url.host_str(), Some("wiki.ffo.jp"));
        assert_eq!(url.path(), "/search.cgi");
        let pairs: std::collections::BTreeMap<_, _> = url.query_pairs().collect();
        assert_eq!(pairs.len(), 6);
        assert_eq!(pairs.get("qf").unwrap(), "サルベージ & 装備");
        assert_eq!(pairs.get("CCC").unwrap(), "愛");
        assert_eq!(pairs.get("Command").unwrap(), "Search");
        crate::i18n::set(crate::i18n::Lang::En);
        assert_eq!(wiki_site().home, "https://www.bg-wiki.com/");
        assert_eq!(wiki_search_url("Sortie"), "https://www.bg-wiki.com/index.php?search=Sortie");
    }

    #[test]
    fn ah_browser_fallback_encodes_the_query_and_world_hint() {
        assert_eq!(
            ah_search_url("Ridill", "Asura"),
            "https://www.ffxiah.com/search/item?q=Ridill&sid=28"
        );
        assert_eq!(
            ah_search_url("Hope Staff", "Bismarck"),
            "https://www.ffxiah.com/search/item?q=Hope+Staff&sid=25"
        );
        assert_eq!(
            ah_search_url("Ridill", "Nowhere"),
            "https://www.ffxiah.com/search/item?q=Ridill"
        );
    }

    #[test]
    fn every_listed_server_has_a_distinct_id() {
        let mut ids: Vec<u8> = AH_SERVERS.iter().map(|(_, id)| *id).collect();
        ids.sort_unstable();
        ids.dedup();
        assert_eq!(ids.len(), AH_SERVERS.len());
        assert_eq!(AH_SERVER_NAMES[0], DEFAULT_AH_SERVER);
        assert!(crate::allowed_link(&wiki_search_url("x")));
        assert!(crate::allowed_link(&ah_search_url("x", "Asura")));
    }
}
fn news_matches(item: &crate::services::NewsItem, filter: u8) -> bool {
    // The feeds carry no category field, so the tabs go by title words. The
    // Japanese titles use the official terms for the same things; each list
    // is checked in both languages, so a title never needs its language known.
    let title = item.title.to_ascii_lowercase();
    let any = |words: &[&str]| words.iter().any(|word| title.contains(word));
    match filter {
        1 => any(&["update", "trial", "バージョンアップ", "アップデート", "更新", "トライアル"]),
        2 => any(&[
            "event", "campaign", "happenings", "bonanza", "イベント", "キャンペーン", "異変", "ボナンザ",
        ]),
        _ => true,
    }
}
fn category(item: &crate::services::NewsItem) -> &'static str {
    if news_matches(item, 1) {
        t("Update")
    } else if item.title.to_ascii_lowercase().contains("bonanza") || item.title.contains("ボナンザ") {
        t("Community")
    } else {
        t("Event")
    }
}
fn portal_article(item: &crate::services::NewsItem) -> (&crate::services::NewsItem, &'static str, Icon) {
    (item, category(item), Icon::Rss)
}
fn portal_notice(item: &crate::services::NewsItem) -> (&crate::services::NewsItem, &'static str, Icon) {
    (item, t("Maintenance"), Icon::Wrench)
}
/// Geometry every news card in the grid shares.
#[derive(Clone, Copy)]
struct NewsGrid {
    s: f32,
    cols: usize,
    /// One card's width in logical px, from the window width.
    card_w: f32,
    /// The banner slot's width / height, shared so a row's images match.
    aspect: f32,
    card_h: f32,
}
impl NewsGrid {
    fn height(self) -> f32 {
        let rows = 3usize.div_ceil(self.cols);
        self.card_h * rows as f32 + NEWS_GAP * self.s * (rows - 1) as f32
    }
}
/// Use the complete feed, so filtering cannot resize the banners or hero.
fn news_grid(app: &App) -> NewsGrid {
    let s = scale(app);
    let width = (app.viewport.width - (96.0 + COL_GUTTER + SIDE_W) * s).max(160.0);
    let gap = NEWS_GAP * s;
    let cols = (1..=3usize).rev()
        .find(|&n| (width - gap * (n - 1) as f32) / n as f32 >= NEWS_MIN_CARD_W)
        .unwrap_or(1);
    let sizes: Vec<_> = app.services.iter()
        .flat_map(|snapshot| snapshot.news.iter().chain(&snapshot.notices))
        .filter_map(|item| item.image.as_ref().and_then(banner_size))
        .collect();
    let aspect = shared_banner_aspect(&sizes);
    let card_w = (width - gap * (cols - 1) as f32) / cols as f32;
    let text_h = 11.0 * 1.3 + 16.0
        + NEWS_TITLE_SIZE * NEWS_TITLE_LH * NEWS_TITLE_LINES as f32
        + NEWS_TEASER_SIZE * NEWS_TEASER_LH * NEWS_TEASER_LINES as f32;
    let card_h = ((card_w - 2.0) / aspect + (2.0 * NEWS_PAD + text_h) * s + 2.0)
        .max(300.0 * s);
    NewsGrid { s, cols, card_w, aspect, card_h }
}
const NEWS_GAP: f32 = 16.0;
const NEWS_PAD: f32 = 16.0;
const NEWS_RADIUS: f32 = 12.0;
/// Narrowest card (logical px) before the grid drops a column.
const NEWS_MIN_CARD_W: f32 = 248.0;
const NEWS_TITLE_SIZE: f32 = 16.0;
const NEWS_TITLE_LH: f32 = 1.3;
const NEWS_TITLE_LINES: usize = 3;
const NEWS_TEASER_SIZE: f32 = 12.0;
const NEWS_TEASER_LH: f32 = 1.45;
const NEWS_TEASER_LINES: usize = 3;
/// Shared card: the whole banner (letterboxed to the grid's aspect), then
/// category · date, the title and the teaser. `title` and `blurb` are already
/// wrapped; fixed text blocks keep empty and short summaries aligned.
fn news_card<'a>(
    item: &'a crate::services::NewsItem,
    label: &'static str,
    placeholder: Icon,
    grid: NewsGrid,
    title: Vec<String>,
    blurb: Vec<String>,
    c: Colors,
) -> Element<'a, Message> {
    let s = grid.s;
    let banner = item
        .image
        .as_ref()
        .and_then(|h| banner_fit(Some(h), grid, c.field))
        .map(|h| (h, true))
        .or_else(|| banner_fit(None, grid, c.select).map(|h| (h, false)));
    // The tiny-skia backend neither clips raster images nor rounds them, so
    // the banner is baked to the slot's aspect with its top corners rounded
    // in the pixels. `expand` sizes the image from the card's width and the
    // banner's aspect, so it is never cropped or stretched.
    let art: Element<'a, Message> = match banner {
        Some((handle, true)) => image(handle)
            .width(Length::Fill)
            .height(Length::Shrink)
            .expand(true)
            .content_fit(ContentFit::Contain)
            .into(),
        Some((handle, false)) => stack![
            image(handle)
                .width(Length::Fill)
                .height(Length::Shrink)
                .expand(true)
                .content_fit(ContentFit::Contain),
            container(
                column![
                    icon(placeholder, 26.0 * s, c.accent),
                    text("PlayOnline").size(11.0 * s).color(c.sub)
                ]
                .spacing(8.0 * s)
                .align_x(Alignment::Center),
            )
            .center(Length::Fill)
        ]
        .into(),
        None => Space::new()
            .width(Length::Fill)
            .height((grid.card_w - 2.0) / grid.aspect)
            .into(),
    };
    let semibold = Font {
        weight: iced::font::Weight::Semibold,
        ..BODY
    };
    let title_size = NEWS_TITLE_SIZE * s;
    let teaser_size = NEWS_TEASER_SIZE * s;
    let title_h = NEWS_TITLE_LINES as f32 * title_size * NEWS_TITLE_LH;
    let blurb_h = NEWS_TEASER_LINES as f32 * teaser_size * NEWS_TEASER_LH;
    let body = column![
        row![
            text(label).size(11.0 * s).font(semibold).color(c.accent),
            Space::new().width(Length::Fill),
            text(wrap_lines(&news_date(&item.date), grid.card_w * 0.45, 11.0 * s, 1).join(""))
                .size(11.0 * s).color(c.sub)
        ]
        .height(11.0 * 1.3 * s)
        .align_y(Alignment::Center),
        Space::new().height(8.0 * s),
        container(text(title.join("\n"))
            .size(title_size)
            .line_height(NEWS_TITLE_LH)
            .width(Length::Fill)
            .color(c.ink))
            .height(title_h).width(Length::Fill).clip(true),
        Space::new().height(8.0 * s),
        container(text(blurb.join("\n"))
            .size(teaser_size)
            .line_height(NEWS_TEASER_LH)
            .width(Length::Fill)
            .color(c.sub))
            .height(blurb_h).width(Length::Fill).clip(true),
    ]
    .width(Length::Fill);
    crate::controller_nav::button(column![art, container(body).padding(NEWS_PAD * s)])
        .padding(1)
        .width(Length::FillPortion(1))
        .height(grid.card_h)
        .clip(true)
        .on_press(Message::OpenUrl(item.url.clone()))
        .style(move |_, status| button::Style {
            background: Some(c.card.into()),
            text_color: c.ink,
            border: Border {
                color: if status == button::Status::Hovered {
                    c.accent
                } else {
                    c.line
                },
                width: 1.0,
                radius: NEWS_RADIUS.into(),
            },
            ..Default::default()
        })
        .into()
}
fn image_key(handle: &image::Handle) -> u64 {
    use std::hash::{Hash, Hasher};
    let mut hasher = std::collections::hash_map::DefaultHasher::new();
    handle.id().hash(&mut hasher);
    hasher.finish()
}
/// A news image's pixel size. Feed images arrive decoded; embedded ones are
/// only probed (header read), once per image.
fn banner_size(handle: &image::Handle) -> Option<(u32, u32)> {
    use std::sync::{Mutex, OnceLock};
    static SIZES: OnceLock<Mutex<HashMap<u64, (u32, u32)>>> = OnceLock::new();
    if let image::Handle::Rgba { width, height, .. } = handle {
        return Some((*width, *height));
    }
    let key = image_key(handle);
    let sizes = SIZES.get_or_init(Default::default);
    if let Some(hit) = sizes.lock().ok()?.get(&key) {
        return Some(*hit);
    }
    let size = match handle {
        image::Handle::Bytes(_, bytes) => {
            imagecrate::ImageReader::new(std::io::Cursor::new(bytes.as_ref()))
                .with_guessed_format()
                .ok()?
                .into_dimensions()
                .ok()?
        }
        image::Handle::Path(_, path) => imagecrate::image_dimensions(path).ok()?,
        image::Handle::Rgba { .. } => unreachable!(),
    };
    if let Ok(mut map) = sizes.lock() {
        map.insert(key, size);
    }
    Some(size)
}
/// The one banner aspect a grid uses: the candidate that wastes the least
/// slot area across the shown images, kept between 6:5 and 5:1 so an odd
/// image cannot make every card huge or a sliver. Images of another aspect
/// are letterboxed, never cropped.
fn shared_banner_aspect(sizes: &[(u32, u32)]) -> f32 {
    let aspects: Vec<f32> = sizes
        .iter()
        .filter(|(w, h)| *w > 0 && *h > 0)
        .map(|(w, h)| (*w as f32 / *h as f32).clamp(1.2, 5.0))
        .collect();
    let waste = |slot: f32| -> f32 {
        aspects
            .iter()
            .map(|a| 1.0 - (a / slot).min(slot / a))
            .sum()
    };
    aspects
        .iter()
        .copied()
        .min_by(|a, b| waste(*a).total_cmp(&waste(*b)).then(b.total_cmp(a)))
        .unwrap_or(16.0 / 9.0)
}
/// Returns `source` scaled whole into a `grid.aspect` canvas filled with
/// `fill` (a plain placeholder when `source` is `None`), sized for the card's
/// width at up to 2x, with the top corners rounded to the card's inner radius.
/// Cached per image, canvas size and fill; the work happens once per image
/// and width step.
fn banner_fit(source: Option<&image::Handle>, grid: NewsGrid, fill: Color) -> Option<image::Handle> {
    use std::sync::{Mutex, OnceLock};
    static CACHE: OnceLock<Mutex<BannerCache>> = OnceLock::new();
    CACHE.get_or_init(|| Mutex::new(BannerCache::new())).lock().ok()?.fit(source, grid, fill)
}

type BannerKey = (u64, u32, u32, u32, [u8; 4]);
const FITTED_BANNER_BYTES: usize = 24 * 1_048_576;
const DECODED_BANNER_BYTES: usize = 12 * 1_048_576;

struct BannerCache {
    fitted: Lru<BannerKey, image::Handle>,
    decoded: Lru<u64, std::sync::Arc<imagecrate::RgbaImage>>,
}
impl BannerCache {
    fn new() -> Self {
        Self {
            // Enough for three visible banners at their largest 2x size;
            // resizing and switching feeds must not retain every old canvas.
            fitted: Lru::new(64, FITTED_BANNER_BYTES),
            decoded: Lru::new(12, DECODED_BANNER_BYTES),
        }
    }

    fn fit(&mut self, source: Option<&image::Handle>, grid: NewsGrid, fill: Color) -> Option<image::Handle> {
        let display_w = grid.card_w - 2.0;
        if display_w < 1.0 || !(grid.aspect > 0.0) {
            return None;
        }
        // Width steps of 64 px keep a window drag from re-baking every frame;
        // iced scales the bucketed texture to the exact card width on the GPU.
        let canvas_w = (((display_w * 2.0) / 64.0).ceil() as u32 * 64).clamp(64, 1536);
        let canvas_h = ((canvas_w as f32 / grid.aspect).round() as u32).max(1);
        // The radius depends on the bucket alone (not the exact card width), so
        // every width in a bucket shares one cache entry. Past the 1536 px cap
        // the on-screen scale is bucketed too.
        let shown_w = if display_w * 2.0 > 1536.0 {
            (display_w / 64.0).ceil() * 64.0
        } else {
            canvas_w as f32 / 2.0
        };
        let radius = ((NEWS_RADIUS - 1.0) * canvas_w as f32 / shown_w).round();
        let rgba8 = fill.into_rgba8();
        let key = (source.map_or(0, image_key), canvas_w, canvas_h, radius as u32, rgba8);
        if let Some(hit) = self.fitted.get(&key) {
            return Some(hit.clone());
        }
        let mut out = imagecrate::RgbaImage::from_pixel(canvas_w, canvas_h, imagecrate::Rgba(rgba8));
        if let Some(source) = source {
            // Feed images already own decoded RGBA pixels. Borrow those directly;
            // only encoded images need a separate decoded cache.
            let decoded = if matches!(source, image::Handle::Rgba { .. }) {
                None
            } else {
                let src_key = image_key(source);
                Some(match self.decoded.get(&src_key).cloned() {
                    Some(hit) => hit,
                    None => {
                        let img = match source {
                            image::Handle::Bytes(_, bytes) => imagecrate::load_from_memory(bytes).ok()?.to_rgba8(),
                            image::Handle::Path(_, path) => imagecrate::open(path).ok()?.to_rgba8(),
                            image::Handle::Rgba { .. } => unreachable!(),
                        };
                        let img = std::sync::Arc::new(img);
                        self.decoded.insert(src_key, img.clone(), img.as_raw().len());
                        img
                    }
                })
            };
            let (width, height, pixels) = match source {
                image::Handle::Rgba { width, height, pixels, .. } => (*width, *height, pixels.as_ref()),
                _ => {
                    let rgba = decoded.as_ref()?;
                    (rgba.width(), rgba.height(), rgba.as_raw().as_slice())
                }
            };
            let rgba = imagecrate::ImageBuffer::<imagecrate::Rgba<u8>, _>::from_raw(width, height, pixels)?;
            let (sw, sh) = (rgba.width() as f32, rgba.height() as f32);
            if sw < 1.0 || sh < 1.0 {
                return None;
            }
            // Contain: the whole image, centred, on the fill.
            let k = (canvas_w as f32 / sw).min(canvas_h as f32 / sh);
            let (fw, fh) = (
                ((sw * k).round() as u32).clamp(1, canvas_w),
                ((sh * k).round() as u32).clamp(1, canvas_h),
            );
            let offset = (((canvas_w - fw) / 2) as i64, ((canvas_h - fh) / 2) as i64);
            if (fw, fh) == (rgba.width(), rgba.height()) {
                imagecrate::imageops::overlay(&mut out, &rgba, offset.0, offset.1);
            } else {
                let fitted = imagecrate::imageops::resize(&rgba, fw, fh, imagecrate::imageops::FilterType::CatmullRom);
                imagecrate::imageops::overlay(&mut out, &fitted, offset.0, offset.1);
            }
        }
        // Anti-aliased top corners, in image pixels.
        let r = radius;
        let ri = r.ceil() as u32;
        let ow = out.width();
        for y in 0..ri.min(out.height()) {
            for x in 0..ri.min(ow) {
                let dx = r - (x as f32 + 0.5);
                let dy = r - (y as f32 + 0.5);
                let cover = (r - (dx * dx + dy * dy).sqrt() + 0.5).clamp(0.0, 1.0);
                if cover < 1.0 {
                    for px in [x, ow - 1 - x] {
                        let p = out.get_pixel_mut(px, y);
                        p.0[3] = (p.0[3] as f32 * cover).round() as u8;
                    }
                }
            }
        }
        let bytes = out.as_raw().len();
        let handle = image::Handle::from_rgba(out.width(), out.height(), out.into_raw());
        self.fitted.insert(key, handle.clone(), bytes);
        Some(handle)
    }
}
/// Image caches need a byte limit as well as an entry limit: a window resize
/// can otherwise keep dozens of multi-megabyte canvases alive.
struct Lru<K, V> {
    cap: usize,
    max_bytes: usize,
    bytes: usize,
    tick: u64,
    map: std::collections::HashMap<K, (V, u64, usize)>,
}
impl<K: std::hash::Hash + Eq + Clone, V> Lru<K, V> {
    fn new(cap: usize, max_bytes: usize) -> Self {
        Self {
            cap,
            max_bytes,
            bytes: 0,
            tick: 0,
            map: Default::default(),
        }
    }
    fn get(&mut self, key: &K) -> Option<&V> {
        self.tick += 1;
        let tick = self.tick;
        self.map.get_mut(key).map(|(v, used, _)| {
            *used = tick;
            &*v
        })
    }
    fn insert(&mut self, key: K, value: V, bytes: usize) {
        if let Some((_, _, old_bytes)) = self.map.remove(&key) {
            self.bytes -= old_bytes;
        }
        if bytes > self.max_bytes { return; }
        self.tick += 1;
        self.map.insert(key, (value, self.tick, bytes));
        self.bytes += bytes;
        while self.map.len() > self.cap || self.bytes > self.max_bytes {
            let oldest = self
                .map
                .iter()
                .min_by_key(|(_, (_, used, _))| *used)
                .map(|(k, _)| k.clone());
            match oldest {
                Some(k) => {
                    if let Some((_, _, bytes)) = self.map.remove(&k) { self.bytes -= bytes; }
                }
                None => break,
            };
        }
    }
}
/// Approximate advance of `ch` in Segoe UI, in ems. Slightly generous, so a
/// line judged to fit really does.
fn glyph_em(ch: char) -> f32 {
    match ch {
        ' ' => 0.28,
        'i' | 'j' | 'l' | 'I' | '.' | ',' | ':' | ';' | '\'' | '’' | '!' | '|' => 0.26,
        'f' | 't' | 'r' | '(' | ')' | '[' | ']' | '"' | '“' | '”' | '/' => 0.36,
        '-' => 0.40,
        's' | 'c' | 'z' | 'J' => 0.47,
        'm' | 'M' | 'W' | '—' => 0.9,
        'w' | '&' | '%' | '@' => 0.76,
        'a'..='z' => 0.56,
        'A'..='Z' => 0.66,
        '0'..='9' => 0.56,
        // Kana, kanji and full-width forms are one em wide.
        '\u{1100}'..='\u{115f}'
        | '\u{2e80}'..='\u{a4cf}'
        | '\u{ac00}'..='\u{d7a3}'
        | '\u{f900}'..='\u{faff}'
        | '\u{fe30}'..='\u{fe6f}'
        | '\u{ff00}'..='\u{ff60}'
        | '\u{ffe0}'..='\u{ffe6}' => 1.0,
        _ => 0.62,
    }
}
fn text_em(line: &str) -> f32 {
    line.chars().map(glyph_em).sum()
}
/// Word-wraps `text` into lines that fit `width` px at `size` px, keeping at
/// most `max_lines`; text beyond the last line ends in an ellipsis. Words
/// too long for a line are broken between characters.
fn wrap_lines(text: &str, width: f32, size: f32, max_lines: usize) -> Vec<String> {
    let max_em = (width / size.max(1.0)).max(1.0);
    // Split over-long words so every piece fits a line on its own.
    let mut words: Vec<String> = Vec::new();
    for word in text.split_whitespace() {
        if text_em(word) <= max_em {
            words.push(word.to_owned());
            continue;
        }
        let mut piece = String::new();
        for ch in word.chars() {
            if !piece.is_empty() && text_em(&piece) + glyph_em(ch) > max_em {
                words.push(std::mem::take(&mut piece));
            }
            piece.push(ch);
        }
        if !piece.is_empty() {
            words.push(piece);
        }
    }
    let mut lines: Vec<String> = Vec::new();
    let mut current = String::new();
    for word in &words {
        if current.is_empty() {
            current.push_str(word);
        } else if text_em(&current) + glyph_em(' ') + text_em(word) <= max_em {
            current.push(' ');
            current.push_str(word);
        } else {
            lines.push(std::mem::replace(&mut current, word.clone()));
        }
    }
    if !current.is_empty() {
        lines.push(current);
    }
    if max_lines > 0 && lines.len() > max_lines {
        lines.truncate(max_lines);
        let last = lines.last_mut().expect("max_lines > 0");
        while !last.is_empty() && text_em(last) + glyph_em('…') > max_em {
            match last.rfind(' ') {
                Some(cut) => last.truncate(cut),
                None => {
                    last.pop();
                }
            }
        }
        let trimmed = last
            .trim_end_matches(|ch: char| ch.is_whitespace() || ",;:-–—".contains(ch))
            .len();
        last.truncate(trimmed);
        last.push('…');
    }
    lines
}
#[cfg(test)]
mod news_card_tests {
    use super::*;

    fn grid(card_w: f32, aspect: f32) -> NewsGrid {
        NewsGrid { s: 1.0, cols: 3, card_w, aspect, card_h: 300.0 }
    }

    #[test]
    fn decoded_feed_pixels_are_borrowed_and_match_encoded_banners() {
        let pixels = imagecrate::RgbaImage::from_fn(256, 128, |x, y| {
            imagecrate::Rgba([x as u8, y as u8, 140, 255])
        });
        let mut png = std::io::Cursor::new(Vec::new());
        imagecrate::DynamicImage::ImageRgba8(pixels.clone())
            .write_to(&mut png, imagecrate::ImageFormat::Png).unwrap();
        let raw = image::Handle::from_rgba(256, 128, pixels.into_raw());
        let encoded = image::Handle::from_bytes(png.into_inner());
        let mut cache = BannerCache::new();
        let size = grid(130.0, 2.0);
        let raw_banner = cache.fit(Some(&raw), size, Color::BLACK).unwrap();
        assert!(cache.decoded.map.is_empty(), "feed pixels must not be copied into another cache");
        let encoded_banner = cache.fit(Some(&encoded), size, Color::BLACK).unwrap();
        let image::Handle::Rgba { width, height, pixels, .. } = &raw_banner else { panic!() };
        let image::Handle::Rgba { pixels: encoded_pixels, .. } = &encoded_banner else { panic!() };
        assert_eq!((*width, *height), (256, 128));
        assert_eq!(pixels.as_ref(), encoded_pixels.as_ref());
        assert_eq!(&pixels[(64 * 256 + 100) * 4..][..4], &[100, 64, 140, 255]);
        assert_eq!(pixels[3], 0, "top corners remain rounded");
        assert_eq!(pixels[(127 * 256) * 4 + 3], 255, "bottom corners remain square");
        assert_eq!(cache.fit(Some(&raw), size, Color::BLACK).unwrap().id(), raw_banner.id());
    }

    #[test]
    fn banner_cache_stays_bounded_across_window_resizes_and_themes() {
        let mut cache = BannerCache::new();
        for aspect in [1.2, 2.0, 3.0] {
            for card_w in (258..=770).step_by(64) {
                for fill in [Color::BLACK, Color::WHITE, DEEP] {
                    cache.fit(None, grid(card_w as f32, aspect), fill).unwrap();
                    assert!(cache.fitted.bytes <= FITTED_BANNER_BYTES);
                    assert!(cache.fitted.map.len() <= 64);
                }
            }
        }
        assert!(cache.fitted.map.len() < 64, "large canvases must hit the byte limit first");
        assert!(cache.decoded.map.is_empty());
    }

    #[test]
    fn image_cache_accounts_for_replacements_and_recently_used_entries() {
        let mut cache = Lru::new(10, 10);
        cache.insert(1, "first", 4);
        cache.insert(2, "second", 4);
        assert_eq!(cache.get(&1), Some(&"first"));
        cache.insert(3, "third", 4);
        assert_eq!(cache.get(&2), None);
        assert_eq!(cache.bytes, 8);
        cache.insert(1, "smaller replacement", 1);
        assert_eq!(cache.bytes, 5);
        cache.insert(4, "too large to retain", 11);
        assert_eq!(cache.get(&4), None);
        assert_eq!(cache.bytes, 5);
    }

    #[test]
    fn maintenance_metadata_uses_a_short_date() {
        assert_eq!(news_date("Sep. 26, 2026 22:00 [PDT] From: FINAL FANTASY XI"), "Sep 26");
        assert_eq!(news_date("Sep. 7, 2026 22:00 [PDT] From: FINAL FANTASY XI"), "Sep 7");
        assert_eq!(news_date("Mon, 05 Oct 2026 03:00:00 +0900"), "Oct 5");
        assert_eq!(news_date("2026-10-05T03:00:00Z"), "Oct 5");
        assert_eq!(news_date("Unknown"), "Unknown");
    }

    #[test]
    fn dates_are_worded_for_the_ui_language_and_keep_the_sources_own_day() {
        let en = [
            ("Wed, 30 Sep 2026 17:00:00 JST", "Sep 30"),
            ("2026.09.28(月) 23:50 From: プレイオンライン", "Sep 28"),
            ("Sep. 26, 2026 22:00 [PDT] From: FINAL FANTASY XI", "Sep 26"),
        ];
        let ja = [
            ("Wed, 30 Sep 2026 17:00:00 JST", "9月30日"),
            ("2026.09.28(月) 23:50 From: プレイオンライン", "9月28日"),
            ("2026.10.01(木) 00:10 From: ファイナルファンタジーXI", "10月1日"),
            ("Sep. 26, 2026 22:00 [PDT] From: FINAL FANTASY XI", "9月26日"),
            ("Mon, 05 Oct 2026 03:00:00 +0900", "10月5日"),
            ("2026-10-05T03:00:00Z", "10月5日"),
        ];
        crate::i18n::set(crate::i18n::Lang::En);
        for (raw, shown) in en {
            assert_eq!(news_date(raw), shown, "{raw}");
        }
        // A Japanese list date shows in English too (a cache never crosses languages,
        // but the formatter does not depend on that).
        crate::i18n::set(crate::i18n::Lang::Ja);
        for (raw, shown) in ja {
            assert_eq!(news_date(raw), shown, "{raw}");
        }
        // 00:10 JST on Oct 1 stays Oct 1: the day is the source's, not this PC's.
        assert_eq!(news_date("Thu, 01 Oct 2026 00:10:00 JST"), "10月1日");
        assert_eq!(news_date("Unknown"), "Unknown");
        assert_eq!(news_date(""), "");
        crate::i18n::set(crate::i18n::Lang::En);
    }

    #[test]
    fn japanese_titles_fall_into_the_same_tabs() {
        let item = |title: &str| crate::services::NewsItem { title: title.into(), ..Default::default() };
        crate::i18n::set(crate::i18n::Lang::Ja);
        for (title, update, event, label) in [
            ("10月バージョンアップのお知らせ", true, false, "Update"),
            ("フリートライアルのレベル上限が75に！ プレイ期間が無制限になりました！", true, false, "Update"),
            ("ファイナルファンタジーXI 更新のお知らせ(9/10)", true, false, "Update"),
            ("モーグリの新米冒険者＆復帰者応援キャンペーン ", false, true, "Event"),
            ("ヴァナ・ディールの異変を再調査せよ ", false, true, "Event"),
            ("「真夏のノマドモグボナンザ2026」当せん番号発表！", false, true, "Community"),
            ("「第2回 『FFXI』冒険者意識調査」を実施します！", false, false, "Event"),
        ] {
            let item = item(title);
            assert_eq!(news_matches(&item, 1), update, "{title}");
            assert_eq!(news_matches(&item, 2), event, "{title}");
            assert!(news_matches(&item, 0), "{title}");
            assert_eq!(category(&item), crate::i18n::t(label), "{title}");
        }
        crate::i18n::set(crate::i18n::Lang::En);
        // English titles keep their tabs.
        assert!(news_matches(&item("Free Trial Level Cap Raised to Level 75"), 1));
        assert!(news_matches(&item("More Strange Happenings in Vana'diel"), 2));
        assert_eq!(category(&item("Mog Bonanza results")), "Community");
    }

    #[test]
    fn japanese_text_wraps_by_em_width_inside_the_card() {
        // Full-width characters are a whole em, so a 20-em line holds 20 of them.
        let title = "フリートライアルのレベル上限が75に！プレイ期間が無制限になりました！";
        let lines = wrap_lines(title, 20.0 * 16.0, 16.0, 3);
        assert!(lines.len() >= 2 && lines.len() <= 3, "{lines:?}");
        for line in &lines {
            assert!(text_em(line) <= 20.0 + 1e-3, "{line}");
        }
        assert_eq!(lines.concat().replace(' ', ""), title);
        assert!(text_em("ああ") > text_em("aa") * 1.5);
    }

    #[test]
    fn short_titles_stay_whole_and_long_ones_end_in_an_ellipsis() {
        let whole = wrap_lines("Free Trial Level Cap Raised to Level 75 and Time Limit Removed", 280.0, 16.0, 3);
        assert!(whole.len() <= 3);
        assert_eq!(whole.join(" "), "Free Trial Level Cap Raised to Level 75 and Time Limit Removed");
        let long = "word ".repeat(60);
        let cut = wrap_lines(&long, 200.0, 16.0, 3);
        assert_eq!(cut.len(), 3);
        assert!(cut[2].ends_with('…'));
        for line in &cut {
            assert!(text_em(line) <= 200.0 / 16.0);
        }
    }

    #[test]
    fn unbroken_text_is_split_rather_than_overflowing() {
        let lines = wrap_lines(&"x".repeat(80), 100.0, 10.0, 0);
        assert!(lines.len() > 1);
        assert!(lines.iter().all(|line| text_em(line) <= 10.0));
    }

    #[test]
    fn the_shared_aspect_fits_the_common_banner() {
        assert!((shared_banner_aspect(&[(600, 200), (600, 200), (640, 360)]) - 3.0).abs() < 1e-3);
        assert!((shared_banner_aspect(&[]) - 16.0 / 9.0).abs() < 1e-3);
        assert!((shared_banner_aspect(&[(100, 1000)]) - 1.2).abs() < 1e-3);
    }
}
fn resource_card<'a>(app: &'a App, wiki: bool, height: f32, s: f32, c: Colors) -> Element<'a, Message> {
    use crate::icons::{icon, Icon};
    let faint = faint(app.settings.light_theme);
    let (title, url) = if wiki {
        let site = wiki_site();
        (site.title, site.home)
    } else {
        ("FFXIAH", "https://www.ffxiah.com/")
    };
    let link = move |content: Element<'a, Message>| {
        crate::controller_nav::button(content)
            .padding(0)
            .on_press(Message::OpenUrl(url.into()))
            .style(|_, _| button::Style::default())
    };
    // Glyph, name and link glyph share one centre line; the subtitle hangs
    // under the name; the search well sits on the card's bottom padding.
    let title_row = row![
        link(icon(
            if wiki { Icon::BookOpen } else { Icon::Scale },
            20.0 * s,
            c.accent
        )),
        Space::new().width(12.0 * s),
        link(
            text(title)
                .font(Font {
                    weight: iced::font::Weight::Semibold,
                    ..BODY
                })
                .size(T_LG * s)
                .color(c.ink)
                .into()
        ),
        Space::new().width(Length::Fill),
        link(icon(Icon::ExternalLink, 16.0 * s, faint))
    ]
    .height(SP3 * s)
    .align_y(Alignment::Center);
    let subtitle = text(if wiki {
        t("Guides, missions & equipment")
    } else {
        t("Items, prices & sales history")
    })
    .font(BODY)
    .size(T_SM * s)
    .color(c.sub);
    container(column![
        title_row,
        Space::new().height(2.0 * s),
        subtitle,
        Space::new().height(Length::Fill),
        search_field(app, wiki, s, c, faint),
    ])
    .padding([20.0 * s, SP3 * s])
    .width(Length::Fill)
    .height(height)
    .style(panel)
    .into()
}
/// The 37px search well: icon and input inside one bordered field, plus the
/// FFXIAH server picker behind a hairline divider.
fn search_field<'a>(
    app: &'a App,
    wiki: bool,
    s: f32,
    c: Colors,
    faint: Color,
) -> Element<'a, Message> {
    use crate::icons::{icon, Icon};
    let input = text_input(
        if wiki { wiki_site().search_placeholder } else { t("Find an item") },
        if wiki { &app.wiki_query } else { &app.ah_query },
    )
    .on_input(if wiki {
        Message::WikiQueryChanged
    } else {
        Message::AhQueryChanged
    })
    .on_submit(if wiki {
        Message::SearchWiki
    } else {
        Message::SearchAh
    })
    .font(BODY)
    .size(T_SM * s)
    .padding(0)
    .width(Length::Fill)
    .style(move |_, _| text_input::Style {
        background: Color::TRANSPARENT.into(),
        border: Border::default(),
        icon: faint,
        placeholder: faint,
        value: c.ink,
        selection: Color { a: 0.35, ..c.accent },
    });
    let mut field = row![
        Space::new().width(SP2 * s),
        icon(Icon::Search, 13.0 * s, faint),
        Space::new().width(SP1 * s + 2.0 * s),
        input,
    ]
    .height(Length::Fill)
    .align_y(Alignment::Center);
    if wiki {
        field = field.push(Space::new().width(SP2 * s));
    } else {
        let server = crate::controller_nav::pick_list(
            &AH_SERVER_NAMES[..],
            Some(app.ah_server),
            Message::AhServerChanged,
        )
        .font(BODY)
        .text_size(T_SM * s)
        .padding(iced::Padding::ZERO.left(12.0 * s).right(30.0 * s))
        .width(Length::Fill)
        .handle(pick_list::Handle::None)
        .style(move |_, _| pick_list::Style {
            text_color: c.sub,
            placeholder_color: faint,
            handle_color: faint,
            background: Color::TRANSPARENT.into(),
            border: Border::default(),
        })
        .menu_style(move |_| iced::widget::overlay::menu::Style {
            background: c.panel.into(),
            border: Border {
                color: c.line,
                width: 1.0,
                radius: RADIUS_INNER.into(),
            },
            text_color: c.ink,
            selected_text_color: c.ink,
            selected_background: c.select.into(),
            shadow: Shadow {
                color: c.shadow,
                offset: Vector::new(0.0, 6.0),
                blur_radius: 18.0,
            },
        });
        field = field
            .push(Space::new().width(SP1 * s))
            .push(
                container(Space::new())
                    .width(1)
                    .height(SP2 * s)
                    .style(move |_| container::Style {
                        background: Some(c.hairline.into()),
                        ..Default::default()
                    }),
            )
            .push(
                // The overlaid chevron takes no input.
                stack![
                    container(server).height(Length::Fill).center_y(Length::Fill),
                    container(icon(Icon::ChevronDown, 12.0 * s, faint))
                        .padding(iced::Padding::ZERO.right(12.0 * s))
                        .align_right(Length::Fill)
                        .center_y(Length::Fill)
                ]
                .width(144.0 * s)
                .height(Length::Fill),
            );
    }
    // A soft inset well, not a second outlined box inside the card.
    container(field)
        .height(36.0 * s)
        .width(Length::Fill)
        .style(move |_| container::Style {
            background: Some(c.well.into()),
            border: Border {
                color: c.hairline,
                width: 1.0,
                radius: RADIUS_INNER.into(),
            },
            ..Default::default()
        })
        .into()
}
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum PlayState {
    Play,
    Starting,
    Playing,
}
/// What the big Play button shows. `launchable` is how many selected accounts could
/// start now; with some already running and others still launchable the button stays
/// a launch button, so the rest can be started.
fn play_state(starting: bool, running: usize, launchable: usize) -> PlayState {
    if launchable > 0 {
        PlayState::Play
    } else if starting {
        PlayState::Starting
    } else if running > 0 && launchable == 0 {
        PlayState::Playing
    } else {
        PlayState::Play
    }
}
fn play_bar(app: &App) -> Element<'_, Message> {
    let c = colors(app.settings.light_theme);
    let s = scale(app);
    let light = app.settings.light_theme;
    let current = profile(app);
    let main_action = app.main_action();
    // The accounts Play acts on: the selected ones, else the launch profile.
    let targets: Vec<&crate::AccountRow> = {
        let selected: Vec<_> = app.accounts.iter().filter(|a| a.selected).collect();
        if selected.is_empty() {
            current.into_iter().collect()
        } else {
            selected
        }
    };
    let play_state = play_state(
        targets.iter().any(|a| a.game_start_pending()),
        targets.iter().filter(|a| a.is_playing()).count(),
        app.selected_count(),
    );
    let playing_row = targets.iter().find(|a| a.is_playing());
    let starting_row = targets.iter().find(|a| a.game_start_pending());
    let running = targets.iter().filter(|a| a.is_playing()).count();
    // Status badge: soft round tint + glyph, coloured by state.
    let (status, detail, tone): (String, String, u8) = if let Some(kind) = app.maintenance {
        // Crystal's own file work, with its progress: never worded like Square Enix's
        // server maintenance.
        (maintenance_status(app), dl_stage(kind, &app.maintenance_progress.stage), 2)
    } else if main_action == MainAction::Update {
        ((if app.viewer.as_ref().is_some_and(crate::viewer::Status::requires_update) { t("PlayOnline needs attention") } else { t("Update available") }).into(), t("Click Update to install official updates").into(), 1)
    } else if matches!(main_action, MainAction::Maintenance | MainAction::Offline) && targets.iter().any(|a| app.is_waiting(&a.key)) {
        // "Wait and play": what the waiting Play will do.
        ((if main_action == MainAction::Maintenance { t("Maintenance") } else { t("Worlds offline") }).into(), app.wait_headline().into(), 2)
    } else if main_action == MainAction::Maintenance {
        (t("Maintenance").into(), t("View official maintenance notices").into(), 1)
    } else if main_action == MainAction::Offline {
        (t("Worlds offline").into(), t("View official world status").into(), 1)
    } else if let (PlayState::Playing, Some(row)) = (play_state, playing_row) {
        let who = if running > 1 {
            tf("{running} accounts", &[("running", &running)])
        } else {
            row.account.label.clone()
        };
        (tf("Playing · {who}", &[("who", &who)]), t("Client running · continue in game").into(), 0)
    } else if let (PlayState::Starting, Some(row)) = (play_state, starting_row) {
        let step = if row.status.is_empty() { t("Signing in").into() } else { row.status_text().into_owned() };
        (tf("Starting · {name}", &[("name", &row.account.label)]), step, 2)
    } else {
        let (s, d, t) = match app.patch.as_ref().map(|p| p.state) {
            Some(crate::patch::PatchState::Current) => {
                (t("Ready to play"), if app.patch.as_ref().is_some_and(|p| p.files_verified == Some(true)) {
                    t("Game files verified")
                } else { t("Game version is up to date") }, 0)
            }
            Some(crate::patch::PatchState::Required) => {
                (t("Update available"), t("Press Update to install it"), 1)
            }
            Some(crate::patch::PatchState::Interrupted) => (t("Interrupted update"), t("Press Play to finish it"), 1),
            _ if app.patch_loading => (t("Checking version…"), t("Reading the installed version"), 2),
            _ if app.patch.as_ref().is_some_and(|p| p.files_verified == Some(true)) => (t("Files verified"), t("Version will be checked when you play"), 0),
            _ => (t("Ready to launch"), t("Version will be checked when you play"), 2),
        };
        (s.into(), d.into(), t)
    };
    let (badge_fill, badge_ink) = match (tone, light) {
        (0, false) => (rgb(0x1F403E), rgb(0xA6D4BE)),
        (0, true) => (rgb(0xDDECE5), rgb(0x347355)),
        (1, false) => (rgb(0x433822), rgb(0xE2BE7A)),
        (1, true) => (rgb(0xF4EAD5), rgb(0x8A6420)),
        (_, false) => (c.select, c.sub),
        (_, true) => (c.select, c.sub),
    };
    let glyph: Element<'_, Message> = match tone {
        0 => shape(r#"<path d="M2.2 6.3 4.8 8.9 9.9 3.8" stroke-width="1.3"/>"#, 12.0, 12.0, badge_ink),
        1 => icon(Icon::AlertCircle, 15.0, badge_ink),
        _ => icon(Icon::Help, 15.0, badge_ink),
    };
    let badge = container(glyph)
        .center(SP3 * s)
        .style(move |_| container::Style {
            background: Some(badge_fill.into()),
            border: Border {
                radius: (SP3 * s / 2.0).into(),
                ..Default::default()
            },
            ..Default::default()
        });
    let link_style = |theme: &Theme, status: button::Status| {
        let c = tc(theme);
        let hot = matches!(status, button::Status::Hovered | button::Status::Pressed);
        button::Style {
            background: hot.then(|| c.select.into()),
            text_color: if hot { c.ink } else { c.sub },
            border: Border {
                radius: 6.0.into(),
                ..Default::default()
            },
            ..Default::default()
        }
    };
    // During downtime the button sets the selected accounts waiting ("Wait and play"), and says
    // so once every one of them waits.
    let wait_label = (app.vault_ready && !app.wait_keys().is_empty()).then(|| t("Wait and play"))
        .or_else(|| targets.iter().any(|a| app.is_waiting(&a.key)).then(|| t("Waiting for FFXI…")));
    let (play_label, play_ink) = match main_action {
        MainAction::Update => (t("Update").to_owned(), c.play_text),
        MainAction::Maintenance | MainAction::Offline if wait_label.is_some() => (wait_label.unwrap_or_default().to_owned(), badge_ink),
        MainAction::Maintenance => (t("Maintenance").to_owned(), badge_ink),
        MainAction::Offline => (t("Worlds offline").to_owned(), badge_ink),
        MainAction::Working if app.reshade.busy => (t("ReShade setup").to_owned(), c.sub),
        MainAction::Working => (match maintenance_percent(&app.maintenance_progress) {
            Some(percent) if app.maintenance.is_some() => tf("{label} · {percent}%", &[("label", &app.maintenance_label()), ("percent", &percent)]),
            _ => app.maintenance_label().to_owned(),
        }, c.sub),
        MainAction::Play => match play_state {
        PlayState::Playing => (t("Playing").to_owned(), if light { rgb(0x347355) } else { rgb(0xA6D4BE) }),
        PlayState::Starting => (t("Starting…").to_owned(), c.sub),
        PlayState::Play if app.selected_count() > 1 => {
            (tf("Play selected ({count})", &[("count", &app.selected_count())]), c.play_text)
        }
        PlayState::Play => (t("Play Final Fantasy XI").to_owned(), c.play_text),
        },
    };
    let play_glyph: Element<'_, Message> = match main_action {
        MainAction::Update => icon(Icon::Download, 19.0 * s, play_ink),
        MainAction::Maintenance => icon(Icon::Wrench, 19.0 * s, play_ink),
        MainAction::Offline => icon(Icon::AlertCircle, 19.0 * s, play_ink),
        MainAction::Working => icon(Icon::Refresh, 19.0 * s, play_ink),
        MainAction::Play => match play_state {
        // The path is drawn in the scaled box's own units (shape's viewBox is
        // w x h), so the triangle is never clipped at small scales.
        PlayState::Play => shape(
            &format!(
                r##"<path d="M0 0 {w} {m} 0 {h}Z" fill="#ffffff" stroke="none"/>"##,
                w = 13.0 * s,
                m = 9.5 * s,
                h = 19.0 * s
            ),
            13.0 * s,
            19.0 * s,
            c.play_text,
        ),
        PlayState::Playing => shape(
            r#"<path d="M2.2 6.3 4.8 8.9 9.9 3.8" stroke-width="1.3"/>"#,
            19.0 * s,
            19.0 * s,
            play_ink,
        ),
        PlayState::Starting => Space::new().into(),
        },
    };
    let play = crate::controller_nav::button(stack![
        container(text(play_label).font(SEMI).size(T_LG * s).color(play_ink))
            .padding(iced::Padding::ZERO.left(SP1 * s))
            .center(Length::Fill),
        container(play_glyph)
            .padding(iced::Padding::ZERO.left(SP3 * s + 4.0 * s))
            .height(Length::Fill)
            .align_y(Alignment::Center)
    ])
    .padding(0)
    .height(56.0 * s)
    .width(SIDE_W * s)
    .style(move |_theme: &Theme, status| {
        let (first, last) = if light {
            (rgb(0x205C78), rgb(0x184C66))
        } else {
            (rgb(0xADDCEC), rgb(0x88BDD7))
        };
        let lift = |col: Color| {
            let k = if light { 0.10 } else { 0.06 };
            Color::from_rgb(
                col.r + (1.0 - col.r) * k,
                col.g + (1.0 - col.g) * k,
                col.b + (1.0 - col.b) * k,
            )
        };
        let background = match status {
            _ if matches!(main_action, MainAction::Maintenance | MainAction::Offline) => badge_fill.into(),
            _ if main_action == MainAction::Working => c.select.into(),
            // Playing is a settled, quiet green; Starting and an unavailable Play are flat.
            _ if main_action == MainAction::Play && play_state == PlayState::Playing => {
                (if light { rgb(0xDDECE5) } else { rgb(0x1F403E) }).into()
            }
            button::Status::Disabled => c.select.into(),
            button::Status::Hovered => gradient::Linear::new(2.356)
                .add_stop(0.0, lift(first))
                .add_stop(1.0, lift(last))
                .into(),
            _ => gradient::Linear::new(2.356)
                .add_stop(0.0, first)
                .add_stop(1.0, last)
                .into(),
        };
        button::Style {
            background: Some(background),
            text_color: c.play_text,
            border: Border {
                radius: RADIUS.into(),
                ..Default::default()
            },
            shadow: if status == button::Status::Disabled
                || !matches!(main_action, MainAction::Play | MainAction::Update)
                || main_action == MainAction::Play && play_state != PlayState::Play {
                Shadow::default()
            } else {
                Shadow {
                    color: with_alpha(first, if light { 0.28 } else { 0.18 }),
                    offset: Vector::new(0.0, 4.0),
                    blur_radius: 16.0,
                }
            },
            ..Default::default()
        }
    })
    .on_press_maybe(
        (main_action != MainAction::Play || app.vault_ready && app.selected_count() > 0).then_some(Message::MainAction),
    );
    let mut info = row![
        container(
            row![
                badge,
                Space::new().width(SP2 * s),
                column![
                    private_text(app, &status, T_MD * s, SEMI, c.ink),
                    private_text(app, &detail, T_SM * s, BODY, c.sub)
                ]
                .spacing(2.0 * s)
            ]
            .align_y(Alignment::Center)
        )
        .width(264.0 * s),
        container(Space::new())
            .width(1)
            .height(SP4 * s)
            .style(move |_| container::Style {
                background: Some(c.hairline.into()),
                ..Default::default()
            }),
        Space::new().width(SP2 * s),
        crate::controller_nav::button(
            row![
                icon(Icon::Wrench, 14.0 * s, c.sub),
                Space::new().width(SP1 * s),
                text(t("Install & repair")).font(BODY).size(T_SM * s)
            ]
            .align_y(Alignment::Center)
        )
        .padding([SP1 * s, 12.0 * s])
        .style(link_style)
        .on_press(Message::Navigate(Page::Maintenance)),
        Space::new().width(SP3 * s),
        crate::controller_nav::button(text(t("Help & support")).font(BODY).size(T_SM * s))
            .padding([SP1 * s, 12.0 * s])
            .style(link_style)
            .on_press(Message::OpenUrl(
                "https://support.na.square-enix.com/".into()
            )),
        Space::new().width(Length::Fill)
    ]
    .align_y(Alignment::Center);
    if app.viewport.width >= 1150.0 {
        info = info.push(
            column![
                match current {
                    Some(a) => name_text(&a.account.label, T_SM * s, SEMI, c.ink),
                    None => text(t("Choose an account")).font(SEMI).size(T_SM * s).color(c.ink).into(),
                },
                text(app.selected_launcher(current))
                .font(BODY)
                .size(T_SM * s)
                .color(c.sub)
            ]
            .spacing(2.0 * s)
            .align_x(Alignment::End),
        );
    }
    // Part of the window surface, not a slab: the surface shows through a
    // faint lift, a hairline marks the top edge, and a soft upward shadow
    // gives it gentle elevation. Side padding is the content's, so the badge
    // lines up with the wordmark and the Play button with the right column.
    let light_theme = light;
    let bar: Element<'_, Message> = container(
        row![
            container(info).width(Length::Fill),
            Space::new().width(COL_GUTTER * s),
            play
        ]
        .align_y(Alignment::Center),
    )
    .padding([0.0, 48.0 * s])
    .width(Length::Fill)
    .height(96.0 * s)
    .center_y(96.0 * s)
    .style(move |_| container::Style {
        background: Some(
            gradient::Linear::new(std::f32::consts::PI)
                .add_stop(0.0, if light_theme { rgba(0xFFFFFF, 0.60) } else { rgba(0x9DD5EA, 0.035) })
                .add_stop(1.0, if light_theme { rgba(0xFFFFFF, 0.30) } else { rgba(0x9DD5EA, 0.01) })
                .into(),
        ),
        text_color: Some(c.ink),
        border: Border {
            color: Color::TRANSPARENT,
            width: 0.0,
            radius: 0.0.into(),
        },
        shadow: Shadow {
            color: c.shadow,
            offset: Vector::new(0.0, -2.0),
            blur_radius: 18.0,
        },
        ..Default::default()
    })
    .into();
    column![
        container(Space::new())
            .width(Length::Fill)
            .height(1)
            .style(move |_| container::Style {
                background: Some(c.hairline.into()),
                ..Default::default()
            }),
        bar
    ]
    .into()
}
fn compact_status(app: &App) -> (&'static str, Color, &'static str, Color) {
    let (service, service_color) = match app.service_availability() {
        crate::services::Availability::Operational => {
            (t("Worlds online"), Color::from_rgb8(72, 184, 104))
        }
        crate::services::Availability::Maintenance => (t("Maintenance"), GOLD_BRIGHT),
        crate::services::Availability::PartialOutage => (t("World disruption"), GOLD_BRIGHT),
        crate::services::Availability::Outage => {
            (t("Worlds offline"), Color::from_rgb8(220, 91, 91))
        }
        _ => (
            if app.news_loading {
                t("Checking services")
            } else {
                t("Status unconfirmed")
            },
            MUTED,
        ),
    };
    let (patch, patch_color) = match app.patch.as_ref().map(|p| &p.state) {
        Some(crate::patch::PatchState::Current) => (t("Version current"), CRYSTAL),
        Some(crate::patch::PatchState::Required) => {
            (t("Update available · press Update"), GOLD_BRIGHT)
        }
        Some(crate::patch::PatchState::Interrupted) => (t("Interrupted update · press Play"), GOLD_BRIGHT),
        _ => (
            if app.patch_loading {
                t("Checking version")
            } else {
                t("Version unchecked")
            },
            MUTED,
        ),
    };
    (service, service_color, patch, patch_color)
}
// ---- the shared page kit --------------------------------------
// One vocabulary for Accounts, Downloads and every dialog: a page header,
// cards on the page surface, 40 px controls and one dialog frame. Spacing
// stays on an 8-pt scale; cards use radius 12, controls radius 8.
pub(crate) const CONTROL_H: f32 = 40.0;
pub(crate) const CONTROL_R: f32 = 8.0;
const CARD_R: f32 = 12.0;
// Accounts/Downloads share the Home GUTTER token.
/// The right-hand column of a page, as wide as Home's launch-profile column.
fn side_width(s: f32) -> f32 {
    341.0 * s
}
pub(crate) fn is_light(c: Colors) -> bool {
    c.bg.r > 0.5
}
pub(crate) fn mix(a: Color, b: Color, t: f32) -> Color {
    Color {
        r: a.r + (b.r - a.r) * t,
        g: a.g + (b.g - a.g) * t,
        b: a.b + (b.b - a.b) * t,
        a: a.a + (b.a - a.a) * t,
    }
}
/// Page title and muted description, in the same place on every page.
pub(crate) fn page_header<'a>(
    c: Colors,
    s: f32,
    title: &'a str,
    description: &'a str,
    trailing: Option<Element<'a, Message>>,
) -> Element<'a, Message> {
    let mut header = row![column![
        text(title).font(BODY).size(26.0 * s).color(c.ink),
        text(description).font(BODY).size(14.0 * s).color(c.sub)
    ]
    .spacing(6.0 * s)
    .width(Length::Fill)]
    .spacing(16)
    .align_y(Alignment::Center);
    if let Some(trailing) = trailing {
        header = header.push(trailing);
    }
    header.into()
}
/// Header, main column and side column; narrow windows stack the side column.
fn page_layout<'a>(
    app: &App,
    header: Element<'a, Message>,
    main: Element<'a, Message>,
    side: Element<'a, Message>,
) -> Element<'a, Message> {
    let s = scale(app);
    let body: Element<'a, Message> = if app.viewport.width >= 1150.0 {
        row![
            container(main).width(Length::Fill),
            container(side).width(side_width(s))
        ]
        .spacing(GUTTER * s)
        .into()
    } else {
        column![main, side].spacing(GUTTER * s).into()
    };
    column![header, body]
        .spacing(GUTTER * s)
        .padding(iced::Padding::ZERO.bottom(GUTTER * s))
        .width(Length::Fill)
        .into()
}
/// A card floating on the page surface: soft fill, hairline, radius 12.
pub(crate) fn card_style(c: Colors) -> impl Fn(&Theme) -> container::Style {
    move |_| container::Style {
        background: Some(c.panel.into()),
        text_color: Some(c.ink),
        border: Border {
            color: c.line,
            width: 1.0,
            radius: CARD_R.into(),
        },
        shadow: Shadow {
            color: Color::from_rgba(0.0, 0.0, 0.0, if is_light(c) { 0.05 } else { 0.18 }),
            offset: Vector::new(0.0, 2.0),
            blur_radius: 10.0,
        },
        ..Default::default()
    }
}
fn card<'a>(c: Colors, content: impl Into<Element<'a, Message>>) -> container::Container<'a, Message> {
    container(content)
        .padding(GUTTER)
        .width(Length::Fill)
        .style(card_style(c))
}
/// Card heading: semibold title over an optional muted line.
fn card_title<'a>(c: Colors, title: &'a str, body: Option<&'a str>) -> Element<'a, Message> {
    let mut block = column![text(title).font(SEMI).size(16).color(c.ink)].spacing(4);
    if let Some(body) = body {
        block = block.push(text(body).font(BODY).size(13).color(c.sub));
    }
    block.width(Length::Fill).into()
}
/// A borderless tinted tile for notes inside cards and dialogs.
pub(crate) fn soft_tile(c: Colors) -> impl Fn(&Theme) -> container::Style {
    move |_| container::Style {
        background: Some(with_alpha(c.select, if is_light(c) { 0.7 } else { 0.55 }).into()),
        border: Border {
            radius: CONTROL_R.into(),
            ..Default::default()
        },
        ..Default::default()
    }
}
#[derive(Clone, Copy, PartialEq, Eq)]
pub(crate) enum Tone {
    /// The one filled call to action in a region.
    Primary,
    /// Bordered secondary action.
    Secondary,
    /// Filled destructive confirmation.
    Danger,
    /// Quiet destructive action.
    DangerLink,
}
pub(crate) fn control_style(c: Colors, tone: Tone) -> impl Fn(&Theme, button::Status) -> button::Style {
    move |_, status| {
        let hot = matches!(status, button::Status::Hovered | button::Status::Pressed);
        let off = status == button::Status::Disabled;
        let radius = CONTROL_R.into();
        match tone {
            Tone::Primary => button::Style {
                background: Some(
                    if off {
                        c.select
                    } else if hot {
                        mix(c.play, c.play_text, 0.1)
                    } else {
                        c.play
                    }
                    .into(),
                ),
                text_color: if off { with_alpha(c.sub, 0.8) } else { c.play_text },
                border: Border {
                    radius,
                    ..Default::default()
                },
                ..Default::default()
            },
            Tone::Secondary => button::Style {
                background: Some(if hot && !off { c.select } else { c.field }.into()),
                text_color: if off { with_alpha(c.sub, 0.7) } else { c.ink },
                border: Border {
                    color: if hot && !off { with_alpha(c.accent, 0.55) } else { c.line },
                    width: 1.0,
                    radius,
                },
                ..Default::default()
            },
            Tone::Danger => button::Style {
                background: Some(if hot { rgb(0xB55353) } else { rgb(0xC25B5B) }.into()),
                text_color: Color::WHITE,
                border: Border {
                    radius,
                    ..Default::default()
                },
                ..Default::default()
            },
            Tone::DangerLink => button::Style {
                background: hot.then(|| with_alpha(dl_danger(c), 0.12).into()),
                text_color: dl_danger(c),
                border: Border {
                    radius,
                    ..Default::default()
                },
                ..Default::default()
            },
        }
    }
}
/// A 40 px button in one of the shared tones, with an optional leading icon.
/// `width` fixes the width and centres the label.
pub(crate) fn control<'a>(
    c: Colors,
    tone: Tone,
    glyph: Option<Icon>,
    label: impl iced::widget::text::IntoFragment<'a>,
    message: Option<Message>,
    width: Option<f32>,
) -> Element<'a, Message> {
    let enabled = message.is_some();
    let tint = match tone {
        Tone::Primary if enabled => c.play_text,
        Tone::Danger => Color::WHITE,
        Tone::DangerLink => dl_danger(c),
        _ if enabled => c.accent,
        _ => with_alpha(c.sub, 0.6),
    };
    let mut content = row![].spacing(8).align_y(Alignment::Center);
    if let Some(glyph) = glyph {
        content = content.push(icon(glyph, 15.0, tint));
    }
    content = content.push(
        text(label)
            .font(if tone == Tone::Primary { SEMI } else { BODY })
            .size(14),
    );
    let inner = container(content).height(Length::Fill).align_y(Alignment::Center);
    let inner = match width {
        Some(_) => inner.width(Length::Fill).align_x(Alignment::Center),
        None => inner,
    };
    let mut b = crate::controller_nav::button(inner)
        .height(CONTROL_H)
        .padding([0.0, if tone == Tone::DangerLink { 10.0 } else { 16.0 }])
        .style(control_style(c, tone))
        .on_press_maybe(message);
    if let Some(width) = width {
        b = b.width(width);
    }
    b.into()
}
/// Full-width card link, the same shape as Home's "Manage accounts" row.
fn card_link<'a>(c: Colors, label: &'a str, message: Message) -> Element<'a, Message> {
    crate::controller_nav::button(
        row![
            text(label).font(SEMI).size(13).color(c.accent),
            Space::new().width(Length::Fill),
            icon(Icon::ArrowRight, 16.0, c.accent)
        ]
        .align_y(Alignment::Center),
    )
    .padding(0)
    .height(24)
    .width(Length::Fill)
    .style(|_, _| button::Style::default())
    .on_press(message)
    .into()
}
/// A small inline text link under an account's status.
fn log_link<'a>(c: Colors, label: &'a str, message: Message) -> Element<'a, Message> {
    crate::controller_nav::button(text(label).font(SEMI).size(12).color(c.accent))
        .padding(0)
        .style(|_, _| button::Style::default())
        .on_press(message)
        .into()
}
/// A square 40 px icon button: bordered (secondary) or bare (ghost).
pub(crate) fn icon_control<'a>(c: Colors, glyph: Icon, bordered: bool, message: Option<Message>) -> Element<'a, Message> {
    let tint = match (message.is_some(), bordered) {
        (false, _) => with_alpha(c.sub, 0.5),
        (true, true) => c.accent,
        (true, false) => c.sub,
    };
    crate::controller_nav::button(container(icon(glyph, 16.0, tint)).center(CONTROL_H))
        .padding(0)
        .width(CONTROL_H)
        .height(CONTROL_H)
        .style(move |theme, status| {
            if bordered {
                control_style(c, Tone::Secondary)(theme, status)
            } else {
                button::Style {
                    background: matches!(status, button::Status::Hovered | button::Status::Pressed)
                        .then(|| with_alpha(c.select, 0.8).into()),
                    text_color: c.sub,
                    border: Border {
                        radius: CONTROL_R.into(),
                        ..Default::default()
                    },
                    ..Default::default()
                }
            }
        })
        .on_press_maybe(message)
        .into()
}
/// The one text field style: 40 px tall, field fill, hairline that warms on focus.
pub(crate) fn text_field<'a>(placeholder: &str, value: &str) -> text_input::TextInput<'a, Message> {
    text_input(placeholder, value)
        .font(BODY)
        .size(14)
        .line_height(iced::widget::text::LineHeight::Absolute(iced::Pixels(18.0)))
        .padding([11, 14])
        .style(input)
}

// ---- Connection details dialog ----
/// Icon and label for the service state, shared by the details dialog and the
/// Accounts side card.
fn service_state<'a>(app: &App, c: Colors) -> (Element<'a, Message>, &'static str) {
    let (service, _, _, _) = compact_status(app);
    let glyph = match app.service_availability() {
        crate::services::Availability::Operational => icon(Icon::CheckCircle, 16.0, dl_ok(c)),
        crate::services::Availability::Outage => icon(Icon::AlertCircle, 16.0, dl_danger(c)),
        crate::services::Availability::Unknown => icon(Icon::AlertCircle, 16.0, c.sub),
        _ => icon(Icon::AlertCircle, 16.0, c.accent),
    };
    (glyph, service)
}
fn patch_state<'a>(app: &App, c: Colors) -> (Element<'a, Message>, &'static str) {
    match app.patch.as_ref().map(|p| p.state) {
        Some(crate::patch::PatchState::Current) => (icon(Icon::CheckCircle, 16.0, dl_ok(c)), t("Version current")),
        Some(crate::patch::PatchState::Required) => (icon(Icon::Download, 16.0, c.accent), t("Update available")),
        Some(crate::patch::PatchState::Interrupted) => (icon(Icon::AlertCircle, 16.0, c.accent), t("Interrupted update")),
        _ if app.patch_loading => (icon(Icon::Refresh, 16.0, c.sub), t("Checking version")),
        _ => (icon(Icon::AlertCircle, 16.0, c.sub), t("Version unchecked")),
    }
}
/// Caption over an icon and a value.
fn status_block<'a>(c: Colors, caption: &'a str, glyph: Element<'a, Message>, value: &'a str) -> Column<'a> {
    column![
        dl_label(c, caption),
        row![glyph, text(value).font(BODY).size(15).color(c.ink)]
            .spacing(8)
            .align_y(Alignment::Center)
    ]
    .spacing(8)
}
type Column<'a> = iced::widget::Column<'a, Message>;
fn details<'a>(app: &'a App, base: Element<'a, Message>) -> Element<'a, Message> {
    let c = colors(app.settings.light_theme);
    let (service_icon, service) = service_state(app, c);
    let (patch_icon, patch) = patch_state(app, c);
    let mut service_block = status_block(c, t("Service"), service_icon, service).width(Length::Fill);
    if let Some(s) = &app.services {
        service_block = service_block.push(
            column![
                text(app.service_note()).size(12).color(c.sub),
                text(tf("Last checked: {time}", &[("time", &s.checked_at)])).size(12).color(c.sub)
            ]
            .spacing(2),
        );
    }
    let mut patch_block = status_block(c, t("Game version"), patch_icon, patch).width(Length::Fill);
    if let Some(p) = &app.patch {
        patch_block = patch_block.push(
            private_text(
                app,
                if matches!(p.state, crate::patch::PatchState::Required) {
                    t("A newer official version is available. Press Update to install it.")
                } else {
                    &p.detail
                },
                12.0,
                BODY,
                c.sub,
            ),
        );
    }
    let mut body = column![row![service_block, patch_block].spacing(GUTTER)].spacing(20);
    if let Some(p) = &app.patch {
        if !p.local_version.is_empty() || !p.remote_version.is_empty() {
            body = body.push(
                container(
                    row![
                        dl_stat(c, t("Installed"), text(&p.local_version).font(MONO).size(13).color(c.ink).into()),
                        dl_stat(c, t("Latest"), text(&p.remote_version).font(MONO).size(13).color(c.ink).into())
                    ]
                    .spacing(GUTTER),
                )
                .padding([12, 16])
                .width(Length::Fill)
                .style(soft_tile(c)),
            );
        }
    }
    body = body.push(row![
        container(control(c, Tone::Secondary, Some(Icon::ArrowRight), t("Official world status"),
            Some(Message::OpenUrl(crate::services::WORLD_STATUS_URL.into())), None)).width(Length::Fill),
        container(control(c, Tone::Secondary, Some(Icon::ArrowRight), t("Maintenance notices"),
            Some(Message::OpenUrl(crate::services::sources(crate::i18n::current()).maintenance.into())), None)).width(Length::Fill),
    ].spacing(8)).push(dl_divider(c)).push(
        column![dl_label(c, t("Latest activity")), private_text(app, &app.status, 14.0, BODY, c.ink)].spacing(8),
    );
    let footer = row![
        Space::new().width(Length::Fill),
        control(
            c,
            Tone::Secondary,
            Some(Icon::ArrowRight),
            t("Open Downloads"),
            Some(Message::Navigate(Page::Maintenance)),
            None
        ),
        control(c, Tone::Primary, None, t("Done"), Some(Message::ToggleDetails), None)
    ]
    .spacing(8)
    .align_y(Alignment::Center);
    modal(
        base,
        dialog_header(
            c,
            t("Connection & activity"),
            t("Service status, your game version and the launcher's latest message."),
            Some(Message::ToggleDetails),
        ),
        Some(body.into()),
        Some(footer.into()),
        600.0,
        app.viewport.height - 48.0,
    )
}
// ---- Accounts page, account editor and close confirmation ----
pub(crate) fn with_alpha(color: Color, a: f32) -> Color {
    Color { a, ..color }
}
fn semibold() -> Font {
    SEMI
}
/// Muted secondary text that follows the active theme.
fn sub_text<'a>(value: impl iced::widget::text::IntoFragment<'a>, size: f32) -> iced::widget::Text<'a> {
    text(value)
        .font(BODY)
        .size(size)
        .style(|theme: &Theme| iced::widget::text::Style {
            color: Some(tc(theme).sub),
        })
}
pub(crate) fn check(theme: &Theme, status: checkbox::Status) -> checkbox::Style {
    let c = tc(theme);
    let (checked, hot) = match status {
        checkbox::Status::Active { is_checked } => (is_checked, false),
        checkbox::Status::Hovered { is_checked } => (is_checked, true),
        checkbox::Status::Disabled { is_checked } => (is_checked, false),
    };
    checkbox::Style {
        background: if checked { c.accent } else { c.field }.into(),
        icon_color: c.play_text,
        border: Border {
            color: if checked || hot {
                c.accent
            } else {
                with_alpha(c.sub, 0.55)
            },
            width: 1.5,
            radius: 5.0.into(),
        },
        text_color: None,
    }
}
pub(crate) fn input(theme: &Theme, status: text_input::Status) -> text_input::Style {
    let c = tc(theme);
    let border = match status {
        text_input::Status::Focused { .. } => c.accent,
        text_input::Status::Hovered => with_alpha(c.accent, 0.45),
        _ => c.line,
    };
    text_input::Style {
        background: c.field.into(),
        border: Border {
            color: border,
            width: 1.0,
            radius: CONTROL_R.into(),
        },
        icon: c.sub,
        placeholder: with_alpha(c.sub, 0.75),
        value: c.ink,
        selection: with_alpha(c.accent, 0.3),
    }
}
pub(crate) fn hline<'a>() -> Element<'a, Message> {
    container(Space::new().height(1))
        .width(Length::Fill)
        .style(|theme: &Theme| container::Style {
            background: Some(tc(theme).line.into()),
            ..Default::default()
        })
        .into()
}
fn avatar<'a>(c: Colors, raised: bool, size: f32) -> Element<'a, Message> {
    container(icon(Icon::User, size * 0.46, c.accent))
        .width(size)
        .height(size)
        .align_x(Alignment::Center)
        .align_y(Alignment::Center)
        .style(move |_| container::Style {
            background: Some(if raised { c.panel } else { c.select }.into()),
            border: Border {
                color: if raised {
                    with_alpha(c.accent, 0.45)
                } else {
                    Color::TRANSPARENT
                },
                width: 1.0,
                radius: (size / 2.0).into(),
            },
            ..Default::default()
        })
        .into()
}
fn region_name(region: u8) -> &'static str {
    match region {
        1 => "EU",
        2 => "JP",
        _ => "US",
    }
}
fn accounts(app: &App) -> Element<'_, Message> {
    let c = colors(app.settings.light_theme);
    let s = scale(app);
    let header = page_header(
        c,
        s,
        t("Accounts"),
        t("Choose who plays together, then launch them in one click."),
        Some(row![
            control(c, Tone::Secondary, Some(Icon::Download), t("Import accounts"),
                (app.vault_ready && !app.vault_saving).then_some(Message::AccountImport(crate::account_import_ui::Msg::Open)), None),
            control(c, Tone::Secondary, Some(Icon::Plus), t("Add account"), Some(Message::NewAccount), None)
        ].spacing(8).into()),
    );
    let main = if app.accounts.is_empty() {
        empty_accounts(c)
    } else {
        account_list(app, c)
    };
    let side = column![connection_card(app, c), security_card(c)].spacing(GUTTER * s);
    inset_scroll(page_layout(app, header, main, side.into()))
        .height(Length::Fill)
        .into()
}
fn account_list(app: &App, c: Colors) -> Element<'_, Message> {
    let total = app.accounts.len();
    let ticked = app.accounts.iter().filter(|a| a.selected).count();
    let ready = app.selected_count();
    let can_launch = app.vault_ready && app.launchable_count() > 0;
    let selected_enabled = can_launch && ready > 0;
    let toolbar = row![
        column![
            text(t("Your accounts")).font(SEMI).size(16).color(c.ink),
            text(if total == 1 {
                tf("{total} account · {ticked} selected", &[("total", &total), ("ticked", &ticked)])
            } else {
                tf("{total} accounts · {ticked} selected", &[("total", &total), ("ticked", &ticked)])
            })
            .font(BODY)
            .size(13)
            .color(c.sub),
            sub_text(t("Drag the handle to reorder. The order is saved automatically."), 12.0)
        ]
        .spacing(4)
        .width(Length::Fill),
        control(c, Tone::Secondary, None, t("Launch all"), can_launch.then_some(Message::LaunchAll), None),
        control(
            c,
            Tone::Primary,
            Some(Icon::Play),
            tf("Launch selected ({count})", &[("count", &ready)]),
            selected_enabled.then_some(Message::LaunchSelected),
            None
        )
    ]
    .spacing(8)
    .align_y(Alignment::Center);
    let mut list = column![].spacing(4);
    for item in &app.accounts {
        list = list.push(account_row(app, item, c));
    }
    container(column![
        container(toolbar).padding([20, 24]),
        hline(),
        container(list).padding(12)
    ])
    .width(Length::Fill)
    .style(card_style(c))
    .into()
}
fn account_row<'a>(app: &'a App, item: &'a crate::AccountRow, c: Colors) -> Element<'a, Message> {
    let idle = !item.busy && item.pid.is_none();
    let dragging = app.account_drag.source.as_deref() == Some(item.key.as_str());
    let grip = iced::widget::mouse_area(container(icon(Icon::Grip, 18.0, if dragging { c.accent } else { c.sub })).padding([12, 3]))
        .on_press(Message::AccountDragStart(item.key.clone()))
        .interaction(if dragging { iced::mouse::Interaction::Grabbing } else { iced::mouse::Interaction::Grab });
    let picked = item.selected;
    let key = item.key.clone();
    let pick = crate::controller_nav::checkbox(item.selected)
        .on_toggle(move |checked| Message::ToggleAccount(key.clone(), checked))
        .size(18)
        .style(check);
    let subtitle = if item.account.mode == AccountMode::Retail {
        tf(
            "Retail · {region} · {client}",
            &[
                ("region", &region_name(item.account.region)),
                ("client", &app.launcher_for(&item.account)),
            ],
        )
    } else if item.account.server.is_empty() {
        t("Private server").to_owned()
    } else {
        // Private servers always use the native client; the server name says enough.
        tf("Private server · {server}", &[("server", &item.account.server)])
    };
    let identity = column![
        name_text(&item.account.label, 15.0, SEMI, c.ink),
        text(subtitle).font(BODY).size(13).color(c.sub)
    ]
    .spacing(2)
    .width(Length::FillPortion(3));
    let dot = |filled: bool, color: Color| -> Element<'a, Message> {
        container(Space::new().width(8).height(8))
            .style(move |_| container::Style {
                background: filled.then(|| color.into()),
                border: Border {
                    color,
                    width: if filled { 0.0 } else { 1.0 },
                    radius: 4.0.into(),
                },
                ..Default::default()
            })
            .into()
    };
    let (tone, marker): (Color, Element<'a, Message>) = if item.is_playing() {
        let ok = dl_ok(c);
        (ok, icon(Icon::CheckCircle, 15.0, ok))
    } else if item.busy {
        (c.accent, dot(true, c.accent))
    } else if item.problem {
        let bad = dl_danger(c);
        (bad, icon(Icon::AlertCircle, 15.0, bad))
    } else {
        (c.sub, dot(false, with_alpha(c.sub, 0.7)))
    };
    // "Wait and play": the wait (with Cancel), or the update its released Play asked for.
    let waiting = app.is_waiting(&item.key);
    let updating = app.waited_update_running(&item.key);
    let shown: std::borrow::Cow<'_, str> = if waiting {
        app.wait_headline().into()
    } else if updating {
        t("Updating FFXI before it starts…").into()
    } else {
        item.status_text()
    };
    let (tone, marker) = if waiting || updating { (c.accent, dot(true, c.accent)) } else { (tone, marker) };
    let status_line = row![marker, private_text(app, &shown, 13.0, BODY, tone)]
        .spacing(10)
        .align_y(Alignment::Center);
    // How long the launch took, once the game has started; aligned under the status text.
    let timing_line: Option<Element<'a, Message>> = (item.is_playing() && !item.timing.is_empty())
        .then(|| container(text(&item.timing).font(BODY).size(12).color(c.sub)).padding([0, 25]).into());
    // Launch diagnostics: once a client has run, its log is one click away.
    let status = if waiting || updating {
        let detail = if waiting {
            app.wait_detail()
        } else {
            match maintenance_percent(&app.maintenance_progress) {
                Some(percent) => tf("{label} · {percent}%", &[("label", &app.maintenance_label()), ("percent", &percent)]),
                None => app.maintenance_label().to_owned(),
            }
        };
        let mut lines = column![status_line].spacing(4).width(Length::FillPortion(3));
        if !detail.is_empty() {
            lines = lines.push(container(text(detail).font(BODY).size(12).color(c.sub)).padding([0, 25]));
        }
        if waiting {
            lines = lines.push(container(log_link(c, t("Cancel"), Message::CancelWait(vec![item.key.clone()]))).padding([0, 25]));
        }
        lines
    } else if item.log.is_some() && idle {
        column![
            status_line,
            row![
                log_link(c, t("View log"), Message::OpenLog(item.key.clone())),
                log_link(c, t("Open logs folder"), Message::OpenLogsFolder(item.key.clone()))
            ]
            .spacing(14)
            .padding([0, 25])
        ]
        .spacing(4)
        .width(Length::FillPortion(3))
    } else {
        column![status_line].extend(timing_line).spacing(2).width(Length::FillPortion(3))
    };
    let otp: Element<'a, Message> = if item.account.mode == AccountMode::Retail && idle {
        let key = item.key.clone();
        text_field(t("One-time code"), &item.otp)
            .secure(true)
            .on_input(move |s| Message::OtpChanged(key.clone(), s))
            .width(152)
            .into()
    } else {
        Space::new().width(152).into()
    };
    let can_launch = item.can_play() && app.vault_ready && !app.launch_blocked() && !waiting;
    let launch = control(
        c,
        Tone::Secondary,
        Some(Icon::Play),
        if item.game_start_pending() {
            t("Launching…")
        } else if item.is_playing() {
            t("Running")
        } else {
            t("Launch")
        },
        can_launch.then(|| Message::Launch(item.key.clone())),
        Some(124.0),
    );
    let edit = icon_control(c, Icon::Edit, false, (!app.vault_saving).then(|| Message::EditAccount(item.key.clone())));
    let sign_out = control(c, Tone::Secondary, None, t("Sign out"),
        (item.can_sign_out() && !app.launch_blocked()).then(|| Message::AccountSignOut(item.key.clone())), Some(100.0));
    iced::widget::mouse_area(container(
        row![grip, pick, avatar(c, picked, 40.0), identity, status, otp, launch, sign_out, edit]
            .spacing(16)
            .align_y(Alignment::Center),
    )
    .padding([12, 12])
    .width(Length::Fill)
    .style(move |_| container::Style {
        background: picked.then(|| with_alpha(c.select, if is_light(c) { 0.75 } else { 0.6 }).into()),
        border: Border {
            color: if dragging { c.accent } else { Color::TRANSPARENT },
            width: if dragging { 1.0 } else { 0.0 },
            radius: CONTROL_R.into(),
            ..Default::default()
        },
        ..Default::default()
    }))
    .on_enter(Message::AccountDragOver(item.key.clone()))
    .into()
}
fn empty_accounts<'a>(c: Colors) -> Element<'a, Message> {
    container(
        column![
            avatar(c, false, 64.0),
            Space::new().height(8),
            text(t("No accounts yet")).font(BODY).size(20).color(c.ink),
            sub_text(t("Add a PlayOnline account or a private-server login to launch from here."), 14.0),
            Space::new().height(16),
            control(c, Tone::Primary, Some(Icon::Plus), t("Add your first account"), Some(Message::NewAccount), None)
        ]
        .spacing(8)
        .align_x(Alignment::Center),
    )
    .padding([64, 24])
    .width(Length::Fill)
    .align_x(Alignment::Center)
    .style(card_style(c))
    .into()
}
/// Accounts side card: whether the service and the game are ready.
fn connection_card(app: &App, c: Colors) -> Element<'_, Message> {
    let (service_icon, service) = service_state(app, c);
    let (patch_icon, patch) = patch_state(app, c);
    let mut content = column![
        card_title(c, t("Connection"), Some(t("Checked before every launch."))),
        status_block(c, t("Service"), service_icon, service),
        status_block(c, t("Game version"), patch_icon, patch),
        dl_divider(c)
    ]
    .spacing(16);
    if matches!(app.patch.as_ref().map(|p| p.state), Some(crate::patch::PatchState::Required)) {
        content = content.push(card_link(c, t("Update in Downloads"), Message::Navigate(Page::Maintenance)));
    }
    content = content.push(card_link(c, t("Connection details"), Message::ToggleDetails));
    card(c, content).into()
}
fn security_card<'a>(c: Colors) -> Element<'a, Message> {
    card(
        c,
        column![
            row![
                container(icon(Icon::CheckCircle, 16.0, c.accent))
                    .center(32)
                    .style(move |_| container::Style {
                        background: Some(c.select.into()),
                        border: Border {
                            radius: 16.0.into(),
                            ..Default::default()
                        },
                        ..Default::default()
                    }),
                text(t("Sign-in security")).font(SEMI).size(16).color(c.ink)
            ]
            .spacing(12)
            .align_y(Alignment::Center),
            text(t("Passwords stay on this PC, protected by Windows encryption. One-time codes are typed before each launch and never saved."))
                .font(BODY)
                .size(13)
                .color(c.sub)
        ]
        .spacing(12),
    )
    .into()
}
fn close_confirm<'a>(app: &'a App, base: Element<'a, Message>) -> Element<'a, Message> {
    let c = colors(app.settings.light_theme);
    let header = row![
        container(icon(Icon::AlertCircle, 20.0, dl_danger(c)))
            .center(40)
            .style(move |_| container::Style {
                background: Some(with_alpha(dl_danger(c), 0.14).into()),
                border: Border {
                    radius: 20.0.into(),
                    ..Default::default()
                },
                ..Default::default()
            }),
        column![
            text(t("Close the launcher?")).font(BODY).size(20).color(c.ink),
            text(t("Account changes are still being saved. Wait for saving to finish before closing."))
                .font(BODY)
                .size(13)
                .color(c.sub)
        ]
        .spacing(4)
        .width(Length::Fill)
    ]
    .spacing(16);
    let footer = row![
        Space::new().width(Length::Fill),
        control(c, Tone::Secondary, None, t("Keep launcher open"), Some(Message::CancelClose), None),
        control(c, Tone::Danger, None, t("Close now"), Some(Message::ConfirmClose), None)
    ]
    .spacing(8);
    modal(base, header.into(), None, Some(footer.into()), 500.0, app.viewport.height - 48.0)
}

fn delete_confirm<'a>(app: &'a App, key: &'a str, base: Element<'a, Message>) -> Element<'a, Message> {
    let c = colors(app.settings.light_theme);
    let label = app.accounts.iter().find(|row| row.key == key)
        .map_or_else(|| t("this account").to_owned(), |row| row.account.label.clone());
    let header = column![
        private_text(app, &tf("Delete {label}?", &[("label", &label)]), 20.0, SEMI, c.ink),
        text(t("This removes its saved sign-in details and launcher choices. Game files and account data folders are kept.")).font(BODY).size(13).color(c.sub),
    ].spacing(12);
    let footer = row![
        Space::new().width(Length::Fill),
        control(c, Tone::Secondary, None, t("Cancel"), Some(Message::CancelDeleteAccount), None),
        control(c, Tone::Danger, None, t("Delete account"), Some(Message::DeleteAccount(key.to_owned())), None),
    ].spacing(8);
    modal(base, header.into(), None, Some(footer.into()), 500.0, app.viewport.height - 48.0)
}
fn form_label<'a>(value: &'a str) -> iced::widget::Text<'a> {
    text(value).font(semibold()).size(13)
}
/// Segmented control, 40 px tall like every other control.
fn segmented<'a>(items: Vec<Element<'a, Message>>) -> Element<'a, Message> {
    container(iced::widget::Row::with_children(items).spacing(4))
        .padding(3)
        .width(Length::Fill)
        .style(|theme: &Theme| {
            let c = tc(theme);
            container::Style {
                background: Some(c.field.into()),
                border: Border {
                    color: c.line,
                    width: 1.0,
                    radius: CONTROL_R.into(),
                },
                ..Default::default()
            }
        })
        .into()
}
fn segment<'a>(label: &'a str, on: bool, message: Message) -> Element<'a, Message> {
    crate::controller_nav::button(
        container(text(label).font(if on { SEMI } else { BODY }).size(14))
            .width(Length::Fill)
            .height(Length::Fill)
            .align_x(Alignment::Center)
            .align_y(Alignment::Center),
    )
    .padding([0, 12])
    .height(CONTROL_H - 8.0)
    .width(Length::Fill)
    .style(move |theme, status| {
        let c = tc(theme);
        let hot = matches!(status, button::Status::Hovered | button::Status::Pressed);
        button::Style {
            background: if on {
                Some(c.select.into())
            } else if hot {
                Some(with_alpha(c.select, 0.5).into())
            } else {
                None
            },
            text_color: if on {
                c.accent
            } else if hot {
                c.ink
            } else {
                c.sub
            },
            border: Border {
                radius: (CONTROL_R - 2.0).into(),
                ..Default::default()
            },
            ..Default::default()
        }
    })
    .on_press(message)
    .into()
}
fn account_editor<'a>(
    app: &'a App,
    editor: &'a crate::Editor,
    base: Element<'a, Message>,
) -> Element<'a, Message> {
    let c = colors(app.settings.light_theme);
    let account = &editor.account;
    let retail = account.mode == AccountMode::Retail;
    let editing = editor.key.is_some();
    let active = editor.key.as_ref().is_some_and(|key| app.accounts.iter().any(|row| &row.key == key && (row.busy || row.pid.is_some())));
    let header = dialog_header(
        c,
        if editing { t("Edit account") } else { t("Add account") },
        if editing {
            t("Update the sign-in details saved for this account.")
        } else {
            t("Save sign-in details once, then launch in one click.")
        },
        Some(Message::CloseEditor),
    );
    let account_type: Element<'a, Message> = if editing {
        column![
            form_label(t("Account type")),
            sub_text(if retail { t("Retail") } else { t("Private server") }, 13.0)
        ]
        .spacing(8)
        .into()
    } else {
        let modes: Vec<_> = [
            (AccountMode::Retail, "Retail"),
            (AccountMode::Private, "Private server"),
        ]
        .into_iter()
        .map(|(mode, label)| segment(t(label), account.mode == mode, Message::EditorMode(mode)))
        .collect();
        column![
            form_label(t("Account type")),
            container(segmented(modes)).width(264)
        ]
        .spacing(8)
        .into()
    };
    let mut form = column![
        field(
            t("Account name"),
            t("Shown in your account list"),
            &account.label,
            Field::Label,
            // Privacy mode masks identifying fields like passwords.
            crate::privacy::on(), true
        ),
        account_type
    ]
    .spacing(20);
    if !retail {
        form = form.push(field(
            t("Server"),
            t("Server hostname"),
            &account.server,
            Field::Server,
            false, !active,
        ));
    }
    form = form.push(
        row![
            field(
                if retail { t("PlayOnline ID") } else { t("Username") },
                t("Account ID"),
                &account.pol_id,
                Field::PolId,
                crate::privacy::on(), !active
            ),
            field(
                if retail { t("PlayOnline password") } else { t("Password") },
                t("Password"),
                &account.pol_password,
                Field::PolPassword,
                true, true
            )
        ]
        .spacing(16),
    );
    if !retail {
        form = form.push(
            column![
                crate::controller_nav::checkbox(editor.hairpin)
                    .label(t("Use xiloader hairpin mode"))
                    .on_toggle(Message::EditorHairpin)
                    .size(18)
                    .text_size(14)
                    .style(check),
                sub_text(
                    t("Pass --hairpin to xiloader. Disable this if your private-server setup does not require it."),
                    12.0
                )
            ]
            .spacing(8),
        );
    }
    if retail {
        form = form.push(
            row![
                field(
                    t("Square Enix ID"),
                    t("Square Enix account ID"),
                    &account.se_id,
                    Field::SeId,
                    crate::privacy::on(), true
                ),
                field(
                    t("Square Enix password"),
                    t("Password"),
                    &account.se_password,
                    Field::SePassword,
                    true, true
                )
            ]
            .spacing(16),
        );
    }
    if retail {
        let regions = [(0u8, "US"), (1, "EU"), (2, "JP")]
            .into_iter()
            .map(|(value, label)| segment(label, account.region == value, Message::EditorRegion(value)))
            .collect();
        form = form.push(
            column![
                form_label(t("Region")),
                container(segmented(regions)).width(264),
                sub_text(t("Retail launch is currently verified with the US configuration."), 12.0)
            ]
            .spacing(8),
        );
        form = form.push(launch_section(app, account, c));
    }
    if active {
        form = form.push(sub_text(t("Changes are saved for the next fresh launch. The running game keeps its current profile; its account ID cannot be changed here."), 12.0));
    }
    form = form.push(column![
        form_label(t("Game folder for this account")),
        row![
            text_field(t("Use the launcher's default game folder"), &crate::privacy::mask_path(&editor.game_install))
                .on_input_maybe(crate::privacy::editable().then_some(|v| Message::AccountInstallChanged(crate::privacy::unmask_path(v)))),
            icon_control(c, Icon::Folder, true, (!app.vault_saving).then_some(Message::Browse(crate::PickTarget::AccountGameFolder)))
        ].spacing(8).align_y(Alignment::Center),
        sub_text(t("Choose a separate installation for retail or a private server. Leave blank to use the default."), 12.0)
    ].spacing(8));
    form = form.push(
        container(
            row![
                icon(Icon::CheckCircle, 16.0, c.accent),
                sub_text(
                    t("Passwords are saved locally with Windows encryption. One-time codes are entered before launch and are not saved."),
                    13.0
                )
            ]
            .spacing(12)
            .align_y(Alignment::Center),
        )
        .padding([12, 16])
        .width(Length::Fill)
        .style(soft_tile(c)),
    );
    if !app.status.is_empty() {
        form = form.push(private_text(app, &app.status, 13.0, BODY, c.accent));
    }
    let delete: Element<'a, Message> = match &editor.key {
        Some(key) => control(
            c,
            Tone::DangerLink,
            Some(Icon::Trash),
            t("Delete account"),
            (!active && !app.vault_saving).then_some(Message::RequestDeleteAccount(key.clone())),
            None,
        ),
        None => Space::new().into(),
    };
    let footer = row![
        delete,
        Space::new().width(Length::Fill),
        control(c, Tone::Secondary, None, t("Cancel"), Some(Message::CloseEditor), None),
        control(c, Tone::Primary, None, t("Save account"), Some(Message::SaveAccount), None)
    ]
    .align_y(Alignment::Center)
    .spacing(8);
    modal_with_footer(
        base,
        header,
        form.into(),
        footer.into(),
        640,
        (app.viewport.height - 48.0).min(760.0),
    )
}

/// A dropdown entry in the editor: the stored value (`None` = the default) and its text.
#[derive(Clone, PartialEq, Eq)]
struct Pick {
    value: Option<String>,
    label: String,
}
impl std::fmt::Display for Pick {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.label)
    }
}
/// "Default (…)" first, then the names found on disk, then the account's own value if it is
/// no longer there. Returns the options and the selected one.
fn picks(default_label: String, found: &[String], current: Option<&String>, missing: impl Fn(&str) -> String) -> (Vec<Pick>, Pick) {
    let default = Pick { value: None, label: default_label };
    let mut options = vec![default.clone()];
    options.extend(found.iter().map(|name| Pick { value: Some(name.clone()), label: name.clone() }));
    let selected = match current {
        None => default,
        Some(name) => match options.iter().find(|p| p.value.as_ref().is_some_and(|v| v.eq_ignore_ascii_case(name))) {
            Some(found) => found.clone(),
            None => {
                let gone = Pick { value: Some(name.clone()), label: missing(name) };
                options.push(gone.clone());
                gone
            }
        },
    };
    (options, selected)
}
fn editor_pick<'a>(c: Colors, light: bool, options: Vec<Pick>, selected: Pick, on: fn(Option<String>) -> Message) -> Element<'a, Message> {
    let base = profile_pick_style(c.field, Border { color: c.line, width: 1.0, radius: CONTROL_R.into() }, false, light);
    crate::controller_nav::pick_list(options, Some(selected), move |pick: Pick| on(pick.value))
        .font(BODY)
        .text_size(14)
        .padding([11, 14])
        .width(Length::Fill)
        .style(move |theme, status| pick_list::Style { handle_color: c.sub, ..base(theme, status) })
        .menu_style(profile_menu_style(light))
        .into()
}
fn import_accounts<'a>(app: &'a App, base: Element<'a, Message>) -> Element<'a, Message> {
    use crate::account_import_ui::Msg;
    let c = colors(app.settings.light_theme);
    let state = &app.account_import;
    let options: Vec<Pick> = state.sources.iter().enumerate().map(|(i, source)| Pick {
        value: Some(i.to_string()), label: format!("{} · {}", match source.kind {
            crate::account_import::SourceKind::Forest => "Forest",
            crate::account_import::SourceKind::PlayOnline => "PlayOnline",
        }, crate::privacy::mask_path(&source.path.to_string_lossy())),
    }).collect();
    let mut form = column![
        sub_text(t("Import from PlayOnline's member list or Forest on this Windows account. Review each account before saving."), 14.0),
        sub_text(t("Existing logins are kept. Some sources do not contain every password; missing fields must be entered in the account editor."), 12.0),
    ].spacing(16);
    if !options.is_empty() && !state.busy {
        let selected = state.sources.iter().position(|s| s.path.to_string_lossy() == state.path)
            .and_then(|i| options.get(i).cloned());
        form = form.push(column![form_label(t("Detected account files")),
            crate::controller_nav::pick_list(options, selected, |pick: Pick| {
                Message::AccountImport(Msg::Source(pick.value.and_then(|v| v.parse().ok()).unwrap_or(0)))
            }).padding([10, 12]).text_size(13).width(Length::Fill)].spacing(8));
    }
    let file_field = |path: &str, companion: bool| {
        row![
            text_field(t("Choose an account file"), &crate::privacy::mask_path(path))
                .on_input_maybe((!state.busy && crate::privacy::editable()).then_some(move |v| Message::AccountImport(
                    if companion { Msg::CompanionPath(crate::privacy::unmask_path(v)) } else { Msg::Path(crate::privacy::unmask_path(v)) })))
                .font(MONO).size(12),
            control(c, Tone::Secondary, Some(Icon::Folder), t("Browse…"),
                (!state.busy).then_some(Message::AccountImport(Msg::Browse(companion))), None)
        ].spacing(8)
    };
    form = form.push(column![form_label(t("Account file")), file_field(&state.path, false)].spacing(8));
    let mut regions = row![form_label(t("Region"))].spacing(8).align_y(Alignment::Center);
    for (region, label) in [(0u8, "US"), (1, "EU"), (2, "JP")] {
        regions = regions.push(control(c, if region == state.region { Tone::Primary } else { Tone::Secondary }, None, label,
            (!state.busy).then_some(Message::AccountImport(Msg::Region(region))), None));
    }
    form = form.push(regions);
    let companion_toggle = crate::controller_nav::checkbox(state.use_companion)
        .label(t("Match Forest profiles with a PlayOnline member file"))
        .size(18).text_size(13).style(check);
    let companion_toggle: Element<'_, Message> = if state.busy { companion_toggle.into() } else {
        companion_toggle.on_toggle(|v| Message::AccountImport(Msg::Companion(v))).into()
    };
    form = form.push(companion_toggle);
    if state.use_companion {
        form = form.push(column![form_label(t("PlayOnline member file")), file_field(&state.companion, true)].spacing(8));
    }
    if !state.error.is_empty() { form = form.push(sub_text(&state.error, 13.0)); }
    if let Some(batch) = &state.batch {
        for warning in &batch.warnings { form = form.push(sub_text(warning, 12.0)); }
        if batch.drafts.is_empty() { form = form.push(sub_text(t("No saved accounts were found in this file."), 13.0)); }
        for (index, draft) in batch.drafts.iter().enumerate() {
            let missing = draft.missing_fields().into_iter().map(t).collect::<Vec<_>>().join(", ");
            let detail = if missing.is_empty() { t("Ready to review").into() }
                else { tf("Missing: {fields}", &[("fields", &missing)]) };
            let mut entry = column![
                crate::privacy::name(text(&draft.label).font(SEMI), &draft.label, 15.0, c.ink),
                sub_text(detail, 12.0)
            ].spacing(5);
            for note in &draft.notes { entry = entry.push(sub_text(note, 12.0)); }
            form = form.push(row![entry.width(Length::Fill),
                control(c, Tone::Secondary, None, t("Review account"), (!state.busy).then_some(Message::AccountImport(Msg::Review(index))), None)
            ].spacing(12).align_y(Alignment::Center));
        }
    }
    modal_with_footer(base,
        dialog_header(c, t("Import accounts"), t("Your source files stay unchanged."), Some(Message::AccountImport(Msg::Close))),
        form.into(),
        row![Space::new().width(Length::Fill),
            control(c, Tone::Secondary, None, t("Close"), Some(Message::AccountImport(Msg::Close)), None),
            control(c, Tone::Primary, Some(Icon::Download), if state.busy { t("Reading…") } else { t("Read accounts") },
                (!state.busy && !state.path.trim().is_empty()).then_some(Message::AccountImport(Msg::Read)), None)
        ].spacing(8).into(), 680, (app.viewport.height - 48.0).min(760.0))
}

/// The account editor's Launch section: which launcher, and its boot config or profile.
fn launch_section<'a>(app: &'a App, account: &'a crate::backend::Account, c: Colors) -> Element<'a, Message> {
    let light = app.settings.light_theme;
    let default = app.settings.launch_with;
    let choices = [
        (t("Use default"), None),
        (t("Vanilla"), Some(LaunchWith::Vanilla)),
        ("Ashita", Some(LaunchWith::Ashita)),
        ("Windower", Some(LaunchWith::Windower)),
    ]
    .into_iter()
    .map(|(label, value)| segment(label, account.launch_with.filter(|tool| *tool != LaunchWith::Native) == value, Message::EditorLaunchWith(value)))
    .collect();
    let hint = match account.launch_with {
        None => tf("Uses the default launcher, {default}, set in Settings → System → Launch tools.", &[("default", &default)]),
        Some(LaunchWith::Native) => tf("The former native client is unavailable. This retail account now uses {default}.", &[("default", &default)]),
        Some(LaunchWith::Vanilla) => t("Starts FFXI directly with Crystal, without Windower or Ashita. Uses the game's settings and Crystal's saved menu resolution.").into(),
        Some(tool) => tf("Signs in without PlayOnline and starts the game through {tool}.", &[("tool", &tool)]),
    };
    let mut section = column![column![form_label(t("Launch with")), segmented(choices), sub_text(hint, 12.0)].spacing(8)].spacing(20);
    let found = &app.launch_tools.detected;
    let tools = &app.settings;
    match account.effective_launch_with(default) {
        LaunchWith::Native | LaunchWith::Vanilla => {}
        LaunchWith::Ashita => {
            let base = tools.ashita_boot_ini.trim();
            let base = if base.is_empty() { crate::pol_less::tools::DEFAULT_BOOT_INI } else { base };
            let (options, selected) = picks(tf("Default ({name})", &[("name", &base)]), &found.boot_inis, account.ashita_boot_ini.as_ref(), |name| tf("{name} (missing)", &[("name", &name)]));
            let note = if found.ashita.is_none() {
                t("Ashita was not found. Set its folder in Settings → System → Launch tools.").to_owned()
            } else {
                t("This account's Ashita settings start from this boot config in config\\boot.").to_owned()
            };
            section = section.push(
                column![
                    form_label(t("Ashita boot config")),
                    editor_pick(c, light, options, selected, Message::EditorBootIni),
                    sub_text(note, 12.0)
                ]
                .spacing(8),
            );
        }
        LaunchWith::Windower => {
            let base = tools.windower_profile.trim();
            let base = if base.is_empty() { t("Windower's default profile") } else { base };
            let (options, selected) = picks(tf("Default ({name})", &[("name", &base)]), &found.profiles, account.windower_profile.as_ref(), |name| tf("{name} (not in settings.xml)", &[("name", &name)]));
            let note = if found.windower.is_none() {
                t("Windower was not found. Set its folder in Settings → System → Launch tools.").to_owned()
            } else {
                t("Video and sound settings come from this Windower profile.").to_owned()
            };
            section = section.push(
                column![
                    form_label(t("Windower profile")),
                    editor_pick(c, light, options, selected, Message::EditorProfile),
                    sub_text(note, 12.0),
                    control(c, Tone::Secondary, Some(Icon::Edit), t("Edit profiles in Windower"),
                        (found.windower.is_some() && !app.windower_editor_open).then_some(Message::EditWindowerProfiles), None)
                ]
                .spacing(8),
            );
        }
    }
    // Signing in when Crystal opens is no per-account choice: Settings → System has its switch.
    let choice = app.editor.as_ref().map(|editor| editor.auto_login).unwrap_or_default();
    section = section.push(column![
        crate::controller_nav::checkbox(choice.enabled)
            .label(t("Log in to a character automatically"))
            .on_toggle(Message::EditorAutoLogin).size(18).text_size(14).style(check),
        row![form_label(t("Character slot")),
            crate::controller_nav::pick_list((1u8..=16).collect::<Vec<_>>(), Some(choice.character_slot), Message::EditorCharacterSlot)
                .text_size(14).padding([8, 12])].spacing(16).align_y(Alignment::Center),
        sub_text(t("1 selects the first character. Each client can log in in the background; unavailable slots stay at character selection."), 12.0)
    ].spacing(8));
    section.into()
}

// ---- Downloads page ----------------------------------------
fn maintenance(app: &App) -> Element<'_, Message> {
    let c = colors(app.settings.light_theme);
    let s = scale(app);
    let idle = app.maintenance.is_none();
    let clients_open = app.has_sessions();
    let update_blocked = app.sessions_block_update();
    let can_check = idle && !app.installation_loading && app.settings.install_selected;
    let can_change = can_check && !update_blocked && app.vault_ready;
    let can_install = idle;

    let header = page_header(
        c,
        s,
        t("Downloads"),
        t("Install FINAL FANTASY XI and PlayOnline, and check or repair their files. Play installs official updates when they are needed."),
        None,
    );

    // Game installation: the folder (chosen in Settings), version summary, troubleshooting.
    let shown_folder = if app.settings.install_path.trim().is_empty() { t("Not selected").to_owned() }
        else { crate::privacy::mask_path(&app.settings.install_path).to_string() };
    let game_folder = column![
        form_label(t("Game folder")).color(c.ink),
        row![
            text(shown_folder).font(MONO).size(13).color(c.ink).width(Length::Fill),
            control(c, Tone::Secondary, Some(Icon::Folder), t("Change in Settings"), Some(Message::ToggleSettings), None)
        ]
        .spacing(8)
        .align_y(Alignment::Center),
        dl_flag(
            c,
            app.settings.install_selected,
            t("Folder confirmed as the target for checks and changes"),
            t("Confirm this folder before starting")
        )
    ]
    .spacing(8);
    let (installed, latest) = app
        .patch
        .as_ref()
        .map(|p| (p.local_version.as_str(), p.remote_version.as_str()))
        .unwrap_or((t("Unknown"), t("Unknown")));
    let (state_icon, state_color, state_text) = match app.patch.as_ref().map(|p| p.state) {
        Some(crate::patch::PatchState::Current) => (Icon::CheckCircle, dl_ok(c), t("Up to date")),
        Some(crate::patch::PatchState::Required) => (Icon::Download, c.accent, t("Update available")),
        Some(crate::patch::PatchState::Interrupted) => (Icon::AlertCircle, c.accent, t("Interrupted update")),
        _ if app.patch_loading => (Icon::Refresh, c.sub, t("Checking version…")),
        _ => (Icon::AlertCircle, c.sub, t("Not checked")),
    };
    let stats = container(
        row![
            dl_stat(c, t("Installed version"), text(installed).font(MONO).size(14).color(c.ink).into()),
            dl_stat(c, t("Latest version"), text(latest).font(MONO).size(14).color(c.ink).into()),
            dl_stat(
                c,
                t("Version status"),
                row![icon(state_icon, 15.0, state_color), text(state_text).size(14).color(c.ink)]
                    .spacing(8)
                    .align_y(Alignment::Center)
                    .into()
            )
        ]
        .spacing(GUTTER),
    )
    .padding([16, 16])
    .width(Length::Fill)
    .style(soft_tile(c));
    // Check and Repair are troubleshooting steps, not part of an update: Play updates.
    let troubleshooting_head = crate::controller_nav::button(
        row![
            icon(if app.troubleshooting_open { Icon::ChevronDown } else { Icon::ChevronRight }, 16.0, c.sub),
            text(t("Troubleshooting")).font(semibold()).size(16).color(c.ink),
        ]
        .spacing(8)
        .align_y(Alignment::Center),
    )
    .padding(0)
    .style(|_, _| button::Style::default())
    .on_press(Message::ToggleTroubleshooting);
    let mut troubleshooting = column![troubleshooting_head].spacing(12);
    if app.troubleshooting_open {
        let mut actions = row![
            control(
                c,
                Tone::Secondary,
                Some(Icon::CheckCircle),
                t("Check files"),
                can_check.then_some(Message::StartMaintenance(MaintenanceKind::Check)),
                None
            ),
            control(
                c,
                Tone::Secondary,
                Some(Icon::Wrench),
                t("Repair files"),
                can_change.then_some(Message::StartMaintenance(MaintenanceKind::Repair)),
                None
            ),
            Space::new().width(Length::Fill)
        ]
        .spacing(8)
        .align_y(Alignment::Center);
        if can_check && update_blocked {
            actions = actions.push(text(t("Close running clients to repair")).size(12).color(c.sub));
        }
        troubleshooting = troubleshooting.push(actions).push(column![
            dl_note(c, Icon::CheckCircle, t("Check files compares your files with the installed file list. It doesn't contact the update service and can run while clients are open.")),
            dl_note(c, Icon::Wrench, t("Repair files restores every official PlayOnline and FINAL FANTASY XI file. Copies of modified files are kept in .xi-updater\\kept; replaced files are backed up.")),
            dl_note(c, Icon::AlertCircle, t("Close all games before repairing. Idle PlayOnline sessions sign out automatically. Sign out before official setup."))
        ].spacing(8));
    }
    let (viewer_local, viewer_latest, viewer_detail) = app.viewer.as_ref().map(|v|
        (v.version.local_version.as_str(), v.version.remote_version.as_str(), v.version.detail.as_str()))
        .unwrap_or((t("Not checked"), t("Not checked"), t("Check files and Repair files cover both PlayOnline and FINAL FANTASY XI.")));
    let viewer_state = if app.viewer_loading { t("Checking…") }
        else if app.viewer.as_ref().is_some_and(crate::viewer::Status::requires_update) { t("Needs attention") }
        else if app.viewer.as_ref().is_some_and(|v| v.version.state == crate::patch::PatchState::Current) { t("Up to date") }
        else { t("Not verified") };
    let viewer_folder = app.viewer.as_ref().and_then(|v| v.folder.as_ref())
        .map(|p| crate::privacy::mask_path(&p.to_string_lossy())).unwrap_or_else(|| t("Not installed").into());
    let mut viewer = column![
        text(t("PlayOnline")).font(semibold()).size(16).color(c.ink),
        text(viewer_folder).font(MONO).size(13).color(c.sub),
        container(row![
            dl_stat(c, t("Installed version"), text(viewer_local).font(MONO).size(14).color(c.ink).into()),
            dl_stat(c, t("Latest version"), text(viewer_latest).font(MONO).size(14).color(c.ink).into()),
            dl_stat(c, t("Version status"), text(viewer_state).size(14).color(c.ink).into()),
        ].spacing(GUTTER)).padding([16, 16]).width(Length::Fill).style(soft_tile(c)),
        text(viewer_detail).size(13).color(c.sub),
    ].spacing(8);
    if let Some(error) = app.viewer.as_ref().and_then(|v| v.core_error.as_deref()) {
        viewer = viewer.push(text(error).size(13).color(dl_danger(c)));
    }

    let game = card(
        c,
        column![
            card_title(
                c,
                t("Installation"),
                Some(t("The FINAL FANTASY XI game folder from Settings. This is separate from the installer download folder."))
            ),
            game_folder,
            text(t("FINAL FANTASY XI")).font(semibold()).size(16).color(c.ink),
            stats,
            text(app.patch.as_ref().map_or("", |report| report.detail.as_str())).size(13).color(c.sub),
            viewer,
            dl_divider(c),
            troubleshooting
        ]
        .spacing(20),
    );

    // Activity: the running operation, then the last result.
    let cancelled = app.maintenance_cancelled;
    let mut head = row![card_title(c, t("Activity"), None)].align_y(Alignment::Center);
    if let Some(kind) = app.maintenance {
        if kind.cancellable() {
            head = head.push(control(
                c,
                Tone::Secondary,
                Some(Icon::Close),
                if cancelled { t("Cancelling…") } else { t("Cancel") },
                (!cancelled).then_some(Message::CancelMaintenance),
                None,
            ));
        }
    }
    let mut activity = column![head].spacing(16);
    if let Some(kind) = app.maintenance {
        let progress = &app.maintenance_progress;
        let remaining_percent = progress.remaining_percent();
        let has_overall = progress.overall.is_some();
        if let Some(overall) = &progress.overall {
            let mut summary = column![text(t("Overall update")).font(semibold()).size(16).color(c.ink)]
                .spacing(10)
                .width(Length::Fill);
            summary = summary.push(dl_progress(c, overall.fraction.unwrap_or(0.0).clamp(0.0, 1.0) as f32, 10.0));
            if let Some(percent) = overall.remaining_percent() {
                summary = summary.push(text(tf("{percent}% remaining overall", &[("percent", &percent)]))
                    .font(semibold()).size(15).color(c.ink).width(Length::Fill));
            } else {
                summary = summary.push(text(t("Calculating total update size…")).size(14).color(c.sub).width(Length::Fill));
            }
            if !cancelled {
                let estimate = match overall.remaining {
                    Some(duration) if duration.is_zero() && !overall.complete => t("Finalizing update…").to_owned(),
                    Some(duration) => tf("About {time} remaining for the whole update", &[("time", &dl_remaining_time(duration))]),
                    None => t("Estimating total time remaining…").to_owned(),
                };
                summary = summary.push(text(estimate).size(14).color(c.ink).width(Length::Fill));
            }
            activity = activity.push(summary).push(dl_divider(c));
        }
        activity = activity.push(
            row![
                icon(Icon::Refresh, if has_overall { 14.0 } else { 18.0 }, if has_overall { c.sub } else { c.accent }),
                column![
                    text(if has_overall {
                        tf("Current step: {step}", &[("step", &dl_stage(kind, &progress.stage))])
                    } else {
                        dl_stage(kind, &progress.stage)
                    }).size(if has_overall { 13 } else { 15 }).color(if has_overall { c.sub } else { c.ink }),
                    text(if kind.cancellable() {
                        t("Cancelling keeps verified downloads for resuming.")
                    } else {
                        kind.label()
                    })
                    .size(12)
                    .color(c.sub)
                ]
                .spacing(2)
            ]
            .spacing(12)
            .align_y(Alignment::Center),
        );
        if progress.total > 0 {
            let fraction = (progress.done as f64 / progress.total as f64).clamp(0.0, 1.0) as f32;
            let amount = if progress.bytes {
                tf("{done} of {total}", &[("done", &dl_bytes(progress.done)), ("total", &dl_bytes(progress.total))])
            } else {
                tf("{done} of {total}", &[("done", &progress.done), ("total", &progress.total)])
            };
            let complete_percent = 100_u8.saturating_sub(remaining_percent.unwrap_or(100));
            activity = activity.push(dl_progress(c, fraction, if has_overall { 6.0 } else { 8.0 })).push(
                row![
                    text(dl_detail(progress)).font(MONO).size(12).color(c.sub).width(Length::Fill),
                    text(tf("{amount} · {percent}%", &[("amount", &amount), ("percent", &complete_percent)])).size(12).color(c.ink)
                ]
                .spacing(16),
            );
        } else {
            activity = activity.push(dl_progress(c, 0.0, if has_overall { 6.0 } else { 8.0 })).push(
                text(t("Working… progress appears as the service reports it.")).size(12).color(c.sub),
            );
            if !progress.detail.is_empty() {
                activity = activity.push(text(dl_detail(progress)).font(MONO).size(12).color(c.sub));
            }
        }
        if !has_overall {
            let mut remaining = Vec::with_capacity(2);
            if let Some(percent) = remaining_percent {
                remaining.push(tf("{percent}% remaining in this step", &[("percent", &percent)]));
            }
            if kind.cancellable() && !cancelled {
                remaining.push(match progress.remaining {
                    Some(duration) => tf("About {time} remaining", &[("time", &dl_remaining_time(duration))]),
                    None => t("Estimating time remaining…").to_owned(),
                });
            }
            if !remaining.is_empty() {
                activity = activity.push(text(remaining.join(" · ")).size(13).color(c.sub).width(Length::Fill));
            }
        }
        if !kind.cancellable() {
            activity = activity.push(dl_note(
                c,
                Icon::AlertCircle,
                t("Complete or cancel this step in the official installer window."),
            ));
        }
    } else {
        activity = activity.push(
            row![
                icon(Icon::CheckCircle, 18.0, c.sub),
                column![
                    text(t("No operation running")).size(15).color(c.ink),
                    text(t("Progress appears here while files are checked, updated or downloaded."))
                        .size(12)
                        .color(c.sub)
                ]
                .spacing(2)
            ]
            .spacing(12)
            .align_y(Alignment::Center),
        );
    }
    if !app.maintenance_result.is_empty() {
        let (result_icon, result_color) = result_look(c, app.maintenance_tone);
        activity = activity.push(dl_divider(c)).push(
            column![
                dl_label(c, t("Last result")),
                row![
                    icon(result_icon, 16.0, result_color),
                    text(&app.maintenance_result).size(14).color(c.ink).width(Length::Fill)
                ]
                .spacing(10)
            ]
            .spacing(8),
        );
    }
    if !app.status.is_empty() && app.status != app.maintenance_result {
        activity = activity.push(text(&app.status).size(12).color(c.sub));
    }
    let activity = card(c, activity);

    // Official installer: folder, three explicit steps, per-part download progress.
    let mut installer_folder = column![
        form_label(t("Installer folder")).color(c.ink),
        row![
            text_field(t("Folder for official installer downloads"), &crate::privacy::mask_path(&app.settings.installer_path))
                .on_input_maybe(crate::privacy::editable().then_some(|v| Message::InstallerPathChanged(crate::privacy::unmask_path(v))))
                .on_submit(Message::SelectInstallerPath)
                .font(MONO)
                .size(13),
            icon_control(c, Icon::Folder, true, idle.then_some(Message::Browse(crate::PickTarget::InstallerFolder)))
        ]
        .spacing(8)
        .align_y(Alignment::Center),
        sub_text(t("Use the download folder, or browse to the extracted setup folder. Each button checks the folder when clicked."), 12.0)
    ]
    .spacing(8);
    if let Some(error) = &app.installer_error {
        let danger = dl_danger(c);
        installer_folder = installer_folder.push(
            container(
                row![
                    icon(Icon::AlertCircle, 16.0, danger),
                    text(crate::privacy::mask_path(error)).font(BODY).size(13).color(danger).width(Length::Fill)
                ]
                .spacing(10)
            )
            .padding(12)
            .width(Length::Fill)
            .style(move |_| container::Style {
                background: Some(with_alpha(danger, 0.08).into()),
                border: Border { color: with_alpha(danger, 0.3), width: 1.0, radius: 8.0.into() },
                ..Default::default()
            }),
        );
    }
    let mut installer = column![
        card_title(
            c,
            t("Install PlayOnline and FFXI"),
            Some(t("Download Square Enix's Windows installer, extract it, then run setup. The bundle includes PlayOnline Viewer and FINAL FANTASY XI."))
        ),
        installer_folder,
        dl_divider(c),
        dl_step(
            c,
            1,
            t("Download official installer"),
            t("Five files from Square Enix, about 7.7 GB. Interrupted downloads resume."),
            Icon::Download,
            t("Download"),
            can_install.then_some(Message::StartMaintenance(MaintenanceKind::Download))
        ),
        dl_step(
            c,
            2,
            t("Extract"),
            t("Unpacks the installer in its own window."),
            Icon::Folder,
            t("Extract"),
            can_install.then_some(Message::StartMaintenance(MaintenanceKind::Extract))
        ),
        dl_step(
            c,
            3,
            t("Run official setup"),
            t("Select PlayOnline Viewer and FINAL FANTASY XI in the wizard. Close clients first."),
            Icon::Play,
            t("Run setup"),
            (can_install && !clients_open).then_some(Message::StartMaintenance(MaintenanceKind::Setup))
        )
    ]
    .spacing(20);
    if let Some(parts) = dl_parts(c, &app.maintenance_progress, app.maintenance == Some(MaintenanceKind::Download)) {
        installer = installer.push(dl_divider(c)).push(parts);
    }
    installer = installer.push(dl_divider(c)).push(dl_note(
        c,
        Icon::AlertCircle,
        t("Choose the game destination in the official setup wizard. After installation, Crystal finds that game folder (or choose it in Settings) and Play installs the official updates. Extraction and setup use their own windows; wait for them to finish before closing this launcher."),
    ));
    // A running operation moves its progress above the fold.
    let main = if idle {
        column![game, activity]
    } else {
        column![activity, game]
    }
    .spacing(GUTTER * s)
    .width(Length::Fill);
    let header = column![header, crate::prereqs::banner(app)]
        .spacing(GUTTER * s)
        .into();
    page_layout(app, header, main.into(), card(c, installer).into())
}
/// The "Last result" icon and colour, from the result's tone (never its words).
fn result_look(c: Colors, tone: crate::ResultTone) -> (Icon, Color) {
    match tone {
        crate::ResultTone::Stopped => (Icon::AlertCircle, dl_danger(c)),
        crate::ResultTone::Unverified => (Icon::AlertCircle, c.sub),
        crate::ResultTone::Done => (Icon::CheckCircle, dl_ok(c)),
    }
}
pub(crate) fn dl_ok(c: Colors) -> Color {
    if is_light(c) {
        rgb(0x2E7A5E)
    } else {
        rgb(0x82CBA8)
    }
}
pub(crate) fn dl_danger(c: Colors) -> Color {
    if is_light(c) {
        rgb(0xB24A4A)
    } else {
        rgb(0xE59494)
    }
}
/// Small muted caption over a value.
fn dl_label<'a>(c: Colors, value: &'a str) -> Element<'a, Message> {
    text(value).font(BODY).size(12).color(c.sub).into()
}
fn dl_divider<'a>(c: Colors) -> Element<'a, Message> {
    container(Space::new().height(1))
        .width(Length::Fill)
        .height(1)
        .style(move |_| container::Style {
            background: Some(c.line.into()),
            ..Default::default()
        })
        .into()
}
fn dl_stat<'a>(c: Colors, label: &'a str, value: Element<'a, Message>) -> Element<'a, Message> {
    column![dl_label(c, label), value].spacing(6).width(Length::Fill).into()
}
fn dl_flag<'a>(c: Colors, ok: bool, yes: &'a str, no: &'a str) -> Element<'a, Message> {
    row![
        if ok {
            icon(Icon::CheckCircle, 13.0, dl_ok(c))
        } else {
            icon(Icon::AlertCircle, 13.0, c.sub)
        },
        text(if ok { yes } else { no }).size(12).color(c.sub)
    ]
    .spacing(8)
    .align_y(Alignment::Center)
    .into()
}
fn dl_note<'a>(c: Colors, which: Icon, body: &'a str) -> Element<'a, Message> {
    row![
        container(icon(which, 13.0, c.sub)).padding(iced::Padding::ZERO.top(2)),
        text(body).size(12).color(c.sub).width(Length::Fill)
    ]
    .spacing(10)
    .into()
}
fn dl_progress<'a>(c: Colors, fraction: f32, girth: f32) -> Element<'a, Message> {
    progress_bar(0.0..=1.0, fraction)
        .girth(girth)
        .style(move |_| progress_bar::Style {
            background: c.select.into(),
            bar: c.accent.into(),
            border: Border {
                radius: (girth / 2.0).into(),
                ..Default::default()
            },
        })
        .into()
}
/// One installer step: number badge, then title, description and its button.
fn dl_step<'a>(
    c: Colors,
    number: u8,
    title: &'a str,
    body: &'a str,
    which: Icon,
    label: &'a str,
    message: Option<Message>,
) -> Element<'a, Message> {
    let badge = container(text(number.to_string()).font(SEMI).size(12).color(c.accent))
        .center(28)
        .style(move |_| container::Style {
            background: Some(c.select.into()),
            border: Border {
                radius: 14.0.into(),
                ..Default::default()
            },
            ..Default::default()
        });
    row![
        badge,
        column![
            text(title).font(SEMI).size(14).color(c.ink),
            text(body).size(12).color(c.sub),
            Space::new().height(4),
            control(c, Tone::Secondary, Some(which), label, message, None)
        ]
        .spacing(4)
        .width(Length::Fill)
    ]
    .spacing(12)
    .into()
}
/// Per-volume state of the official installer download, derived from the
/// operation's progress detail (it names the volume being transferred).
fn dl_parts<'a>(
    c: Colors,
    progress: &crate::operation::Progress,
    running: bool,
) -> Option<Element<'a, Message>> {
    const COUNT: u64 = 5;
    let metadata = progress.stage == "installer-metadata";
    if !metadata && progress.stage != "installer-download" {
        return None;
    }
    use crate::operation::PartState;
    let part = progress.part.filter(|_| !metadata);
    let current = part.map_or(0, |p| u64::from(p.number));
    let current_complete = part.is_some_and(|p| matches!(p.state, PartState::Complete | PartState::AlreadyComplete));
    let all_done = !metadata && progress.total > 0 && progress.done >= progress.total;
    let current_fraction = match part.map(|p| p.state) {
        Some(PartState::Downloading { done, expected }) if expected > 0 => (done as f64 / expected as f64).clamp(0.0, 1.0) as f32,
        _ => 0.0,
    };
    let mut list = column![row![
        text(t("Installer files")).font(SEMI).size(14).color(c.ink),
        Space::new().width(Length::Fill),
        text(tf("{count} parts", &[("count", &COUNT)])).size(12).color(c.sub)
    ]
    .align_y(Alignment::Center)]
    .spacing(12);
    for n in 1..=COUNT {
        let (fraction, state) = if metadata {
            if n <= progress.done {
                (0.0, t("Checked").to_owned())
            } else if n == progress.done + 1 && running {
                (0.0, t("Checking").to_owned())
            } else {
                (0.0, t("Waiting").to_owned())
            }
        } else if all_done || n < current || (n == current && current_complete) {
            (1.0, t("Complete").to_owned())
        } else if n == current {
            let pct = (current_fraction * 100.0).round();
            (
                current_fraction,
                if running {
                    format!("{pct:.0}%")
                } else {
                    tf("Stopped at {percent}%", &[("percent", &format!("{pct:.0}"))])
                },
            )
        } else {
            (0.0, if running { t("Waiting") } else { t("Not downloaded") }.to_owned())
        };
        let ext = if n == 1 { "exe" } else { "rar" };
        list = list.push(
            column![
                row![
                    text(format!("FFXIFullSetup_US.part{n}.{ext}"))
                        .font(MONO)
                        .size(12)
                        .color(c.ink),
                    Space::new().width(Length::Fill),
                    text(state).size(12).color(if fraction >= 1.0 { dl_ok(c) } else { c.sub })
                ]
                .align_y(Alignment::Center),
                dl_progress(c, fraction, 4.0)
            ]
            .spacing(6),
        );
    }
    Some(list.into())
}
/// The operation's detail line in the current language: an installer volume's
/// step is worded from its data, anything else is shown as reported.
fn dl_detail(progress: &crate::operation::Progress) -> std::borrow::Cow<'_, str> {
    use crate::operation::PartState;
    let Some(part) = progress.part else { return progress.detail.as_str().into() };
    let name = part.name;
    match part.state {
        PartState::Checking => tf("Checking {name}", &[("name", &name)]),
        PartState::Downloading { done: 0, .. } => tf("Downloading {name}", &[("name", &name)]),
        PartState::Downloading { done, expected } => tf(
            "Downloading {name}: {done} of {total}",
            &[("name", &name), ("done", &dl_bytes(done)), ("total", &dl_bytes(expected))],
        ),
        PartState::AlreadyComplete => tf("Already complete: {name}", &[("name", &name)]),
        PartState::Complete => tf("Complete: {name}", &[("name", &name)]),
        PartState::Changed => tf("{name} changed; downloading the official file again", &[("name", &name)]),
    }
    .into()
}
fn dl_bytes(value: u64) -> String {
    let v = value as f64;
    if v >= 1e9 {
        format!("{:.2} GB", v / 1e9)
    } else if v >= 1e6 {
        format!("{:.0} MB", v / 1e6)
    } else {
        format!("{value} B")
    }
}
fn dl_remaining_time(duration: std::time::Duration) -> String {
    // Round up without overflowing even Duration::MAX or showing zero for a fraction of a second.
    let seconds = duration.as_secs() as u128 + u128::from(duration.subsec_nanos() != 0);
    if seconds < 60 {
        return tf("{seconds} sec", &[("seconds", &seconds)]);
    }
    let minutes = seconds.div_ceil(60);
    if minutes < 60 {
        return tf("{minutes} min", &[("minutes", &minutes)]);
    }
    let (hours, minutes) = (minutes / 60, minutes % 60);
    if minutes == 0 {
        tf("{hours} hr", &[("hours", &hours)])
    } else {
        tf("{hours} hr {minutes} min", &[("hours", &hours), ("minutes", &minutes)])
    }
}
/// Percent done of the running file operation: the whole update's estimate when there is
/// one, else the current step's.
fn maintenance_percent(progress: &crate::operation::Progress) -> Option<u8> {
    match &progress.overall {
        Some(overall) => overall.remaining_percent().map(|left| 100 - left),
        None => progress.remaining_percent().map(|left| 100 - left),
    }
}
/// Home's line for Crystal's own file work: "Updating FINAL FANTASY XI · 43% · 3 min left".
fn maintenance_status(app: &App) -> String {
    let progress = &app.maintenance_progress;
    let mut parts = vec![app.maintenance_label().to_owned()];
    if let Some(percent) = maintenance_percent(progress) { parts.push(format!("{percent}%")); }
    let left = progress.overall.map_or(progress.remaining, |overall| overall.remaining);
    if let Some(left) = left.filter(|left| !left.is_zero()) {
        parts.push(tf("{time} left", &[("time", &dl_remaining_time(left))]));
    }
    parts.join(" · ")
}
fn dl_stage(kind: MaintenanceKind, stage: &str) -> String {
    match stage {
        "installer-metadata" => t("Checking the official installer files").into(),
        "installer-download" => t("Downloading the official installer").into(),
        "Probe" => t("Contacting the update service").into(),
        "Manifest" => t("Reading the official file manifest").into(),
        "SignOut" => t("Signing out idle PlayOnline sessions before updating").into(),
        "Check" => t("Checking local files").into(),
        "Download" => t("Downloading official files").into(),
        "Verify" => t("Verifying downloaded files").into(),
        "Apply" => t("Applying files").into(),
        "Finish" => t("Finalizing update…").into(),
        // Stage ids are internal; none yet ("") or an unknown one is named by
        // the operation itself.
        _ => kind.label().to_owned(),
    }
}
/// Launcher settings dialog.
fn settings_panel<'a>(app: &'a App, base: Element<'a, Message>) -> Element<'a, Message> {
    let c = colors(app.settings.light_theme);
    let body = column![
        column![
            form_label(t("FINAL FANTASY XI installation")).color(c.ink),
            row![
                text_field(t("Path to the FINAL FANTASY XI folder"), &crate::privacy::mask_path(&app.settings.install_path))
                    .on_input_maybe(crate::privacy::editable().then_some(|v| Message::InstallPathChanged(crate::privacy::unmask_path(v))))
                    .on_submit(Message::SelectInstallPath)
                    .font(MONO)
                    .size(13),
                control(
                    c,
                    Tone::Secondary,
                    Some(Icon::Search),
                    if app.installation_loading { t("Finding…") } else { t("Find game") },
                    (app.maintenance.is_none() && !app.installation_loading).then_some(Message::FindInstallation),
                    None
                ),
                icon_control(c, Icon::Folder, true, (app.maintenance.is_none()).then_some(Message::Browse(crate::PickTarget::GameFolder)))
            ]
            .spacing(8)
            .align_y(Alignment::Center),
            text(t("Used to check the installed version against the official update service."))
                .size(12)
                .color(c.sub),
            dl_flag(
                c,
                app.settings.install_selected,
                t("Folder confirmed as the target for checks and changes"),
                t("Confirm this folder before starting")
            )
        ]
        .spacing(8),
        container(
            column![
                row![
                    icon(Icon::Wrench, 16.0, c.accent),
                    text(t("Install & repair")).font(SEMI).size(14).color(c.ink)
                ]
                .spacing(10)
                .align_y(Alignment::Center),
                text(t("Check or repair this installation under Downloads → Troubleshooting. Play installs official updates."))
                    .size(12)
                    .color(c.sub),
                Space::new().height(4),
                card_link(c, t("Open Downloads"), Message::Navigate(Page::Maintenance))
            ]
            .spacing(8),
        )
        .padding(16)
        .width(Length::Fill)
        .style(soft_tile(c))
    ]
    .spacing(20);
    let footer = row![
        container(private_text(app, &app.status, 12.0, BODY, c.sub)).width(Length::Fill),
        control(c, Tone::Secondary, None, t("Close"), Some(Message::ToggleSettings), None),
        control(c, Tone::Primary, None, t("Save settings"), Some(Message::SaveSettings), None)
    ]
    .spacing(8)
    .align_y(Alignment::Center);
    modal(
        base,
        dialog_header(
            c,
            t("Launcher settings"),
            t("Where the game lives and how the launcher looks after it."),
            Some(Message::ToggleSettings),
        ),
        Some(body.into()),
        Some(footer.into()),
        600.0,
        app.viewport.height - 48.0,
    )
}
/// A news date as month and day, worded for the UI language: "Sep 30" or
/// "9月30日". The day is the one the source states in its own zone (feed dates
/// are JST, US maintenance dates PDT/PST), never shifted to this PC's zone, so
/// both languages agree with the official page. A date that is not recognised
/// is shown as it came.
fn news_date(raw: &str) -> String {
    let Some((month, day)) = news_month_day(raw) else {
        return raw.to_owned();
    };
    const MONTHS: [&str; 12] = ["Jan", "Feb", "Mar", "Apr", "May", "Jun", "Jul", "Aug", "Sep", "Oct", "Nov", "Dec"];
    match crate::i18n::current() {
        crate::i18n::Lang::Ja => format!("{month}月{day}日"),
        crate::i18n::Lang::En => format!("{} {day}", MONTHS[month as usize - 1]),
    }
}
/// The month and day in any date text the official sources use: RSS and ISO
/// timestamps, the US list's "Sep. 26, 2026 22:00 [PDT] From: …" and the
/// Japanese list's "2026.09.28(月) 23:50 From: …".
fn news_month_day(raw: &str) -> Option<(u32, u32)> {
    use chrono::Datelike;
    let normalized = raw.replace(" JST", " +0900");
    if let Ok(date) = chrono::DateTime::parse_from_rfc2822(&normalized)
        .or_else(|_| chrono::DateTime::parse_from_rfc3339(raw))
    {
        return Some((date.month(), date.day()));
    }
    let prefix = raw.split(" [").next().unwrap_or(raw);
    if let Ok(date) = chrono::NaiveDateTime::parse_from_str(prefix, "%b. %d, %Y %H:%M") {
        return Some((date.month(), date.day()));
    }
    // "2026.09.28(月) 23:50 …": the date runs to the weekday mark.
    let ymd = raw.trim().split(['(', '（', ' ']).next()?;
    let date = chrono::NaiveDate::parse_from_str(ymd, "%Y.%m.%d").ok()?;
    Some((date.month(), date.day()))
}
/// The feed summary as one run of text; the card wraps and ellipsises it.
fn teaser(summary: &str) -> String {
    summary.split_whitespace().collect::<Vec<_>>().join(" ")
}
fn field<'a>(
    label: &'a str,
    placeholder: &'a str,
    value: &'a str,
    field: Field,
    secure: bool,
    enabled: bool,
) -> Element<'a, Message> {
    let hide = crate::privacy::on();
    let shown = crate::privacy::mask(value);
    column![
        form_label(label),
        text_field(placeholder, &shown)
            .secure(secure)
            .on_input_maybe((enabled && !hide).then_some(move |value| Message::EditorChanged(field, value)))
    ]
    .spacing(8)
    .width(Length::Fill)
    .into()
}
#[cfg(test)]
mod privacy_field_tests {
    use super::*;

    #[test]
    fn private_account_fields_have_fixed_masks_and_cannot_replace_saved_values() {
        crate::privacy::set(true);
        let mut expected = None;
        for value in ["Al", "An exceptionally long account name", "アリスドラゴン", "e\u{301}🐉"] {
            let build = || field("Account name", "Name", value, Field::Label, true, true);
            let texts = crate::privacy::testing::text_bounds(build(), iced::Size::new(500.0, 200.0));
            assert_eq!(texts[1].0, "••••••••");
            if let Some(expected) = &expected { assert_eq!(&texts, expected); } else { expected = Some(texts); }
            assert!(crate::privacy::testing::type_text(build(), "X").is_empty());
        }
        crate::privacy::set(false);
        let messages = crate::privacy::testing::type_text(field("Account name", "Name", "Al", Field::Label, false, true), "X");
        assert!(matches!(messages.as_slice(), [Message::EditorChanged(Field::Label, value)] if value == "AlX"));
    }

    #[test]
    fn password_fields_keep_normal_security_with_privacy_off() {
        crate::privacy::set(false);
        let build = || field("Password", "Password", "synthetic", Field::PolPassword, true, true);
        let texts = crate::privacy::testing::texts(build(), iced::Size::new(500.0, 200.0));
        assert_eq!(texts[1], "•••••••••");
        let messages = crate::privacy::testing::type_text(build(), "X");
        assert!(matches!(messages.as_slice(), [Message::EditorChanged(Field::PolPassword, value)] if value == "syntheticX"));
        crate::privacy::set(true);
        assert!(crate::privacy::testing::type_text(build(), "X").is_empty());
        crate::privacy::set(false);
    }
}
// ---- Dialogs: one frame for every modal ----
fn dialog_style(theme: &Theme) -> container::Style {
    let c = tc(theme);
    container::Style {
        background: Some(c.panel.into()),
        text_color: Some(c.ink),
        border: Border {
            color: c.line,
            width: 1.0,
            radius: CARD_R.into(),
        },
        shadow: Shadow {
            color: Color::from_rgba(0.0, 0.0, 0.0, if is_light(c) { 0.18 } else { 0.4 }),
            offset: Vector::new(0.0, 16.0),
            blur_radius: 48.0,
        },
        ..Default::default()
    }
}
fn backdrop(theme: &Theme) -> container::Style {
    let light = theme.palette().background.r > 0.5;
    container::Style {
        background: Some(
            if light {
                Color::from_rgba8(18, 36, 50, 0.38)
            } else {
                Color::from_rgba8(3, 8, 13, 0.72)
            }
            .into(),
        ),
        ..Default::default()
    }
}
/// Dialog title and muted subtitle, with an optional close button.
fn dialog_header<'a>(
    c: Colors,
    title: &'a str,
    subtitle: &'a str,
    close: Option<Message>,
) -> Element<'a, Message> {
    let mut header = row![column![
        text(title).font(BODY).size(20).color(c.ink),
        text(subtitle).font(BODY).size(13).color(c.sub)
    ]
    .spacing(4)
    .width(Length::Fill)]
    .spacing(16)
    .align_y(Alignment::Start);
    if let Some(close) = close {
        header = header.push(icon_control(c, Icon::Close, false, Some(close)));
    }
    header.into()
}
fn dialog_overlay<'a>(
    base: Element<'a, Message>,
    dialog: container::Container<'a, Message>,
) -> Element<'a, Message> {
    stack![
        base,
        opaque(
            container(dialog.style(dialog_style))
                .center_x(Length::Fill)
                .center_y(Length::Fill)
                .padding(24)
                .style(backdrop)
        )
    ]
    .into()
}

fn ah_lookup_panel<'a>(app: &'a App, base: Element<'a, Message>) -> Element<'a, Message> {
    let lookup = &app.ah_lookup;
    let c = colors(app.settings.light_theme);
    let s = scale(app);
    let header = dialog_header(c, "FFXIAH", &lookup.query, Some(Message::CloseAh));
    let mut body = column![
        text(tf("Prices on {world}", &[("world", &lookup.world)])).font(SEMI).size(16).color(c.accent),
        row![container(search_field(app, false, s, c, c.sub)).height(37.0 * s).width(Length::Fill),
            crate::controller_nav::button(text(t("Search"))).padding([9, 16]).on_press(Message::SearchAh).style(quiet)]
            .spacing(12).align_y(Alignment::Center),
    ].spacing(16);
    match &lookup.result {
        None => body = body.push(text(tf("Searching {world}…", &[("world", &lookup.world)])).font(BODY).size(14).color(c.sub)),
        Some(Err(error)) => body = body.push(text(error).font(BODY).size(14).color(c.sub)),
        Some(Ok(results)) => {
            if results.items.is_empty() { body = body.push(text(t("No matching items found.")).font(BODY).size(14).color(c.sub)); }
            for item in &results.items {
                let name = match item.stack {
                    Some(count) => format!("{} ×{count}", item.name),
                    None => item.name.clone(),
                };
                let price = item.price.map(|v| tf("{price} gil", &[("price", &grouped_number(v))]))
                    .unwrap_or_else(|| t("No sales listed").into());
                let stock = item.stock.map(|v| tf("{count} in stock", &[("count", &v)]))
                    .unwrap_or_else(|| t("Stock unavailable").into());
                let line = row![
                    text(name).font(BODY).size(14).color(c.ink).width(Length::Fill),
                    column![text(if item.last_sale { t("Last sale") } else { t("Price") }).font(BODY).size(11).color(c.sub),
                        text(price).font(SEMI).size(14).color(c.ink)].width(150),
                    text(stock).font(BODY).size(12).color(c.sub).width(105),
                    icon(Icon::ExternalLink, 14.0, c.sub),
                ].spacing(12).align_y(Alignment::Center);
                body = body.push(crate::controller_nav::button(line).padding([12, 10]).width(Length::Fill)
                    .style(quiet).on_press(Message::OpenUrl(item.url.clone())));
            }
            if results.truncated { body = body.push(text(t("Showing the first 30 matches. Refine the item name for more specific results.")).font(BODY).size(12).color(c.sub)); }
            if let Some(warning) = &results.warning { body = body.push(text(warning).font(BODY).size(12).color(c.sub)); }
        }
    }
    let footer = column![
        text(t("Website links use the world saved in your browser. These results use the world selected in Crystal."))
            .font(BODY).size(12).color(c.sub),
        row![crate::controller_nav::button(text(t("Open FFXIAH"))).padding([9, 16]).style(quiet)
                .on_press(Message::OpenUrl(ah_search_url(&lookup.query, lookup.world))),
            Space::new().width(Length::Fill),
            crate::controller_nav::button(text(t("Close"))).padding([9, 16]).style(primary).on_press(Message::CloseAh)]
    ].spacing(12).into();
    modal(base, header, Some(body.into()), Some(footer), (app.viewport.width - 48.0).min(760.0), app.viewport.height - 48.0)
}

fn grouped_number(value: u64) -> String {
    let digits = value.to_string();
    let mut out = String::new();
    for (i, c) in digits.chars().enumerate() {
        if i > 0 && (digits.len() - i) % 3 == 0 { out.push(','); }
        out.push(c);
    }
    out
}
/// A dialog that sizes to its content: header, optional body and footer,
/// separated by hairlines; the whole frame scrolls if the window is short.
fn modal<'a>(
    base: Element<'a, Message>,
    header: Element<'a, Message>,
    body: Option<Element<'a, Message>>,
    footer: Option<Element<'a, Message>>,
    width: f32,
    max_height: f32,
) -> Element<'a, Message> {
    let has_body = body.is_some();
    let mut frame = column![container(header)
        .padding(iced::Padding::new(GUTTER).bottom(if has_body { 20.0 } else { 8.0 }))
        .width(Length::Fill)]
    .width(Length::Fill);
    if let Some(body) = body {
        frame = frame
            .push(hline())
            .push(container(body).padding(GUTTER).width(Length::Fill));
    }
    if let Some(footer) = footer {
        if has_body {
            frame = frame.push(hline());
        }
        // Without a body the footer closes the dialog, so it takes the
        // header's 24 px outer margin at the bottom.
        let bottom = if has_body { 16.0 } else { GUTTER };
        frame = frame.push(
            container(footer)
                .padding(iced::Padding::new(16.0).left(GUTTER).right(GUTTER).bottom(bottom))
                .width(Length::Fill),
        );
    }
    dialog_overlay(
        base,
        container(inset_scroll(frame)).width(width).max_height(max_height.max(200.0)),
    )
}
/// A fixed-height dialog whose body scrolls between a pinned header and footer.
fn modal_with_footer<'a>(
    base: Element<'a, Message>,
    header: Element<'a, Message>,
    content: Element<'a, Message>,
    footer: Element<'a, Message>,
    width: u32,
    height: f32,
) -> Element<'a, Message> {
    let frame = column![
        container(header).padding(iced::Padding::new(GUTTER).bottom(20.0)),
        hline(),
        inset_scroll(container(content).padding(GUTTER)).height(Length::Fill),
        hline(),
        container(footer).padding([16.0, GUTTER])
    ]
    .height(Length::Fill);
    dialog_overlay(base, container(frame).width(width).height(height))
}

/// Scrolling for pages and dialogs: a thin, quiet scroller that floats over
/// the content's edge instead of a track that narrows it, so page edges line
/// up with Home's.
fn inset_scroll<'a>(
    content: impl Into<Element<'a, Message>>,
) -> scrollable::Scrollable<'a, Message> {
    scrollable(container(content).width(Length::Fill))
        .direction(scrollable::Direction::Vertical(
            scrollable::Scrollbar::new().width(6).scroller_width(6).margin(3),
        ))
        .style(|theme: &Theme, status| {
            let c = tc(theme);
            let alpha = match status {
                scrollable::Status::Dragged {
                    is_vertical_scrollbar_dragged: true,
                    ..
                } => 0.7,
                scrollable::Status::Hovered {
                    is_vertical_scrollbar_hovered: true,
                    ..
                } => 0.55,
                _ => 0.3,
            };
            let rail = scrollable::Rail {
                background: None,
                border: Border::default(),
                scroller: scrollable::Scroller {
                    background: with_alpha(c.sub, alpha).into(),
                    border: Border {
                        radius: 3.0.into(),
                        ..Default::default()
                    },
                },
            };
            let mut style = scrollable::default(theme, status);
            style.vertical_rail = rail;
            style.horizontal_rail = rail;
            style
        })
}

#[cfg(test)]
mod play_state_tests {
    use super::*;

    #[test]
    fn play_button_follows_running_and_launching_accounts() {
        assert_eq!(play_state(false, 0, 1), PlayState::Play);
        assert_eq!(play_state(false, 0, 0), PlayState::Play);
        assert_eq!(play_state(true, 0, 0), PlayState::Starting);
        assert_eq!(play_state(true, 0, 1), PlayState::Play);
        assert_eq!(play_state(false, 1, 0), PlayState::Playing);
        // Some running, others still launchable: Play stays a launch button.
        assert_eq!(play_state(false, 1, 2), PlayState::Play);
    }
}
