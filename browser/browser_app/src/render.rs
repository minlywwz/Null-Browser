use std::{
    sync::{Arc, Condvar, Mutex},
    thread::{self, JoinHandle},
    time::{Duration, Instant},
};

use display_list::{Color, Rect};
use rendering_ffi::{Layer, MotionFrame, PreparedScene, Renderer, TextFactory};
use windows_sys::Win32::{Foundation::HWND, UI::WindowsAndMessaging::PostMessageW};

use crate::{
    Log,
    chrome::{self, Model},
    window::{MOTION_COMPLETE, PAGE_PRESENTED, RENDER_STOPPED},
};

#[derive(Clone)]
pub struct View {
    pub width: u32,
    pub height: u32,
    pub dpi: u32,
    pub chrome: Model,
    pub motion: Option<MotionFrame>,
    pub motion_ms: u32,
}

pub struct PageUpdate {
    pub generation: u32,
    pub scene: PreparedScene,
    pub transition: bool,
}

#[derive(Default)]
struct Mailbox {
    view: Option<View>,
    page: Option<PageUpdate>,
    stop: bool,
}

pub struct RenderWorker {
    mailbox: Arc<(Mutex<Mailbox>, Condvar)>,
    error: Arc<Mutex<Option<String>>>,
    thread: Option<JoinHandle<()>>,
}

impl RenderWorker {
    pub fn start(window: usize, warp: bool, log: Log) -> std::io::Result<Self> {
        let mailbox = Arc::new((Mutex::new(Mailbox::default()), Condvar::new()));
        let worker_mailbox = Arc::clone(&mailbox);
        let error = Arc::new(Mutex::new(None));
        let worker_error = Arc::clone(&error);
        let thread = thread::Builder::new()
            .name("GPU Compositor".into())
            .spawn(move || {
                let result = render(window, warp, &worker_mailbox, &log);
                if let Err(error) = result {
                    log.write(format_args!("{error}"));
                    *worker_error.lock().unwrap() = Some(error);
                }
                unsafe { PostMessageW(window as HWND, RENDER_STOPPED, 0, 0) };
            })?;
        Ok(Self {
            mailbox,
            error,
            thread: Some(thread),
        })
    }

    pub fn update(&self, view: View, page: Option<PageUpdate>) {
        let (lock, wake) = &*self.mailbox;
        let mut input = lock.lock().unwrap();
        input.view = Some(view);
        if page.is_some() {
            input.page = page;
        }
        wake.notify_one();
    }

    pub fn stop(&self) {
        let (lock, wake) = &*self.mailbox;
        lock.lock().unwrap().stop = true;
        wake.notify_one();
    }

    pub fn error(&self) -> Option<String> {
        self.error.lock().unwrap().take()
    }
}

impl Drop for RenderWorker {
    fn drop(&mut self) {
        self.stop();
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }
}

struct Transition {
    page: PageUpdate,
    scroll: f32,
    started: Instant,
}

struct MotionTiming {
    id: u32,
    start_frame: u64,
    count: u32,
    started: Instant,
    last: Instant,
    max_gap_ms: f64,
}

impl MotionTiming {
    fn start(id: u32, frame: u64) -> Self {
        let now = Instant::now();
        Self {
            id,
            start_frame: frame,
            count: 0,
            started: now,
            last: now,
            max_gap_ms: 0.0,
        }
    }

    fn presented(&mut self) {
        let now = Instant::now();
        if self.count > 0 {
            self.max_gap_ms = self
                .max_gap_ms
                .max((now - self.last).as_secs_f64() * 1000.0);
        }
        self.count += 1;
        self.last = now;
    }
}

fn render(
    window: usize,
    warp: bool,
    mailbox: &(Mutex<Mailbox>, Condvar),
    log: &Log,
) -> Result<(), String> {
    let factory = TextFactory::new()?;
    let mut device: Option<Renderer> = None;
    let mut view: Option<View> = None;
    let mut chrome: Option<(Model, PreparedScene)> = None;
    let mut page = None;
    let mut previous = None;
    let mut stream = None;
    let mut timing: Option<MotionTiming> = None;
    let mut acknowledged_motion = None;
    let mut acknowledged_page = None;
    let mut first_frame = true;
    let mut attempts = 0;
    let mut delay = None;
    loop {
        let (next_view, next_page) = {
            let (lock, wake) = mailbox;
            let input = lock.lock().unwrap();
            let idle =
                |input: &mut Mailbox| !input.stop && input.view.is_none() && input.page.is_none();
            let mut input = match delay {
                Some(duration) if duration != Duration::ZERO => {
                    wake.wait_timeout_while(input, duration, idle).unwrap().0
                }
                Some(_) => input,
                None => wake.wait_while(input, idle).unwrap(),
            };
            if input.stop {
                return Ok(());
            }
            (input.view.take(), input.page.take())
        };

        if let Some(new_page) = next_page {
            let old_scroll = view.as_ref().map_or(0.0, |view| view.chrome.scroll);
            previous = if new_page.transition {
                page.take().map(|page| Transition {
                    page,
                    scroll: old_scroll,
                    started: Instant::now(),
                })
            } else {
                None
            };
            page = Some(new_page);
        }
        if let Some(new_view) = next_view {
            view = Some(new_view);
        }
        let Some(view) = &view else { continue };
        if view.width == 0 || view.height == 0 {
            delay = None;
            continue;
        }

        if !view.chrome.fullscreen
            && chrome
                .as_ref()
                .is_none_or(|(model, _)| *model != view.chrome)
        {
            chrome = Some((view.chrome.clone(), chrome::build(&factory, &view.chrome)?));
        }
        let viewport = chrome::viewport(
            view.chrome.width,
            view.chrome.height,
            view.chrome.settings.position,
        );
        let mut motion = view.motion;
        if let Some(motion) = &mut motion {
            let elapsed = stream.get_or_insert_with(Instant::now).elapsed();
            motion.source_frame = (elapsed.as_nanos() * 60 / 1_000_000_000) as u64;
            if timing
                .as_ref()
                .is_none_or(|timing| timing.id != motion.transition)
            {
                timing = Some(MotionTiming::start(motion.transition, motion.source_frame));
            }
        } else {
            stream = None;
            timing = None;
            acknowledged_motion = None;
        }

        let opacity = previous.as_ref().map_or(1.0, |previous| {
            if view.motion_ms == 0 {
                return 1.0;
            }
            let t = (previous.started.elapsed().as_secs_f32() * 1000.0 / view.motion_ms as f32)
                .clamp(0.0, 1.0);
            t * t * (3.0 - 2.0 * t)
        });
        if opacity >= 1.0 || motion.is_some() {
            previous = None;
        }
        let mut layers = Vec::with_capacity(3);
        if motion.is_none() {
            if let Some(previous) = &previous {
                layers.push(Layer {
                    scene: &previous.page.scene,
                    clip: viewport,
                    offset_x: 0.0,
                    offset_y: viewport.y - previous.scroll,
                    opacity: 1.0,
                    fill_background: true,
                });
            }
            if let Some(page) = &page {
                layers.push(Layer {
                    scene: &page.scene,
                    clip: viewport,
                    offset_x: 0.0,
                    offset_y: viewport.y - view.chrome.scroll,
                    opacity,
                    fill_background: true,
                });
            }
        }
        if !view.chrome.fullscreen
            && let Some((_, scene)) = &chrome
        {
            layers.push(Layer {
                scene,
                clip: Rect::new(0.0, 0.0, view.chrome.width, view.chrome.height),
                offset_x: 0.0,
                offset_y: 0.0,
                opacity: 1.0,
                fill_background: false,
            });
        }
        let background = if view.chrome.fullscreen {
            Color::rgb(0x040810)
        } else {
            chrome::BACKGROUND
        };

        let result = (|| {
            let renderer = if let Some(renderer) = &mut device {
                renderer.resize(view.width, view.height, view.dpi)?;
                renderer
            } else {
                // The UI thread keeps the HWND and pumps messages until RENDER_STOPPED.
                let renderer = unsafe {
                    Renderer::for_window(window, view.width, view.height, view.dpi, warp)
                }?;
                log.write(format_args!(
                    "renderer={} viewport={}x{} dpi={}",
                    if renderer.uses_warp() {
                        "D3D11 WARP"
                    } else {
                        "D3D11 hardware"
                    },
                    view.width,
                    view.height,
                    view.dpi
                ));
                device.insert(renderer)
            };
            renderer.draw(&layers, background, motion.as_ref())
        })();
        let result = match result {
            Ok(result) => {
                attempts = 0;
                result
            }
            Err(error) if attempts < 2 => {
                log.write(format_args!("{error} Reinitializing D3D11."));
                attempts += 1;
                device = None;
                delay = Some(Duration::from_millis(50));
                continue;
            }
            Err(error) => return Err(error),
        };
        if result.presented {
            if first_frame {
                log.write(format_args!("first_present"));
                first_frame = false;
            }
            if let Some(motion) = motion {
                let timing = timing.as_mut().unwrap();
                timing.presented();
                if result.motion_complete && acknowledged_motion != Some(motion.transition) {
                    acknowledged_motion = Some(motion.transition);
                    log.write(format_args!(
                        "motion={} present_count={} elapsed_ms={:.2} max_submit_gap_ms={:.2} source_frames={}..{}",
                        timing.id, timing.count, timing.started.elapsed().as_secs_f64() * 1000.0,
                        timing.max_gap_ms, timing.start_frame, motion.source_frame,
                    ));
                    unsafe {
                        PostMessageW(
                            window as HWND,
                            MOTION_COMPLETE,
                            motion.transition as usize,
                            0,
                        )
                    };
                }
            } else if let Some(page) = &page
                && previous.is_none()
                && acknowledged_page != Some(page.generation)
            {
                acknowledged_page = Some(page.generation);
                log.write(format_args!("document={} presented", page.generation));
                unsafe {
                    PostMessageW(window as HWND, PAGE_PRESENTED, page.generation as usize, 0)
                };
            }
        }
        delay = if !result.presented {
            Some(Duration::from_millis(30))
        } else if previous.is_some()
            || motion.is_some_and(|motion| acknowledged_motion != Some(motion.transition))
        {
            Some(Duration::ZERO)
        } else if let Some(epoch) = stream {
            let elapsed = epoch.elapsed().as_nanos();
            let next = ((elapsed * 60 / 1_000_000_000 + 1) * 1_000_000_000).div_ceil(60);
            Some(Duration::from_nanos((next - elapsed) as u64))
        } else {
            None
        };
    }
}
