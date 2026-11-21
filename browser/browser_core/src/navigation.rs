use std::{
    fs::File,
    io::Read,
    path::{Path, PathBuf},
};

use network_stack::Url;
use web_engine::MAX_HTML_BYTES;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Example {
    Start,
    Rendering,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Location {
    Example(Example),
    File(PathBuf),
    Web(Url),
}

impl Location {
    pub fn label(&self) -> String {
        match self {
            Self::Example(Example::Start) => "Start".into(),
            Self::Example(Example::Rendering) => "Rendering".into(),
            Self::File(path) => path
                .file_name()
                .unwrap_or(path.as_os_str())
                .to_string_lossy()
                .into_owned(),
            Self::Web(url) => url.host_str().unwrap_or(url.as_str()).to_owned(),
        }
    }

    pub fn read(&self) -> Result<String, String> {
        match self {
            Self::Example(Example::Start) => {
                Ok(include_str!("../../../testing/fixtures/index.html").into())
            }
            Self::Example(Example::Rendering) => {
                Ok(include_str!("../../../testing/fixtures/rendering.html").into())
            }
            Self::File(path) => read_local_html(path),
            Self::Web(_) => Err("A web address requires the Network Stack.".into()),
        }
    }
}

fn read_local_html(path: &Path) -> Result<String, String> {
    if path
        .to_string_lossy()
        .replace('/', "\\")
        .starts_with("\\\\")
        || !path
            .extension()
            .and_then(|extension| extension.to_str())
            .is_some_and(|ext| ext.eq_ignore_ascii_case("html") || ext.eq_ignore_ascii_case("htm"))
    {
        return Err("Choose a local .html or .htm file.".into());
    }
    let file = File::open(path).map_err(|error| format!("Could not open HTML. {error}"))?;
    let metadata = file.metadata().map_err(|error| error.to_string())?;
    if !metadata.is_file() || metadata.len() > MAX_HTML_BYTES as u64 {
        return Err("Choose an HTML file smaller than 2 MiB.".into());
    }
    let mut bytes = Vec::new();
    file.take(MAX_HTML_BYTES as u64 + 1)
        .read_to_end(&mut bytes)
        .map_err(|error| error.to_string())?;
    if bytes.len() > MAX_HTML_BYTES {
        return Err("The HTML file exceeds the 2 MiB limit.".into());
    }
    String::from_utf8(bytes).map_err(|_| "This prototype opens HTML saved as UTF-8.".into())
}

pub fn resolve_link(current: &Location, href: &str) -> Option<(Location, Option<String>)> {
    let href = href.trim();
    if let Location::Web(base) = current {
        if href.is_empty() || href.len() > network_stack::MAX_URL_LENGTH {
            return None;
        }
        let mut destination = base.join(href).ok()?;
        network_stack::validate(&destination).ok()?;
        let anchor = destination.fragment().map(str::to_owned);
        destination.set_fragment(None);
        return Some((Location::Web(destination), anchor));
    }
    let (path, fragment) = href
        .split_once('#')
        .map_or((href, None), |(path, id)| (path, Some(id.to_owned())));
    let destination = match (current, path) {
        (_, "") if fragment.is_some() => current.clone(),
        (Location::Example(_), "index.html") => Location::Example(Example::Start),
        (Location::Example(_), "rendering.html") => Location::Example(Example::Rendering),
        _ => return None,
    };
    Some((destination, fragment))
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Visit {
    Current,
    New,
    Back,
    Forward,
}

#[derive(Debug, Clone)]
pub struct History {
    entries: Vec<(Location, f32)>,
    index: usize,
}

impl Default for History {
    fn default() -> Self {
        Self::new(Location::Example(Example::Start))
    }
}

impl History {
    pub fn new(location: Location) -> Self {
        Self {
            entries: vec![(location, 0.0)],
            index: 0,
        }
    }

    pub fn can_back(&self) -> bool {
        self.index > 0
    }

    pub fn can_forward(&self) -> bool {
        self.index + 1 < self.entries.len()
    }

    pub fn current(&self) -> &Location {
        &self.entries[self.index].0
    }

    pub fn target(&self, visit: Visit) -> Option<(Location, f32)> {
        let index = match visit {
            Visit::Back => self.index.checked_sub(1)?,
            Visit::Forward => self.index + 1,
            Visit::Current | Visit::New => self.index,
        };
        self.entries.get(index).cloned()
    }

    pub fn set_scroll(&mut self, scroll: f32) {
        self.entries[self.index].1 = scroll;
    }

    pub fn commit(&mut self, visit: Visit, location: Location, scroll: f32) {
        match visit {
            Visit::Current => {}
            Visit::Back => self.index -= 1,
            Visit::Forward => self.index += 1,
            Visit::New => {
                self.entries.truncate(self.index + 1);
                self.entries.push((location, scroll));
                self.index += 1;
                if self.entries.len() > 100 {
                    self.entries.remove(0);
                    self.index -= 1;
                }
                return;
            }
        }
        self.entries[self.index] = (location, scroll);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn document_links_cannot_open_files_or_network_resources() {
        let local = Location::File(PathBuf::from("user.html"));
        for target in [
            "rendering.html",
            "../secret.html",
            "file:///C:/secret.html",
            "//server/x",
            "https://example.com",
        ] {
            assert!(resolve_link(&local, target).is_none());
        }
        assert_eq!(
            resolve_link(&local, "#anchor"),
            Some((local.clone(), Some("anchor".into())))
        );
        assert_eq!(
            resolve_link(&Location::Example(Example::Start), "rendering.html#image"),
            Some((Location::Example(Example::Rendering), Some("image".into())))
        );
    }
}
