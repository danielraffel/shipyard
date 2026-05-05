use std::collections::BTreeMap;
use std::io::Write;

use serde::Serialize;
use serde_json::Value;

/// Stable JSON schema version for structured CLI output.
pub const SCHEMA_VERSION: u32 = 1;

/// Write a structured JSON envelope matching Shipyard's current CLI contract.
pub fn write_json_envelope<W: Write>(
    writer: &mut W,
    command: &str,
    data: BTreeMap<String, Value>,
) -> Result<(), Box<dyn std::error::Error>> {
    let mut root = serde_json::Map::new();
    root.insert("schema_version".to_owned(), Value::from(SCHEMA_VERSION));
    root.insert("command".to_owned(), Value::from(command.to_owned()));
    for (key, value) in data {
        root.insert(key, value);
    }
    serde_json::to_writer_pretty(&mut *writer, &Value::Object(root))?;
    writer.write_all(b"\n")?;
    Ok(())
}

/// Write pretty JSON without the Shipyard command envelope.
pub fn write_pretty_json<W: Write, T: Serialize>(
    writer: &mut W,
    value: &T,
) -> Result<(), Box<dyn std::error::Error>> {
    serde_json::to_writer_pretty(&mut *writer, value)?;
    writer.write_all(b"\n")?;
    Ok(())
}
