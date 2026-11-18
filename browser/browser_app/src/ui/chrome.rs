use browser_core::{Accent, Settings, ToolbarPosition};
use display_list::{Color, Command, DisplayList, Rect, TextAlign, TextStyle};
use rendering_ffi::{PreparedScene, TextBatch, TextFactory};

pub const BACKGROUND: Color = Color::rgb(0x101216);
pub const ADDRESS_BACKGROUND: u32 = 0x20262e;
const TOOLBAR: f32 = 56.0;
const INK: Color = Color::rgb(0xe8eef8);
const MUTED: Color = Color::rgb(0xa6b6cc);
const PANEL: Color = Color::rgb(0x121c2a);

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Action {
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
        let y = toolbar_y + 9.0;
        let address = Rect::new(128.0, y, (model.width - 185.0).max(1.0), 38.0);
        let mut buttons = vec![
            (Action::Back, Rect::new(12.0, y, 34.0, 38.0)),
            (Action::Forward, Rect::new(48.0, y, 34.0, 38.0)),
            (Action::Reload, Rect::new(84.0, y, 34.0, 38.0)),
            (Action::Address, address),
            (Action::Menu, Rect::new(model.width - 46.0, y, 34.0, 38.0)),
        ];
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

        for &(action, rect) in bounds.buttons.iter().take(5) {
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
