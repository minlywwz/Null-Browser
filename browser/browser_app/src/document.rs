use std::{
    collections::HashMap,
    io::Cursor,
    sync::{Arc, Condvar, Mutex, mpsc},
    thread::{self, JoinHandle},
    time::Instant,
};

use browser_core::{Example, Location};
use network_stack::{NetworkStack, Url};
use rendering_ffi::{Image, PreparedScene, TextFactory};
use web_content_process::Request;
use web_engine::{Document, ImageInfo, LaidOutPage, Link, Viewport};
use windows_sys::Win32::{Foundation::HWND, UI::WindowsAndMessaging::PostMessageW};

use crate::{
    Log,
    site::{Site, WebContentProcess},
    window::DOCUMENT_READY,
};

pub struct Geometry {
    pub title: String,
    pub height: f32,
    pub links: Vec<Link>,
    pub anchors: HashMap<String, f32>,
}

pub struct Page {
    pub scene: PreparedScene,
    pub geometry: Geometry,
    pub resolved: Option<Url>,
}

struct Job {
    generation: u32,
    location: Location,
    viewport: Viewport,
    reload: bool,
    click: Option<u32>,
}

pub struct Completion {
    pub generation: u32,
    pub result: Result<Page, String>,
}

#[derive(Default)]
struct Mailbox {
    next: Option<Job>,
    stop: bool,
}

pub struct DocumentWorker {
    input: Arc<(Mutex<Mailbox>, Condvar)>,
    pub complete: mpsc::Receiver<Completion>,
    thread: Option<JoinHandle<()>>,
}

impl DocumentWorker {
    pub fn start(window: usize, log: Log) -> std::io::Result<Self> {
        let input = Arc::new((Mutex::new(Mailbox::default()), Condvar::new()));
        let worker_input = Arc::clone(&input);
        let (send, complete) = mpsc::channel();
        let thread = thread::Builder::new()
            .name("Web Engine".into())
            .spawn(move || {
                let factory = TextFactory::new();
                let mut network = None;
                let mut site: Option<Site> = None;
                let mut local: Option<(Location, WebContentProcess)> = None;
                let mut cached: Option<(Location, Document, Vec<Image>)> = None;
                loop {
                    let job = {
                        let (lock, wake) = &*worker_input;
                        let mut input = wake
                            .wait_while(lock.lock().unwrap(), |input| {
                                !input.stop && input.next.is_none()
                            })
                            .unwrap();
                        if input.stop {
                            break;
                        }
                        input.next.take().unwrap()
                    };
                    let start = Instant::now();
                    let result = (|| {
                        if let Location::Web(url) = &job.location {
                            local = None;
                            if job.reload || site.as_ref().is_none_or(|site| !site.matches(url)) {
                                if network.is_none() {
                                    network = Some(NetworkStack::new()?);
                                }
                                site = Some(Site::open(network.as_ref().unwrap(), url, &log)?);
                            }
                            let page = if let Some(node) = job.click {
                                site.as_mut()
                                    .unwrap()
                                    .click(node, job.viewport, job.generation)
                            } else {
                                site.as_mut().unwrap().layout(job.viewport, job.generation)
                            };
                            if page.is_err() {
                                site = None;
                            }
                            return page;
                        }
                        site = None;
                        if let Location::File(_) = &job.location {
                            if job.reload
                                || local
                                    .as_ref()
                                    .is_none_or(|(location, _)| *location != job.location)
                            {
                                let html = job.location.read()?;
                                let process = WebContentProcess::start()?;
                                process.open_html(html)?;
                                local = Some((job.location.clone(), process));
                            }
                            let request = match job.click {
                                Some(node) => Request::Click {
                                    node,
                                    viewport: job.viewport,
                                },
                                None => Request::Layout {
                                    viewport: job.viewport,
                                    assets: None,
                                },
                            };
                            let page = local
                                .as_ref()
                                .unwrap()
                                .1
                                .paint_page(request, job.generation);
                            if page.is_err() {
                                local = None;
                            }
                            return page;
                        }
                        local = None;
                        let factory = factory.as_ref().map_err(Clone::clone)?;
                        if job.reload
                            || cached
                                .as_ref()
                                .is_none_or(|(location, _, _)| *location != job.location)
                        {
                            let document = web_engine::parse_html(&job.location.read()?)?;
                            let images = if job.location == Location::Example(Example::Rendering) {
                                vec![sample_image()?]
                            } else {
                                Vec::new()
                            };
                            cached = Some((job.location.clone(), document, images));
                        }
                        let (_, document, images) = cached.as_ref().unwrap();
                        build_page(document, factory, job.generation, job.viewport, images)
                    })();
                    log.write(format_args!(
                        "document={} layout_ms={:.2} ok={}",
                        job.generation,
                        start.elapsed().as_secs_f64() * 1_000.0,
                        result.is_ok()
                    ));
                    if let Err(error) = &result {
                        log.write(format_args!("document={} error={error}", job.generation));
                    }
                    if send
                        .send(Completion {
                            generation: job.generation,
                            result,
                        })
                        .is_err()
                    {
                        break;
                    }
                    unsafe { PostMessageW(window as HWND, DOCUMENT_READY, 0, 0) };
                }
            })?;
        Ok(Self {
            input,
            complete,
            thread: Some(thread),
        })
    }

    pub fn request(
        &self,
        generation: u32,
        location: Location,
        viewport: Viewport,
        reload: bool,
        click: Option<u32>,
    ) {
        let (lock, wake) = &*self.input;
        lock.lock().unwrap().next = Some(Job {
            generation,
            location,
            viewport,
            reload,
            click,
        });
        wake.notify_one();
    }

    pub fn stop(&self) {
        let (lock, wake) = &*self.input;
        lock.lock().unwrap().stop = true;
        wake.notify_one();
    }
}

impl Drop for DocumentWorker {
    fn drop(&mut self) {
        self.stop();
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }
}

pub fn build_page(
    document: &Document,
    factory: &TextFactory,
    generation: u32,
    viewport: Viewport,
    images: &[Image],
) -> Result<Page, String> {
    let mut batch = factory.batch();
    let LaidOutPage {
        display_list,
        height,
        links,
        anchors,
    } = web_engine::layout_with_stylesheets(
        document,
        viewport,
        &HashMap::new(),
        &mut batch,
        |src| {
            let image = (src == "rendering.png").then(|| images.first()).flatten()?;
            Some(ImageInfo {
                id: 0,
                width: image.width,
                height: image.height,
            })
        },
    )?;
    let geometry = Geometry {
        title: document.title().to_owned(),
        height,
        links,
        anchors,
    };
    Ok(Page {
        scene: batch.prepare(u64::from(generation), display_list, images.to_vec()),
        geometry,
        resolved: None,
    })
}

pub fn sample_image() -> Result<Image, String> {
    let bytes = include_bytes!("../../../testing/fixtures/rendering.png");
    let mut decoder = png::Decoder::new(Cursor::new(bytes));
    decoder.set_transformations(png::Transformations::normalize_to_color8());
    decoder.set_limits(png::Limits {
        bytes: 8 * 1024 * 1024,
    });
    let mut reader = decoder.read_info().map_err(|error| error.to_string())?;
    let size = reader
        .output_buffer_size()
        .filter(|size| *size <= 8 * 1024 * 1024)
        .ok_or("The bundled PNG exceeds the image limit.")?;
    let mut pixels = vec![0; size];
    let output = reader
        .next_frame(&mut pixels)
        .map_err(|error| error.to_string())?;
    let pixels = &pixels[..output.buffer_size()];
    let bgra = match output.color_type {
        png::ColorType::Rgb => pixels
            .as_chunks::<3>()
            .0
            .iter()
            .flat_map(|rgb| [rgb[2], rgb[1], rgb[0], 255])
            .collect(),
        png::ColorType::Rgba => pixels
            .as_chunks::<4>()
            .0
            .iter()
            .flat_map(|rgba| {
                let mul =
                    |component| ((u16::from(component) * u16::from(rgba[3]) + 127) / 255) as u8;
                [mul(rgba[2]), mul(rgba[1]), mul(rgba[0]), rgba[3]]
            })
            .collect(),
        _ => return Err("The bundled PNG must be RGB or RGBA.".into()),
    };
    Image::new(output.width, output.height, bgra)
}

#[cfg(test)]
mod tests {
    use super::*;
    use display_list::{Color, Rect};
    use rendering_ffi::{Layer, Renderer};

    fn draw_page(page: &Page, width: u32, height: u32) -> Renderer {
        let mut renderer = Renderer::offscreen(width, height).unwrap();
        renderer
            .draw(
                &[Layer {
                    scene: &page.scene,
                    clip: Rect::new(0.0, 0.0, width as f32, height as f32),
                    offset_x: 0.0,
                    offset_y: 0.0,
                    opacity: 1.0,
                    fill_background: true,
                }],
                Color::rgb(0),
                None,
            )
            .unwrap();
        renderer
    }

    #[test]
    fn paints_a_completed_stream_boundary_instead_of_a_blank_canvas() {
        let html = web_engine::parse_html(r#"
            <body style="margin:0"><main><!--$?--><template id="B:0"></template><!--/$--></main>
            <div hidden id="S:1"><div style="background:#23678b;height:64px"><h1>Ready</h1></div></div>
            <script>$RC("B:0","S:1")</script></body>
        "#).unwrap();
        let factory = TextFactory::new().unwrap();
        let page = build_page(
            &html,
            &factory,
            1,
            Viewport {
                width: 320.0,
                height: 160.0,
            },
            &[],
        )
        .unwrap();
        let mut renderer = draw_page(&page, 320, 160);
        assert_eq!(renderer.read_pixel(310, 15).unwrap() & 0xffffff, 0x23678b);
        assert_eq!(renderer.read_pixel(310, 100).unwrap() & 0xffffff, 0xffffff);
    }

    #[test]
    fn draws_bundled_html_and_png_through_the_native_compositor() {
        let location = Location::Example(Example::Rendering);
        let html = web_engine::parse_html(&location.read().unwrap()).unwrap();
        let factory = TextFactory::new().unwrap();
        let page = build_page(
            &html,
            &factory,
            1,
            Viewport {
                width: 800.0,
                height: 850.0,
            },
            &[sample_image().unwrap()],
        )
        .unwrap();
        assert_eq!(page.geometry.title, "Rendering sample");
        assert!(page.geometry.links.iter().any(|link| link.href == "#type"));
        let image_y = (page.geometry.anchors["image"] + 23.0 + 50.0) as u32;
        let mut renderer = draw_page(&page, 800, 850);
        assert_eq!(renderer.read_pixel(4, 4).unwrap() & 0xffffff, 0x0b111a);
        assert_ne!(
            renderer.read_pixel(170, image_y).unwrap() & 0xffffff,
            0x131e2c
        );
        assert_ne!(
            renderer.read_pixel(170, image_y).unwrap() & 0xffffff,
            renderer.read_pixel(270, image_y + 70).unwrap() & 0xffffff
        );
    }

    #[test]
    fn paints_inline_colors_from_the_cascade() {
        let html = web_engine::parse_html(
            r#"<style>b { color: red ! important } em { color: blue }</style>
            <body style="margin:0;font-size:40px"><b style="color: green">█</b><em>█</em></body>"#,
        )
        .unwrap();
        let page = build_page(
            &html,
            &TextFactory::new().unwrap(),
            1,
            Viewport {
                width: 160.0,
                height: 80.0,
            },
            &[],
        )
        .unwrap();
        let mut renderer = draw_page(&page, 160, 80);
        let row: Vec<_> = (0..100)
            .map(|x| renderer.read_pixel(x, 20).unwrap() & 0xffffff)
            .collect();
        let red = row.iter().position(|pixel| *pixel == 0xff0000).unwrap();
        let blue = row.iter().position(|pixel| *pixel == 0x0000ff).unwrap();
        assert!(red < blue);
    }
}
