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

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum FormAction {
    Click(u32),
    Input { node: u32, value: String },
    Toggle(u32),
    Radio(u32),
    Submit(u32),
}

struct Job {
    generation: u32,
    location: Location,
    viewport: Viewport,
    reload: bool,
    action: Option<FormAction>,
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
                            let page = match job.action {
                                Some(FormAction::Click(node)) => {
                                    site.as_mut()
                                        .unwrap()
                                        .click(node, job.viewport, job.generation)
                                }
                                Some(FormAction::Input { node, value }) => site
                                    .as_mut()
                                    .unwrap()
                                    .form_input(node, value, job.viewport, job.generation),
                                Some(FormAction::Toggle(node)) => site
                                    .as_mut()
                                    .unwrap()
                                    .form_toggle(node, job.viewport, job.generation),
                                Some(FormAction::Radio(node)) => site.as_mut().unwrap().form_radio(
                                    node,
                                    job.viewport,
                                    job.generation,
                                ),
                                Some(FormAction::Submit(node)) => {
                                    if let Some((action, query)) =
                                        site.as_ref().unwrap().submit_form(node)
                                    {
                                        let target_href = if action.is_empty() {
                                            query
                                        } else if query.is_empty() {
                                            action
                                        } else if action.contains('?') {
                                            format!("{}&{}", action, &query[1..])
                                        } else {
                                            format!("{}{}", action, query)
                                        };
                                        let target_url = url.join(&target_href).ok();
                                        let mut page = site
                                            .as_mut()
                                            .unwrap()
                                            .layout(job.viewport, job.generation)?;
                                        page.resolved = target_url;
                                        Ok(page)
                                    } else {
                                        site.as_mut().unwrap().layout(job.viewport, job.generation)
                                    }
                                }
                                None => site.as_mut().unwrap().layout(job.viewport, job.generation),
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
                            let proc = &local.as_ref().unwrap().1;
                            let page = match job.action {
                                Some(FormAction::Click(node)) => proc.paint_page(
                                    Request::Click {
                                        node,
                                        viewport: job.viewport,
                                    },
                                    job.generation,
                                ),
                                Some(FormAction::Input { node, value }) => {
                                    proc.form_input(node, value, job.viewport, job.generation)
                                }
                                Some(FormAction::Toggle(node)) => {
                                    proc.form_toggle(node, job.viewport, job.generation)
                                }
                                Some(FormAction::Radio(node)) => {
                                    proc.form_radio(node, job.viewport, job.generation)
                                }
                                Some(FormAction::Submit(node)) => {
                                    if let Some((action, query)) = proc.submit_form(node) {
                                        let target_href = if action.is_empty() {
                                            query
                                        } else if query.is_empty() {
                                            action
                                        } else if action.contains('?') {
                                            format!("{}&{}", action, &query[1..])
                                        } else {
                                            format!("{}{}", action, query)
                                        };
                                        let target_url = Url::parse(&target_href).ok();
                                        let mut page = proc.paint_page(
                                            Request::Layout {
                                                viewport: job.viewport,
                                                assets: None,
                                            },
                                            job.generation,
                                        )?;
                                        page.resolved = target_url;
                                        Ok(page)
                                    } else {
                                        proc.paint_page(
                                            Request::Layout {
                                                viewport: job.viewport,
                                                assets: None,
                                            },
                                            job.generation,
                                        )
                                    }
                                }
                                None => proc.paint_page(
                                    Request::Layout {
                                        viewport: job.viewport,
                                        assets: None,
                                    },
                                    job.generation,
                                ),
                            };
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
                        let (_, document, images) = cached.as_mut().unwrap();
                        let mut input_state = None;
                        let mut resolved = None;
                        match job.action {
                            Some(FormAction::Input { node, value }) => {
                                document.set_attr(node as usize, "value", value);
                                input_state = Some(web_engine::InputState {
                                    focused: Some(node),
                                    caret: usize::MAX,
                                });
                            }
                            Some(FormAction::Toggle(node)) => {
                                document.toggle_checkbox(node as usize);
                            }
                            Some(FormAction::Radio(node)) => {
                                document.select_radio(node as usize);
                            }
                            Some(FormAction::Submit(node)) => {
                                if let Some(form_id) = document.form_for_node(node as usize)
                                    && let Some((action, _method, pairs)) =
                                        document.serialize_form(form_id)
                                {
                                    let query = if pairs.is_empty() {
                                        String::new()
                                    } else {
                                        let qs = pairs
                                            .iter()
                                            .map(|(k, v)| {
                                                format!(
                                                    "{}={}",
                                                    web_engine::url_encode(k),
                                                    web_engine::url_encode(v)
                                                )
                                            })
                                            .collect::<Vec<_>>()
                                            .join("&");
                                        format!("?{qs}")
                                    };
                                    let target_href = if action.is_empty() {
                                        query
                                    } else if query.is_empty() {
                                        action
                                    } else if action.contains('?') {
                                        format!("{}&{}", action, &query[1..])
                                    } else {
                                        format!("{}{}", action, query)
                                    };
                                    resolved = Url::parse(&target_href).ok();
                                }
                            }
                            _ => {}
                        }
                        let mut page = build_page(
                            document,
                            factory,
                            job.generation,
                            job.viewport,
                            images,
                            input_state,
                        )?;
                        page.resolved = resolved;
                        Ok(page)
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
        action: Option<FormAction>,
    ) {
        let (lock, wake) = &*self.input;
        lock.lock().unwrap().next = Some(Job {
            generation,
            location,
            viewport,
            reload,
            action,
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
    input_state: Option<web_engine::InputState>,
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
        input_state,
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
            None,
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
            None,
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
            None,
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

    #[test]
    fn test_document_worker_form_actions_on_cached_document() {
        let html = r#"
            <form action="https://example.com/login" method="GET">
                <input type="text" id="user" name="username" value="">
                <input type="checkbox" id="remember" name="remember">
                <input type="submit" id="btn" value="Log in">
            </form>
        "#;
        let mut doc = web_engine::parse_html(html).unwrap();
        let user_id = doc.element_by_id("user").unwrap() as u32;
        let remember_id = doc.element_by_id("remember").unwrap() as u32;

        // Input
        doc.set_attr(user_id as usize, "value", "alice".into());
        assert_eq!(doc.get_attr(user_id as usize, "value"), Some("alice"));

        // Toggle
        doc.toggle_checkbox(remember_id as usize);
        assert_eq!(doc.get_attr(remember_id as usize, "checked"), Some(""));

        // Submit
        let form_id = doc.form_for_node(user_id as usize).unwrap();
        let (action, _method, pairs) = doc.serialize_form(form_id).unwrap();
        let qs = pairs
            .iter()
            .map(|(k, v)| {
                format!(
                    "{}={}",
                    web_engine::url_encode(k),
                    web_engine::url_encode(v)
                )
            })
            .collect::<Vec<_>>()
            .join("&");
        let query = format!("?{qs}");
        let target_href = format!("{}{}", action, query);
        let target_url = Url::parse(&target_href).unwrap();
        assert_eq!(
            target_url.as_str(),
            "https://example.com/login?username=alice&remember=on"
        );
    }
}
