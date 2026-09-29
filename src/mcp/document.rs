use super::Format;
use miette::{IntoDiagnostic, Result, bail};
use serde_json::{Map, Value};
use toml_edit::{DocumentMut, Item, Table, TableLike};

pub(super) enum Document {
    Json(Value),
    Toml(DocumentMut),
}

fn json_table<'a>(
    value: &'a mut Value,
    root: &[String],
    create: bool,
) -> Result<Option<&'a mut Map<String, Value>>> {
    let Some(table) = value.as_object_mut() else {
        bail!("MCP root contains a non-object");
    };
    if let Some((head, tail)) = root.split_first() {
        if !table.contains_key(head) {
            if !create {
                return Ok(None);
            }
            table.insert(head.clone(), Value::Object(Map::new()));
        }
        json_table(table.get_mut(head).expect("present key"), tail, create)
    } else {
        Ok(Some(table))
    }
}

fn toml_table<'a>(
    table: &'a mut dyn TableLike,
    root: &[String],
    create: bool,
) -> Result<Option<&'a mut dyn TableLike>> {
    if let Some((head, tail)) = root.split_first() {
        if !table.contains_key(head) {
            if !create {
                return Ok(None);
            }
            let mut child = Table::new();
            child.set_implicit(true);
            table.insert(head, Item::Table(child));
        }
        let child = table
            .get_mut(head)
            .and_then(Item::as_table_like_mut)
            .ok_or_else(|| miette::miette!("MCP root contains a non-table at {head}"))?;
        toml_table(child, tail, create)
    } else {
        Ok(Some(table))
    }
}

impl Document {
    pub(super) fn parse(format: &Format, bytes: Option<&[u8]>) -> Result<Self> {
        let text = std::str::from_utf8(bytes.unwrap_or(b"")).into_diagnostic()?;
        Ok(match format {
            Format::Json => Self::Json(if bytes.is_none() {
                serde_json::json!({})
            } else {
                serde_json::from_str(text).into_diagnostic()?
            }),
            Format::Toml => Self::Toml(text.parse().into_diagnostic()?),
        })
    }
    pub(super) fn get(&self, root: &[String], key: &str) -> Result<Option<Value>> {
        match self {
            Self::Json(value) => {
                let mut value = value;
                for segment in root {
                    let object = value
                        .as_object()
                        .ok_or_else(|| miette::miette!("MCP root contains a non-object"))?;
                    let Some(next) = object.get(segment) else {
                        return Ok(None);
                    };
                    value = next;
                }
                Ok(value
                    .as_object()
                    .ok_or_else(|| miette::miette!("MCP root is not an object"))?
                    .get(key)
                    .cloned())
            }
            Self::Toml(doc) => {
                let mut table: &dyn TableLike = doc.as_table();
                for segment in root {
                    let Some(item) = table.get(segment) else {
                        return Ok(None);
                    };
                    table = item
                        .as_table_like()
                        .ok_or_else(|| miette::miette!("MCP root is not a table"))?;
                }
                table
                    .get(key)
                    .map(|item| {
                        let mut wrapper = DocumentMut::new();
                        wrapper["entry"] = item.clone();
                        let value: Value =
                            toml_edit::de::from_str(&wrapper.to_string()).into_diagnostic()?;
                        Ok(value["entry"].clone())
                    })
                    .transpose()
            }
        }
    }
    pub(super) fn set(&mut self, root: &[String], key: &str, value: &Value) -> Result<()> {
        if self.get(root, key)?.as_ref() == Some(value) {
            return Ok(());
        }
        match self {
            Self::Json(doc) => {
                json_table(doc, root, true)?
                    .expect("created root")
                    .insert(key.into(), value.clone());
            }
            Self::Toml(doc) => {
                let table = toml_edit::ser::to_document(value).into_diagnostic()?;
                toml_table(doc.as_table_mut(), root, true)?
                    .expect("created root")
                    .insert(key, Item::Table(table.into_table()));
            }
        }
        Ok(())
    }
    pub(super) fn remove(&mut self, root: &[String], key: &str) -> Result<()> {
        match self {
            Self::Json(doc) => {
                if let Some(table) = json_table(doc, root, false)? {
                    table.remove(key);
                }
            }
            Self::Toml(doc) => {
                if let Some(table) = toml_table(doc.as_table_mut(), root, false)? {
                    table.remove(key);
                }
            }
        }
        Ok(())
    }
    pub(super) fn render(&self) -> Result<String> {
        match self {
            Self::Json(value) => Ok(serde_json::to_string_pretty(value).into_diagnostic()? + "\n"),
            Self::Toml(doc) => Ok(doc.to_string()),
        }
    }
    pub(super) fn is_empty(&self) -> bool {
        match self {
            Self::Json(value) => value.as_object().is_some_and(Map::is_empty),
            Self::Toml(doc) => doc.is_empty(),
        }
    }
}
