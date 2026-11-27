//! Open extensions (`openTypeExtension`) on messages: an app's own untyped data on a message,
//! which Outlook keeps on the item and Gmail has no place for, so they live only here.
//!
//! An extension is named once, at its creation, and Outlook gives it the fully qualified id
//! `Microsoft.OutlookServices.OpenTypeExtension.{name}`. Requests may name it either way, and
//! filters match either. Extensions belong to the Outlook item, so they stay with a message
//! through moves, draft edits and sending, and a copy gets copies of them.

use std::collections::HashMap;

use axum::extract::Path;
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use serde_json::{Map, Value};

use super::error::{GraphError, Result};
use super::filter::{Expr, Literal, Op};
use super::folders::{body_json, resolve};
use super::messages::{entity, key_of};
use super::project::View;
use super::{Call, odata, origin, reply, reply_map};

/// The prefix of an extension's fully qualified id on an Outlook item.
const QUALIFIED: &str = "Microsoft.OutlookServices.OpenTypeExtension.";

/// The other prefix Graph's documents write a message's extension ids with.
const QUALIFIED_GRAPH: &str = "microsoft.graph.openTypeExtension.";

/// The `@odata.type` values that name an open extension.
const TYPES: &[&str] = &[
    "microsoft.graph.openTypeExtension",
    "Microsoft.OutlookServices.OpenTypeExtension",
];

/// An open extension on a message.
#[derive(Debug, Clone, PartialEq)]
pub struct OpenExtension {
    /// The name it was created with, which its id carries for good.
    pub id: String,
    /// `extensionName`, which an update may change or drop.
    pub name: Option<String>,
    /// The app's own properties, in the order they were written.
    pub data: Map<String, Value>,
}

impl OpenExtension {
    /// The extension as Graph renders it.
    pub fn json(&self) -> Map<String, Value> {
        let mut json = Map::new();
        json.insert(
            "@odata.type".into(),
            "#microsoft.graph.openTypeExtension".into(),
        );
        if let Some(name) = &self.name {
            json.insert("extensionName".into(), name.clone().into());
        }
        json.insert("id".into(), format!("{QUALIFIED}{}", self.id).into());
        json.extend(self.data.clone());
        json
    }

    /// Whether `wanted` (a name, or a fully qualified id) names this extension.
    pub fn answers(&self, wanted: &str) -> bool {
        let wanted = short(wanted);
        wanted.eq_ignore_ascii_case(&self.id)
            || self
                .name
                .as_deref()
                .is_some_and(|name| wanted.eq_ignore_ascii_case(name))
    }

    /// What a `$filter` on extensions sees of this one: its id by name and fully qualified, so
    /// `id eq` matches either.
    fn filter_forms(&self) -> [Value; 2] {
        let form = |id: String| Value::Object(Map::from_iter([("id".to_owned(), id.into())]));
        [
            form(self.id.clone()),
            form(format!("{QUALIFIED}{}", self.id)),
        ]
    }
}

/// A name without either fully qualified prefix.
fn short(name: &str) -> &str {
    for prefix in [QUALIFIED, QUALIFIED_GRAPH] {
        if name.len() > prefix.len() && name[..prefix.len()].eq_ignore_ascii_case(prefix) {
            return &name[prefix.len()..];
        }
    }
    name
}

/// A name Outlook takes: a reverse domain name (at least two dot-separated labels of letters,
/// digits, `-` and `_`) outside Microsoft's own `com.microsoft` and `com.onmicrosoft`.
fn check_name(name: &str) -> Result<()> {
    let labels: Vec<&str> = name.split('.').collect();
    let well_formed = labels.len() >= 2
        && labels.iter().all(|label| {
            !label.is_empty()
                && label
                    .chars()
                    .all(|ch| ch.is_ascii_alphanumeric() || matches!(ch, '-' | '_'))
        });
    let reserved = labels.len() >= 2
        && labels[0].eq_ignore_ascii_case("com")
        && (labels[1].eq_ignore_ascii_case("microsoft")
            || labels[1].eq_ignore_ascii_case("onmicrosoft"));
    if !well_formed || reserved {
        return Err(GraphError::invalid_request(format!(
            "The extension name '{name}' isn't valid. Use a reverse domain name of your own, not com.microsoft or com.onmicrosoft."
        )));
    }
    Ok(())
}

/// The app's own properties from a body: everything but the extension's own and the OData
/// annotations, each a primitive or an array of primitives, as the documents allow. `nulls`
/// says whether a property may be null (an update's may not).
fn data_of(body: &Map<String, Value>, nulls: bool) -> Result<Map<String, Value>> {
    let mut data = Map::new();
    for (key, value) in body {
        if key.starts_with('@') || key == "extensionName" || key == "id" {
            continue;
        }
        let primitive = |value: &Value| !matches!(value, Value::Object(_) | Value::Array(_));
        let allowed = match value {
            Value::Null => nulls,
            Value::Array(items) => items.iter().all(|item| primitive(item) && !item.is_null()),
            other => primitive(other),
        };
        if !allowed {
            return Err(GraphError::unreadable_body(format!(
                "The property '{key}' must be a primitive value or an array of them{}.",
                if nulls { "" } else { ", and not null" }
            )));
        }
        data.insert(key.clone(), value.clone());
    }
    Ok(data)
}

/// Checks a body's `@odata.type` names an open extension: required on a create, as the
/// documents have it, and checked on an update when given.
fn check_type(body: &Map<String, Value>, required: bool) -> Result<()> {
    match body.get("@odata.type").and_then(Value::as_str) {
        Some(kind) => {
            let kind = kind.trim_start_matches('#');
            if TYPES.iter().any(|known| known.eq_ignore_ascii_case(kind)) {
                Ok(())
            } else {
                Err(GraphError::unreadable_body(format!(
                    "'{kind}' isn't an open extension's type."
                )))
            }
        }
        None if required => Err(GraphError::unreadable_body(
            "An open extension needs its @odata.type, microsoft.graph.openTypeExtension.",
        )),
        None => Ok(()),
    }
}

/// A new extension from a create's body: named by `extensionName`, or by `id`.
pub fn from_body(body: &Map<String, Value>) -> Result<OpenExtension> {
    check_type(body, true)?;
    let named = |key: &str| body.get(key).and_then(Value::as_str).map(short);
    let id = named("extensionName")
        .or_else(|| named("id"))
        .ok_or_else(|| GraphError::invalid_request("An open extension needs its extensionName."))?
        .to_owned();
    check_name(&id)?;
    Ok(OpenExtension {
        name: Some(id.clone()),
        id,
        data: data_of(body, true)?,
    })
}

/// Adds `extension` to a message's, refusing a second one of the same name.
pub fn add(list: &mut Vec<OpenExtension>, extension: OpenExtension) -> Result<()> {
    if list.iter().any(|known| known.answers(&extension.id)) {
        return Err(GraphError::new(
            409,
            "NameAlreadyExists",
            format!(
                "An open extension named '{}' already exists on this item.",
                extension.id
            ),
        ));
    }
    list.push(extension);
    Ok(())
}

/// `$filter`'s `Extensions/any(f:f/id eq '…')`: the property name the filter calls the
/// extensions by, when it uses them, after checking it reads nothing of them but their ids, as
/// open extensions aren't filterable by their data.
pub fn filter_property(filter: &Expr) -> Result<Option<String>> {
    let mut found = None;
    visit(filter, &mut found)?;
    Ok(found)
}

fn visit(expr: &Expr, found: &mut Option<String>) -> Result<()> {
    let extensions = |name: &str| name.eq_ignore_ascii_case("extensions");
    match expr {
        Expr::And(a, b) | Expr::Or(a, b) => {
            visit(a, found)?;
            visit(b, found)
        }
        Expr::Not(a) => visit(a, found),
        Expr::Any(path, var, body) if extensions(path.property()) => {
            if path.0.len() != 1 || !ids_only(body, var) {
                return Err(GraphError::invalid_filter());
            }
            *found = Some(path.property().to_owned());
            Ok(())
        }
        Expr::Any(path, ..)
        | Expr::Compare(path, ..)
        | Expr::Function(_, path, _)
        | Expr::Truthy(path)
            if extensions(path.property()) =>
        {
            Err(GraphError::invalid_filter())
        }
        _ => Ok(()),
    }
}

/// Whether an `any` body over extensions compares only `var/id` for equality.
fn ids_only(body: &Expr, var: &str) -> bool {
    match body {
        Expr::And(a, b) | Expr::Or(a, b) => ids_only(a, var) && ids_only(b, var),
        Expr::Compare(path, Op::Eq, Literal::Text(_)) => {
            path.0.len() == 2 && path.0[0] == var && path.0[1].eq_ignore_ascii_case("id")
        }
        _ => false,
    }
}

/// The values a message's `$filter` sees for its extensions.
pub fn filter_values(list: &[OpenExtension]) -> Value {
    list.iter()
        .flat_map(OpenExtension::filter_forms)
        .collect::<Vec<_>>()
        .into()
}

/// Which of a message's extensions an `$expand=extensions(…)` takes: all, or those its inner
/// `$filter` (on `id`) matches.
#[derive(Debug, Clone, Default)]
pub struct Selection(Option<Expr>);

impl Selection {
    /// The inner options of `extensions(…)`, such as `$filter=id eq 'com.contoso.x'`.
    pub fn parse(inner: Option<&str>) -> Result<Self> {
        let filter = inner.and_then(|inner| {
            inner.split(';').find_map(|option| {
                let (key, value) = option.split_once('=')?;
                key.trim()
                    .eq_ignore_ascii_case("$filter")
                    .then(|| value.trim().to_owned())
            })
        });
        let filter = filter.as_deref().map(Expr::parse).transpose()?;
        Ok(Self(filter))
    }

    fn takes(&self, extension: &OpenExtension) -> bool {
        self.0.as_ref().is_none_or(|filter| {
            extension
                .filter_forms()
                .iter()
                .any(|form| filter.matches(form))
        })
    }

    /// The selected extensions of `list`, rendered.
    pub fn render(&self, list: &[OpenExtension]) -> Value {
        list.iter()
            .filter(|extension| self.takes(extension))
            .map(|extension| Value::Object(extension.json()))
            .collect::<Vec<_>>()
            .into()
    }
}

/// The message a request names (under its folder, when the path has one) and its key.
fn message_key(view: &View<'_>, params: &HashMap<String, String>) -> Result<u64> {
    let key = key_of(view, &params["id"])?;
    if let Some(folder) = params.get("folder") {
        let folder = resolve(view, folder)?;
        if view.message(key)?.0.folder != folder {
            return Err(GraphError::not_found());
        }
    }
    view.message(key)?;
    Ok(key)
}

/// The `@odata.context` path of a message's extensions.
fn context(view: &View<'_>, params: &HashMap<String, String>, entity: bool) -> String {
    let fragment = format!(
        "messages('{}')/extensions{}",
        params["id"],
        if entity { "/$entity" } else { "" }
    );
    view.context(&fragment)
}

/// `GET /me/messages/{id}/extensions`.
pub async fn list(call: Call, Path(params): super::Params) -> Result<Response> {
    let world = call.world();
    let origin = origin(&world);
    let view = call.view(&world, &origin);
    let key = message_key(&view, &params)?;
    let filter = call
        .options
        .filter
        .as_deref()
        .map(Expr::parse)
        .transpose()?;
    let value = Selection(filter).render(
        world
            .graph
            .extensions
            .get(&key)
            .map(Vec::as_slice)
            .unwrap_or_default(),
    );
    let value = value.as_array().cloned().unwrap_or_default();
    Ok(reply(
        200,
        odata::collection(context(&view, &params, false), value, None, None),
    ))
}

/// `GET /me/messages/{id}/extensions/{name}`, by name or fully qualified id.
pub async fn get(call: Call, Path(params): super::Params) -> Result<Response> {
    let world = call.world();
    let origin = origin(&world);
    let view = call.view(&world, &origin);
    let key = message_key(&view, &params)?;
    let extension = world
        .graph
        .extensions
        .get(&key)
        .and_then(|list| list.iter().find(|e| e.answers(&params["extension"])))
        .ok_or_else(GraphError::not_found)?;
    Ok(reply_map(
        200,
        entity(context(&view, &params, true), extension.json()),
    ))
}

/// `POST /me/messages/{id}/extensions` (201): a new extension on the message.
pub async fn create(
    call: Call,
    Path(params): super::Params,
    body: axum::body::Bytes,
) -> Result<Response> {
    let request: Map<String, Value> = body_json(&body, "openTypeExtension")?;
    let extension = from_body(&request)?;
    let mut world = call.world();
    let key = {
        let origin = origin(&world);
        let view = call.view(&world, &origin);
        message_key(&view, &params)?
    };
    let list = world.graph.extensions.entry(key).or_default();
    add(list, extension.clone())?;
    let origin = origin(&world);
    let view = call.view(&world, &origin);
    Ok(reply_map(
        201,
        entity(context(&view, &params, true), extension.json()),
    ))
}

/// `PATCH /me/messages/{id}/extensions/{name}` (200): the given properties changed or added and
/// the rest kept, as Outlook updates a message's extension, and null refused.
pub async fn update(
    call: Call,
    Path(params): super::Params,
    body: axum::body::Bytes,
) -> Result<Response> {
    let request: Map<String, Value> = body_json(&body, "openTypeExtension")?;
    check_type(&request, false)?;
    let data = data_of(&request, false)?;
    let mut world = call.world();
    let key = {
        let origin = origin(&world);
        let view = call.view(&world, &origin);
        message_key(&view, &params)?
    };
    let extension = world
        .graph
        .extensions
        .get_mut(&key)
        .and_then(|list| list.iter_mut().find(|e| e.answers(&params["extension"])))
        .ok_or_else(GraphError::not_found)?;
    match request.get("extensionName") {
        Some(Value::String(name)) => {
            let name = short(name);
            check_name(name)?;
            extension.name = Some(name.to_owned());
        }
        // Naming the extension by its id alone drops its extensionName.
        None if request.contains_key("id") => extension.name = None,
        None => {}
        Some(_) => {
            return Err(GraphError::unreadable_body(
                "extensionName must be a string.",
            ));
        }
    }
    extension.data.extend(data);
    let json = extension.json();
    let origin = origin(&world);
    let view = call.view(&world, &origin);
    Ok(reply_map(200, entity(context(&view, &params, true), json)))
}

/// `DELETE /me/messages/{id}/extensions/{name}` (204).
pub async fn delete(call: Call, Path(params): super::Params) -> Result<Response> {
    let mut world = call.world();
    let key = {
        let origin = origin(&world);
        let view = call.view(&world, &origin);
        message_key(&view, &params)?
    };
    let list = world
        .graph
        .extensions
        .get_mut(&key)
        .ok_or_else(GraphError::not_found)?;
    let before = list.len();
    list.retain(|extension| !extension.answers(&params["extension"]));
    if list.len() == before {
        return Err(GraphError::not_found());
    }
    Ok(StatusCode::NO_CONTENT.into_response())
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn names_are_reverse_domain_names_of_ones_own() {
        assert!(check_name("com.brightmoor.reminders").is_ok());
        assert!(check_name("Com.Contoso.Referral").is_ok());
        for name in [
            "reminders",
            "com..x",
            "com.contoso.",
            "com.microsoft.x",
            "COM.OnMicrosoft.x",
            "com.contoso.a b",
        ] {
            assert!(check_name(name).is_err(), "{name}");
        }
    }

    #[test]
    fn extensions_answer_to_their_name_and_full_id() {
        let extension = from_body(
            json!({ "@odata.type": "microsoft.graph.openTypeExtension", "extensionName": "com.brightmoor.snoozed", "until": "2026-12-01" })
                .as_object()
                .unwrap(),
        )
        .unwrap();
        assert!(extension.answers("com.brightmoor.snoozed"));
        assert!(
            extension.answers("Microsoft.OutlookServices.OpenTypeExtension.com.brightmoor.snoozed")
        );
        assert!(extension.answers("microsoft.graph.openTypeExtension.COM.Brightmoor.Snoozed"));
        assert!(!extension.answers("com.brightmoor.other"));
        assert_eq!(
            extension.json()["id"],
            "Microsoft.OutlookServices.OpenTypeExtension.com.brightmoor.snoozed"
        );
        let selected = Selection::parse(Some("$filter=id eq 'com.brightmoor.snoozed'")).unwrap();
        assert_eq!(
            selected
                .render(std::slice::from_ref(&extension))
                .as_array()
                .unwrap()
                .len(),
            1
        );
        let other = Selection::parse(Some("$filter=id eq 'com.brightmoor.other'")).unwrap();
        assert_eq!(other.render(&[extension]).as_array().unwrap().len(), 0);
    }

    #[test]
    fn filters_read_only_extension_ids() {
        let property = |text: &str| filter_property(&Expr::parse(text).unwrap());
        assert_eq!(
            property("Extensions/any(f:f/id eq 'com.a.b')")
                .unwrap()
                .as_deref(),
            Some("Extensions")
        );
        assert_eq!(property("isRead eq false").unwrap(), None);
        assert!(
            property("Extensions/any(f:f/until eq '2026')").is_err(),
            "not by their data"
        );
        assert!(property("Extensions/any(f:f/id ne 'com.a.b')").is_err());
    }
}
