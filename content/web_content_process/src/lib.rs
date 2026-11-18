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

pub const MAX_STYLESHEETS: usize = 8;
pub const MAX_IMAGES: usize = 16;
pub const MAX_SCRIPTS: usize = 24;
const MAX_REFERENCE_BYTES: usize = 4096;
const MAX_DATA_IMAGE_BYTES: usize = 512 * 1024;
pub const MAX_PIXEL_BYTES: usize = 24 * 1024 * 1024;

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
                };
                drop(document);
                self.session = Some(session);
                Ok(Reply::Resources(resources))
            }
            Request::Layout { viewport, assets } => {
                if !valid_viewport(viewport) {
                    return Err("Invalid viewport.".into());
                }
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
                let session = self.session.as_mut().ok_or("No document is open.")?;
                session.click(node, viewport)?;
                self.finish_page(viewport, false, input, output)
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
}
