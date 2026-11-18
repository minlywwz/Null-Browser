use std::collections::HashMap;

use crate::dom::{Document, Kind, Node, NodeId, StreamMarker, is_inert};

const MAX_INSTRUCTIONS: usize = 256;

struct Instruction {
    target: String,
    segment: String,
    boundary: bool,
}

/// Resolve completed React Fizz segments already present in the HTML response.
/// Scripts remain inert. Only the literal completion record and its matching
/// template, comment markers and hidden segment participate in the move.
pub(crate) fn restore_streamed_html(
    document: &mut Document,
    depth_limit: usize,
) -> Result<(), String> {
    let instructions: Vec<_> = document
        .nodes
        .iter()
        .enumerate()
        .filter_map(|(id, node)| instruction(document, node).map(|instruction| (id, instruction)))
        .take(MAX_INSTRUCTIONS)
        .collect();
    if instructions.is_empty() {
        return Ok(());
    }

    let mut ids = HashMap::new();
    for (node_id, node) in document.nodes.iter().enumerate() {
        if let Some(id) = node.attr("id")
            && ['B', 'P', 'S']
                .into_iter()
                .any(|kind| stream_prefix(id, kind).is_some())
        {
            ids.entry(id.to_owned())
                .and_modify(|value| *value = None)
                .or_insert(Some(node_id));
        }
    }
    let mut changed = false;
    for (record, instruction) in instructions {
        let (Some(&Some(target)), Some(&Some(segment))) =
            (ids.get(&instruction.target), ids.get(&instruction.segment))
        else {
            continue;
        };
        if target < record && segment < record && available(document, record) {
            changed |= place_segment(document, target, segment, instruction.boundary);
        }
    }

    if changed {
        let mut pending = vec![(0, 0)];
        while let Some((id, depth)) = pending.pop() {
            if depth >= depth_limit {
                return Err("The HTML file exceeds the nesting limit.".into());
            }
            pending.extend(
                document.nodes[id]
                    .children
                    .iter()
                    .map(|child| (*child, depth + 1)),
            );
        }
    }
    Ok(())
}

fn instruction(document: &Document, node: &Node) -> Option<Instruction> {
    if node.name() == "template" {
        if node.attr("data-rci").is_some() {
            return completion(node.attr("data-bid")?, node.attr("data-sid")?, true);
        }
        if node.attr("data-rsi").is_some() {
            return completion(node.attr("data-pid")?, node.attr("data-sid")?, false);
        }
        return None;
    }
    if node.name() != "script"
        || node.attr("src").is_some()
        || node.attr("type").is_some_and(|mime| {
            !matches!(
                mime.trim(),
                "" | "text/javascript" | "application/javascript"
            )
        })
    {
        return None;
    }
    let [text] = *node.children.as_slice() else {
        return None;
    };
    let Kind::Text(script) = &document.nodes[text].kind else {
        return None;
    };
    let script = script.trim().trim_end_matches(';').trim_end();
    let (prelude, call) = script.rsplit_once(';').unwrap_or(("", script));
    let call = call.trim();
    let (arguments, boundary) = if let Some(args) = call.strip_prefix("$RC(") {
        (args, true)
    } else {
        (call.strip_prefix("$RS(")?, false)
    };
    let prelude = prelude.trim();
    if !prelude.is_empty()
        && !(prelude.ends_with('}')
            && ((boundary
                && (prelude.starts_with("$RC=function(")
                    || prelude.starts_with("$RB=[];$RV=function(")))
                || (!boundary && prelude.starts_with("$RS=function("))))
    {
        return None;
    }
    let (first, second) = arguments.strip_suffix(')')?.split_once(',')?;
    let first = quoted_id(first)?;
    let second = quoted_id(second)?;
    if boundary {
        completion(first, second, true)
    } else {
        completion(second, first, false)
    }
}

fn quoted_id(argument: &str) -> Option<&str> {
    argument.trim().strip_prefix('"')?.strip_suffix('"')
}

fn completion(target: &str, segment: &str, boundary: bool) -> Option<Instruction> {
    let target_prefix = stream_prefix(target, if boundary { 'B' } else { 'P' })?;
    (stream_prefix(segment, 'S')? == target_prefix).then(|| Instruction {
        target: target.to_owned(),
        segment: segment.to_owned(),
        boundary,
    })
}

fn stream_prefix(id: &str, kind: char) -> Option<&str> {
    let (prefix, suffix) = id.rsplit_once(':')?;
    let prefix = prefix.strip_suffix(kind)?;
    if prefix.len() > 32
        || !prefix
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_'))
        || suffix.is_empty()
        || suffix.len() > 32
        || !suffix.bytes().all(|byte| byte.is_ascii_hexdigit())
    {
        return None;
    }
    Some(prefix)
}

fn available(document: &Document, mut id: NodeId) -> bool {
    while let Some(parent) = document.nodes[id].parent {
        if is_inert(document.nodes[parent].name()) || document.nodes[parent].name() == "noscript" {
            return false;
        }
        id = parent;
    }
    id == 0
}

fn place_segment(document: &mut Document, target: NodeId, segment: NodeId, boundary: bool) -> bool {
    let target_node = &document.nodes[target];
    let segment_node = &document.nodes[segment];
    let (Some(parent), Some(old_parent)) = (target_node.parent, segment_node.parent) else {
        return false;
    };
    if target_node.name() != "template"
        || !target_node.children.is_empty()
        || !matches!(segment_node.name(), "div" | "table" | "svg" | "math")
        || segment_node.attr("hidden").is_none()
        || !available(document, target)
        || !available(document, segment)
        || std::iter::successors(Some(parent), |id| document.nodes[*id].parent)
            .any(|id| id == segment)
    {
        return false;
    }
    let siblings = &document.nodes[parent].children;
    let Some(start) = siblings.iter().position(|id| *id == target) else {
        return false;
    };
    let (end, marker) = if boundary {
        let Some(&marker) = start.checked_sub(1).and_then(|index| siblings.get(index)) else {
            return false;
        };
        if !matches!(
            document.nodes[marker].kind,
            Kind::StreamMarker(StreamMarker::Pending)
        ) {
            return false;
        }
        let Some(end) = boundary_end(document, siblings, start + 1) else {
            return false;
        };
        (end, Some(marker))
    } else {
        (start + 1, None)
    };

    let moved = std::mem::take(&mut document.nodes[segment].children);
    let removed: Vec<_> = document.nodes[parent]
        .children
        .splice(start..end, moved.iter().copied())
        .collect();
    document.nodes[old_parent]
        .children
        .retain(|id| *id != segment);
    document.nodes[segment].parent = None;
    for id in removed {
        document.nodes[id].parent = None;
    }
    for id in moved {
        document.nodes[id].parent = Some(parent);
    }
    if let Some(marker) = marker {
        document.nodes[marker].kind = Kind::StreamMarker(StreamMarker::Start);
    }
    true
}

fn boundary_end(document: &Document, siblings: &[NodeId], start: usize) -> Option<usize> {
    let mut nested = 0usize;
    for (index, id) in siblings.iter().enumerate().skip(start) {
        match document.nodes[*id].kind {
            Kind::StreamMarker(StreamMarker::Start | StreamMarker::Pending) => nested += 1,
            Kind::StreamMarker(StreamMarker::End) if nested == 0 => return Some(index),
            Kind::StreamMarker(StreamMarker::End) => nested -= 1,
            _ => {}
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use crate::parse_html;

    #[test]
    fn restores_nested_segments_in_place_and_only_discovers_attached_images() {
        let document = parse_html(
            r#"<main><!--$?--><template id="B:0"></template>
            <img src="/loading.png"><!--$--><span>Nested fallback</span><!--/$--><!--/$--></main>
            <div hidden id="S:0"><p>Ready<template id="P:1"></template></p></div>
            <div hidden id="S:1"><img src="/actual.png"></div>
            <script>$RS=function(a,b){};$RS("S:1","P:1")</script>
            <script>$RB=[];$RV=function(a){};$RC=function(a,b){};$RC("B:0","S:0")</script>
            <div hidden id="S:2"><img src="/stay-hidden.png"></div>"#,
        )
        .unwrap();
        let main = &document.nodes[document
            .tree_order()
            .find(|id| document.nodes[*id].name() == "main")
            .unwrap()];
        assert!(
            main.children
                .iter()
                .any(|id| document.nodes[*id].name() == "p")
        );
        assert_eq!(document.images().collect::<Vec<_>>(), ["/actual.png"]);
    }

    #[test]
    fn preserves_fallback_when_the_boundary_or_instruction_is_invalid() {
        let document = parse_html(r#"<main><!--$?--><template id="B:0"></template><img src="/fallback.png"><!--/$--></main>
            <div hidden id="S:0"><img src="/hidden.png"></div>
            <script type="application/json">$RC("B:0","S:0")</script>
            <script>$RC("B:0","S:0","error")</script>
            <script>something();$RC("B:0","S:0")</script>
            <template id="B:1"></template><div hidden id="S:1"><img src="/invalid.png"></div>
            <script>$RC("B:1","S:1")</script>"#).unwrap();
        assert_eq!(document.images().collect::<Vec<_>>(), ["/fallback.png"]);
    }
}
