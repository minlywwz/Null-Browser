mod css_parser;
mod dom;
mod html_parser;
mod layout_engine;
mod script;
mod streamed_html;
mod style_engine;

pub use css_parser::{BoxShadow, FontFace, ObjectFit, parse_font_faces};
pub use dom::{Document, NodeId};
pub use html_parser::{MAX_HTML_BYTES, parse_html};
pub use layout_engine::{
    ImageInfo, InputState, LaidOutPage, Link, Viewport, layout, layout_with_stylesheets,
};
pub use script::{ScriptSession, module_imports};

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ControlAction {
    Click(u32),
    Input(u32),
    Submit(u32),
    Toggle(u32),
    Radio(u32),
    MediaToggle(u32),
}

pub fn parse_control_action(href: &str) -> Option<ControlAction> {
    if let Some(id) = href.strip_prefix("ko4:click:").and_then(|v| v.parse().ok()) {
        Some(ControlAction::Click(id))
    } else if let Some(id) = href.strip_prefix("ko4:input:").and_then(|v| v.parse().ok()) {
        Some(ControlAction::Input(id))
    } else if let Some(id) = href
        .strip_prefix("ko4:submit:")
        .and_then(|v| v.parse().ok())
    {
        Some(ControlAction::Submit(id))
    } else if let Some(id) = href
        .strip_prefix("ko4:toggle:")
        .and_then(|v| v.parse().ok())
    {
        Some(ControlAction::Toggle(id))
    } else if let Some(id) = href.strip_prefix("ko4:radio:").and_then(|v| v.parse().ok()) {
        Some(ControlAction::Radio(id))
    } else {
        href.strip_prefix("ko4:media:toggle:")
            .and_then(|v| v.parse().ok())
            .map(ControlAction::MediaToggle)
    }
}

pub fn control_target(href: &str) -> Option<u32> {
    parse_control_action(href).map(|a| match a {
        ControlAction::Click(id)
        | ControlAction::Input(id)
        | ControlAction::Submit(id)
        | ControlAction::Toggle(id)
        | ControlAction::Radio(id)
        | ControlAction::MediaToggle(id) => id,
    })
}

pub fn url_encode(input: &str) -> String {
    let mut encoded = String::new();
    for byte in input.bytes() {
        match byte {
            b'a'..=b'z' | b'A'..=b'Z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                encoded.push(byte as char);
            }
            b' ' => encoded.push('+'),
            _ => {
                use std::fmt::Write;
                let _ = write!(encoded, "%{:02X}", byte);
            }
        }
    }
    encoded
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_parse_control_action_and_control_target() {
        assert_eq!(
            parse_control_action("ko4:click:12"),
            Some(ControlAction::Click(12))
        );
        assert_eq!(control_target("ko4:click:12"), Some(12));

        assert_eq!(
            parse_control_action("ko4:input:34"),
            Some(ControlAction::Input(34))
        );
        assert_eq!(control_target("ko4:input:34"), Some(34));

        assert_eq!(
            parse_control_action("ko4:submit:56"),
            Some(ControlAction::Submit(56))
        );
        assert_eq!(control_target("ko4:submit:56"), Some(56));

        assert_eq!(
            parse_control_action("ko4:toggle:78"),
            Some(ControlAction::Toggle(78))
        );
        assert_eq!(control_target("ko4:toggle:78"), Some(78));

        assert_eq!(
            parse_control_action("ko4:radio:90"),
            Some(ControlAction::Radio(90))
        );
        assert_eq!(control_target("ko4:radio:90"), Some(90));

        assert_eq!(
            parse_control_action("ko4:media:toggle:42"),
            Some(ControlAction::MediaToggle(42))
        );
        assert_eq!(control_target("ko4:media:toggle:42"), Some(42));

        assert_eq!(parse_control_action("https://example.com"), None);
        assert_eq!(control_target("https://example.com"), None);

        assert_eq!(parse_control_action("ko4:unknown:1"), None);
        assert_eq!(control_target("ko4:unknown:1"), None);
    }

    #[test]
    fn test_url_encode() {
        assert_eq!(url_encode("hello world"), "hello+world");
        assert_eq!(url_encode("query=val&num=12"), "query%3Dval%26num%3D12");
        assert_eq!(url_encode("test-a_b.c~d"), "test-a_b.c~d");
    }
}
