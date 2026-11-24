mod protocol;

use std::{
    collections::{HashMap, HashSet},
    io::{self, Cursor, Read, Write},
    net::{SocketAddr, TcpStream},
    os::windows::io::{AsRawHandle, FromRawHandle, OwnedHandle},
    process::{Command, Stdio},
    ptr::{from_mut, null_mut},
    time::Duration,
};

use base64::{Engine, engine::general_purpose};
use display_list::{TextMeasurer, TextMetrics, TextRange, TextRun, TextStyle};
use image::{ImageReader, Limits};
use resvg::{tiny_skia, usvg};
use web_engine::{ImageInfo, ScriptSession, Viewport};
use windows_sys::Win32::{
    Security::{GetTokenInformation, TOKEN_QUERY, TokenIsAppContainer},
    System::Threading::{GetCurrentProcess, OpenProcessToken},
};

use protocol::Text;
pub use protocol::{Assets, Pixels, Probe, Reply, Request, Resources, Snapshot, receive, transmit};

pub const MAX_STYLESHEETS: usize = 32;
pub const MAX_IMAGES: usize = 64;
pub const MAX_SCRIPTS: usize = 32;
pub const MAX_FONTS: usize = 24;
const MAX_REFERENCE_BYTES: usize = 4096;
const MAX_DATA_IMAGE_BYTES: usize = 512 * 1024;
pub const MAX_PIXEL_BYTES: usize = 64 * 1024 * 1024;

pub fn serve() -> Result<(), String> {
    if !is_app_container().map_err(|error| error.to_string())? {
        return Err("Web Content Process requires AppContainer.".into());
    }
    let mut worker = Worker::default();
    let mut input = io::stdin().lock();
    let mut output = io::stdout().lock();
    while let Some(request) = receive(&mut input).map_err(|error| error.to_string())? {
        let reply = worker
            .handle(request, &mut input, &mut output)
            .unwrap_or_else(Reply::Error);
        transmit(&mut output, &reply).map_err(|error| error.to_string())?;
    }
    Ok(())
}

#[derive(Default)]
struct Worker {
    session: Option<ScriptSession>,
    stylesheets: HashMap<String, String>,
    images: Vec<Pixels>,
    image_ids: HashMap<String, u32>,
    published_images: bool,
    focused: Option<u32>,
}

impl Worker {
    fn handle(
        &mut self,
        request: Request,
        input: &mut impl Read,
        output: &mut impl Write,
    ) -> Result<Reply, String> {
        match request {
            Request::Measured(_) => Err("Unexpected measurement response.".into()),
            Request::Open(html) => {
                *self = Self::default();
                let session = ScriptSession::open(&html)?;
                let mut scripts = session.requested_scripts();
                scripts.retain(|script| script.len() <= MAX_REFERENCE_BYTES);
                scripts.truncate(MAX_SCRIPTS);
                let document = session.document();
                let resources = Resources {
                    script_shell: document.is_script_shell(),
                    stylesheets: references(document.external_stylesheets(), MAX_STYLESHEETS),
                    images: references(
                        document.images().filter(|src| !is_data_url(src)),
                        MAX_IMAGES,
                    ),
                    scripts,
                    fonts: references(
                        document.external_fonts().filter(|src| !is_data_url(src)),
                        MAX_FONTS,
                    ),
                };
                drop(document);
                self.session = Some(session);
                Ok(Reply::Resources(resources))
            }
            Request::Layout { viewport, assets } => {
                if !valid_viewport(viewport) {
                    return Err("Invalid viewport.".into());
                }
                self.focused = None;
                let include_images = !self.published_images || assets.is_some();
                if include_images {
                    self.install_assets(assets.unwrap_or_default());
                }
                let page = self.finish_page(viewport, include_images, input, output)?;
                self.published_images = true;
                Ok(page)
            }
            Request::Click { node, viewport } => {
                if !valid_viewport(viewport) {
                    return Err("Invalid viewport.".into());
                }
                self.focused = None;
                let session = self.session.as_mut().ok_or("No document is open.")?;
                session.click(node, viewport)?;
                self.finish_page(viewport, false, input, output)
            }
            Request::FormInput {
                node,
                value,
                viewport,
            } => {
                if !valid_viewport(viewport) {
                    return Err("Invalid viewport.".into());
                }
                self.focused = Some(node);
                let session = self.session.as_mut().ok_or("No document is open.")?;
                session
                    .document_mut()
                    .set_attr(node as usize, "value", value);
                self.finish_page(viewport, false, input, output)
            }
            Request::FormToggle { node, viewport } => {
                if !valid_viewport(viewport) {
                    return Err("Invalid viewport.".into());
                }
                let session = self.session.as_mut().ok_or("No document is open.")?;
                session.document_mut().toggle_checkbox(node as usize);
                self.finish_page(viewport, false, input, output)
            }
            Request::FormRadio { node, viewport } => {
                if !valid_viewport(viewport) {
                    return Err("Invalid viewport.".into());
                }
                let session = self.session.as_mut().ok_or("No document is open.")?;
                session.document_mut().select_radio(node as usize);
                self.finish_page(viewport, false, input, output)
            }
            Request::SubmitForm { node } => {
                let session = self.session.as_ref().ok_or("No document is open.")?;
                let document = session.document();
                let form_id = document
                    .form_for_node(node as usize)
                    .ok_or("No enclosing form found.")?;
                let (action, _method, pairs) = document
                    .serialize_form(form_id)
                    .ok_or("Failed to serialize form.")?;
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
                Ok(Reply::Submitted { action, query })
            }
            Request::Probe {
                private_file,
                loopback_port,
            } => {
                let file_blocked = std::fs::File::open(private_file).is_err();
                let loopback = SocketAddr::from(([127, 0, 0, 1], loopback_port));
                let network_blocked =
                    TcpStream::connect_timeout(&loopback, Duration::from_millis(350)).is_err();
                let child_blocked =
                    Command::new(std::env::current_exe().map_err(|error| error.to_string())?)
                        .stdin(Stdio::null())
                        .stdout(Stdio::null())
                        .stderr(Stdio::null())
                        .status()
                        .is_err();
                Ok(Reply::Probe(Probe {
                    file_blocked,
                    network_blocked,
                    child_blocked,
                }))
            }
        }
    }

    fn install_assets(&mut self, assets: Assets) {
        if let Some(session) = self.session.as_mut() {
            session.install_scripts(assets.scripts);
            for (_, css) in &assets.stylesheets {
                for face in web_engine::parse_font_faces(css) {
                    session.document_mut().add_font_face(face);
                }
            }
        }
        self.stylesheets = assets
            .stylesheets
            .into_iter()
            .take(MAX_STYLESHEETS)
            .collect();
        self.images.clear();
        self.image_ids.clear();
        let mut remaining = MAX_PIXEL_BYTES;
        let inline = self.session.iter().flat_map(|session| {
            let document = session.document();
            document
                .images()
                .filter(|src| src.len() <= MAX_DATA_IMAGE_BYTES + 70 && is_data_url(src))
                .take(MAX_IMAGES)
                .map(str::to_owned)
                .collect::<Vec<_>>()
        });
        let assets = inline
            .filter_map(|reference| {
                let bytes = data_image(&reference)?;
                Some((reference, bytes))
            })
            .chain(assets.images);
        for (reference, bytes) in assets {
            if self.images.len() >= MAX_IMAGES || remaining < 4 {
                break;
            }
            if self.image_ids.contains_key(&reference) {
                continue;
            }
            if let Some(image) = decode_image(&bytes, remaining) {
                remaining -= image.bgra.len();
                self.image_ids.insert(reference, self.images.len() as u32);
                self.images.push(image);
            }
        }
    }
}

impl Worker {
    fn finish_page(
        &mut self,
        viewport: Viewport,
        include_images: bool,
        input: &mut impl Read,
        output: &mut impl Write,
    ) -> Result<Reply, String> {
        self.session
            .as_mut()
            .ok_or("No document is open.")?
            .run(viewport)?;
        let failure = self.session.as_ref().unwrap().failure();

        let inline_svgs: Vec<(usize, String)> = {
            let document = self.session.as_ref().unwrap().document();
            document
                .inline_svg_elements()
                .into_iter()
                .map(|id| (id, document.serialize_subtree_xml(id)))
                .collect()
        };

        let mut remaining = MAX_PIXEL_BYTES.saturating_sub(self.images.iter().map(|img| img.bgra.len()).sum());
        for (svg_id, xml) in inline_svgs {
            let key = format!("__inline_svg__{svg_id}");
            if !self.image_ids.contains_key(&key) {
                if let Some(pixels) = decode_svg(xml.as_bytes(), remaining) {
                    remaining = remaining.saturating_sub(pixels.bgra.len());
                    let image_id = self.images.len() as u32;
                    self.image_ids.insert(key, image_id);
                    self.images.push(pixels);
                }
            }
        }

        let document = self.session.as_ref().unwrap().document();
        let mut recorder = RemoteText { input, output };
        let page = web_engine::layout_with_stylesheets(
            &document,
            viewport,
            &self.stylesheets,
            &mut recorder,
            |src| {
                let &id = self.image_ids.get(src)?;
                let image = self.images.get(id as usize)?;
                Some(ImageInfo {
                    id,
                    width: image.width,
                    height: image.height,
                })
            },
            self.focused.map(|id| web_engine::InputState {
                focused: Some(id),
                caret: usize::MAX,
            }),
        )?;
        if page.height == 0.0
            && let Some(failure) = failure.filter(|failure| !failure.is_empty())
        {
            return Err(failure);
        }
        Ok(Reply::Page(Snapshot {
            title: document.title().chars().take(160).collect(),
            page,
            images: include_images.then(|| self.images.clone()),
        }))
    }
}

fn references<'a>(values: impl Iterator<Item = &'a str>, limit: usize) -> Vec<String> {
    let mut seen = HashSet::new();
    values
        .filter(|value| {
            !value.trim().is_empty() && value.len() <= MAX_REFERENCE_BYTES && seen.insert(*value)
        })
        .take(limit)
        .map(str::to_owned)
        .collect()
}

fn is_data_url(src: &str) -> bool {
    src.trim()
        .get(..5)
        .is_some_and(|scheme| scheme.eq_ignore_ascii_case("data:"))
}

fn data_image(src: &str) -> Option<Vec<u8>> {
    let (metadata, payload) = src.trim().get(5..)?.split_once(',')?;
    if metadata.len() > 64 || payload.len() > MAX_DATA_IMAGE_BYTES {
        return None;
    }
    let (mime, encoding) = metadata.split_once(';')?;
    if !encoding.eq_ignore_ascii_case("base64")
        || !matches!(
            mime.to_ascii_lowercase().as_str(),
            "image/png" | "image/jpeg" | "image/webp" | "image/gif" | "image/svg+xml"
        )
    {
        return None;
    }
    general_purpose::STANDARD
        .decode(payload)
        .or_else(|_| general_purpose::STANDARD_NO_PAD.decode(payload))
        .ok()
}

fn valid_viewport(viewport: Viewport) -> bool {
    (1.0..=16_384.0).contains(&viewport.width) && (0.0..=16_384.0).contains(&viewport.height)
}

fn decode_image(bytes: &[u8], remaining: usize) -> Option<Pixels> {
    if remaining < 4 || bytes.len() > 2 * 1024 * 1024 {
        return None;
    }
    let mut reader = ImageReader::new(Cursor::new(bytes))
        .with_guessed_format()
        .ok()?;
    if reader.format().is_none() {
        return decode_svg(bytes, remaining);
    }
    let mut limits = Limits::default();
    limits.max_image_width = Some(2048);
    limits.max_image_height = Some(2048);
    limits.max_alloc = Some(32 * 1024 * 1024);
    reader.limits(limits);
    let decoded = reader.decode().ok()?.into_rgba8();
    let (width, height) = decoded.dimensions();
    let mut bgra = decoded.into_raw();
    if !(1..=2048).contains(&width) || !(1..=2048).contains(&height) || bgra.len() > remaining {
        return None;
    }
    for pixel in bgra.as_chunks_mut::<4>().0 {
        let alpha = u16::from(pixel[3]);
        for component in &mut pixel[..3] {
            *component = ((u16::from(*component) * alpha + 127) / 255) as u8;
        }
        pixel.swap(0, 2);
    }
    Some(Pixels {
        width,
        height,
        bgra,
    })
}

fn decode_svg(bytes: &[u8], remaining: usize) -> Option<Pixels> {
    if bytes.len() > MAX_DATA_IMAGE_BYTES
        || !std::str::from_utf8(bytes)
            .ok()?
            .trim_start_matches('\u{feff}')
            .trim_start()
            .starts_with('<')
    {
        return None;
    }
    let options = usvg::Options {
        image_href_resolver: usvg::ImageHrefResolver {
            resolve_data: Box::new(|_, _, _| None),
            resolve_string: Box::new(|_, _| None),
        },
        ..usvg::Options::default()
    };
    let tree = usvg::Tree::from_data(bytes, &options).ok()?;
    let (width, height) = (tree.size().width(), tree.size().height());
    let scale = (1024.0 / width.max(height)).min(1.0);
    let output_width = (width * scale).ceil().clamp(1.0, 1024.0) as u32;
    let output_height = (height * scale).ceil().clamp(1.0, 1024.0) as u32;
    if output_width as usize * output_height as usize * 4 > remaining {
        return None;
    }
    let mut pixmap = tiny_skia::Pixmap::new(output_width, output_height)?;
    resvg::render(
        &tree,
        tiny_skia::Transform::from_scale(scale, scale),
        &mut pixmap.as_mut(),
    );
    let mut bgra = pixmap.take();
    for pixel in bgra.as_chunks_mut::<4>().0 {
        pixel.swap(0, 2);
    }
    Some(Pixels {
        width: output_width,
        height: output_height,
        bgra,
    })
}

struct RemoteText<'a, I, O> {
    input: &'a mut I,
    output: &'a mut O,
}

impl<I: Read, O: Write> TextMeasurer for RemoteText<'_, I, O> {
    fn measure(
        &mut self,
        text: &str,
        style: TextStyle,
        width: f32,
        links: &[TextRange],
    ) -> Result<TextMetrics, String> {
        self.measure_styled(text, style, width, links, &[], true)
    }

    fn measure_styled(
        &mut self,
        text: &str,
        style: TextStyle,
        width: f32,
        links: &[TextRange],
        runs: &[TextRun],
        wrap: bool,
    ) -> Result<TextMetrics, String> {
        transmit(
            self.output,
            &Reply::Measure(Text {
                value: text.to_owned(),
                style,
                width,
                links: links.to_vec(),
                runs: runs.to_vec(),
                wrap,
            }),
        )
        .map_err(|error| error.to_string())?;
        match receive(self.input).map_err(|error| error.to_string())? {
            Some(Request::Measured(result)) => result,
            _ => Err("Expected text metrics from the browser.".into()),
        }
    }
}

fn is_app_container() -> io::Result<bool> {
    let mut token = null_mut();
    if unsafe { OpenProcessToken(GetCurrentProcess(), TOKEN_QUERY, &mut token) } == 0 {
        return Err(io::Error::last_os_error());
    }
    let token = unsafe { OwnedHandle::from_raw_handle(token) };
    let (mut app_container, mut bytes) = (0u32, 0u32);
    if unsafe {
        GetTokenInformation(
            token.as_raw_handle(),
            TokenIsAppContainer,
            from_mut(&mut app_container).cast(),
            std::mem::size_of::<u32>() as u32,
            &mut bytes,
        )
    } == 0
    {
        return Err(io::Error::last_os_error());
    }
    Ok(app_container != 0)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn decodes_svg_to_premultiplied_bgra_under_the_pixel_budget() {
        let svg = br#"<svg xmlns="http://www.w3.org/2000/svg" width="8" height="4">
            <rect width="8" height="4" fill="red" fill-opacity="0.5"/>
        </svg>"#;
        assert!(decode_image(svg, 127).is_none());
        let pixels = decode_image(svg, 128).unwrap();
        assert_eq!((pixels.width, pixels.height), (8, 4));
        assert_eq!(&pixels.bgra[..4], &[0, 0, 128, 128]);
        let src = format!(
            "data:image/svg+xml;base64,{}",
            general_purpose::STANDARD.encode(svg)
        );
        assert_eq!(data_image(&src).unwrap(), svg);
    }

    struct MockBrowserInput {
        buffer: Vec<u8>,
    }

    impl Read for MockBrowserInput {
        fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
            if self.buffer.is_empty() {
                let msg = Request::Measured(Ok(display_list::TextMetrics {
                    layout: 0,
                    width: 50.0,
                    height: 16.0,
                    links: Vec::new(),
                }));
                let mut encoded = Vec::new();
                transmit(&mut encoded, &msg).map_err(io::Error::other)?;
                self.buffer = encoded;
            }
            let len = buf.len().min(self.buffer.len());
            buf[..len].copy_from_slice(&self.buffer[..len]);
            self.buffer.drain(..len);
            Ok(len)
        }
    }

    #[test]
    fn test_worker_form_handling_and_submission() {
        let mut worker = Worker::default();
        let html = r#"
            <form action="/search" method="GET">
                <input type="text" id="q" name="query" value="initial">
                <input type="checkbox" id="c" name="subscribe" checked>
                <input type="radio" id="r1" name="opt" value="1">
                <input type="radio" id="r2" name="opt" value="2" checked>
                <input type="submit" id="btn" value="Go">
            </form>
        "#;
        let mut mock_input = MockBrowserInput { buffer: Vec::new() };
        let mut dummy_output = Vec::<u8>::new();
        let reply = worker
            .handle(
                Request::Open(html.into()),
                &mut mock_input,
                &mut dummy_output,
            )
            .unwrap();
        assert!(matches!(reply, Reply::Resources(_)));

        let input_id = {
            let session = worker.session.as_ref().unwrap();
            session.document().element_by_id("q").unwrap() as u32
        };
        let toggle_id = {
            let session = worker.session.as_ref().unwrap();
            session.document().element_by_id("c").unwrap() as u32
        };
        let radio_id = {
            let session = worker.session.as_ref().unwrap();
            session.document().element_by_id("r1").unwrap() as u32
        };

        // FormInput
        let reply_input = worker
            .handle(
                Request::FormInput {
                    node: input_id,
                    value: "typed text".into(),
                    viewport: Viewport {
                        width: 400.0,
                        height: 300.0,
                    },
                },
                &mut mock_input,
                &mut dummy_output,
            )
            .unwrap();
        assert!(matches!(reply_input, Reply::Page(_)));
        {
            let session = worker.session.as_ref().unwrap();
            assert_eq!(
                session.document().get_attr(input_id as usize, "value"),
                Some("typed text")
            );
        }

        // FormToggle
        let reply_toggle = worker
            .handle(
                Request::FormToggle {
                    node: toggle_id,
                    viewport: Viewport {
                        width: 400.0,
                        height: 300.0,
                    },
                },
                &mut mock_input,
                &mut dummy_output,
            )
            .unwrap();
        assert!(matches!(reply_toggle, Reply::Page(_)));
        {
            let session = worker.session.as_ref().unwrap();
            assert_eq!(
                session.document().get_attr(toggle_id as usize, "checked"),
                None
            );
        }

        // FormRadio
        let reply_radio = worker
            .handle(
                Request::FormRadio {
                    node: radio_id,
                    viewport: Viewport {
                        width: 400.0,
                        height: 300.0,
                    },
                },
                &mut mock_input,
                &mut dummy_output,
            )
            .unwrap();
        assert!(matches!(reply_radio, Reply::Page(_)));
        {
            let session = worker.session.as_ref().unwrap();
            assert_eq!(
                session.document().get_attr(radio_id as usize, "checked"),
                Some("")
            );
        }

        // SubmitForm
        let submit_reply = worker
            .handle(
                Request::SubmitForm { node: input_id },
                &mut mock_input,
                &mut dummy_output,
            )
            .unwrap();
        match submit_reply {
            Reply::Submitted { action, query } => {
                assert_eq!(action, "/search");
                assert_eq!(query, "?query=typed+text&opt=1");
            }
            _ => panic!("Expected Reply::Submitted"),
        }
    }

    #[test]
    fn test_inline_svg_discovery_and_registration() {
        let mut worker = Worker::default();
        let html = r#"
            <body style="margin:0">
                <svg id="s1" width="16" height="16" viewBox="0 0 16 16">
                    <rect width="16" height="16" fill="blue" />
                </svg>
            </body>
        "#;
        let mut mock_input = MockBrowserInput { buffer: Vec::new() };
        let mut dummy_output = Vec::<u8>::new();
        let reply = worker
            .handle(
                Request::Open(html.into()),
                &mut mock_input,
                &mut dummy_output,
            )
            .unwrap();
        assert!(matches!(reply, Reply::Resources(_)));

        let svg_id = {
            let session = worker.session.as_ref().unwrap();
            session.document().element_by_id("s1").unwrap()
        };
        let expected_key = format!("__inline_svg__{svg_id}");

        let page_reply = worker
            .handle(
                Request::Layout {
                    viewport: Viewport {
                        width: 800.0,
                        height: 600.0,
                    },
                    assets: None,
                },
                &mut mock_input,
                &mut dummy_output,
            )
            .unwrap();

        match page_reply {
            Reply::Page(snapshot) => {
                // Ensure image was registered
                assert!(worker.image_ids.contains_key(&expected_key));
                let img_id = worker.image_ids[&expected_key];
                assert!((img_id as usize) < worker.images.len());

                // Ensure Snapshot received images
                let images = snapshot.images.expect("images should be included on initial layout");
                assert!((img_id as usize) < images.len());
                let pixels = &images[img_id as usize];
                assert_eq!((pixels.width, pixels.height), (16, 16));

                // Display list should contain an Image command for the SVG
                let has_img_cmd = snapshot.page.display_list.commands.iter().any(|cmd| {
                    matches!(cmd, display_list::Command::Image { image, .. } if *image == img_id)
                });
                assert!(has_img_cmd, "DisplayList must contain Command::Image for the inline SVG");
            }
            _ => panic!("Expected Reply::Page"),
        }
    }
}
