mod css_parser;
mod dom;
mod html_parser;
mod layout_engine;
mod script;
mod streamed_html;
mod style_engine;

pub use dom::Document;
pub use html_parser::{MAX_HTML_BYTES, parse_html};
pub use layout_engine::{ImageInfo, LaidOutPage, Link, Viewport, layout, layout_with_stylesheets};
pub use script::{ScriptSession, module_imports};

pub fn control_target(href: &str) -> Option<u32> {
    href.strip_prefix("ko4:click:")
        .and_then(|value| value.parse().ok())
}
