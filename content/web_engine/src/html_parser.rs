use crate::{
    dom::{Document, Kind, Node, NodeId, StreamMarker, is_inert},
    streamed_html::restore_streamed_html,
};

pub const MAX_HTML_BYTES: usize = 2 * 1024 * 1024;
const MAX_NODES: usize = 10_000;
const MAX_DEPTH: usize = 128;
const MAX_ATTRIBUTES: usize = 64;

enum Token {
    Start(String, Vec<(String, String)>),
    End(String),
    Text(String),
    StreamMarker(StreamMarker),
}

struct Tokenizer<'a> {
    input: &'a str,
    at: usize,
    raw: Option<String>,
}

impl<'a> Tokenizer<'a> {
    fn remaining(&self) -> &'a str {
        &self.input[self.at..]
    }

    fn skip_space(&mut self) {
        while self
            .input
            .as_bytes()
            .get(self.at)
            .is_some_and(|byte| byte.is_ascii_whitespace())
        {
            self.at += 1;
        }
    }

    fn name(&mut self) -> String {
        let start = self.at;
        while self.input.as_bytes().get(self.at).is_some_and(|byte| {
            byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_' | b':' | b'.')
        }) {
            self.at += 1;
        }
        self.input[start..self.at].to_ascii_lowercase()
    }

    fn skip_until(&mut self, delimiter: &str) {
        self.at += self
            .remaining()
            .find(delimiter)
            .map_or(self.remaining().len(), |at| at + delimiter.len());
    }

    fn raw_text(&mut self, name: &str) -> Option<Token> {
        let remaining = self.remaining();
        let bytes = remaining.as_bytes();
        let end = remaining
            .match_indices("</")
            .find_map(|(at, _)| {
                let start = at + 2;
                let end = start + name.len();
                (bytes
                    .get(start..end)
                    .is_some_and(|tag| tag.eq_ignore_ascii_case(name.as_bytes()))
                    && bytes.get(end).is_some_and(|byte| {
                        byte.is_ascii_whitespace() || matches!(byte, b'>' | b'/')
                    }))
                .then_some(at)
            })
            .unwrap_or(remaining.len());
        self.at += end;
        if end == 0 {
            return None;
        }
        let text = &remaining[..end];
        Some(Token::Text(if matches!(name, "title" | "textarea") {
            decode_entities(text)
        } else {
            text.to_owned()
        }))
    }

    fn next(&mut self) -> Result<Option<Token>, String> {
        if let Some(name) = self.raw.take()
            && let Some(token) = self.raw_text(&name)
        {
            return Ok(Some(token));
        }
        while self.at < self.input.len() {
            let remaining = self.remaining();
            if let Some(comment) = remaining.strip_prefix("<!--") {
                if let Some(end) = comment.find("-->") {
                    self.at += 4 + end + 3;
                    let marker = match &comment[..end] {
                        "$?" | "$~" => StreamMarker::Pending,
                        "$" | "$!" => StreamMarker::Start,
                        "/$" => StreamMarker::End,
                        _ => continue,
                    };
                    return Ok(Some(Token::StreamMarker(marker)));
                }
                self.at = self.input.len();
                continue;
            }
            if remaining.starts_with("<!") || remaining.starts_with("<?") {
                self.skip_until(">");
                continue;
            }
            if remaining.starts_with("</")
                && remaining
                    .as_bytes()
                    .get(2)
                    .is_some_and(u8::is_ascii_alphabetic)
            {
                self.at += 2;
                let name = self.name();
                self.skip_until(">");
                return Ok(Some(Token::End(name)));
            }
            if remaining.starts_with('<')
                && remaining
                    .as_bytes()
                    .get(1)
                    .is_some_and(u8::is_ascii_alphabetic)
            {
                self.at += 1;
                let name = self.name();
                let mut attributes = Vec::new();
                loop {
                    self.skip_space();
                    match self.input.as_bytes().get(self.at) {
                        Some(b'>') => {
                            self.at += 1;
                            break;
                        }
                        None => return Ok(None),
                        _ => {}
                    }
                    let key = self.name();
                    if key.is_empty() {
                        let length = self.remaining().chars().next().map_or(0, char::len_utf8);
                        self.at += length;
                        continue;
                    }
                    self.skip_space();
                    let mut value = "";
                    if self.remaining().starts_with('=') {
                        self.at += 1;
                        self.skip_space();
                        let remainder = self.remaining();
                        let length = if let Some(quote) =
                            remainder.chars().next().filter(|c| matches!(c, '\'' | '"'))
                        {
                            let unquoted = &remainder[1..];
                            let end = unquoted.find(quote).unwrap_or(unquoted.len());
                            value = &unquoted[..end];
                            1 + end + usize::from(end < unquoted.len())
                        } else {
                            let end = remainder
                                .find(|c: char| c.is_ascii_whitespace() || c == '>')
                                .unwrap_or(remainder.len());
                            value = &remainder[..end];
                            end
                        };
                        self.at += length;
                    }
                    if attributes.len() >= MAX_ATTRIBUTES {
                        return Err("An HTML element has too many attributes.".into());
                    }
                    if !attributes.iter().any(|(existing, _)| existing == &key) {
                        attributes.push((key, decode_entities(value)));
                    }
                }
                if matches!(name.as_str(), "script" | "style" | "title" | "textarea") {
                    self.raw = Some(name.clone());
                }
                return Ok(Some(Token::Start(name, attributes)));
            }
            let end = remaining[usize::from(remaining.starts_with('<'))..]
                .find('<')
                .map_or(remaining.len(), |at| {
                    at + usize::from(remaining.starts_with('<'))
                });
            self.at += end;
            return Ok(Some(Token::Text(decode_entities(&remaining[..end]))));
        }
        Ok(None)
    }
}

fn decode_entities(value: &str) -> String {
    let mut rest = value;
    let mut result = String::with_capacity(value.len());
    while let Some(at) = rest.find('&') {
        result.push_str(&rest[..at]);
        rest = &rest[at + 1..];
        let semicolon = rest
            .as_bytes()
            .iter()
            .take(16)
            .position(|byte| *byte == b';');
        if let Some(end) = semicolon
            && rest.is_char_boundary(end)
            && let Some(character) = decode_entity(&rest[..end])
        {
            result.push(character);
            rest = &rest[end + 1..];
        } else {
            result.push('&');
        }
    }
    result.push_str(rest);
    result
}

fn decode_entity(entity: &str) -> Option<char> {
    Some(match entity {
        "amp" => '&',
        "lt" => '<',
        "gt" => '>',
        "quot" => '"',
        "apos" => '\'',
        "nbsp" => '\u{00a0}',
        "mdash" => '—',
        "ndash" => '–',
        "copy" => '©',
        "reg" => '®',
        "trade" => '™',
        "hellip" => '…',
        "bull" => '•',
        "middot" => '·',
        "lsquo" => '‘',
        "rsquo" => '’',
        "ldquo" => '“',
        "rdquo" => '”',
        "euro" => '€',
        "pound" => '£',
        "times" => '×',
        "divide" => '÷',
        _ => {
            let digits = entity.strip_prefix('#')?;
            let code = if let Some(hex) = digits.strip_prefix(['x', 'X']) {
                u32::from_str_radix(hex, 16).ok()?
            } else {
                digits.parse().ok()?
            };
            char::from_u32(code)
                .filter(|c| *c != '\0')
                .unwrap_or('\u{fffd}')
        }
    })
}

fn is_void(name: &str) -> bool {
    matches!(
        name,
        "area"
            | "base"
            | "br"
            | "col"
            | "embed"
            | "hr"
            | "img"
            | "input"
            | "link"
            | "meta"
            | "param"
            | "source"
            | "track"
            | "wbr"
    )
}

fn close_open_element(document: &Document, stack: &mut Vec<NodeId>, name: &str) {
    if let Some(index) = stack
        .iter()
        .rposition(|id| document.nodes[*id].name() == name)
    {
        stack.truncate(index);
    }
}

fn close_in_scope(
    document: &Document,
    stack: &mut Vec<NodeId>,
    names: &[&str],
    boundaries: &[&str],
) {
    for (index, &id) in stack.iter().enumerate().rev() {
        let tag = document.nodes[id].name();
        if names.contains(&tag) {
            stack.truncate(index);
            break;
        }
        if boundaries.contains(&tag) {
            break;
        }
    }
}

pub fn parse_html(input: &str) -> Result<Document, String> {
    if input.len() > MAX_HTML_BYTES {
        return Err("The HTML file exceeds the 2 MiB limit.".into());
    }
    let mut tokenizer = Tokenizer {
        input: input.trim_start_matches('\u{feff}'),
        at: 0,
        raw: None,
    };
    let mut document = Document {
        nodes: vec![Node {
            kind: Kind::Root,
            children: Vec::new(),
            parent: None,
        }],
        stylesheets: Vec::new(),
        title: String::new(),
        scripting: false,
        clickable: Default::default(),
    };
    let mut stack = vec![0];
    while let Some(token) = tokenizer.next()? {
        if document.nodes.len() >= MAX_NODES {
            return Err("The HTML file exceeds the node limit.".into());
        }
        let id = document.nodes.len();
        let kind = match token {
            Token::StreamMarker(marker) => Kind::StreamMarker(marker),
            Token::End(name) => {
                close_open_element(&document, &mut stack, &name);
                continue;
            }
            Token::Start(name, attributes) => {
                if matches!(
                    name.as_str(),
                    "p" | "div"
                        | "h1"
                        | "h2"
                        | "h3"
                        | "h4"
                        | "h5"
                        | "h6"
                        | "section"
                        | "main"
                        | "ul"
                        | "ol"
                        | "blockquote"
                        | "pre"
                        | "hr"
                        | "table"
                        | "form"
                        | "li"
                ) {
                    close_in_scope(
                        &document,
                        &mut stack,
                        &["p"],
                        &["table", "td", "th", "template"],
                    );
                }
                match name.as_str() {
                    "body" => close_open_element(&document, &mut stack, "head"),
                    "li" => {
                        close_in_scope(&document, &mut stack, &["li"], &["ol", "ul", "template"])
                    }
                    "dt" | "dd" => {
                        close_in_scope(&document, &mut stack, &["dt", "dd"], &["dl", "template"])
                    }
                    "option" => close_in_scope(
                        &document,
                        &mut stack,
                        &["option"],
                        &["select", "datalist", "template"],
                    ),
                    "td" | "th" => close_in_scope(
                        &document,
                        &mut stack,
                        &["td", "th"],
                        &["tr", "table", "template"],
                    ),
                    "tr" => close_in_scope(&document, &mut stack, &["tr"], &["table", "template"]),
                    "thead" | "tbody" | "tfoot" => {
                        close_in_scope(
                            &document,
                            &mut stack,
                            &["thead", "tbody", "tfoot"],
                            &["table", "template"],
                        );
                        close_in_scope(&document, &mut stack, &["tr"], &["table", "template"]);
                    }
                    _ => {}
                }
                let stylesheet = name == "style"
                    || (name == "link"
                        && attributes.iter().any(|(key, value)| {
                            key == "rel"
                                && value
                                    .split_ascii_whitespace()
                                    .any(|item| item.eq_ignore_ascii_case("stylesheet"))
                                && !value
                                    .split_ascii_whitespace()
                                    .any(|item| item.eq_ignore_ascii_case("alternate"))
                        })
                        && !attributes.iter().any(|(key, value)| {
                            key == "disabled"
                                || (key == "media"
                                    && !matches!(
                                        value.trim().to_ascii_lowercase().as_str(),
                                        "" | "screen" | "all"
                                    ))
                        }));
                if stylesheet && !stack.iter().any(|id| is_inert(document.nodes[*id].name())) {
                    document.stylesheets.push(id);
                }
                Kind::Element { name, attributes }
            }
            Token::Text(mut text) => {
                let parent = &document.nodes[*stack.last().unwrap()];
                if parent.children.is_empty() && matches!(parent.name(), "pre" | "textarea") {
                    let skip = if text.starts_with("\r\n") {
                        2
                    } else {
                        usize::from(text.starts_with(['\n', '\r']))
                    };
                    text.drain(..skip);
                }
                if document.nodes[*stack.last().unwrap()].name() == "title"
                    && document.title.is_empty()
                {
                    document.title = text
                        .split_whitespace()
                        .collect::<Vec<_>>()
                        .join(" ")
                        .chars()
                        .take(160)
                        .collect();
                }
                Kind::Text(text)
            }
        };
        let parent = *stack.last().unwrap();
        document.nodes[parent].children.push(id);
        let open = matches!(&kind, Kind::Element { name, .. } if !is_void(name));
        document.nodes.push(Node {
            kind,
            children: Vec::new(),
            parent: Some(parent),
        });
        if open {
            if stack.len() >= MAX_DEPTH {
                return Err("The HTML file exceeds the nesting limit.".into());
            }
            stack.push(id);
        }
    }
    restore_streamed_html(&mut document, MAX_DEPTH)?;
    Ok(document)
}

pub(crate) fn replace_children(
    document: &mut Document,
    parent: NodeId,
    html: &str,
) -> Result<(), String> {
    if document.nodes.get(parent).is_none() {
        return Err("The element no longer exists.".into());
    }
    if html.len() > MAX_HTML_BYTES {
        return Err("The HTML fragment exceeds the size limit.".into());
    }
    document.clear_children(parent);
    if html.trim().is_empty() {
        return Ok(());
    }
    let wrapped = format!("<div id=\"__ko4_fragment\">{html}</div>");
    let parsed = parse_html(&wrapped)?;
    let host = parsed
        .element_by_id("__ko4_fragment")
        .ok_or("Could not parse that HTML fragment.")?;
    copy_children(document, &parsed, host, parent)
}

fn copy_children(
    document: &mut Document,
    source: &Document,
    source_parent: NodeId,
    parent: NodeId,
) -> Result<(), String> {
    for child in source.nodes[source_parent].children.clone() {
        copy_node(document, source, child, parent)?;
    }
    Ok(())
}

fn copy_node(
    document: &mut Document,
    source: &Document,
    id: NodeId,
    parent: NodeId,
) -> Result<(), String> {
    if document.nodes.len() >= MAX_NODES {
        return Err("The document exceeds the node limit.".into());
    }
    let kind = match &source.nodes[id].kind {
        Kind::Element { name, attributes } => Kind::Element {
            name: name.clone(),
            attributes: attributes.clone(),
        },
        Kind::Text(text) => Kind::Text(text.clone()),
        Kind::StreamMarker(marker) => Kind::StreamMarker(*marker),
        Kind::Root => return Ok(()),
    };
    let new_id = document.nodes.len();
    document.nodes[parent].children.push(new_id);
    let stylesheet = matches!(&kind, Kind::Element { name, .. } if name == "style")
        || matches!(&kind, Kind::Element { name, attributes } if name == "link" && attributes.iter().any(|(key, value)| key == "rel" && value.split_ascii_whitespace().any(|item| item.eq_ignore_ascii_case("stylesheet"))));
    document.nodes.push(Node {
        kind,
        children: Vec::new(),
        parent: Some(parent),
    });
    if stylesheet {
        document.stylesheets.push(new_id);
    }
    for child in source.nodes[id].children.clone() {
        copy_node(document, source, child, new_id)?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn handles_raw_text_unicode_and_paragraph_recovery() {
        let page = parse_html("<title>Пример &amp; Test</title><!--ignore--><p>A<p>B<script>if (a < b) { x = '</scripted>'; }</script>").unwrap();
        assert_eq!(page.title(), "Пример & Test");
        let root = &page.nodes[0];
        assert_eq!(
            root.children
                .iter()
                .map(|id| page.nodes[*id].name())
                .collect::<Vec<_>>(),
            ["title", "p", "p"]
        );
        assert!(
            matches!(&page.nodes.last().unwrap().kind, Kind::Text(raw) if raw.contains("</scripted>"))
        );
    }

    #[test]
    fn rejects_deep_documents_without_recursing() {
        assert!(parse_html(&"<div>".repeat(MAX_DEPTH)).is_err());
    }
}
