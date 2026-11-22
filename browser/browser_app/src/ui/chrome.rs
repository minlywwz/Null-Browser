use browser_core::{Accent, Settings, ToolbarPosition};
use display_list::{Color, Command, DisplayList, Rect, TextAlign, TextStyle};
use rendering_ffi::{PreparedScene, TextBatch, TextFactory};

pub const BACKGROUND: Color = Color::rgb(0x101216);
pub const ADDRESS_BACKGROUND: u32 = 0x20262e;
pub const TAB_BAR: f32 = 34.0;
pub const NAV_BAR: f32 = 52.0;
pub const TOOLBAR: f32 = TAB_BAR + NAV_BAR;
const INK: Color = Color::rgb(0xe8eef8);
const MUTED: Color = Color::rgb(0xa6b6cc);
const PANEL: Color = Color::rgb(0x121c2a);

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TabViewModel {
    pub id: usize,
    pub title: String,
    pub active: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Action {
    SelectTab(usize),
    CloseTab(usize),
    NewTab,
    Back,
    Forward,
    Reload,
    Address,
    Menu,
    Home,
    Open,
    Motion,
    Studio,
    Position,
    Accent,
    Duration,
    Fullscreen,
}

#[derive(Clone, PartialEq)]
pub struct Model {
    pub width: f32,
    pub height: f32,
    pub title: String,
    pub status: Option<String>,
    pub settings: Settings,
    pub tabs: Vec<TabViewModel>,
    pub can_back: bool,
    pub can_forward: bool,
    pub editing: bool,
    pub loading: bool,
    pub menu: bool,
    pub studio: bool,
    pub demo: bool,
    pub fullscreen: bool,
    pub hovered: Option<Action>,
    pub focused: Option<Action>,
    pub scroll: f32,
    pub scroll_max: f32,
}

pub struct Geometry {
    pub toolbar: Rect,
    pub tab_bar: Rect,
    pub viewport: Rect,
    pub address: Rect,
    pub studio: Option<Rect>,
    pub menu: Option<Rect>,
    pub preview: Rect,
    pub buttons: Vec<(Action, Rect)>,
}

pub fn viewport(width: f32, height: f32, position: ToolbarPosition) -> Rect {
    let toolbar = TOOLBAR.min(height);
    Rect::new(
        0.0,
        if position == ToolbarPosition::Top {
            toolbar
        } else {
            0.0
        },
        width,
        height - toolbar,
    )
}

pub fn preview(view: Rect) -> Rect {
    let width = (view.width - 64.0)
        .clamp(1.0, 820.0)
        .min((view.height - 128.0).max(1.0) * 16.0 / 9.0);
    let height = width * 9.0 / 16.0;
    Rect::new(
        view.x + (view.width - width) / 2.0,
        view.y + 92.0,
        width,
        height,
    )
}

impl Geometry {
    pub fn for_model(model: &Model) -> Self {
        let view = viewport(model.width, model.height, model.settings.position);
        let toolbar_y = if model.settings.position == ToolbarPosition::Top {
            0.0
        } else {
            view.height
        };
        let toolbar = Rect::new(0.0, toolbar_y, model.width, TOOLBAR);
        let (tab_bar_y, nav_y) = if model.settings.position == ToolbarPosition::Top {
            (toolbar_y, toolbar_y + TAB_BAR + 7.0)
        } else {
            (toolbar_y + NAV_BAR, toolbar_y + 7.0)
        };
        let tab_bar = Rect::new(0.0, tab_bar_y, model.width, TAB_BAR);
        let address = Rect::new(128.0, nav_y, (model.width - 185.0).max(1.0), 38.0);
        let mut buttons = Vec::new();

        let tab_count = model.tabs.len().max(1) as f32;
        let available_w = (model.width - 60.0).max(60.0);
        let tab_w = (available_w / tab_count - 4.0).clamp(70.0, 180.0);
        for (i, tab) in model.tabs.iter().enumerate() {
            let tab_rect = Rect::new(8.0 + i as f32 * (tab_w + 4.0), tab_bar_y + 3.0, tab_w, 28.0);
            buttons.push((Action::SelectTab(tab.id), tab_rect));
            let close_rect = Rect::new(
                tab_rect.x + tab_rect.width - 22.0,
                tab_rect.y + 5.0,
                18.0,
                18.0,
            );
            buttons.push((Action::CloseTab(tab.id), close_rect));
        }

        let new_tab_x = 8.0 + model.tabs.len() as f32 * (tab_w + 4.0) + 2.0;
        let new_tab_rect = Rect::new(new_tab_x, tab_bar_y + 4.0, 26.0, 26.0);
        buttons.push((Action::NewTab, new_tab_rect));

        buttons.push((Action::Back, Rect::new(12.0, nav_y, 34.0, 38.0)));
        buttons.push((Action::Forward, Rect::new(48.0, nav_y, 34.0, 38.0)));
        buttons.push((Action::Reload, Rect::new(84.0, nav_y, 34.0, 38.0)));
        buttons.push((Action::Address, address));
        buttons.push((
            Action::Menu,
            Rect::new(model.width - 46.0, nav_y, 34.0, 38.0),
        ));

        let preview = preview(view);
        if model.demo {
            buttons.push((Action::Fullscreen, preview));
        }
        let menu = model.menu.then(|| {
            let y = if model.settings.position == ToolbarPosition::Top {
                view.y + 8.0
            } else {
                view.height - 170.0
            };
            let panel = Rect::new(model.width - 256.0, y, 244.0, 162.0);
            for (index, action) in [Action::Home, Action::Open, Action::Studio, Action::Motion]
                .into_iter()
                .enumerate()
            {
                buttons.push((
                    action,
                    Rect::new(
                        panel.x + 6.0,
                        panel.y + 8.0 + index as f32 * 36.0,
                        232.0,
                        36.0,
                    ),
                ));
            }
            panel
        });
        let studio = model.studio.then(|| {
            let panel = Rect::new((model.width - 338.0).max(8.0), view.y + 13.0, 322.0, 242.0);
            for (index, action) in [Action::Position, Action::Accent, Action::Duration]
                .into_iter()
                .enumerate()
            {
                buttons.push((
                    action,
                    Rect::new(
                        panel.x + 14.0,
                        panel.y + 78.0 + index as f32 * 44.0,
                        294.0,
                        36.0,
                    ),
                ));
            }
            panel
        });
        Self {
            toolbar,
            tab_bar,
            viewport: view,
            address,
            studio,
            menu,
            preview,
            buttons,
        }
    }

    pub fn action(&self, x: f32, y: f32) -> Option<Action> {
        self.buttons.iter().rev().find_map(|(action, rect)| {
            if !rect.contains(x, y) {
                return None;
            }
            if self.studio.is_some_and(|panel| panel.contains(x, y))
                && !matches!(action, Action::Position | Action::Accent | Action::Duration)
            {
                return None;
            }
            if self.menu.is_some_and(|panel| panel.contains(x, y))
                && !matches!(
                    action,
                    Action::Home | Action::Open | Action::Studio | Action::Motion
                )
            {
                return None;
            }
            Some(*action)
        })
    }
}

fn accent(value: Accent) -> (Color, &'static str) {
    match value {
        Accent::Mint => (Color::rgb(0x86ebd7), "Mint"),
        Accent::Iris => (Color::rgb(0xc4b3ff), "Iris"),
        Accent::Amber => (Color::rgb(0xffce83), "Amber"),
    }
}

struct Painter<'a> {
    commands: Vec<Command>,
    text: TextBatch<'a>,
}

impl Painter<'_> {
    fn fill(&mut self, rect: Rect, color: Color, radius: f32) {
        self.commands.push(Command::Fill {
            rect,
            color,
            radius,
        });
    }

    fn label(
        &mut self,
        value: &str,
        rect: Rect,
        color: Color,
        style: TextStyle,
    ) -> Result<(), String> {
        if value.is_empty() || rect.width <= 1.0 {
            return Ok(());
        }
        let metrics = self.text.label(value, style, rect.width)?;
        self.commands.push(Command::Text {
            bounds: Rect::new(
                rect.x,
                rect.y + ((rect.height - metrics.height) / 2.0).max(0.0),
                rect.width,
                metrics.height,
            ),
            color,
            layout: metrics.layout,
        });
        Ok(())
    }
}

pub fn build(factory: &TextFactory, model: &Model) -> Result<PreparedScene, String> {
    let mut paint = Painter {
        commands: Vec::new(),
        text: factory.batch(),
    };
    let bounds = Geometry::for_model(model);
    let (accent_color, accent_name) = accent(model.settings.accent);
    let label = TextStyle {
        size: 13.0,
        weight: 600,
        align: TextAlign::Start,
    };
    let center = TextStyle {
        align: TextAlign::Center,
        ..label
    };

    if !model.fullscreen {
        paint.fill(bounds.toolbar, Color::rgb(0x171a20), 0.0);
        let divider_y = if model.settings.position == ToolbarPosition::Top {
            bounds.toolbar.y + bounds.toolbar.height - 1.0
        } else {
            bounds.toolbar.y
        };
        paint.fill(
            Rect::new(0.0, divider_y, model.width, 1.0),
            Color::rgb(0x2b3038),
            0.0,
        );

        paint.fill(bounds.tab_bar, Color::rgb(0x13151a), 0.0);
        let tab_divider_y = if model.settings.position == ToolbarPosition::Top {
            bounds.tab_bar.y + bounds.tab_bar.height - 1.0
        } else {
            bounds.tab_bar.y
        };
        paint.fill(
            Rect::new(0.0, tab_divider_y, model.width, 1.0),
            Color::rgb(0x242830),
            0.0,
        );

        let tab_count = model.tabs.len().max(1) as f32;
        let available_w = (model.width - 60.0).max(60.0);
        let tab_w = (available_w / tab_count - 4.0).clamp(70.0, 180.0);
        let tab_bar_y = bounds.tab_bar.y;

        for (i, tab) in model.tabs.iter().enumerate() {
            let tab_rect = Rect::new(8.0 + i as f32 * (tab_w + 4.0), tab_bar_y + 3.0, tab_w, 28.0);
            let close_rect = Rect::new(
                tab_rect.x + tab_rect.width - 22.0,
                tab_rect.y + 5.0,
                18.0,
                18.0,
            );

            if tab.active {
                paint.fill(tab_rect, Color::rgb(0x222730), 6.0);
                paint.fill(
                    Rect::new(tab_rect.x, tab_rect.y, tab_rect.width, 2.0),
                    accent_color,
                    1.0,
                );
            } else {
                let hovered = model.hovered == Some(Action::SelectTab(tab.id));
                let bg = if hovered {
                    Color::rgb(0x1c202a)
                } else {
                    Color::rgb(0x161920)
                };
                paint.fill(tab_rect, bg, 6.0);
            }

            let tab_text_color = if tab.active { INK } else { MUTED };
            let tab_style = TextStyle {
                size: 12.0,
                weight: if tab.active { 600 } else { 400 },
                align: TextAlign::Start,
            };
            let label_width = (tab_rect.width - 34.0).max(1.0);
            let label_rect = Rect::new(tab_rect.x + 10.0, tab_rect.y, label_width, tab_rect.height);
            let title = if tab.title.is_empty() {
                "New Tab"
            } else {
                &tab.title
            };
            paint.label(title, label_rect, tab_text_color, tab_style)?;

            let close_hovered = model.hovered == Some(Action::CloseTab(tab.id));
            if close_hovered {
                paint.fill(close_rect, Color::rgb(0x3a4252), 4.0);
            }
            paint.label(
                "×",
                close_rect,
                if close_hovered { INK } else { MUTED },
                TextStyle {
                    size: 14.0,
                    weight: 500,
                    align: TextAlign::Center,
                },
            )?;
        }

        let new_tab_x = 8.0 + model.tabs.len() as f32 * (tab_w + 4.0) + 2.0;
        let new_tab_rect = Rect::new(new_tab_x, tab_bar_y + 4.0, 26.0, 26.0);
        let new_tab_hovered = model.hovered == Some(Action::NewTab);
        let new_tab_focused = model.focused == Some(Action::NewTab);
        let new_tab_bg = if new_tab_focused {
            accent_color
        } else if new_tab_hovered {
            Color::rgb(0x282f3c)
        } else {
            Color::rgb(0x161920)
        };
        paint.fill(new_tab_rect, new_tab_bg, 5.0);
        let plus_color = if new_tab_focused {
            Color::rgb(0x111a24)
        } else if new_tab_hovered {
            INK
        } else {
            MUTED
        };
        paint.label(
            "+",
            new_tab_rect,
            plus_color,
            TextStyle {
                size: 16.0,
                weight: 500,
                align: TextAlign::Center,
            },
        )?;

        let address_outline = if model.editing || model.focused == Some(Action::Address) {
            accent_color
        } else {
            Color::rgb(0x303640)
        };
        paint.fill(bounds.address, address_outline, 10.0);
        let inner = Rect::new(
            bounds.address.x + 1.0,
            bounds.address.y + 1.0,
            (bounds.address.width - 2.0).max(1.0),
            bounds.address.height - 2.0,
        );
        paint.fill(inner, Color::rgb(ADDRESS_BACKGROUND), 9.0);
        if bounds.address.width > 56.0 && !model.editing {
            paint.fill(
                Rect::new(bounds.address.x + 15.0, bounds.address.y + 16.0, 6.0, 6.0),
                if model.loading {
                    accent_color
                } else {
                    Color::rgb(0x7b8797)
                },
                3.0,
            );
            paint.label(
                if model.title.is_empty() {
                    "Enter a domain or URL"
                } else {
                    &model.title
                },
                Rect::new(
                    bounds.address.x + 33.0,
                    bounds.address.y,
                    bounds.address.width - 40.0,
                    38.0,
                ),
                if model.title.is_empty() { MUTED } else { INK },
                TextStyle {
                    size: 14.0,
                    weight: 400,
                    ..label
                },
            )?;
        }
        if model.loading {
            paint.fill(
                Rect::new(
                    bounds.address.x + 10.0,
                    bounds.address.y + 35.0,
                    (bounds.address.width * 0.22).max(1.0),
                    1.0,
                ),
                accent_color,
                0.0,
            );
        }

        for &(action, rect) in &bounds.buttons {
            let (value, enabled, active) = match action {
                Action::Back => ("←", model.can_back, false),
                Action::Forward => ("→", model.can_forward, false),
                Action::Reload => ("↻", true, model.loading),
                Action::Menu => ("⋯", true, model.menu),
                _ => continue,
            };
            let focused = model.focused == Some(action);
            let hot = enabled && model.hovered == Some(action);
            let tint = if focused {
                accent_color
            } else if active || hot {
                Color::rgb(0x303844)
            } else {
                Color::TRANSPARENT
            };
            paint.fill(rect, tint, 9.0);
            let color = if focused {
                Color::rgb(0x111a24)
            } else if active || hot {
                accent_color
            } else if enabled {
                INK
            } else {
                Color::rgb(0x61728a)
            };
            let font = TextStyle {
                size: 19.0,
                weight: 500,
                ..center
            };
            paint.label(value, rect, color, font)?;
        }

        if let Some(panel) = bounds.menu {
            paint.fill(panel.translated(0.0, 3.0), Color::rgba(0x00000070), 11.0);
            paint.fill(panel, Color::rgb(0x292f38), 11.0);
            paint.fill(
                Rect::new(
                    panel.x + 1.0,
                    panel.y + 1.0,
                    panel.width - 2.0,
                    panel.height - 2.0,
                ),
                PANEL,
                10.0,
            );
            for &(action, rect) in &bounds.buttons {
                let (name, shortcut) = match action {
                    Action::Home => ("Start page", ""),
                    Action::Open => ("Open HTML", "Ctrl+O"),
                    Action::Studio => ("Browser Studio", "F2"),
                    Action::Motion => ("Motion Engine", ""),
                    _ => continue,
                };
                if model.hovered == Some(action) || model.focused == Some(action) {
                    paint.fill(rect, Color::rgb(0x2b3745), 7.0);
                }
                paint.label(
                    name,
                    Rect::new(rect.x + 12.0, rect.y, 149.0, rect.height),
                    INK,
                    label,
                )?;
                paint.label(
                    shortcut,
                    Rect::new(rect.x + 161.0, rect.y, 59.0, rect.height),
                    MUTED,
                    TextStyle {
                        size: 12.0,
                        align: TextAlign::End,
                        ..label
                    },
                )?;
            }
        }

        if model.demo {
            let y = bounds.viewport.y;
            paint.label(
                "Motion Engine",
                Rect::new(0.0, y + 15.0, model.width, 40.0),
                INK,
                TextStyle {
                    size: 29.0,
                    weight: 700,
                    align: TextAlign::Center,
                },
            )?;
            paint.label(
                "Live synthetic source  /  F to expand  /  Esc to return",
                Rect::new(0.0, y + 55.0, model.width, 24.0),
                MUTED,
                center,
            )?;
            let frame = bounds.preview;
            // The image is drawn below this four-sided frame by GPU Compositor.
            for border in [
                Rect::new(frame.x - 1.0, frame.y - 1.0, frame.width + 2.0, 1.0),
                Rect::new(
                    frame.x - 1.0,
                    frame.y + frame.height,
                    frame.width + 2.0,
                    1.0,
                ),
                Rect::new(frame.x - 1.0, frame.y, 1.0, frame.height),
                Rect::new(frame.x + frame.width, frame.y, 1.0, frame.height),
            ] {
                paint.fill(border, accent_color, 0.0);
            }
        } else if model.scroll_max > 0.0 {
            let track = (bounds.viewport.height - 24.0).max(1.0);
            let length = (track * bounds.viewport.height
                / (bounds.viewport.height + model.scroll_max))
                .clamp(18.0_f32.min(track), track);
            let y = bounds.viewport.y + 12.0 + (track - length) * model.scroll / model.scroll_max;
            paint.fill(
                Rect::new(model.width - 6.0, y, 3.0, length),
                Color::rgba(0x9eb4c880),
                1.5,
            );
        }

        if let Some(panel) = bounds.studio {
            paint.fill(panel.translated(0.0, 4.0), Color::rgba(0x00000070), 14.0);
            paint.fill(panel, Color::rgb(0x192637), 14.0);
            paint.label(
                "Browser Studio",
                Rect::new(panel.x + 16.0, panel.y + 10.0, 280.0, 28.0),
                INK,
                TextStyle {
                    size: 21.0,
                    weight: 700,
                    ..label
                },
            )?;
            paint.label(
                "Customize the browser interface",
                Rect::new(panel.x + 16.0, panel.y + 40.0, 280.0, 20.0),
                MUTED,
                label,
            )?;
            for (action, rect) in &bounds.buttons {
                let (name, value) = match action {
                    Action::Position => (
                        "Toolbar position",
                        if model.settings.position == ToolbarPosition::Top {
                            "Top".into()
                        } else {
                            "Bottom".into()
                        },
                    ),
                    Action::Accent => ("Accent color", accent_name.into()),
                    Action::Duration => (
                        "Motion duration",
                        if model.settings.motion_ms == 0 {
                            "Off".into()
                        } else {
                            format!("{} ms", model.settings.motion_ms)
                        },
                    ),
                    _ => continue,
                };
                let hot = model.hovered == Some(*action) || model.focused == Some(*action);
                paint.fill(
                    *rect,
                    if hot {
                        Color::rgb(0x304858)
                    } else {
                        Color::rgb(0x101a28)
                    },
                    8.0,
                );
                paint.label(
                    name,
                    Rect::new(rect.x + 12.0, rect.y, 160.0, rect.height),
                    INK,
                    label,
                )?;
                paint.label(
                    &value,
                    Rect::new(rect.x + 175.0, rect.y, 107.0, rect.height),
                    accent_color,
                    TextStyle {
                        align: TextAlign::End,
                        ..label
                    },
                )?;
            }
            paint.label(
                "F2 or Esc to close",
                Rect::new(panel.x + 16.0, panel.y + 212.0, 280.0, 20.0),
                MUTED,
                TextStyle {
                    size: 12.0,
                    ..label
                },
            )?;
        }
        if let Some(status) = &model.status {
            let width = (model.width - 44.0).clamp(1.0, 690.0);
            let x = (model.width - width) / 2.0;
            let y = bounds.viewport.y + (bounds.viewport.height - 48.0).max(2.0);
            paint.fill(Rect::new(x, y, width, 36.0), Color::rgb(0x302d3b), 9.0);
            paint.label(
                status,
                Rect::new(x + 13.0, y, width - 26.0, 36.0),
                INK,
                label,
            )?;
        }
    }

    Ok(paint.text.prepare(
        0,
        DisplayList {
            background: BACKGROUND,
            commands: paint.commands,
        },
        Vec::new(),
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn geometry_layout_tabs_and_nav() {
        let model = Model {
            width: 800.0,
            height: 600.0,
            title: "Test".into(),
            status: None,
            settings: Settings::default(),
            tabs: vec![
                TabViewModel {
                    id: 0,
                    title: "Tab 1".into(),
                    active: true,
                },
                TabViewModel {
                    id: 1,
                    title: "Tab 2".into(),
                    active: false,
                },
            ],
            can_back: true,
            can_forward: false,
            editing: false,
            loading: false,
            menu: false,
            studio: false,
            demo: false,
            fullscreen: false,
            hovered: None,
            focused: None,
            scroll: 0.0,
            scroll_max: 0.0,
        };
        let geom = Geometry::for_model(&model);
        assert_eq!(geom.toolbar.height, TOOLBAR);
        assert_eq!(geom.tab_bar.height, TAB_BAR);
        assert_eq!(geom.tab_bar.y, 0.0);
        assert_eq!(geom.action(10.0, 10.0), Some(Action::SelectTab(0)));

        let close0_rect = geom
            .buttons
            .iter()
            .find(|(a, _)| *a == Action::CloseTab(0))
            .unwrap()
            .1;
        assert_eq!(
            geom.action(close0_rect.x + 2.0, close0_rect.y + 2.0),
            Some(Action::CloseTab(0))
        );

        let new_tab_rect = geom
            .buttons
            .iter()
            .find(|(a, _)| *a == Action::NewTab)
            .unwrap()
            .1;
        assert_eq!(
            geom.action(new_tab_rect.x + 2.0, new_tab_rect.y + 2.0),
            Some(Action::NewTab)
        );

        assert_eq!(geom.action(15.0, 50.0), Some(Action::Back));
    }

    #[test]
    fn geometry_layout_bottom_position() {
        let settings = Settings {
            position: ToolbarPosition::Bottom,
            ..Settings::default()
        };
        let model = Model {
            width: 800.0,
            height: 600.0,
            title: "Test".into(),
            status: None,
            settings,
            tabs: vec![TabViewModel {
                id: 0,
                title: "Start".into(),
                active: true,
            }],
            can_back: false,
            can_forward: false,
            editing: false,
            loading: false,
            menu: false,
            studio: false,
            demo: false,
            fullscreen: false,
            hovered: None,
            focused: None,
            scroll: 0.0,
            scroll_max: 0.0,
        };
        let geom = Geometry::for_model(&model);
        assert_eq!(geom.toolbar.y, 600.0 - TOOLBAR);
        assert_eq!(geom.tab_bar.y, geom.toolbar.y + NAV_BAR);
        assert_eq!(geom.viewport.y, 0.0);
        assert_eq!(geom.viewport.height, 600.0 - TOOLBAR);
    }

    #[test]
    fn tab_width_clamping() {
        // 1 tab with 800 width -> should clamp to 180.0
        let model = Model {
            width: 800.0,
            height: 600.0,
            title: "Test".into(),
            status: None,
            settings: Settings::default(),
            tabs: vec![TabViewModel {
                id: 0,
                title: "Start".into(),
                active: true,
            }],
            can_back: false,
            can_forward: false,
            editing: false,
            loading: false,
            menu: false,
            studio: false,
            demo: false,
            fullscreen: false,
            hovered: None,
            focused: None,
            scroll: 0.0,
            scroll_max: 0.0,
        };
        let geom = Geometry::for_model(&model);
        let tab_rect = geom
            .buttons
            .iter()
            .find(|(a, _)| *a == Action::SelectTab(0))
            .unwrap()
            .1;
        assert_eq!(tab_rect.width, 180.0);

        // 20 tabs -> should clamp down to 70.0 min
        let tabs = (0..20)
            .map(|id| TabViewModel {
                id,
                title: format!("Tab {id}"),
                active: id == 0,
            })
            .collect();
        let model_many = Model { tabs, ..model };
        let geom_many = Geometry::for_model(&model_many);
        let tab_rect_many = geom_many
            .buttons
            .iter()
            .find(|(a, _)| *a == Action::SelectTab(0))
            .unwrap()
            .1;
        assert_eq!(tab_rect_many.width, 70.0);
    }

    #[test]
    fn build_scene_with_tabs() {
        let factory = TextFactory::new().unwrap();
        let model = Model {
            width: 800.0,
            height: 600.0,
            title: "Test".into(),
            status: None,
            settings: Settings::default(),
            tabs: vec![
                TabViewModel {
                    id: 0,
                    title: "Active Tab".into(),
                    active: true,
                },
                TabViewModel {
                    id: 1,
                    title: "Inactive Tab".into(),
                    active: false,
                },
            ],
            can_back: true,
            can_forward: false,
            editing: false,
            loading: false,
            menu: false,
            studio: false,
            demo: false,
            fullscreen: false,
            hovered: Some(Action::CloseTab(0)),
            focused: Some(Action::NewTab),
            scroll: 0.0,
            scroll_max: 0.0,
        };
        let scene = build(&factory, &model);
        assert!(scene.is_ok());
    }
}
