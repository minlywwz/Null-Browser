use std::collections::HashSet;

pub(crate) type NodeId = usize;

#[derive(Clone, Copy, PartialEq, Eq)]
pub(crate) enum StreamMarker {
    Pending,
    Start,
    End,
}

pub(crate) enum Kind {
    Root,
    Element {
        name: String,
        attributes: Vec<(String, String)>,
    },
    Text(String),
    StreamMarker(StreamMarker),
}

pub(crate) struct Node {
    pub(crate) kind: Kind,
    pub(crate) children: Vec<NodeId>,
    pub(crate) parent: Option<NodeId>,
}

impl Node {
    pub(crate) fn name(&self) -> &str {
        match &self.kind {
            Kind::Element { name, .. } => name,
            _ => "",
        }
    }

    pub(crate) fn attr(&self, key: &str) -> Option<&str> {
        match &self.kind {
            Kind::Element { attributes, .. } => attributes
                .iter()
                .find(|(name, _)| name == key)
                .map(|(_, value)| value.as_str()),
            _ => None,
        }
    }
}

pub struct Document {
    pub(crate) nodes: Vec<Node>,
    pub(crate) stylesheets: Vec<NodeId>,
    pub(crate) title: String,
    pub(crate) scripting: bool,
    pub(crate) clickable: HashSet<NodeId>,
}

impl Document {
    pub fn title(&self) -> &str {
        &self.title
    }

    pub fn external_stylesheets(&self) -> impl Iterator<Item = &str> {
        self.stylesheet_nodes().filter_map(|id| {
            let node = &self.nodes[id];
            (node.name() == "link").then(|| node.attr("href")).flatten()
        })
    }

    pub fn images(&self) -> impl Iterator<Item = &str> {
        self.tree_order().filter_map(|id| {
            let node = &self.nodes[id];
            if node.name() != "img" || node.attr("hidden").is_some() {
                return None;
            }
            let mut parent = node.parent;
            while let Some(id) = parent {
                let ancestor = &self.nodes[id];
                if is_inert(ancestor.name())
                    || ancestor.name() == "head"
                    || ancestor.attr("hidden").is_some()
                {
                    return None;
                }
                parent = ancestor.parent;
            }
            node.attr("src")
        })
    }

    pub fn external_scripts(&self) -> Vec<String> {
        self.script_blocks()
            .into_iter()
            .filter_map(|script| script.src)
            .collect()
    }

    pub(crate) fn script_blocks(&self) -> Vec<ScriptBlock> {
        self.tree_order()
            .filter_map(|id| script_block(self, id))
            .collect()
    }

    pub(crate) fn element_by_id(&self, id: &str) -> Option<NodeId> {
        let id = id.trim();
        if id.is_empty() || id.len() > 256 {
            return None;
        }
        self.tree_order()
            .find(|node| self.nodes[*node].attr("id") == Some(id))
    }

    pub(crate) fn query(&self, scope: Option<NodeId>, selector: &str, all: bool) -> Vec<NodeId> {
        let selector = selector.trim();
        if selector.is_empty() || selector.len() > 512 {
            return Vec::new();
        }
        let mut found = Vec::new();
        for id in self.tree_order() {
            if id == 0 || scope.is_some_and(|scope| scope == id || !descendant(self, scope, id)) {
                continue;
            }
            if crate::style_engine::node_matches(self, id, selector) {
                found.push(id);
                if !all || found.len() == 256 {
                    break;
                }
            }
        }
        found
    }

    pub(crate) fn text_of(&self, id: NodeId) -> String {
        let mut text = String::new();
        let mut pending = vec![id];
        while let Some(id) = pending.pop() {
            if text.len() > 65_536 {
                break;
            }
            match &self.nodes[id].kind {
                Kind::Text(value) => text.push_str(value),
                Kind::Element { .. } if !matches!(self.nodes[id].name(), "script" | "style") => {
                    pending.extend(self.nodes[id].children.iter().rev().copied());
                }
                _ => {}
            }
        }
        text
    }

    pub(crate) fn set_title(&mut self, title: &str) {
        self.title = title.split_whitespace().collect::<Vec<_>>().join(" ");
        self.title.truncate(160);
        let title_node = self
            .tree_order()
            .find(|id| self.nodes[*id].name() == "title");
        if let Some(id) = title_node {
            let title = self.title.clone();
            self.set_text(id, &title);
        }
    }

    pub(crate) fn ensure_body(&mut self) -> NodeId {
        if let Some(id) = self
            .tree_order()
            .find(|id| self.nodes[*id].name() == "body")
        {
            return id;
        }
        self.create_element(Some(0), "body")
    }

    pub(crate) fn create_element(&mut self, parent: Option<NodeId>, name: &str) -> NodeId {
        let name = name
            .trim()
            .to_ascii_lowercase()
            .chars()
            .filter(|character| character.is_ascii_alphanumeric() || *character == '-')
            .take(32)
            .collect::<String>();
        let name = if name.is_empty() { "div".into() } else { name };
        self.insert(
            parent,
            Kind::Element {
                name,
                attributes: Vec::new(),
            },
        )
    }

    pub(crate) fn create_text(&mut self, parent: NodeId, text: &str) -> NodeId {
        self.insert(
            Some(parent),
            Kind::Text(text.chars().take(65_536).collect()),
        )
    }

    pub(crate) fn set_text(&mut self, id: NodeId, text: &str) {
        if self.nodes.get(id).is_none() {
            return;
        }
        self.detach_children(id);
        if !text.is_empty() {
            self.create_text(id, text);
        }
    }

    pub(crate) fn set_attribute(&mut self, id: NodeId, name: &str, value: &str) -> bool {
        let name = name.trim().to_ascii_lowercase();
        if name.is_empty() || name.len() > 64 || value.len() > 8_192 {
            return false;
        }
        let Kind::Element { attributes, .. } = &mut self.nodes[id].kind else {
            return false;
        };
        if let Some((_, existing)) = attributes.iter_mut().find(|(key, _)| key == &name) {
            *existing = value.to_owned();
            return true;
        }
        if attributes.len() >= 64 {
            return false;
        }
        attributes.push((name, value.to_owned()));
        true
    }

    pub(crate) fn remove_attribute(&mut self, id: NodeId, name: &str) {
        let name = name.trim().to_ascii_lowercase();
        if let Kind::Element { attributes, .. } = &mut self.nodes[id].kind {
            attributes.retain(|(key, _)| key != &name);
        }
    }

    pub(crate) fn class_list(&mut self, id: NodeId, action: &str, name: &str) -> bool {
        let name = name.trim();
        if name.is_empty()
            || name.len() > 128
            || name.split_ascii_whitespace().nth(1).is_some()
            || !matches!(
                &self.nodes.get(id).map(|node| &node.kind),
                Some(Kind::Element { .. })
            )
        {
            return false;
        }
        let mut names: Vec<&str> = self.nodes[id]
            .attr("class")
            .unwrap_or("")
            .split_ascii_whitespace()
            .collect();
        let contains = names.contains(&name);
        match action {
            "contains" => return contains,
            "add" if !contains => names.push(name),
            "remove" => names.retain(|item| *item != name),
            "toggle" if contains => names.retain(|item| *item != name),
            "toggle" => names.push(name),
            _ => return contains,
        }
        let value = names.join(" ");
        self.set_attribute(id, "class", &value);
        !contains
    }

    pub(crate) fn append_child(&mut self, parent: NodeId, child: NodeId) -> Result<(), String> {
        if parent == child || descendant(self, child, parent) || self.nodes.get(child).is_none() {
            return Err("That node cannot be inserted there.".into());
        }
        self.detach(child);
        self.nodes[child].parent = Some(parent);
        self.nodes[parent].children.push(child);
        Ok(())
    }

    pub(crate) fn remove_node(&mut self, id: NodeId) {
        self.detach(id);
    }

    pub(crate) fn clear_children(&mut self, id: NodeId) {
        if self.nodes.get(id).is_some() {
            self.detach_children(id);
        }
    }

    pub(crate) fn element_children(&self, id: NodeId) -> Vec<NodeId> {
        self.nodes
            .get(id)
            .map(|node| {
                node.children
                    .iter()
                    .copied()
                    .filter(|child| matches!(self.nodes[*child].kind, Kind::Element { .. }))
                    .collect()
            })
            .unwrap_or_default()
    }

    fn insert(&mut self, parent: Option<NodeId>, kind: Kind) -> NodeId {
        let id = self.nodes.len();
        if let Some(parent) = parent {
            self.nodes[parent].children.push(id);
        }
        let stylesheet = match &kind {
            Kind::Element { name, attributes } if name == "style" => true,
            Kind::Element { name, attributes } if name == "link" => {
                attributes.iter().any(|(key, value)| {
                    key == "rel"
                        && value
                            .split_ascii_whitespace()
                            .any(|item| item.eq_ignore_ascii_case("stylesheet"))
                })
            }
            _ => false,
        };
        self.nodes.push(Node {
            kind,
            children: Vec::new(),
            parent,
        });
        if stylesheet {
            self.stylesheets.push(id);
        }
        id
    }

    fn detach(&mut self, id: NodeId) {
        if let Some(parent) = self.nodes[id].parent {
            self.nodes[parent].children.retain(|child| *child != id);
            self.nodes[id].parent = None;
        }
    }

    fn detach_children(&mut self, id: NodeId) {
        let mut pending = std::mem::take(&mut self.nodes[id].children);
        let mut dropped = Vec::new();
        while let Some(child) = pending.pop() {
            dropped.push(child);
            pending.extend(self.nodes[child].children.iter().copied());
            self.nodes[child].parent = None;
            self.clickable.remove(&child);
        }
        self.stylesheets.retain(|style| !dropped.contains(style));
    }

    pub fn is_script_shell(&self) -> bool {
        let has_script = self
            .script_blocks()
            .into_iter()
            .any(|script| script.src.is_some() || !script.code.trim().is_empty());
        has_script
            && !self.tree_order().any(|id| {
                let node = &self.nodes[id];
                if node.name() != "img"
                    && !matches!(&node.kind, Kind::Text(text) if !text.trim().is_empty())
                {
                    return false;
                }
                std::iter::successors(Some(id), |id| self.nodes[*id].parent).all(|id| {
                    let node = &self.nodes[id];
                    node.attr("hidden").is_none()
                        && !is_inert(node.name())
                        && !matches!(
                            node.name(),
                            "head" | "title" | "style" | "script" | "noscript" | "svg"
                        )
                })
            })
    }

    pub(crate) fn tree_order(&self) -> impl Iterator<Item = NodeId> + '_ {
        let mut pending = vec![0];
        std::iter::from_fn(move || {
            let id = pending.pop()?;
            pending.extend(self.nodes[id].children.iter().rev().copied());
            Some(id)
        })
    }

    pub(crate) fn stylesheet_nodes(&self) -> impl Iterator<Item = NodeId> + '_ {
        self.tree_order()
            .filter(|id| self.stylesheets.binary_search(id).is_ok())
    }
}

pub(crate) struct ScriptBlock {
    pub(crate) id: NodeId,
    pub(crate) src: Option<String>,
    pub(crate) code: String,
    pub(crate) module: bool,
    pub(crate) typescript: bool,
}

fn script_block(document: &Document, id: NodeId) -> Option<ScriptBlock> {
    let node = &document.nodes[id];
    if node.name() != "script" || ancestor_inert(document, id) {
        return None;
    }
    let kind = node.attr("type").unwrap_or("").trim().to_ascii_lowercase();
    if matches!(
        kind.as_str(),
        "application/json"
            | "importmap"
            | "speculationrules"
            | "text/html"
            | "text/template"
            | "application/ld+json"
    ) || kind.ends_with("+json")
    {
        return None;
    }
    let src = node
        .attr("src")
        .map(str::trim)
        .filter(|src| !src.is_empty());
    let typescript = kind.contains("typescript")
        || kind == "ts"
        || src.is_some_and(|src| {
            let path = src.split(['?', '#']).next().unwrap_or(src);
            matches!(
                path.rsplit('.').next().unwrap_or(""),
                "ts" | "tsx" | "mts" | "cts"
            )
        });
    let module = kind == "module"
        || kind.starts_with("module")
        || src.is_some_and(|src| {
            src.split(['?', '#'])
                .next()
                .unwrap_or(src)
                .ends_with(".mjs")
        });
    let executable = kind.is_empty()
        || module
        || typescript
        || kind.contains("javascript")
        || kind.contains("ecmascript")
        || kind == "ts";
    if !executable {
        return None;
    }
    let mut code = String::new();
    for child in &node.children {
        if let Kind::Text(text) = &document.nodes[*child].kind {
            code.push_str(text);
        }
    }
    Some(ScriptBlock {
        id,
        src: src.map(str::to_owned),
        code,
        module,
        typescript,
    })
}

fn ancestor_inert(document: &Document, id: NodeId) -> bool {
    std::iter::successors(document.nodes[id].parent, |id| document.nodes[*id].parent)
        .any(|id| is_inert(document.nodes[id].name()))
}

fn descendant(document: &Document, ancestor: NodeId, node: NodeId) -> bool {
    std::iter::successors(Some(node), |id| document.nodes[*id].parent).any(|id| id == ancestor)
}

pub(crate) fn is_inert(tag: &str) -> bool {
    matches!(
        tag,
        "template" | "iframe" | "object" | "embed" | "video" | "audio"
    )
}
