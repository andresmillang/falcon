//! HTML parsing (F3). We use html5ever to parse documents and fragments into a
//! normalized JSON tree that the in-V8 DOM shim reconstructs into live nodes.
//! This gives spec-correct HTML parsing (implied tags, error recovery) while the
//! mutable DOM + selector engine live in JavaScript.

use html5ever::tendril::TendrilSink;
use html5ever::{parse_document, parse_fragment, ns, LocalName, QualName};
use markup5ever_rcdom::{Handle, NodeData, RcDom};
use serde_json::{json, Value};

/// Parse a full HTML document into a normalized JSON tree (root = the <html>
/// element, or a synthetic root holding top-level nodes).
pub fn parse_document_json(html: &str) -> Value {
    let dom = parse_document(RcDom::default(), Default::default())
        .from_utf8()
        .read_from(&mut html.as_bytes())
        .unwrap_or_else(|_| RcDom::default());
    node_to_json(&dom.document)
}

/// Parse an HTML fragment (for innerHTML assignment) into an array of top-level
/// normalized nodes.
pub fn parse_fragment_json(html: &str) -> Value {
    let context = QualName::new(None, ns!(html), LocalName::from("body"));
    let dom = parse_fragment(RcDom::default(), Default::default(), context, vec![], false)
        .from_utf8()
        .read_from(&mut html.as_bytes())
        .unwrap_or_else(|_| RcDom::default());
    // parse_fragment wraps children under an <html> root; unwrap to its children.
    let root = node_to_json(&dom.document);
    if let Some(inner) = root
        .get("children")
        .and_then(|c| c.as_array())
        .and_then(|ch| ch.first())
        .and_then(|f| f.get("children"))
    {
        return inner.clone();
    }
    json!([])
}

fn node_to_json(handle: &Handle) -> Value {
    match &handle.data {
        NodeData::Document => {
            json!({ "t": "root", "children": children_json(handle) })
        }
        NodeData::Element { name, attrs, .. } => {
            let mut attr_map = serde_json::Map::new();
            for a in attrs.borrow().iter() {
                attr_map.insert(a.name.local.to_string(), Value::String(a.value.to_string()));
            }
            json!({
                "t": "element",
                "tag": name.local.to_string().to_lowercase(),
                "attrs": Value::Object(attr_map),
                "children": children_json(handle),
            })
        }
        NodeData::Text { contents } => {
            json!({ "t": "text", "text": contents.borrow().to_string() })
        }
        NodeData::Comment { contents } => {
            json!({ "t": "comment", "text": contents.to_string() })
        }
        _ => json!({ "t": "skip" }),
    }
}

fn children_json(handle: &Handle) -> Value {
    let mut out = Vec::new();
    for child in handle.children.borrow().iter() {
        let v = node_to_json(child);
        if v.get("t").and_then(|t| t.as_str()) == Some("skip") {
            continue;
        }
        out.push(v);
    }
    Value::Array(out)
}

/// A script found in document order.
pub struct ScriptRef {
    pub src: Option<String>,
    pub code: String,
    pub is_module: bool,
}

/// A non-script subresource reference (img/link) for block-filtered loading (F1/F5).
pub struct SubResource {
    pub url: String,
    /// One of: "image", "font", "media", "style".
    pub kind: String,
}

/// Walk the parsed document collecting scripts (in order) and subresources.
pub fn collect_resources(html: &str) -> (Vec<ScriptRef>, Vec<SubResource>) {
    let dom = parse_document(RcDom::default(), Default::default())
        .from_utf8()
        .read_from(&mut html.as_bytes())
        .unwrap_or_else(|_| RcDom::default());
    let mut scripts = Vec::new();
    let mut subs = Vec::new();
    walk_collect(&dom.document, &mut scripts, &mut subs);
    (scripts, subs)
}

fn walk_collect(handle: &Handle, scripts: &mut Vec<ScriptRef>, subs: &mut Vec<SubResource>) {
    if let NodeData::Element { name, attrs, .. } = &handle.data {
        let tag = name.local.to_string().to_lowercase();
        let get_attr = |key: &str| -> Option<String> {
            attrs
                .borrow()
                .iter()
                .find(|a| a.name.local.to_string().eq_ignore_ascii_case(key))
                .map(|a| a.value.to_string())
        };
        match tag.as_str() {
            "script" => {
                let src = get_attr("src");
                let typ = get_attr("type").unwrap_or_default().to_lowercase();
                let is_module = typ == "module";
                let mut code = String::new();
                for child in handle.children.borrow().iter() {
                    if let NodeData::Text { contents } = &child.data {
                        code.push_str(&contents.borrow());
                    }
                }
                scripts.push(ScriptRef { src, code, is_module });
            }
            "img" => {
                if let Some(u) = get_attr("src") {
                    subs.push(SubResource { url: u, kind: "image".into() });
                }
            }
            "link" => {
                let rel = get_attr("rel").unwrap_or_default().to_lowercase();
                if let (true, Some(u)) = (rel.contains("stylesheet"), get_attr("href")) {
                    subs.push(SubResource { url: u, kind: "style".into() });
                }
            }
            _ => {}
        }
    }
    for child in handle.children.borrow().iter() {
        walk_collect(child, scripts, subs);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_document_with_implied_tags() {
        let v = parse_document_json("<title>Hi</title><p>hello");
        let s = v.to_string();
        assert!(s.contains("\"tag\":\"html\""));
        assert!(s.contains("\"tag\":\"head\""));
        assert!(s.contains("\"tag\":\"body\""));
        assert!(s.contains("hello"));
    }

    #[test]
    fn fragment_parse_returns_top_level_nodes() {
        let v = parse_fragment_json("<b>x</b><i>y</i>");
        let arr = v.as_array().expect("array");
        assert_eq!(arr.len(), 2);
        assert_eq!(arr[0]["tag"], "b");
        assert_eq!(arr[1]["tag"], "i");
    }

    #[test]
    fn collects_scripts_in_order_and_subresources() {
        let html = "<script>A</script><img src=/a.png><script src=/b.js></script><link rel=stylesheet href=/c.css>";
        let (scripts, subs) = collect_resources(html);
        assert_eq!(scripts.len(), 2);
        assert_eq!(scripts[0].code, "A");
        assert_eq!(scripts[1].src.as_deref(), Some("/b.js"));
        assert!(subs.iter().any(|s| s.kind == "image" && s.url == "/a.png"));
        assert!(subs.iter().any(|s| s.kind == "style" && s.url == "/c.css"));
    }

    #[test]
    fn module_scripts_flagged() {
        let (scripts, _) = collect_resources("<script type=module>x</script>");
        assert!(scripts[0].is_module);
    }
}
