use std::{
    cell::RefCell,
    collections::VecDeque,
    ffi::OsString,
    mem::size_of,
    os::windows::ffi::OsStringExt,
    path::PathBuf,
    ptr::{null, null_mut},
};

use browser_core::{
    Accent, Example, Location, Settings, TabId, TabManager, ToolbarPosition, Visit, resolve_link,
};
use display_list::Rect;
use network_stack::Url;
use rendering_ffi::MotionFrame;
use web_engine::Viewport;
use windows_sys::Win32::{
    Foundation::{HWND, LPARAM, LRESULT, POINT, RECT, WPARAM},
    Graphics::{
        Dwm::{DWMWA_USE_IMMERSIVE_DARK_MODE, DwmSetWindowAttribute},
        Gdi::{
            BeginPaint, ClientToScreen, DEVMODEW, ENUM_CURRENT_SETTINGS, EndPaint,
            EnumDisplaySettingsW, GetMonitorInfoW, MONITOR_DEFAULTTONEAREST, MONITORINFO,
            MONITORINFOEXW, MonitorFromWindow, PAINTSTRUCT,
        },
    },
    System::LibraryLoader::GetModuleHandleW,
    UI::{
        Controls::Dialogs::{
            GetOpenFileNameW, OFN_DONTADDTORECENT, OFN_EXPLORER, OFN_FILEMUSTEXIST,
            OFN_NOCHANGEDIR, OFN_PATHMUSTEXIST, OPENFILENAMEW,
        },
        HiDpi::{
            DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2, GetDpiForWindow,
            SetProcessDpiAwarenessContext,
        },
        Input::KeyboardAndMouse::{
            GetKeyState, TME_LEAVE, TRACKMOUSEEVENT, TrackMouseEvent, VK_CONTROL, VK_DOWN, VK_END,
            VK_ESCAPE, VK_F2, VK_F5, VK_F6, VK_F11, VK_HOME, VK_LEFT, VK_MENU, VK_NEXT, VK_PRIOR,
            VK_RETURN, VK_RIGHT, VK_SHIFT, VK_SPACE, VK_TAB, VK_UP,
        },
        WindowsAndMessaging::*,
    },
};

use super::address_bar::{self, AddressBar};
use crate::{
    Log, Options,
    chrome::{self, Action, Geometry as ChromeGeometry, Model},
    document::{DocumentWorker, FormAction, Geometry},
    render::{PageUpdate, RenderWorker, View},
};
use web_engine::ControlAction;

pub const DOCUMENT_READY: u32 = WM_APP + 1;
pub const PAGE_PRESENTED: u32 = WM_APP + 2;
pub const RENDER_STOPPED: u32 = WM_APP + 3;
pub const MOTION_COMPLETE: u32 = WM_APP + 4;
const PROBE_STEP_TIMER: usize = 1;
const PROBE_TIMEOUT_TIMER: usize = 2;

thread_local! {
    static APP: RefCell<Option<App>> = const { RefCell::new(None) };
    static EVENTS: RefCell<VecDeque<Event>> = const { RefCell::new(VecDeque::new()) };
}

pub(super) enum Event {
    Size,
    Dpi(RECT),
    Display,
    Paint,
    Move(f32, f32),
    Leave,
    Click(f32, f32),
    Wheel(f32),
    Key {
        key: u16,
        control: bool,
        shift: bool,
        alt: bool,
    },
    AddressCommit(String),
    AddressCancel,
    AddressBlur,
    SettingsChanged,
    Char(char),
    Document,
    Presented(u32),
    MotionComplete(u32),
    ProbeStep,
    ProbeTimeout,
    Close,
    RendererStopped,
}

pub(super) fn send(event: Event) {
    EVENTS.with(|events| events.borrow_mut().push_back(event));
    drain();
}

fn drain() {
    APP.with(|slot| {
        // Changing window styles can synchronously reenter the window procedure.
        // Those events are queued until the current action has returned.
        let Ok(mut slot) = slot.try_borrow_mut() else {
            return;
        };
        let Some(app) = slot.as_mut() else { return };
        loop {
            let event = EVENTS.with(|events| events.borrow_mut().pop_front());
            let Some(event) = event else { break };
            app.handle(event);
        }
    });
}

fn signed_low(value: isize) -> f32 {
    value as i16 as f32
}

fn signed_high(value: isize) -> f32 {
    (value >> 16) as i16 as f32
}

unsafe extern "system" fn procedure(
    window: HWND,
    message: u32,
    wparam: WPARAM,
    lparam: LPARAM,
) -> LRESULT {
    let event = match message {
        WM_CTLCOLOREDIT => {
            return address_bar::paint(wparam as _, lparam as HWND)
                .unwrap_or_else(|| unsafe { DefWindowProcW(window, message, wparam, lparam) });
        }
        WM_PAINT => {
            let mut paint = PAINTSTRUCT::default();
            unsafe {
                BeginPaint(window, &mut paint);
                EndPaint(window, &paint);
            }
            Event::Paint
        }
        WM_ERASEBKGND => return 1,
        WM_GETMINMAXINFO => {
            if lparam != 0 {
                let dpi = unsafe { GetDpiForWindow(window) }.max(96) as i32;
                let info = unsafe { &mut *(lparam as *mut MINMAXINFO) };
                info.ptMinTrackSize.x = 600 * dpi / 96;
                info.ptMinTrackSize.y = 380 * dpi / 96;
            }
            return 0;
        }
        WM_CLOSE => Event::Close,
        WM_DESTROY => {
            unsafe { PostQuitMessage(0) };
            return 0;
        }
        WM_SIZE | WM_MOVE => Event::Size,
        WM_DPICHANGED if lparam != 0 => Event::Dpi(unsafe { *(lparam as *const RECT) }),
        WM_DISPLAYCHANGE => Event::Display,
        WM_SETTINGCHANGE => Event::SettingsChanged,
        WM_MOUSEMOVE => Event::Move(signed_low(lparam), signed_high(lparam)),
        windows_sys::Win32::UI::Controls::WM_MOUSELEAVE => Event::Leave,
        WM_LBUTTONUP => Event::Click(signed_low(lparam), signed_high(lparam)),
        WM_MOUSEWHEEL => Event::Wheel(signed_high(wparam as isize)),
        WM_KEYDOWN | WM_SYSKEYDOWN => {
            let key = wparam as u16;
            let control = unsafe { GetKeyState(i32::from(VK_CONTROL)) < 0 };
            let shift = unsafe { GetKeyState(i32::from(VK_SHIFT)) < 0 };
            let alt = message == WM_SYSKEYDOWN || unsafe { GetKeyState(i32::from(VK_MENU)) < 0 };
            if alt && !matches!(key, VK_LEFT | VK_RIGHT | 0x44) {
                return unsafe { DefWindowProcW(window, message, wparam, lparam) };
            }
            if lparam & (1 << 30) != 0
                && matches!(key, VK_F2 | VK_F11 | VK_ESCAPE | 0x46 | 0x4f | 0x52)
            {
                return 0;
            }
            Event::Key {
                key,
                control,
                shift,
                alt,
            }
        }
        WM_CHAR => {
            let editing = APP.with(|slot| {
                slot.try_borrow()
                    .ok()
                    .and_then(|app| app.as_ref().map(|a| a.editing))
                    .unwrap_or(false)
            });
            if !editing && let Some(ch) = char::from_u32(wparam as u32) {
                send(Event::Char(ch));
            }
            return 0;
        }
        DOCUMENT_READY => Event::Document,
        PAGE_PRESENTED => Event::Presented(wparam as u32),
        RENDER_STOPPED => Event::RendererStopped,
        MOTION_COMPLETE => Event::MotionComplete(wparam as u32),
        WM_TIMER if wparam == PROBE_STEP_TIMER => Event::ProbeStep,
        WM_TIMER if wparam == PROBE_TIMEOUT_TIMER => Event::ProbeTimeout,
        _ => return unsafe { DefWindowProcW(window, message, wparam, lparam) },
    };
    send(event);
    0
}

#[derive(Clone, Copy)]
struct Client {
    width: u32,
    height: u32,
    dpi: u32,
    origin: POINT,
}

impl Client {
    fn get(window: HWND) -> Self {
        let mut area = RECT::default();
        let mut origin = POINT::default();
        unsafe {
            GetClientRect(window, &mut area);
            ClientToScreen(window, &mut origin);
        }
        let minimized = unsafe { IsIconic(window) != 0 };
        Self {
            width: if minimized {
                0
            } else {
                (area.right - area.left).max(0) as u32
            },
            height: if minimized {
                0
            } else {
                (area.bottom - area.top).max(0) as u32
            },
            dpi: unsafe { GetDpiForWindow(window) }.max(96),
            origin,
        }
    }

    fn scale(self) -> f32 {
        self.dpi as f32 / 96.0
    }

    fn width(self) -> f32 {
        self.width as f32 / self.scale()
    }

    fn height(self) -> f32 {
        self.height as f32 / self.scale()
    }

    fn to_screen(self, rect: Rect) -> Rect {
        let scale = self.scale();
        Rect::new(
            self.origin.x as f32 + rect.x * scale,
            self.origin.y as f32 + rect.y * scale,
            rect.width * scale,
            rect.height * scale,
        )
    }
}

#[derive(Clone, Copy)]
struct Placement {
    outer: RECT,
    style: isize,
    preview: Rect,
}

#[derive(Clone, Copy)]
enum Fullscreen {
    Windowed,
    Expanded(Placement),
    Returning(Placement),
}

impl Fullscreen {
    fn active(self) -> bool {
        !matches!(self, Self::Windowed)
    }
}

struct Navigation {
    location: Location,
    visit: Visit,
    scroll: Option<f32>,
    anchor: Option<String>,
    reload: bool,
    action: Option<FormAction>,
}

struct MotionProbe {
    cycles: u32,
    completed: u32,
}

struct App {
    window: HWND,
    address_bar: AddressBar,
    editing: bool,
    menu: bool,
    client: Client,
    profile: PathBuf,
    log: Log,
    document: DocumentWorker,
    renderer: RenderWorker,
    tabs: TabManager,
    settings: Settings,
    geometry: Option<Geometry>,
    pending: Option<Navigation>,
    generation: u32,
    presented: u32,
    committed: u32,
    layout_viewport: Option<Viewport>,
    scroll: f32,
    status: Option<String>,
    studio: bool,
    demo: bool,
    fullscreen: Fullscreen,
    motion_id: u32,
    probe: Option<MotionProbe>,
    system_motion: bool,
    hovered: Option<Action>,
    focused: Option<Action>,
    tracking_mouse: bool,
    closing: bool,
    error: Option<String>,
    focused_input: Option<(u32, String)>,
    input_values: std::collections::HashMap<u32, String>,
    media_playing: std::collections::HashSet<u32>,
}

impl App {
    fn viewport(&self) -> Rect {
        chrome::viewport(
            self.client.width(),
            self.client.height(),
            self.settings.position,
        )
    }

    fn max_scroll(&self) -> f32 {
        self.geometry
            .as_ref()
            .map_or(0.0, |page| (page.height - self.viewport().height).max(0.0))
    }

    fn model(&self) -> Model {
        let location = self.tabs.active_tab().location();
        let loading = self.pending.as_ref().is_some_and(|nav| {
            nav.visit != Visit::Current || nav.reload || self.geometry.is_none()
        });
        let title = if self.demo {
            "Motion Engine".into()
        } else {
            match location {
                Location::Web(url) => url.as_str().to_owned(),
                Location::Example(Example::Start) => self
                    .pending
                    .as_ref()
                    .and_then(|nav| {
                        if let Location::Web(url) = &nav.location {
                            Some(url.as_str().to_owned())
                        } else {
                            None
                        }
                    })
                    .unwrap_or_default(),
                _ => format!("Local  /  {}", location.label()),
            }
        };
        let tabs = self
            .tabs
            .tabs()
            .iter()
            .map(|tab| chrome::TabViewModel {
                id: tab.id.0,
                title: tab.title.clone(),
                active: tab.id == self.tabs.active_id(),
            })
            .collect();
        Model {
            width: self.client.width(),
            height: self.client.height(),
            title,
            status: self.status.clone(),
            settings: self.settings,
            tabs,
            can_back: self.tabs.active_tab().history.target(Visit::Back).is_some(),
            can_forward: self
                .tabs
                .active_tab()
                .history
                .target(Visit::Forward)
                .is_some(),
            loading,
            menu: self.menu,
            editing: self.editing,
            studio: self.studio,
            demo: self.demo,
            fullscreen: self.fullscreen.active(),
            hovered: self.hovered,
            focused: self.focused,
            scroll: self.scroll,
            scroll_max: self.max_scroll(),
        }
    }

    fn motion_ms(&self) -> u32 {
        if self.system_motion {
            self.settings.motion_ms
        } else {
            0
        }
    }

    fn update(&self, page: Option<PageUpdate>) {
        if self.closing {
            return;
        }
        let client = self.client;
        let motion = self.demo.then(|| {
            let preview = client.to_screen(chrome::preview(self.viewport()));
            let full = client.to_screen(Rect::new(0.0, 0.0, client.width(), client.height()));
            let (initial_screen, target_screen) = match self.fullscreen {
                Fullscreen::Windowed => (preview, preview),
                Fullscreen::Expanded(placement) => (placement.preview, full),
                Fullscreen::Returning(placement) => (full, placement.preview),
            };
            MotionFrame {
                initial_screen,
                target_screen,
                source_frame: 0,
                screen_x: client.origin.x as f32,
                screen_y: client.origin.y as f32,
                scale: client.scale(),
                transition: self.motion_id,
                duration_ms: self.motion_ms(),
            }
        });
        self.renderer.update(
            View {
                width: client.width,
                height: client.height,
                dpi: client.dpi,
                chrome: self.model(),
                motion,
                motion_ms: self.motion_ms(),
            },
            page,
        );
    }

    fn request(&mut self, navigation: Navigation) {
        self.pending = Some(navigation);
        self.request_layout();
    }

    fn request_layout(&mut self) {
        let content = self.viewport();
        if content.width < 1.0 || self.fullscreen.active() {
            return;
        }
        let navigation = self.pending.get_or_insert_with(|| Navigation {
            location: self.tabs.active_tab().location().clone(),
            visit: Visit::Current,
            scroll: None,
            anchor: None,
            reload: false,
            action: None,
        });
        self.generation = self.generation.wrapping_add(1);
        let viewport = Viewport {
            width: content.width,
            height: content.height,
        };
        self.layout_viewport = Some(viewport);
        self.document.request(
            self.generation,
            navigation.location.clone(),
            viewport,
            navigation.reload,
            navigation.action.clone(),
        );
    }

    fn size(&mut self) {
        self.client = Client::get(self.window);
        if !self.fullscreen.active() {
            self.scroll = self.scroll.min(self.max_scroll());
            let content = self.viewport();
            if self.client.width > 0
                && self.layout_viewport.is_none_or(|viewport| {
                    (content.width - viewport.width).abs() >= 0.5
                        || (content.height - viewport.height).abs() >= 0.5
                })
            {
                self.request_layout();
            }
            if self.editing {
                self.position_address();
            }
        }
        self.update(None);
    }

    fn document_ready(&mut self) {
        while let Ok(completion) = self.document.complete.try_recv() {
            if self.closing || completion.generation != self.generation {
                continue;
            }
            let Some(pending) = self.pending.take() else {
                continue;
            };
            match completion.result {
                Ok(page) => {
                    let was_submit = matches!(pending.action, Some(FormAction::Submit(_)));
                    if was_submit && let Some(target_url) = page.resolved {
                        self.navigate_web(target_url);
                        return;
                    }
                    let transition = self.geometry.is_some()
                        && !self.demo
                        && self.motion_ms() > 0
                        && pending.location != *self.tabs.active_tab().location()
                        && matches!(
                            (&pending.location, self.tabs.active_tab().location()),
                            (Location::Example(_), Location::Example(_))
                        );
                    let scroll = pending
                        .anchor
                        .as_ref()
                        .and_then(|id| {
                            if id.is_empty() {
                                Some(0.0)
                            } else {
                                page.geometry.anchors.get(id).copied()
                            }
                        })
                        .or(pending.scroll)
                        .unwrap_or(self.scroll);
                    self.geometry = Some(page.geometry);
                    self.scroll = scroll.clamp(0.0, self.max_scroll());
                    let location = page.resolved.map(Location::Web).unwrap_or(pending.location);
                    self.tabs
                        .active_tab_mut()
                        .history
                        .commit(pending.visit, location, self.scroll);
                    self.committed = completion.generation;
                    let page_title = self
                        .geometry
                        .as_ref()
                        .map(|page| page.title.trim())
                        .filter(|title| !title.is_empty())
                        .map(str::to_owned)
                        .unwrap_or_else(|| self.tabs.active_tab().location().label());
                    let page_title: String = page_title.chars().filter(|character| {
                        !character.is_control() && !matches!(character, '\u{202a}'..='\u{202e}' | '\u{2066}'..='\u{2069}')
                    }).take(160).collect();
                    self.tabs.set_active_title(page_title.clone());
                    let title = wide(&format!("{page_title} — Browser"));
                    unsafe { SetWindowTextW(self.window, title.as_ptr()) };
                    self.update(Some(PageUpdate {
                        generation: completion.generation,
                        scene: page.scene,
                        transition,
                    }));
                }
                Err(error) => {
                    self.status = Some(error);
                    if self.geometry.is_none()
                        && pending.location != Location::Example(Example::Start)
                    {
                        self.request_layout();
                    }
                    self.update(None);
                }
            }
        }
    }

    fn new_tab(&mut self) {
        self.tabs.active_tab_mut().scroll = self.scroll;
        self.tabs.new_tab(Location::Example(Example::Start));
        self.geometry = None;
        self.scroll = 0.0;
        self.open(Location::Example(Example::Start));
    }

    fn close_tab(&mut self, id: usize) {
        let was_active = self.tabs.active_id() == TabId(id);
        if was_active {
            self.tabs.active_tab_mut().scroll = self.scroll;
        }
        self.tabs.close_tab(TabId(id));
        self.geometry = None;
        self.scroll = self.tabs.active_tab().scroll;
        self.open(self.tabs.active_tab().location().clone());
    }

    fn close_active_tab(&mut self) {
        self.close_tab(self.tabs.active_id().0);
    }

    fn select_tab(&mut self, id: usize) {
        if id == self.tabs.active_id().0 {
            return;
        }
        self.tabs.active_tab_mut().scroll = self.scroll;
        if self.tabs.select_tab(TabId(id)) {
            self.scroll = self.tabs.active_tab().scroll;
            self.geometry = None;
            self.open(self.tabs.active_tab().location().clone());
        }
    }

    fn select_tab_index(&mut self, index: usize) {
        if index == self.tabs.active_index() {
            return;
        }
        self.tabs.active_tab_mut().scroll = self.scroll;
        if self.tabs.select_index(index) {
            self.scroll = self.tabs.active_tab().scroll;
            self.geometry = None;
            self.open(self.tabs.active_tab().location().clone());
        }
    }

    fn select_next_tab(&mut self) {
        if self.tabs.tab_count() <= 1 {
            return;
        }
        self.tabs.active_tab_mut().scroll = self.scroll;
        self.tabs.select_next();
        self.scroll = self.tabs.active_tab().scroll;
        self.geometry = None;
        self.open(self.tabs.active_tab().location().clone());
    }

    fn select_prev_tab(&mut self) {
        if self.tabs.tab_count() <= 1 {
            return;
        }
        self.tabs.active_tab_mut().scroll = self.scroll;
        self.tabs.select_prev();
        self.scroll = self.tabs.active_tab().scroll;
        self.geometry = None;
        self.open(self.tabs.active_tab().location().clone());
    }

    fn open(&mut self, location: Location) {
        let same = &location == self.tabs.active_tab().location();
        let scroll = if same { Some(self.scroll) } else { Some(0.0) };
        let visit = if same { Visit::Current } else { Visit::New };
        self.demo = false;
        self.studio = false;
        self.menu = false;
        self.status = None;
        self.focused_input = None;
        self.request(Navigation {
            location,
            visit,
            scroll,
            anchor: None,
            reload: false,
            action: None,
        });
    }

    fn travel(&mut self, visit: Visit) {
        if let Some((location, scroll)) = self.tabs.active_tab().history.target(visit) {
            self.demo = false;
            self.studio = false;
            self.menu = false;
            self.status = None;
            self.focused_input = None;
            self.request(Navigation {
                location,
                visit,
                scroll: Some(scroll),
                anchor: None,
                reload: false,
                action: None,
            });
        }
    }

    fn navigate_web(&mut self, mut url: Url) {
        let anchor = url.fragment().map(str::to_owned);
        url.set_fragment(None);
        let location = Location::Web(url);
        let same = &location == self.tabs.active_tab().location();
        self.demo = false;
        self.studio = false;
        self.menu = false;
        self.status = None;
        self.focused_input = None;
        self.request(Navigation {
            location,
            visit: if same { Visit::Current } else { Visit::New },
            scroll: Some(0.0),
            anchor,
            reload: same,
            action: None,
        });
    }

    fn reload(&mut self) {
        self.menu = false;
        self.status = None;
        self.focused_input = None;
        self.request(Navigation {
            location: self.tabs.active_tab().location().clone(),
            visit: Visit::Current,
            scroll: None,
            anchor: None,
            reload: true,
            action: None,
        });
    }

    fn position_address(&mut self) {
        let bounds = ChromeGeometry::for_model(&self.model()).address;
        self.address_bar.position(bounds, self.client.scale());
    }

    fn edit_address(&mut self) {
        if self.editing {
            return;
        }
        self.menu = false;
        self.studio = false;
        self.status = None;
        self.editing = true;
        self.position_address();
        let value = match self.tabs.active_tab().location() {
            Location::Web(url) => url.as_str(),
            _ => "",
        };
        self.address_bar.show(value);
    }

    fn end_address(&mut self) {
        if self.editing {
            self.editing = false;
            self.address_bar.hide();
        }
    }

    fn set_scroll(&mut self, scroll: f32) {
        if !self.demo && !self.studio {
            self.scroll = scroll.clamp(0.0, self.max_scroll());
            self.tabs.active_tab_mut().history.set_scroll(self.scroll);
        }
    }

    fn send_form_action(&mut self, action: FormAction) {
        self.status = None;
        self.request(Navigation {
            location: self.tabs.active_tab().location().clone(),
            visit: Visit::Current,
            scroll: Some(self.scroll),
            anchor: None,
            reload: false,
            action: Some(action),
        });
    }

    fn activate_control(&mut self, node: u32) {
        self.send_form_action(FormAction::Click(node));
    }

    fn follow(&mut self, href: &str) {
        let Some((location, anchor)) = resolve_link(self.tabs.active_tab().location(), href) else {
            self.status = Some("This link cannot be opened in the current document.".into());
            return;
        };
        self.status = None;
        if &location == self.tabs.active_tab().location() {
            if let Some(anchor) = anchor {
                let y = if anchor.is_empty() {
                    Some(0.0)
                } else {
                    self.geometry
                        .as_ref()
                        .and_then(|page| page.anchors.get(&anchor).copied())
                };
                if let Some(y) = y {
                    self.set_scroll(y);
                }
            }
        } else {
            self.focused_input = None;
            self.request(Navigation {
                location,
                visit: Visit::New,
                scroll: Some(0.0),
                anchor,
                reload: false,
                action: None,
            });
        }
    }

    fn activate(&mut self, action: Action) {
        if action != Action::Address {
            self.end_address();
        }
        match action {
            Action::Back => self.travel(Visit::Back),
            Action::Forward => self.travel(Visit::Forward),
            Action::Reload => self.reload(),
            Action::Address => self.edit_address(),
            Action::SelectTab(id) => self.select_tab(id),
            Action::CloseTab(id) => self.close_tab(id),
            Action::NewTab => self.new_tab(),
            Action::Menu => {
                self.menu = !self.menu;
                self.studio = false;
                self.focused = None;
            }
            Action::Home => self.open(Location::Example(Example::Start)),
            Action::Open => {
                self.menu = false;
                if let Some(path) = open_dialog(self.window) {
                    self.open(Location::File(path));
                }
            }
            Action::Studio => {
                self.menu = false;
                self.studio = !self.studio;
                self.focused = None;
            }
            Action::Motion => {
                self.menu = false;
                self.demo = !self.demo;
                self.studio = false;
                self.status = None;
                self.motion_id = self.motion_id.wrapping_add(1);
            }
            Action::Position | Action::Accent | Action::Duration => {
                match action {
                    Action::Position => {
                        self.settings.position = match self.settings.position {
                            ToolbarPosition::Top => ToolbarPosition::Bottom,
                            ToolbarPosition::Bottom => ToolbarPosition::Top,
                        };
                    }
                    Action::Accent => {
                        self.settings.accent = match self.settings.accent {
                            Accent::Mint => Accent::Iris,
                            Accent::Iris => Accent::Amber,
                            Accent::Amber => Accent::Mint,
                        };
                    }
                    Action::Duration => {
                        self.settings.motion_ms = match self.settings.motion_ms {
                            0 => 180,
                            180 => 220,
                            220 => 250,
                            _ => 0,
                        };
                    }
                    _ => unreachable!(),
                }
                self.status = self
                    .settings
                    .save(&self.profile)
                    .err()
                    .map(|error| format!("Could not save Browser Studio settings. {error}"));
            }
            Action::Fullscreen => self.toggle_fullscreen(),
        }
    }

    fn toggle_fullscreen(&mut self) {
        if !self.demo {
            return;
        }
        self.studio = false;
        self.menu = false;
        self.end_address();
        self.focused = None;
        self.motion_id = self.motion_id.wrapping_add(1);
        self.fullscreen = match self.fullscreen {
            Fullscreen::Windowed => {
                let mut outer = RECT::default();
                if unsafe { GetWindowRect(self.window, &mut outer) } == 0 {
                    return;
                }
                let Some(monitor) = monitor_rect(self.window) else {
                    return;
                };
                let style = unsafe { GetWindowLongPtrW(self.window, GWL_STYLE) };
                let preview = self.client.to_screen(chrome::preview(self.viewport()));
                unsafe {
                    SetWindowLongPtrW(
                        self.window,
                        GWL_STYLE,
                        style & !(WS_OVERLAPPEDWINDOW as isize),
                    );
                }
                set_bounds(self.window, monitor, SWP_FRAMECHANGED);
                Fullscreen::Expanded(Placement {
                    outer,
                    style,
                    preview,
                })
            }
            Fullscreen::Expanded(placement) => Fullscreen::Returning(placement),
            Fullscreen::Returning(placement) => Fullscreen::Expanded(placement),
        };
        self.client = Client::get(self.window);
    }

    fn restore_window(&mut self, placement: Placement) {
        self.fullscreen = Fullscreen::Windowed;
        self.motion_id = self.motion_id.wrapping_add(1);
        unsafe {
            SetWindowLongPtrW(self.window, GWL_STYLE, placement.style);
        }
        set_bounds(self.window, placement.outer, SWP_FRAMECHANGED);
        self.size();
    }

    fn motion_complete(&mut self, id: u32) {
        if id != self.motion_id {
            return;
        }
        if let Fullscreen::Returning(placement) = self.fullscreen {
            self.restore_window(placement);
            if let Some(probe) = &mut self.probe {
                probe.completed += 1;
                self.log.write(format_args!(
                    "probe_cycle={} returned motion={id}",
                    probe.completed
                ));
            }
            return;
        }
        if let Some(probe) = &self.probe {
            if probe.completed >= probe.cycles {
                self.log
                    .write(format_args!("probe_done cycles={}", probe.completed));
                self.handle(Event::Close);
            } else {
                unsafe {
                    SetTimer(self.window, PROBE_STEP_TIMER, 65, None);
                }
            }
        }
    }

    fn cycle_focus(&mut self, reverse: bool) {
        let actions = if self.studio {
            vec![
                Action::Position,
                Action::Accent,
                Action::Duration,
                Action::Menu,
            ]
        } else {
            ChromeGeometry::for_model(&self.model())
                .buttons
                .into_iter()
                .filter_map(|(action, _)| {
                    (action != Action::Back
                        || self.tabs.active_tab().history.target(Visit::Back).is_some())
                    .then_some(action)
                    .filter(|action| {
                        *action != Action::Forward
                            || self
                                .tabs
                                .active_tab()
                                .history
                                .target(Visit::Forward)
                                .is_some()
                    })
                })
                .collect()
        };
        let next = match self
            .focused
            .and_then(|action| actions.iter().position(|candidate| *candidate == action))
        {
            Some(index) if reverse => (index + actions.len() - 1) % actions.len(),
            Some(index) => (index + 1) % actions.len(),
            None if reverse => actions.len() - 1,
            None => 0,
        };
        self.focused = Some(actions[next]);
    }

    fn key(&mut self, key: u16, control: bool, shift: bool, alt: bool) {
        if self.fullscreen.active() {
            if matches!(key, 0x46 | VK_F11)
                || (key == VK_ESCAPE && matches!(self.fullscreen, Fullscreen::Expanded(_)))
            {
                self.toggle_fullscreen();
            }
            return;
        }
        match (key, control, alt) {
            (0x54, true, false) => self.new_tab(),
            (0x57, true, false) => self.close_active_tab(),
            (VK_TAB, true, false) => {
                if shift {
                    self.select_prev_tab();
                } else {
                    self.select_next_tab();
                }
            }
            (0x31..=0x39, true, false) => self.select_tab_index((key - 0x31) as usize),
            (0x4f, true, false) => self.activate(Action::Open),
            (0x4c, true, false) | (0x44, _, true) | (VK_F6, _, false) => self.edit_address(),
            (0x52, true, false) | (VK_F5, _, false) => self.reload(),
            (VK_LEFT, _, true) => self.travel(Visit::Back),
            (VK_RIGHT, _, true) => self.travel(Visit::Forward),
            (VK_F2, _, false) => self.activate(Action::Studio),
            (0x46 | VK_F11, false, false) if self.demo => self.toggle_fullscreen(),
            (VK_ESCAPE, _, false) => {
                if self.focused_input.is_some() {
                    self.focused_input = None;
                    self.request_layout();
                } else if self.studio {
                    self.studio = false;
                } else if self.menu {
                    self.menu = false;
                } else if self.demo {
                    self.demo = false;
                }
                self.status = None;
                self.focused = None;
            }
            (VK_TAB, false, false) => self.cycle_focus(shift),
            (VK_RETURN | VK_SPACE, false, false) if self.focused.is_some() => {
                self.activate(self.focused.unwrap())
            }
            (VK_UP, _, false) => self.set_scroll(self.scroll - 42.0),
            (VK_DOWN, _, false) => self.set_scroll(self.scroll + 42.0),
            (VK_PRIOR, _, false) => self.set_scroll(self.scroll - self.viewport().height * 0.85),
            (VK_NEXT, _, false) => self.set_scroll(self.scroll + self.viewport().height * 0.85),
            (VK_SPACE, _, false) if self.focused_input.is_none() => {
                self.set_scroll(self.scroll + self.viewport().height * 0.85)
            }
            (VK_HOME, _, false) => self.set_scroll(0.0),
            (VK_END, _, false) => self.set_scroll(self.max_scroll()),
            _ => {}
        }
    }

    fn mouse_action(&self, x: f32, y: f32) -> Option<Action> {
        ChromeGeometry::for_model(&self.model()).action(x, y)
    }

    fn link_at(&self, x: f32, y: f32) -> Option<&str> {
        let view = self.viewport();
        if self.demo
            || self.studio
            || self.menu
            || !view.contains(x, y)
            || self.presented != self.committed
        {
            return None;
        }
        self.geometry
            .as_ref()?
            .links
            .iter()
            .rev()
            .find(|link| link.rect.contains(x - view.x, y - view.y + self.scroll))
            .map(|link| link.href.as_str())
    }

    fn handle(&mut self, event: Event) {
        if self.closing && !matches!(event, Event::RendererStopped) {
            return;
        }
        match event {
            Event::Size => {
                self.size();
                return;
            }
            Event::Paint => {}
            Event::Dpi(suggested) => {
                if !self.fullscreen.active() {
                    set_bounds(self.window, suggested, 0);
                }
                self.size();
                return;
            }
            Event::Display => {
                match self.fullscreen {
                    Fullscreen::Expanded(_) => {
                        if let Some(rect) = monitor_rect(self.window) {
                            set_bounds(self.window, rect, 0);
                        }
                    }
                    Fullscreen::Returning(placement) => self.restore_window(placement),
                    Fullscreen::Windowed => {}
                }
                self.size();
                return;
            }
            Event::SettingsChanged => self.system_motion = system_motion(),
            Event::Move(px, py) => {
                if !self.tracking_mouse {
                    let mut event = TRACKMOUSEEVENT {
                        cbSize: size_of::<TRACKMOUSEEVENT>() as u32,
                        dwFlags: TME_LEAVE,
                        hwndTrack: self.window,
                        dwHoverTime: 0,
                    };
                    self.tracking_mouse = unsafe { TrackMouseEvent(&mut event) } != 0;
                }
                let (x, y) = (px / self.client.scale(), py / self.client.scale());
                let hovered = if self.fullscreen.active() {
                    None
                } else {
                    self.mouse_action(x, y)
                };
                let cursor = if hovered.is_some() || self.link_at(x, y).is_some() {
                    IDC_HAND
                } else {
                    IDC_ARROW
                };
                unsafe {
                    SetCursor(LoadCursorW(null_mut(), cursor));
                }
                if self.hovered == hovered {
                    return;
                }
                self.hovered = hovered;
            }
            Event::Leave => {
                self.tracking_mouse = false;
                self.hovered = None;
            }
            Event::Click(px, py) => {
                let (x, y) = (px / self.client.scale(), py / self.client.scale());
                self.focused = None;
                if self.fullscreen.active() {
                    self.toggle_fullscreen();
                } else if let Some(action) = self.mouse_action(x, y) {
                    self.activate(action);
                } else if self.menu {
                    self.menu = false;
                } else if self.studio {
                    if !ChromeGeometry::for_model(&self.model())
                        .studio
                        .is_some_and(|panel| panel.contains(x, y))
                    {
                        self.studio = false;
                    }
                } else if let Some(href) = self.link_at(x, y).map(str::to_owned) {
                    self.end_address();
                    match web_engine::parse_control_action(&href) {
                        Some(ControlAction::Input(id)) => {
                            let current_value =
                                self.input_values.get(&id).cloned().unwrap_or_default();
                            self.focused_input = Some((id, current_value.clone()));
                            self.send_form_action(FormAction::Input {
                                node: id,
                                value: current_value,
                            });
                        }
                        Some(ControlAction::Toggle(id)) => {
                            self.send_form_action(FormAction::Toggle(id));
                        }
                        Some(ControlAction::Radio(id)) => {
                            self.send_form_action(FormAction::Radio(id));
                        }
                        Some(ControlAction::Submit(id)) => {
                            self.send_form_action(FormAction::Submit(id));
                        }
                        Some(ControlAction::Click(id)) => {
                            self.activate_control(id);
                        }
                        Some(ControlAction::MediaToggle(id)) => {
                            if self.media_playing.contains(&id) {
                                self.media_playing.remove(&id);
                            } else {
                                self.media_playing.insert(id);
                            }
                            self.request_layout();
                            self.update(None);
                        }
                        None => {
                            if self.focused_input.is_some() {
                                self.focused_input = None;
                                self.request_layout();
                            }
                            self.follow(&href);
                        }
                    }
                } else {
                    self.end_address();
                    if self.focused_input.is_some() {
                        self.focused_input = None;
                        self.request_layout();
                    }
                }
            }
            Event::Wheel(delta) => self.set_scroll(self.scroll - delta * 0.75),
            Event::Key {
                key,
                control,
                shift,
                alt,
            } => self.key(key, control, shift, alt),
            Event::AddressCommit(value) => {
                self.end_address();
                match network_stack::address(&value) {
                    Ok(url) => self.navigate_web(url),
                    Err(error) if !value.trim().is_empty() => self.status = Some(error),
                    Err(_) => {}
                }
            }
            Event::AddressCancel | Event::AddressBlur => self.end_address(),
            Event::Char(ch) => {
                if let Some((node, ref mut text)) = self.focused_input {
                    if ch == '\r' {
                        self.send_form_action(FormAction::Submit(node));
                    } else if ch == '\x08' {
                        text.pop();
                        let value = text.clone();
                        self.input_values.insert(node, value.clone());
                        self.send_form_action(FormAction::Input { node, value });
                    } else if ch == '\x1b' {
                        self.focused_input = None;
                        self.request_layout();
                    } else if !ch.is_control() {
                        text.push(ch);
                        let value = text.clone();
                        self.input_values.insert(node, value.clone());
                        self.send_form_action(FormAction::Input { node, value });
                    }
                }
            }
            Event::Document => {
                self.document_ready();
                return;
            }
            Event::Presented(generation) => {
                self.presented = generation;
                return;
            }
            Event::MotionComplete(id) => {
                self.motion_complete(id);
                return;
            }
            Event::ProbeStep => {
                unsafe {
                    KillTimer(self.window, PROBE_STEP_TIMER);
                }
                if self.probe.is_some() {
                    self.toggle_fullscreen();
                }
            }
            Event::ProbeTimeout => {
                let message = "Motion Engine probe timed out waiting for the compositor.";
                self.log.write(format_args!("{message}"));
                self.error = Some(message.into());
                self.handle(Event::Close);
                return;
            }
            Event::Close => {
                self.closing = true;
                self.document.stop();
                self.renderer.stop();
                return;
            }
            Event::RendererStopped => {
                self.error = self.renderer.error().or(self.error.take());
                self.closing = true;
                self.document.stop();
                self.log.write(format_args!("shutdown"));
                unsafe {
                    DestroyWindow(self.window);
                }
                return;
            }
        }
        self.update(None);
    }
}

fn wide(text: &str) -> Vec<u16> {
    text.encode_utf16().chain([0]).collect()
}

fn set_bounds(window: HWND, bounds: RECT, flags: u32) {
    unsafe {
        SetWindowPos(
            window,
            null_mut(),
            bounds.left,
            bounds.top,
            bounds.right - bounds.left,
            bounds.bottom - bounds.top,
            SWP_NOZORDER | SWP_NOACTIVATE | flags,
        );
    }
}

fn monitor_rect(window: HWND) -> Option<RECT> {
    let mut info = MONITORINFO {
        cbSize: size_of::<MONITORINFO>() as u32,
        ..Default::default()
    };
    let monitor = unsafe { MonitorFromWindow(window, MONITOR_DEFAULTTONEAREST) };
    (unsafe { GetMonitorInfoW(monitor, &mut info) } != 0).then_some(info.rcMonitor)
}

fn display_hz(window: HWND) -> u32 {
    let mut monitor = MONITORINFOEXW::default();
    monitor.monitorInfo.cbSize = size_of::<MONITORINFOEXW>() as u32;
    let handle = unsafe { MonitorFromWindow(window, MONITOR_DEFAULTTONEAREST) };
    if unsafe { GetMonitorInfoW(handle, &mut monitor.monitorInfo) } == 0 {
        return 0;
    }
    let mut mode = DEVMODEW {
        dmSize: size_of::<DEVMODEW>() as u16,
        ..Default::default()
    };
    if unsafe { EnumDisplaySettingsW(monitor.szDevice.as_ptr(), ENUM_CURRENT_SETTINGS, &mut mode) }
        == 0
    {
        return 0;
    }
    mode.dmDisplayFrequency
}

fn system_motion() -> bool {
    let mut enabled = 1i32;
    unsafe {
        SystemParametersInfoW(
            SPI_GETCLIENTAREAANIMATION,
            0,
            std::ptr::from_mut(&mut enabled).cast(),
            0,
        );
    }
    enabled != 0
}

fn open_dialog(window: HWND) -> Option<PathBuf> {
    let mut path = vec![0u16; 32768];
    let filter = wide("HTML files\0*.html;*.htm\0\0");
    let title = wide("Open HTML");
    let mut dialog = OPENFILENAMEW {
        lStructSize: size_of::<OPENFILENAMEW>() as u32,
        hwndOwner: window,
        lpstrFilter: filter.as_ptr(),
        nFilterIndex: 1,
        lpstrFile: path.as_mut_ptr(),
        nMaxFile: path.len() as u32,
        lpstrTitle: title.as_ptr(),
        Flags: OFN_EXPLORER
            | OFN_FILEMUSTEXIST
            | OFN_PATHMUSTEXIST
            | OFN_NOCHANGEDIR
            | OFN_DONTADDTORECENT,
        ..Default::default()
    };
    if unsafe { GetOpenFileNameW(&mut dialog) } == 0 {
        return None;
    }
    let end = path.iter().position(|character| *character == 0)?;
    Some(PathBuf::from(OsString::from_wide(&path[..end])))
}

pub fn show_error(error: &str) {
    let message = wide(error);
    let title = wide("Browser");
    unsafe {
        MessageBoxW(
            null_mut(),
            message.as_ptr(),
            title.as_ptr(),
            MB_ICONERROR | MB_OK,
        );
    }
}

pub fn run(options: Options, log: Log) -> Result<(), String> {
    let probing = options.motion_probe.is_some();
    let class_name = wide("BrowserWindow");
    let title = wide("Browser");
    let instance = unsafe { GetModuleHandleW(null()) };
    unsafe {
        SetProcessDpiAwarenessContext(DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2);
    }
    let class = WNDCLASSEXW {
        cbSize: size_of::<WNDCLASSEXW>() as u32,
        lpfnWndProc: Some(procedure),
        hInstance: instance,
        hCursor: unsafe { LoadCursorW(null_mut(), IDC_ARROW) },
        hIcon: unsafe { LoadIconW(null_mut(), IDI_APPLICATION) },
        lpszClassName: class_name.as_ptr(),
        ..Default::default()
    };
    if unsafe { RegisterClassExW(&class) } == 0 {
        return Err(format!(
            "Could not register the browser window. {}",
            std::io::Error::last_os_error()
        ));
    }
    let window = unsafe {
        CreateWindowExW(
            0,
            class_name.as_ptr(),
            title.as_ptr(),
            WS_OVERLAPPEDWINDOW | WS_CLIPCHILDREN,
            CW_USEDEFAULT,
            CW_USEDEFAULT,
            1160,
            806,
            null_mut(),
            null_mut(),
            instance,
            null(),
        )
    };
    if window.is_null() {
        return Err(format!(
            "Could not open the browser window. {}",
            std::io::Error::last_os_error()
        ));
    }
    let dark = 1i32;
    unsafe {
        DwmSetWindowAttribute(
            window,
            DWMWA_USE_IMMERSIVE_DARK_MODE as u32,
            std::ptr::from_ref(&dark).cast(),
            size_of::<i32>() as u32,
        );
    }
    log.write(format_args!("window={:#x}", window as usize));
    log.write(format_args!("display_hz={}", display_hz(window)));
    let settings = Settings::load(&options.profile);
    let address_bar = AddressBar::new(window, instance)?;
    let document =
        DocumentWorker::start(window as usize, log.clone()).map_err(|error| error.to_string())?;
    let renderer = RenderWorker::start(window as usize, options.warp, log.clone())
        .map_err(|error| error.to_string())?;
    let initial_location = if let Some(path) = &options.file {
        Location::File(path.clone())
    } else if let Some(url) = &options.url {
        Location::Web(url.clone())
    } else {
        Location::Example(Example::Start)
    };
    let mut app = App {
        window,
        address_bar,
        editing: false,
        menu: false,
        client: Client::get(window),
        profile: options.profile,
        log,
        document,
        renderer,
        tabs: TabManager::new(initial_location),
        settings,
        geometry: None,
        pending: None,
        generation: 0,
        presented: 0,
        committed: 0,
        layout_viewport: None,
        scroll: 0.0,
        status: None,
        studio: false,
        demo: options.motion,
        fullscreen: Fullscreen::Windowed,
        motion_id: 1,
        probe: options.motion_probe.map(|cycles| MotionProbe {
            cycles,
            completed: 0,
        }),
        system_motion: system_motion(),
        hovered: None,
        focused: None,
        tracking_mouse: false,
        closing: false,
        error: None,
        focused_input: None,
        input_values: std::collections::HashMap::new(),
        media_playing: std::collections::HashSet::new(),
    };
    if let Some(path) = options.file {
        app.open(Location::File(path));
    } else if let Some(url) = options.url {
        app.navigate_web(url);
    } else {
        app.request_layout();
    }
    if let Some(probe) = &app.probe {
        app.log.write(format_args!(
            "probe_start cycles={} duration_ms={}",
            probe.cycles,
            app.motion_ms()
        ));
        unsafe {
            SetTimer(
                window,
                PROBE_TIMEOUT_TIMER,
                probe.cycles * 1300 + 10_000,
                None,
            );
        }
    }
    APP.with(|slot| slot.replace(Some(app)));
    unsafe {
        ShowWindow(window, if probing { SW_SHOWNOACTIVATE } else { SW_SHOW });
        if probing {
            SetWindowPos(
                window,
                HWND_BOTTOM,
                0,
                0,
                0,
                0,
                SWP_NOACTIVATE | SWP_NOMOVE | SWP_NOSIZE,
            );
        }
    }
    send(Event::Size);

    let mut message = MSG::default();
    let mut result = Ok(());
    loop {
        let status = unsafe { GetMessageW(&mut message, null_mut(), 0, 0) };
        if status <= 0 {
            if status < 0 {
                result = Err(format!(
                    "Windows message loop failed. {}",
                    std::io::Error::last_os_error()
                ));
            }
            break;
        }
        unsafe {
            TranslateMessage(&message);
            DispatchMessageW(&message);
        }
    }
    let app = APP.with(|slot| slot.take()).unwrap();
    if let Some(error) = &app.error {
        return Err(error.clone());
    }
    result
}
