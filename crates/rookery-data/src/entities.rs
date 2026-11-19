//! `entities.*`: the index of everything with a URI, for chips, mention pickers, search and the walk of
//! access to parents. Rows of entity collections are indexed by the store when they are written; a
//! plugin registers its other entities, in a scheme it declares (`contributes.entityTypes`), with
//! `entities.register`.
//!
//! The text of an entity (`entities.text`) is made only for a caller who may read it: a plugin that
//! declares the scheme gives it through a [`TextProvider`], and a row of an entity collection is
//! written out from its fields as Markdown.

use std::collections::HashMap;
use std::sync::{Arc, RwLock};

use rookery_kernel::{Call, Caller, Empty, Kernel, Level, Methods, RpcError};
use rookery_proto::ErrorCode;
use rookery_proto::data::{
    ENTITY_TEXT_DEFAULT_CHARS, ENTITY_TEXT_MAX_CHARS, ENTITY_TEXT_MAX_URIS, EntityAlias,
    EntityInfo, EntityList, EntityRef, EntityRefs, EntityRegister, EntitySearch, EntityText,
    EntityTextCall, EntityTextFormat, EntityTextList, EntityTextQuery, EntityTextResult,
};
use rookery_store::collections::{CollectionDef, FieldType};
use rookery_store::entities::{Entity, EntityRecord, EntitySearch as StoreSearch};
use rookery_store::{Caller as StoreCaller, Level as StoreLevel, Uri};
use serde_json::Value;

use crate::{parse_id, rpc, store_caller, time};

/// Gives the text of entities of the schemes a plugin declares: the plugin system's half, which asks
/// the plugin's host half.
#[async_trait::async_trait]
pub trait TextProvider: Send + Sync {
    /// The text of one entity, for `call`'s caller, or `None` for none. `NOT_FOUND` means the same.
    async fn text(
        &self,
        call: &Call,
        plugin: &str,
        entity: EntityTextCall,
    ) -> Result<Option<EntityTextResult>, RpcError>;
}

/// The entity types plugins declare, by plugin, and the provider of their text.
#[derive(Default)]
struct Types {
    schemes: RwLock<HashMap<String, Vec<String>>>,
    text: RwLock<Option<Arc<dyn TextProvider>>>,
}

/// Sets the schemes a plugin declares in `contributes.entityTypes`, replacing those it had.
pub fn set_entity_types(kernel: &Kernel, plugin: &str, schemes: Vec<String>) {
    let types = kernel.facility::<Types>();
    let mut all = types.schemes.write().unwrap();
    if schemes.is_empty() {
        all.remove(plugin);
    } else {
        all.insert(plugin.to_owned(), schemes);
    }
}

/// Sets what gives the text of plugins' entity types.
pub fn set_text_provider(kernel: &Kernel, provider: Arc<dyn TextProvider>) {
    *kernel.facility::<Types>().text.write().unwrap() = Some(provider);
}

/// The plugin that declares a scheme.
fn declarer(kernel: &Kernel, scheme: &str) -> Option<String> {
    let types = kernel.facility::<Types>();
    let all = types.schemes.read().unwrap();
    all.iter()
        .find(|(_, s)| s.iter().any(|x| x == scheme))
        .map(|(p, _)| p.clone())
}

pub fn entity_info(e: &Entity) -> EntityInfo {
    EntityInfo {
        uri: e.uri.clone(),
        scheme: e.scheme.clone(),
        id: e.id.clone(),
        title: e.title.clone(),
        subtitle: e.subtitle.clone(),
        icon: e.icon.clone(),
        space_id: e.space_id.map(|s| s.to_string()),
        parent: e.parent.clone(),
        plugin: e.plugin.clone(),
        private: e.private,
        updated_at: time(e.updated_at),
        deleted_at: e.deleted_at.map(time),
    }
}

pub fn register(methods: &mut Methods) {
    methods
        .add("entities.resolve", |call: Call, p: EntityRef| async move {
            let caller = store_caller(&call, None)?;
            let e = call
                .store()
                .entities()
                .get(&caller, &p.uri)
                .await
                .map_err(rpc)?;
            Ok(e.map(|e| entity_info(&e)))
        })
        .summary("An entity by its URI, or null when there is none the caller may see.")
        .level(Level::Own);
    methods
        .add(
            "entities.resolveMany",
            |call: Call, p: EntityRefs| async move {
                if p.uris.len() > 500 {
                    return Err(RpcError::invalid("at most 500 URIs at once"));
                }
                let caller = store_caller(&call, None)?;
                let found = call
                    .store()
                    .entities()
                    .get_many(&caller, &p.uris)
                    .await
                    .map_err(rpc)?;
                Ok(EntityList {
                    entities: found.iter().map(entity_info).collect(),
                })
            },
        )
        .summary("The entities of some URIs that the caller may see.")
        .level(Level::Own);
    methods
        .add("entities.search", search)
        .summary("Entities whose titles and text match words, of some schemes and in a space, as far as the caller may see.")
        .level(Level::Own);
    methods
        .add("entities.register", register_entity)
        .summary("Registers or updates an entity of a scheme the plugin declares.")
        .level(Level::Plugin);
    methods
        .add("entities.remove", |call: Call, p: EntityRef| async move {
            let plugin = calling_plugin(&call)?;
            own_scheme(&call, &plugin, &p.uri)?;
            call.store().entities().remove(&p.uri).await.map_err(rpc)?;
            Ok(Empty {})
        })
        .summary("Removes an entity of a scheme the plugin declares.")
        .level(Level::Plugin);
    methods
        .add("entities.alias", alias)
        .summary("Makes a URI of the plugin's name another entity, which it then resolves to.")
        .level(Level::Plugin);
    methods
        .add("entities.text", text)
        .summary("The text of entities the caller may read, to read, search or give a model.")
        .level(Level::Own);
}

fn calling_plugin(call: &Call) -> Result<String, RpcError> {
    match &call.caller {
        Caller::Plugin(p) => Ok(p.clone()),
        _ => Err(RpcError::forbidden("this is a plugin's")),
    }
}

async fn search(call: Call, p: EntitySearch) -> Result<EntityList, RpcError> {
    let caller = store_caller(&call, None)?;
    let space = p
        .space
        .as_deref()
        .map(|s| parse_id("space", s))
        .transpose()?;
    let hits = call
        .store()
        .entities()
        .search(
            &caller,
            &StoreSearch {
                text: p.text,
                schemes: p.schemes,
                space,
                limit: p.limit,
            },
        )
        .await
        .map_err(rpc)?;
    Ok(EntityList {
        entities: hits.iter().map(|h| entity_info(&h.entity)).collect(),
    })
}

/// A URI in a scheme the plugin declares, and not one of its collections', whose rows the store indexes.
fn own_scheme(call: &Call, plugin: &str, uri: &str) -> Result<(), RpcError> {
    let parsed = Uri::parse(uri).map_err(rpc)?;
    if declarer(&call.kernel, &parsed.scheme).as_deref() != Some(plugin) {
        return Err(RpcError::forbidden(format!(
            "the plugin {plugin} declares no entity type {} in contributes.entityTypes",
            parsed.scheme
        )));
    }
    if call
        .store()
        .collections()
        .by_scheme(&parsed.scheme)
        .is_some()
    {
        return Err(RpcError::invalid(format!(
            "{} entities are rows of a collection, which the store indexes when they are written",
            parsed.scheme
        )));
    }
    Ok(())
}

async fn register_entity(call: Call, p: EntityRegister) -> Result<EntityInfo, RpcError> {
    let plugin = calling_plugin(&call)?;
    own_scheme(&call, &plugin, &p.uri)?;
    let space_id = p
        .space
        .as_deref()
        .map(|s| parse_id("space", s))
        .transpose()?;
    let store = call.store();
    let e = store
        .entities()
        .register(&EntityRecord {
            uri: p.uri.clone(),
            space_id,
            parent: p.parent,
            title: p.title,
            subtitle: p.subtitle,
            icon: p.icon,
            search_text: p.search_text,
            owner_id: None,
            private: p.private,
            plugin: Some(plugin),
        })
        .await
        .map_err(rpc)?;
    if let Some(refs) = p.refs {
        store
            .entities()
            .set_refs(&p.uri, "refs", &refs)
            .await
            .map_err(rpc)?;
    }
    Ok(entity_info(&e))
}

async fn alias(call: Call, p: EntityAlias) -> Result<Empty, RpcError> {
    let plugin = calling_plugin(&call)?;
    let parsed = Uri::parse(&p.uri).map_err(rpc)?;
    let owns = declarer(&call.kernel, &parsed.scheme).as_deref() == Some(plugin.as_str())
        || call
            .store()
            .collections()
            .by_scheme(&parsed.scheme)
            .is_some_and(|c| c.plugin == plugin);
    if !owns {
        return Err(RpcError::forbidden(format!(
            "{} is no scheme of the plugin {plugin}",
            parsed.scheme
        )));
    }
    call.store()
        .entities()
        .alias(&p.uri, &p.target, Some(&plugin))
        .await
        .map_err(rpc)?;
    Ok(Empty {})
}

async fn text(call: Call, p: EntityTextQuery) -> Result<EntityTextList, RpcError> {
    let max = p
        .max_chars
        .unwrap_or(ENTITY_TEXT_DEFAULT_CHARS)
        .min(ENTITY_TEXT_MAX_CHARS) as usize;
    texts(&call, &p.uris, max).await
}

/// `uris` as text for the caller of `call`. Entities that do not exist, that the caller may not read,
/// or that have no text, are listed in `missing`.
pub async fn texts(
    call: &Call,
    uris: &[String],
    max_chars: usize,
) -> Result<EntityTextList, RpcError> {
    if uris.len() > ENTITY_TEXT_MAX_URIS {
        return Err(RpcError::invalid(format!(
            "at most {ENTITY_TEXT_MAX_URIS} entities at once"
        )));
    }
    let caller = store_caller(call, None)?;
    let found = call
        .store()
        .entities()
        .get_many(&caller, uris)
        .await
        .map_err(rpc)?;
    let provider = call.kernel.facility::<Types>().text.read().unwrap().clone();
    let mut out = EntityTextList {
        texts: Vec::new(),
        missing: Vec::new(),
    };
    for uri in uris {
        let Some(e) = found
            .iter()
            .find(|e| &e.uri == uri && e.deleted_at.is_none())
        else {
            out.missing.push(uri.clone());
            continue;
        };
        let mut text: Option<(Option<String>, String, EntityTextFormat)> = None;
        if let (Some(plugin), Some(provider)) = (declarer(&call.kernel, &e.scheme), &provider) {
            let asked = EntityTextCall {
                uri: e.uri.clone(),
                scheme: e.scheme.clone(),
                id: e.id.clone(),
            };
            match provider.text(call, &plugin, asked).await {
                Ok(Some(r)) => text = Some((r.title, r.text, r.format.unwrap_or_default())),
                Ok(None) => {}
                Err(err) if err.error_code() == ErrorCode::NotFound => {}
                Err(err) => return Err(err),
            }
        }
        if text.is_none()
            && let (Some(collection), Some(owner)) = (&e.collection, &e.plugin)
            && let Some(generated) = row_text(call, &caller, owner, collection, e).await?
        {
            text = Some((None, generated, EntityTextFormat::Markdown));
        }
        let Some((title, body, format)) = text else {
            out.missing.push(uri.clone());
            continue;
        };
        let (text, truncated) = cut(&body, max_chars);
        out.texts.push(EntityText {
            uri: uri.clone(),
            title: title.or_else(|| Some(e.title.clone()).filter(|t| !t.is_empty())),
            text,
            format,
            truncated,
        });
    }
    Ok(out)
}

/// The text of a row of an entity collection, from its fields, read with the caller's rights: `None`
/// when the collection gives none, the row is gone, or the caller may not read it.
async fn row_text(
    call: &Call,
    caller: &StoreCaller,
    plugin: &str,
    collection: &str,
    e: &Entity,
) -> Result<Option<String>, RpcError> {
    let Ok(coll) = call.store().collections().get(plugin, collection) else {
        return Ok(None);
    };
    let Some(entity) = &coll.def.entity else {
        return Ok(None);
    };
    if entity.text == Some(Value::Bool(false)) {
        return Ok(None);
    }
    let Ok(id) = uuid::Uuid::parse_str(&e.id) else {
        return Ok(None);
    };
    let reader = caller.clone().with_space(e.space_id.or(caller.space));
    let row = match call.store().data(plugin).get(&reader, collection, id).await {
        Ok(Some(row)) => row,
        Ok(None) => return Ok(None),
        Err(err)
            if matches!(
                err.code(),
                ErrorCode::NotFound | ErrorCode::Forbidden | ErrorCode::Invalid
            ) =>
        {
            return Ok(None);
        }
        Err(err) => return Err(rpc(err)),
    };
    let only: Option<Vec<String>> = entity
        .text
        .as_ref()
        .and_then(|t| t.get("fields"))
        .and_then(|f| serde_json::from_value(f.clone()).ok());
    Ok(Some(generate(
        &coll.def,
        &entity.title,
        only.as_deref(),
        &row,
        &e.title,
    )))
}

/// A row as Markdown: its title as the heading, short values as a list, long ones under headings of
/// their own. Binary and vector fields, and empty values, are left out.
pub fn generate(
    def: &CollectionDef,
    title_field: &str,
    only: Option<&[String]>,
    row: &Value,
    title: &str,
) -> String {
    let mut text = String::new();
    if !title.is_empty() {
        text.push_str(&format!("# {title}\n\n"));
    }
    let chosen: Vec<_> = match only {
        Some(names) => names.iter().filter_map(|n| def.field(n)).collect(),
        None => def
            .fields
            .iter()
            .filter(|f| f.name != title_field)
            .collect(),
    };
    let mut blocks = String::new();
    for f in chosen {
        let Some((value, block)) = render(&f.ty, row.get(&f.name).unwrap_or(&Value::Null)) else {
            continue;
        };
        let label = f.title.as_deref().or(f.label.as_deref()).unwrap_or(&f.name);
        if block {
            blocks.push_str(&format!("## {label}\n\n{value}\n\n"));
        } else {
            text.push_str(&format!("- {label}: {value}\n"));
        }
    }
    if !text.ends_with("\n\n") && !text.is_empty() && !blocks.is_empty() {
        text.push('\n');
    }
    text.push_str(&blocks);
    text.trim_end().to_string()
}

/// A field's value as text, and whether it goes under a heading of its own.
fn render(ty: &FieldType, v: &Value) -> Option<(String, bool)> {
    let plain = |s: &str| Some(s.to_string()).filter(|s| !s.trim().is_empty());
    match ty {
        FieldType::Bytes { .. } | FieldType::Blob | FieldType::Vector { .. } => None,
        FieldType::Richtext => {
            plain(&rookery_store::collections::richtext::plain_text(v)).map(|t| (t, true))
        }
        FieldType::Longtext => v.as_str().and_then(plain).map(|t| (t, true)),
        FieldType::Json => match v {
            Value::Null => None,
            other => Some((other.to_string(), true)),
        },
        FieldType::Boolean => v
            .as_bool()
            .map(|b| (if b { "yes" } else { "no" }.to_string(), false)),
        FieldType::Refs { .. } => {
            let list: Vec<&str> = v.as_array()?.iter().filter_map(Value::as_str).collect();
            (!list.is_empty()).then(|| (list.join(", "), false))
        }
        FieldType::Enum { values } => {
            let value = v.as_str()?;
            let title = values
                .iter()
                .find_map(|x| match x {
                    rookery_store::collections::EnumValue::Detail {
                        value: name,
                        title: Some(t),
                        ..
                    } if name == value => Some(t.clone()),
                    _ => None,
                })
                .unwrap_or_else(|| value.to_string());
            Some((title, false))
        }
        FieldType::Text
        | FieldType::Path
        | FieldType::Ref { .. }
        | FieldType::Date
        | FieldType::Datetime => v
            .as_str()
            .and_then(plain)
            .map(|t| (t.clone(), t.contains('\n'))),
        FieldType::Integer | FieldType::Number | FieldType::Sequence { .. } => match v {
            Value::Number(n) => Some((n.to_string(), false)),
            _ => None,
        },
    }
}

/// At most `max` characters, cut at a character boundary.
fn cut(text: &str, max: usize) -> (String, bool) {
    match text.char_indices().nth(max) {
        Some((at, _)) => (text[..at].to_string(), true),
        None => (text.to_string(), false),
    }
}

/// Whether a person may see an entity, for code that tells them of it.
pub async fn can_view(kernel: &Kernel, user: uuid::Uuid, uri: &str) -> bool {
    kernel
        .store
        .entities()
        .can(&StoreCaller::user(user), uri, StoreLevel::View)
        .await
        .unwrap_or(false)
}

#[cfg(test)]
mod tests {
    #[test]
    fn cuts_at_characters() {
        assert_eq!(super::cut("héllo", 2), ("hé".to_string(), true));
        assert_eq!(super::cut("hi", 5), ("hi".to_string(), false));
    }
}
