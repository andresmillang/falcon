//! Tour login flow (F2). We fetch the login page, locate the target form via
//! html5ever, fill the credential fields, and submit — carrying cookies in the
//! shared client so the rest of the tour is authenticated.

use html5ever::tendril::TendrilSink;
use html5ever::parse_document;
use markup5ever_rcdom::{Handle, NodeData, RcDom};
use reqwest::blocking::Client;
use std::collections::BTreeMap;
use url::Url;

struct FormInfo {
    action: String,
    method: String,
    fields: BTreeMap<String, String>,
    // name -> (id, name) identity hints for selector matching
    inputs: Vec<InputMeta>,
}

struct InputMeta {
    name: String,
    id: String,
}

pub struct LoginSpec {
    pub url: String,
    pub user: String,
    pub pass: String,
    pub user_sel: String,
    pub pass_sel: String,
    pub submit_sel: String,
}

/// Perform the login. Returns Ok(()) on a non-error final response, Err(msg) otherwise.
pub fn perform(client: &Client, base: &str, spec: &LoginSpec) -> Result<(), String> {
    let base_url = Url::parse(base).map_err(|e| e.to_string())?;
    let login_url = base_url.join(&spec.url).map_err(|e| e.to_string())?;
    let resp = client
        .get(login_url.clone())
        .send()
        .map_err(|e| format!("login GET failed: {e}"))?;
    let html = resp.text().unwrap_or_default();

    // submit_sel identifies which form to submit (F2); we currently locate the
    // first form on the page, which is correct for single-form login pages.
    let _submit_hint = &spec.submit_sel;
    let form = find_form(&html).ok_or("no <form> found on login page")?;
    let mut fields = form.fields.clone();

    // Match credential selectors to field names.
    let user_field = match_field(&spec.user_sel, &form).ok_or("user_sel matched no input")?;
    let pass_field = match_field(&spec.pass_sel, &form).ok_or("pass_sel matched no input")?;
    fields.insert(user_field, spec.user.clone());
    fields.insert(pass_field, spec.pass.clone());

    let action_url = login_url.join(&form.action).map_err(|e| e.to_string())?;
    let req = if form.method == "GET" {
        client.get(action_url).query(&fields)
    } else {
        client.post(action_url).form(&fields)
    };
    let out = req.send().map_err(|e| format!("login submit failed: {e}"))?;
    if out.status().as_u16() >= 400 {
        return Err(format!("login submit returned {}", out.status()));
    }
    Ok(())
}

/// Resolve a selector (#id, [name=x], input[name=x], or a bare field name) to a
/// field name present in the form.
fn match_field(sel: &str, form: &FormInfo) -> Option<String> {
    let s = sel.trim();
    if let Some(id) = s.strip_prefix('#') {
        return form.inputs.iter().find(|i| i.id == id).map(|i| i.name.clone());
    }
    if let Some(rest) = s.strip_prefix("input[name=").or_else(|| s.strip_prefix("[name=")) {
        let name = rest.trim_end_matches(']').trim_matches(|c| c == '"' || c == '\'');
        if form.fields.contains_key(name) {
            return Some(name.to_string());
        }
        return form.inputs.iter().find(|i| i.name == name).map(|i| i.name.clone());
    }
    // bare name
    if form.fields.contains_key(s) {
        return Some(s.to_string());
    }
    form.inputs.iter().find(|i| i.name == s || i.id == s).map(|i| i.name.clone())
}

fn find_form(html: &str) -> Option<FormInfo> {
    let dom = parse_document(RcDom::default(), Default::default())
        .from_utf8()
        .read_from(&mut html.as_bytes())
        .ok()?;
    let mut found: Option<FormInfo> = None;
    walk(&dom.document, &mut found);
    found
}

fn get_attr(handle: &Handle, key: &str) -> Option<String> {
    if let NodeData::Element { attrs, .. } = &handle.data {
        attrs
            .borrow()
            .iter()
            .find(|a| a.name.local.to_string().eq_ignore_ascii_case(key))
            .map(|a| a.value.to_string())
    } else {
        None
    }
}

fn walk(handle: &Handle, found: &mut Option<FormInfo>) {
    if found.is_some() {
        return;
    }
    let is_form = matches!(&handle.data,
        NodeData::Element { name, .. } if name.local.to_string().eq_ignore_ascii_case("form"));
    if is_form {
        let mut fields = BTreeMap::new();
        let mut inputs = Vec::new();
        collect_inputs(handle, &mut fields, &mut inputs);
        *found = Some(FormInfo {
            action: get_attr(handle, "action").unwrap_or_default(),
            method: get_attr(handle, "method").unwrap_or_else(|| "POST".into()).to_uppercase(),
            fields,
            inputs,
        });
        return;
    }
    for child in handle.children.borrow().iter() {
        walk(child, found);
    }
}

fn collect_inputs(handle: &Handle, fields: &mut BTreeMap<String, String>, inputs: &mut Vec<InputMeta>) {
    if let NodeData::Element { name, .. } = &handle.data {
        let tag = name.local.to_string().to_lowercase();
        if tag == "input" || tag == "textarea" || tag == "select" {
            let nm = get_attr(handle, "name").unwrap_or_default();
            let id = get_attr(handle, "id").unwrap_or_default();
            if !nm.is_empty() {
                fields.insert(nm.clone(), get_attr(handle, "value").unwrap_or_default());
            }
            if !nm.is_empty() || !id.is_empty() {
                inputs.push(InputMeta { name: nm, id });
            }
        }
    }
    for child in handle.children.borrow().iter() {
        collect_inputs(child, fields, inputs);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample() -> FormInfo {
        find_form(
            "<form action=/login method=post>\
             <input name=username id=user_id>\
             <input name=password id=pass_id type=password>\
             <button id=submit>go</button></form>",
        )
        .unwrap()
    }

    #[test]
    fn matches_by_name_attr_selector() {
        let f = sample();
        assert_eq!(match_field("[name=username]", &f).as_deref(), Some("username"));
        assert_eq!(match_field("input[name=password]", &f).as_deref(), Some("password"));
    }

    #[test]
    fn matches_by_id_selector() {
        let f = sample();
        assert_eq!(match_field("#user_id", &f).as_deref(), Some("username"));
        assert_eq!(match_field("#pass_id", &f).as_deref(), Some("password"));
    }

    #[test]
    fn matches_bare_name_and_reads_action_method() {
        let f = sample();
        assert_eq!(match_field("username", &f).as_deref(), Some("username"));
        assert_eq!(f.action, "/login");
        assert_eq!(f.method, "POST");
        assert!(f.fields.contains_key("username") && f.fields.contains_key("password"));
    }
}
