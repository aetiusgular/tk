//! Format-preserving TOML edits for Codex's `config.toml`, through
//! `toml_edit`: comments, key order and spacing outside the entry survive.
//! Codex 0.163.0 also kept comments when `codex mcp add` wrote to a
//! commented file (recorded 27 Oct, lane C status doc), so the old note
//! that Codex drops them no longer holds at that version.

use toml_edit::{Array, DocumentMut, Item, Table, Value};

/// One key of an entry table, in the order written.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Field {
    Str(String),
    Strings(Vec<String>),
    /// A sub-table such as `[mcp_servers.<name>.env_http_headers]`, written
    /// as its own `[a.b.c]` header the way Codex writes `env`.
    Table(Vec<(String, String)>),
}

#[derive(Debug, PartialEq, Eq)]
pub enum TomlError {
    Parse(String),
    Exists { same: bool },
    Missing,
    NotTable(String),
}

fn entry_table(fields: &[(String, Field)]) -> Table {
    let mut t = Table::new();
    for (k, f) in fields {
        match f {
            Field::Str(s) => {
                t.insert(k, Item::Value(Value::from(s.as_str())));
            }
            Field::Strings(items) => {
                let mut a = Array::new();
                for i in items {
                    a.push(i.as_str());
                }
                t.insert(k, Item::Value(Value::Array(a)));
            }
            Field::Table(pairs) => {
                let mut sub = Table::new();
                for (sk, sv) in pairs {
                    sub.insert(sk, Item::Value(Value::from(sv.as_str())));
                }
                t.insert(k, Item::Table(sub));
            }
        }
    }
    t
}

/// What the table at `container.name` holds now, as fields, when it is
/// there and has only the shapes tk writes. Used to tell "already
/// connected" from "a different entry under the same name".
fn read_fields(t: &Table) -> Vec<(String, Field)> {
    let mut out = Vec::new();
    for (k, item) in t.iter() {
        let f = match item {
            Item::Value(Value::String(s)) => Field::Str(s.value().clone()),
            Item::Value(Value::Array(a)) => Field::Strings(
                a.iter()
                    .map(|v| v.as_str().unwrap_or("\u{0}").to_string())
                    .collect(),
            ),
            Item::Table(sub) => Field::Table(
                sub.iter()
                    .map(|(sk, sv)| (sk.to_string(), sv.as_str().unwrap_or("\u{0}").to_string()))
                    .collect(),
            ),
            Item::Value(Value::InlineTable(sub)) => Field::Table(
                sub.iter()
                    .map(|(sk, sv)| (sk.to_string(), sv.as_str().unwrap_or("\u{0}").to_string()))
                    .collect(),
            ),
            _ => Field::Str("\u{0}".into()),
        };
        out.push((k.to_string(), f));
    }
    out
}

/// The new text with `[<container>.<name>]` set to `fields`.
pub fn set_entry(
    text: &str,
    container: &str,
    name: &str,
    fields: &[(String, Field)],
    replace: bool,
) -> Result<String, TomlError> {
    let mut doc: DocumentMut = text
        .parse()
        .map_err(|e: toml_edit::TomlError| TomlError::Parse(e.to_string()))?;
    let had_content = !text.trim().is_empty();
    if !doc.contains_key(container) {
        let mut parent = Table::new();
        parent.set_implicit(true);
        doc.insert(container, Item::Table(parent));
    }
    let parent = doc
        .get_mut(container)
        .and_then(Item::as_table_mut)
        .ok_or_else(|| TomlError::NotTable(container.to_string()))?;
    if let Some(existing) = parent.get(name) {
        let same = existing
            .as_table()
            .is_some_and(|t| read_fields(t) == fields);
        if !replace || same {
            return Err(TomlError::Exists { same });
        }
        parent.remove(name);
    }
    let mut table = entry_table(fields);
    if had_content || !parent.is_empty() {
        table.decor_mut().set_prefix("\n");
    }
    for (k, f) in fields {
        if matches!(f, Field::Table(_)) {
            if let Some(sub) = table.get_mut(k).and_then(Item::as_table_mut) {
                sub.decor_mut().set_prefix("\n");
            }
        }
    }
    parent.insert(name, Item::Table(table));
    Ok(doc.to_string())
}

/// The new text without `[<container>.<name>]` and its sub-tables.
pub fn remove_entry(text: &str, container: &str, name: &str) -> Result<String, TomlError> {
    let mut doc: DocumentMut = text
        .parse()
        .map_err(|e: toml_edit::TomlError| TomlError::Parse(e.to_string()))?;
    let parent = doc
        .get_mut(container)
        .and_then(Item::as_table_mut)
        .ok_or(TomlError::Missing)?;
    parent.remove(name).ok_or(TomlError::Missing)?;
    Ok(doc.to_string())
}

/// Whether `[<container>.<name>]` exists.
pub fn has_entry(text: &str, container: &str, name: &str) -> Result<bool, TomlError> {
    let doc: DocumentMut = text
        .parse()
        .map_err(|e: toml_edit::TomlError| TomlError::Parse(e.to_string()))?;
    Ok(doc
        .get(container)
        .and_then(Item::as_table_like)
        .is_some_and(|t| t.contains_key(name)))
}

/// The entry alone, as it would appear in a new file, for printing and for
/// the reject list.
pub fn preview(container: &str, fields: &[(String, Field)]) -> String {
    set_entry("", container, "entry", fields, false).unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn f(k: &str, v: Field) -> (String, Field) {
        (k.to_string(), v)
    }

    #[test]
    fn new_file_has_no_leading_blank_line() {
        let out = set_entry(
            "",
            "mcp_servers",
            "fs",
            &[
                f("command", Field::Str("npx".into())),
                f("args", Field::Strings(vec!["-y".into(), "pkg".into()])),
            ],
            false,
        )
        .unwrap();
        assert_eq!(
            out,
            "[mcp_servers.fs]\ncommand = \"npx\"\nargs = [\"-y\", \"pkg\"]\n"
        );
    }

    #[test]
    fn sub_tables_get_their_own_header() {
        let out = set_entry(
            "",
            "mcp_servers",
            "x",
            &[
                f("url", Field::Str("https://h/mcp".into())),
                f(
                    "env_http_headers",
                    Field::Table(vec![("X-Key".into(), "XK".into())]),
                ),
            ],
            false,
        )
        .unwrap();
        assert_eq!(
            out,
            "[mcp_servers.x]\nurl = \"https://h/mcp\"\n\n[mcp_servers.x.env_http_headers]\nX-Key = \"XK\"\n"
        );
    }

    #[test]
    fn existing_entry_is_refused_or_replaced_and_comments_survive() {
        let text = "# mine\nmodel = \"o3\" # inline\n\n[mcp_servers.a]\ncommand = \"a\"\n";
        let fields = [f("command", Field::Str("b".into()))];
        assert_eq!(
            set_entry(text, "mcp_servers", "a", &fields, false),
            Err(TomlError::Exists { same: false })
        );
        assert_eq!(
            set_entry(
                text,
                "mcp_servers",
                "a",
                &[f("command", Field::Str("a".into()))],
                true
            ),
            Err(TomlError::Exists { same: true })
        );
        let out = set_entry(text, "mcp_servers", "a", &fields, true).unwrap();
        assert!(
            out.starts_with("# mine\nmodel = \"o3\" # inline\n"),
            "{out}"
        );
        assert!(out.contains("[mcp_servers.a]\ncommand = \"b\"\n"), "{out}");
    }

    #[test]
    fn removes_an_entry_and_its_sub_tables() {
        let text = "x = 1\n\n[mcp_servers.a]\nurl = \"u\"\n\n[mcp_servers.a.env_http_headers]\nK = \"V\"\n\n[mcp_servers.b]\ncommand = \"b\"\n";
        let out = remove_entry(text, "mcp_servers", "a").unwrap();
        assert_eq!(out, "x = 1\n\n[mcp_servers.b]\ncommand = \"b\"\n");
        assert_eq!(
            remove_entry(text, "mcp_servers", "z"),
            Err(TomlError::Missing)
        );
        assert!(has_entry(text, "mcp_servers", "b").unwrap());
    }

    #[test]
    fn bad_toml_and_non_table_container_are_refused() {
        assert!(matches!(
            set_entry("a = ", "mcp_servers", "x", &[], false),
            Err(TomlError::Parse(_))
        ));
        assert!(matches!(
            set_entry("mcp_servers = 1\n", "mcp_servers", "x", &[], false),
            Err(TomlError::NotTable(_))
        ));
    }
}
