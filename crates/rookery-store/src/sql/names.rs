//! The names raw SQL derives from a plugin id: its role, its schemas and its views, and the identifier and
//! literal quoting every statement of this module goes through. Pure: no database.
//!
//! - `rookery_x_<plugin>` is the role (`rookery_<tag>_x_<plugin>` on a deployment with a role tag, see
//!   [`Names::new`]), `x_<plugin>` the schema the plugin owns, `v_<plugin>` the schema of read views over its
//!   collections and `p_<plugin>` the schema of the collection tables themselves. In all of them `-` of the
//!   plugin id is written `_`.
//! - A plugin id is 1 to 40 characters of lower case letters, digits and `-`, starting with a letter
//!   ([`crate::collections::validate_plugin_id`]); collection and field names are letters, digits and `_`.
//!   Every identifier is quoted anyway, so a name that slipped past a check is still one identifier.

use crate::collections::{CollectionDef, plugin_schema, validate_plugin_id};
use crate::error::{Error, Result};

/// The longest a role tag is.
pub const MAX_TAG: usize = 12;

/// The names of one plugin's raw SQL objects.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Names {
    pub plugin: String,
    /// `rookery_x_<plugin>`, or with a tag `rookery_<tag>_x_<plugin>`.
    pub role: String,
    /// `x_<plugin>`: the schema the plugin owns.
    pub raw_schema: String,
    /// `v_<plugin>`: the read views.
    pub view_schema: String,
    /// `p_<plugin>`: the collection tables.
    pub data_schema: String,
}

impl Names {
    /// The names of `plugin`. Roles are cluster-wide in Postgres, so two deployments (or two test
    /// databases) on one server would share `rookery_x_<plugin>`; a `tag` (`ROOKERY_DATABASE_ROLE_TAG`) puts one
    /// deployment's roles in a namespace of their own: 2 to 12 lower case letters and digits, written
    /// between `rookery_` and `_x_`. The two forms cannot meet: an untagged role's name continues `rookery_x_`,
    /// and a tag has no `_` and at least two characters, so `x_` is never one.
    pub fn new(plugin: &str, tag: Option<&str>) -> Result<Names> {
        validate_plugin_id(plugin)?;
        let id = plugin.replace('-', "_");
        let role = match tag {
            None => format!("rookery_x_{id}"),
            Some(t) => {
                check_tag(t)?;
                format!("rookery_{t}_x_{id}")
            }
        };
        Ok(Names {
            plugin: plugin.to_string(),
            role,
            raw_schema: format!("x_{id}"),
            view_schema: format!("v_{id}"),
            data_schema: plugin_schema(plugin),
        })
    }
}

/// A role tag: 2 to 12 lower case letters and digits.
pub fn check_tag(tag: &str) -> Result<()> {
    let ok = (2..=MAX_TAG).contains(&tag.len())
        && tag
            .bytes()
            .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit());
    if ok {
        Ok(())
    } else {
        Err(Error::invalid(format!(
            "the role tag {tag:?} must be 2 to {MAX_TAG} lower case letters and digits"
        )))
    }
}

/// The prefix every role of a tag starts with: `rookery_<tag>_x_`.
pub fn tag_prefix(tag: &str) -> String {
    format!("rookery_{tag}_x_")
}

/// `"name"`, for use as an identifier. Doubles quotes; a NUL byte is refused by Postgres itself and cannot
/// get here from a validated name.
pub fn quote_ident(s: &str) -> String {
    format!("\"{}\"", s.replace('"', "\"\""))
}

/// `"schema"."name"`.
pub fn qualified(schema: &str, name: &str) -> String {
    format!("{}.{}", quote_ident(schema), quote_ident(name))
}

/// `'text'`, for the few places a statement cannot take a parameter (utility statements: a comment, a
/// password verifier, a setting). Only text made of letters, digits and punctuation without `'`, `\` or
/// control characters is accepted, so no escaping rule of the server's settings applies.
pub fn quote_literal(s: &str) -> Result<String> {
    if s.chars().any(|c| c == '\'' || c == '\\' || c.is_control()) {
        return Err(Error::internal(
            "a literal in a utility statement holds a quote, a backslash or a control character",
        ));
    }
    Ok(format!("'{s}'"))
}

/// The two views of a collection: `<name>` (live rows) and `<name>__all` (trashed ones too).
pub fn view_names(collection: &str) -> (String, String) {
    (collection.to_string(), format!("{collection}__all"))
}

/// One view of a collection and the statement that creates it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct View {
    pub name: String,
    pub create: String,
}

/// The system columns every view has, in order, then the fields.
pub const SYSTEM_COLUMNS: &[&str] = &[
    "id",
    "space_id",
    "user_id",
    "created_at",
    "updated_at",
    "created_by",
    "updated_by",
    "rev",
];

/// What a log collection's table has before its fields (no `updated_*`, `rev` or `deleted_at`: rows are only
/// appended).
pub const LOG_COLUMNS: &[&str] = &[
    "seq",
    "id",
    "space_id",
    "user_id",
    "created_at",
    "created_by",
];

/// The views of a plugin's collections, two each (a log collection has one, `<name>`, of every row): a live view (`deleted_at IS NULL`; no `deleted_at`) and
/// an `__all` view with trashed rows and `deleted_at`. The hidden `_search` column is never selected. Fails
/// when two views would have one name (a collection `a__all` next to a collection `a`).
pub fn views(names: &Names, defs: &[CollectionDef]) -> Result<Vec<View>> {
    let mut out: Vec<View> = Vec::new();
    for d in defs {
        let table = qualified(&names.data_schema, &d.name);
        let system = if d.is_log() {
            LOG_COLUMNS
        } else {
            SYSTEM_COLUMNS
        };
        let mut cols: Vec<String> = system.iter().map(|c| quote_ident(c)).collect();
        cols.extend(d.fields.iter().map(|f| quote_ident(&f.name)));
        let (live, all) = view_names(&d.name);
        if d.is_log() {
            if out.iter().any(|v| v.name == live) {
                return Err(Error::invalid(format!(
                    "collection {}: its view {live} has the name of another collection's view",
                    d.name
                )));
            }
            out.push(View {
                create: format!(
                    "CREATE VIEW {} AS SELECT {} FROM {table}",
                    qualified(&names.view_schema, &live),
                    cols.join(", ")
                ),
                name: live,
            });
            continue;
        }
        let select = |extra: &str, filter: &str| {
            let mut c = cols.clone();
            if !extra.is_empty() {
                c.insert(system.len(), extra.to_string());
            }
            format!("SELECT {} FROM {table}{filter}", c.join(", "))
        };
        for (name, body) in [
            (live, select("", " WHERE deleted_at IS NULL")),
            (all, select("\"deleted_at\"", "")),
        ] {
            if out.iter().any(|v| v.name == name) {
                return Err(Error::invalid(format!(
                    "collection {}: its view {name} has the name of another collection's view",
                    d.name
                )));
            }
            out.push(View {
                create: format!(
                    "CREATE VIEW {} AS {body}",
                    qualified(&names.view_schema, &name)
                ),
                name,
            });
        }
    }
    Ok(out)
}

/// A hash of a view's definition, kept in the view's comment: an unchanged view is left alone.
pub fn definition_hash(create: &str) -> String {
    use sha2::{Digest, Sha256};
    hex::encode(&Sha256::digest(create.as_bytes())[..8])
}
