use serde::de::DeserializeOwned;
use serde_json::{Map, Value};

use crate::source::error::SourceError;

const ENV_PREFIX: &str = "KAIRO_SOURCES_";

/// One provider's `[sources.<name>]` table, with `enabled` already removed.
pub struct Settings<T> {
    pub enabled: bool,
    pub value: T,
}

/// Read one provider table from the process environment.
///
/// A missing table yields `T::default()` and leaves the provider enabled. Unknown keys are
/// rejected by the provider's `#[serde(deny_unknown_fields)]` config struct.
pub fn read<T>(name: &str, table: Option<&Value>) -> Result<Settings<T>, SourceError>
where
    T: DeserializeOwned + Default,
{
    read_with(name, table, std::env::vars())
}

/// The testable form of [`read`]: environment overrides come from the caller's iterator.
#[doc(hidden)]
pub fn read_with<T, I>(
    name: &str,
    table: Option<&Value>,
    env: I,
) -> Result<Settings<T>, SourceError>
where
    T: DeserializeOwned + Default,
    I: IntoIterator<Item = (String, String)>,
{
    let mut object = match table {
        None => Map::new(),
        Some(Value::Object(object)) => object.clone(),
        Some(_) => return Err(config_error(name, "must be a table")),
    };
    let enabled = match object.remove("enabled") {
        None => true,
        Some(Value::Bool(enabled)) => enabled,
        Some(_) => return Err(config_error(name, "enabled must be true or false")),
    };
    let prefix = env_prefix(name);
    let overrides: Vec<(String, String)> = env
        .into_iter()
        .filter_map(|(key, value)| {
            let key = key.strip_prefix(&prefix)?;
            (!key.is_empty()).then(|| (field_name(key), value))
        })
        .collect();
    let mut strings = object.clone();
    let mut typed = object;
    for (field, value) in overrides {
        strings.insert(field.clone(), Value::String(value.clone()));
        typed.insert(field, serde_json::from_str(&value).unwrap_or(Value::String(value)));
    }
    match serde_json::from_value(Value::Object(typed)) {
        Ok(value) => Ok(Settings { enabled, value }),
        Err(error) => serde_json::from_value(Value::Object(strings))
            .map(|value| Settings { enabled, value })
            .map_err(|_| config_error(name, error)),
    }
}

fn config_error(name: &str, message: impl std::fmt::Display) -> SourceError {
    SourceError::fault(&format!("sources.{name}"), message)
}

fn env_prefix(name: &str) -> String {
    let mut prefix = String::from(ENV_PREFIX);
    for character in name.chars() {
        if character.is_ascii_alphanumeric() {
            prefix.push(character.to_ascii_uppercase());
        } else {
            prefix.push('_');
        }
    }
    prefix.push('_');
    prefix
}

fn field_name(key: &str) -> String {
    let mut parts = key.split('_').filter(|part| !part.is_empty());
    let mut field = parts.next().unwrap_or_default().to_ascii_lowercase();
    for part in parts {
        let mut characters = part.chars();
        if let Some(first) = characters.next() {
            field.extend(first.to_uppercase());
            field.push_str(&characters.as_str().to_ascii_lowercase());
        }
    }
    field
}
