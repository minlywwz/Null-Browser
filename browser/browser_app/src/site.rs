mod process;
mod validate;

use std::{
    thread,
    time::{Duration, Instant},
};

use network_stack::{NetworkStack, Resource, ResourceKind, Url};
pub(crate) use process::WebContentProcess;
use std::collections::HashSet;
use web_content_process::{Assets, Reply, Request, Resources};
use web_engine::Viewport;
use web_engine::module_imports;

use crate::{Log, document::Page};

pub struct Site {
    requested: Url,
    resolved: Url,
    process: WebContentProcess,
    assets: Option<Assets>,
    script_shell: bool,
}

impl Site {
    pub fn open(network: &NetworkStack, requested: &Url, log: &Log) -> Result<Self, String> {
        let mut response = network.document(requested)?;
        let process = WebContentProcess::start()?;
        let mut resources = open_response(&process, &response)?;
        if resources.script_shell {
            let deadline = Instant::now() + Duration::from_secs(5);
            let mut tried_alternate = false;
            for language in [Some("en-US,en;q=0.9"), None] {
                let Ok(alternate) = network.html_document(&response.url, language, deadline) else {
                    continue;
                };
                if !matches!(
                    alternate.media_type(),
                    "text/html" | "application/xhtml+xml" | ""
                ) || alternate.bytes == response.bytes
                {
                    continue;
                }
                tried_alternate = true;
                if let Ok(candidate) = open_response(&process, &alternate)
                    && !candidate.script_shell
                {
                    response = alternate;
                    resources = candidate;
                    break;
                }
            }
            if tried_alternate && resources.script_shell {
                resources = open_response(&process, &response)?;
            }
        }
        let script_shell = resources.script_shell;
        let resolved = response.url;
        let assets = fetch_assets(network, &resolved, resources);
        log.write(format_args!(
            "site={} html_bytes={} stylesheets={} images={} script_shell={} AppContainer",
            resolved.host_str().unwrap_or(""),
            response.bytes.len(),
            assets.stylesheets.len(),
            assets.images.len(),
            script_shell,
        ));
        Ok(Self {
            requested: requested.clone(),
            resolved,
            process,
            assets: Some(assets),
            script_shell,
        })
    }

    pub fn matches(&self, url: &Url) -> bool {
        &self.requested == url || &self.resolved == url
    }

    pub fn layout(&mut self, viewport: Viewport, generation: u32) -> Result<Page, String> {
        let assets = self.assets.take();
        self.paint(Request::Layout { viewport, assets }, generation)
    }

    pub fn click(
        &mut self,
        node: u32,
        viewport: Viewport,
        generation: u32,
    ) -> Result<Page, String> {
        self.paint(Request::Click { node, viewport }, generation)
    }

    pub fn form_input(
        &mut self,
        node: u32,
        value: String,
        viewport: Viewport,
        generation: u32,
    ) -> Result<Page, String> {
        self.paint(
            Request::FormInput {
                node,
                value,
                viewport,
            },
            generation,
        )
    }

    pub fn form_toggle(
        &mut self,
        node: u32,
        viewport: Viewport,
        generation: u32,
    ) -> Result<Page, String> {
        self.paint(Request::FormToggle { node, viewport }, generation)
    }

    pub fn form_radio(
        &mut self,
        node: u32,
        viewport: Viewport,
        generation: u32,
    ) -> Result<Page, String> {
        self.paint(Request::FormRadio { node, viewport }, generation)
    }

    pub fn submit_form(&self, node: u32) -> Option<(String, String)> {
        self.process.submit_form(node)
    }

    fn paint(&mut self, request: Request, generation: u32) -> Result<Page, String> {
        let mut page = self.process.paint_page(request, generation)?;
        if self.script_shell && page.geometry.height == 0.0 {
            return Err("The page did not produce any content with the supported web APIs.".into());
        }
        page.resolved = Some(self.resolved.clone());
        Ok(page)
    }
}

impl WebContentProcess {
    pub(crate) fn open_html(&self, html: String) -> Result<Resources, String> {
        match self.call(Request::Open(html))? {
            Reply::Resources(resources) => Ok(resources),
            _ => Err("Unexpected response from the Web Content Process.".into()),
        }
    }

    pub(crate) fn paint_page(&self, request: Request, generation: u32) -> Result<Page, String> {
        let scene = self.layout(request, generation)?;
        Ok(Page {
            scene: scene.prepared,
            geometry: scene.geometry,
            resolved: None,
        })
    }

    pub fn form_input(
        &self,
        node: u32,
        value: String,
        viewport: Viewport,
        generation: u32,
    ) -> Result<Page, String> {
        self.paint_page(
            Request::FormInput {
                node,
                value,
                viewport,
            },
            generation,
        )
    }

    pub fn form_toggle(
        &self,
        node: u32,
        viewport: Viewport,
        generation: u32,
    ) -> Result<Page, String> {
        self.paint_page(Request::FormToggle { node, viewport }, generation)
    }

    pub fn form_radio(
        &self,
        node: u32,
        viewport: Viewport,
        generation: u32,
    ) -> Result<Page, String> {
        self.paint_page(Request::FormRadio { node, viewport }, generation)
    }
}

fn open_response(process: &WebContentProcess, response: &Resource) -> Result<Resources, String> {
    let html = match response.media_type() {
        "text/html" | "application/xhtml+xml" => response.text(),
        "" if response
            .bytes
            .iter()
            .copied()
            .find(|byte| !byte.is_ascii_whitespace())
            == Some(b'<') =>
        {
            response.text()
        }
        "text/plain" => {
            let text = response.text();
            // HTML consumes the first LF after the pre start tag.
            let mut html = String::from("<body><pre style=\"white-space:pre-wrap\">\n");
            for c in text.chars() {
                match c {
                    '<' => html.push_str("&lt;"),
                    '>' => html.push_str("&gt;"),
                    '&' => html.push_str("&amp;"),
                    _ => html.push(c),
                }
                if html.len() > web_engine::MAX_HTML_BYTES {
                    return Err("The text document exceeds the size limit.".into());
                }
            }
            html.push_str("</pre></body>");
            html
        }
        _ => return Err("This response is not an HTML or plain-text page.".into()),
    };
    process.open_html(html)
}

fn fetch_assets(network: &NetworkStack, document: &Url, resources: Resources) -> Assets {
    let requests: Vec<_> = resources
        .stylesheets
        .into_iter()
        .take(web_content_process::MAX_STYLESHEETS)
        .map(|reference| (reference, ResourceKind::Stylesheet))
        .chain(
            resources
                .images
                .into_iter()
                .take(web_content_process::MAX_IMAGES)
                .map(|reference| (reference, ResourceKind::Image)),
        )
        .chain(
            resources
                .scripts
                .into_iter()
                .take(web_content_process::MAX_SCRIPTS)
                .map(|reference| (reference, ResourceKind::Script)),
        )
        .collect();
    if requests.is_empty() {
        return Assets::default();
    }
    let deadline = Instant::now() + Duration::from_secs(5);
    thread::scope(|scope| {
        let lanes = requests.len().min(4);
        let mut workers = Vec::new();
        for lane in 0..lanes {
            let requests = &requests;
            workers.push(scope.spawn(move || {
                let mut assets = Assets::default();
                for (reference, kind) in requests.iter().skip(lane).step_by(lanes) {
                    if reference.len() > network_stack::MAX_URL_LENGTH || Instant::now() >= deadline
                    {
                        continue;
                    }
                    let Ok(response) = network.asset(document, reference, *kind, deadline) else {
                        continue;
                    };
                    match kind {
                        ResourceKind::Stylesheet
                            if matches!(response.media_type(), "text/css" | "") =>
                        {
                            assets
                                .stylesheets
                                .push((reference.clone(), response.text()));
                        }
                        ResourceKind::Image
                            if matches!(
                                response.media_type(),
                                "image/png"
                                    | "image/jpeg"
                                    | "image/webp"
                                    | "image/gif"
                                    | "image/svg+xml"
                                    | "application/octet-stream"
                                    | ""
                            ) =>
                        {
                            assets.images.push((reference.clone(), response.bytes));
                        }
                        ResourceKind::Script if script_media_type(response.media_type()) => {
                            assets.scripts.push((reference.clone(), response.text()));
                        }
                        _ => {}
                    }
                }
                assets
            }));
        }
        let mut loaded = Assets::default();
        for worker in workers {
            let assets = worker.join().expect("resource loader");
            loaded.stylesheets.extend(assets.stylesheets);
            loaded.images.extend(assets.images);
            loaded.scripts.extend(assets.scripts);
        }
        let mut seen: HashSet<String> = loaded.scripts.iter().map(|(key, _)| key.clone()).collect();
        for _ in 0..4 {
            let mut more = Vec::new();
            'scripts: for (reference, source) in &loaded.scripts {
                for specifier in module_imports(source) {
                    if seen.len() >= web_content_process::MAX_SCRIPTS {
                        break 'scripts;
                    }
                    if seen.contains(&specifier)
                        || !(specifier.starts_with('.') || specifier.starts_with('/'))
                    {
                        continue;
                    }
                    seen.insert(specifier.clone());
                    let resolved = resolve_import(document, reference, &specifier);
                    more.push((specifier, resolved));
                }
            }
            if more.is_empty() || Instant::now() >= deadline {
                break;
            }
            for (specifier, reference) in more {
                if reference.len() > network_stack::MAX_URL_LENGTH {
                    continue;
                }
                let Ok(response) =
                    network.asset(document, &reference, ResourceKind::Script, deadline)
                else {
                    continue;
                };
                if script_media_type(response.media_type()) {
                    loaded.scripts.push((specifier, response.text()));
                }
            }
        }
        const MAX_ASSET_BYTES: usize = 32 * 1024 * 1024;
        let mut remaining = MAX_ASSET_BYTES;
        let mut fits = |len| {
            if len > remaining {
                return false;
            }
            remaining -= len;
            true
        };
        loaded
            .stylesheets
            .retain(|(key, value)| fits(key.len() + value.len()));
        loaded
            .scripts
            .retain(|(key, value)| fits(key.len() + value.len()));
        loaded
            .images
            .retain(|(key, value)| fits(key.len() + value.len()));
        loaded
    })
}

fn script_media_type(mime: &str) -> bool {
    matches!(
        mime,
        "" | "text/javascript"
            | "application/javascript"
            | "text/ecmascript"
            | "application/ecmascript"
            | "text/typescript"
            | "application/typescript"
            | "text/plain"
            | "application/octet-stream"
            | "application/x-javascript"
    )
}

fn resolve_import(document: &Url, importer: &str, specifier: &str) -> String {
    if specifier.starts_with('/') {
        return specifier.to_owned();
    }
    let Ok(base) = document.join(importer) else {
        return specifier.to_owned();
    };
    base.join(specifier)
        .ok()
        .and_then(|target| document.make_relative(&target))
        .filter(|reference| !reference.is_empty())
        .unwrap_or_else(|| specifier.to_owned())
}
