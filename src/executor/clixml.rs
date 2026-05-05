//! Best-effort PowerShell CLIXML stderr decoding.
//!
//! Windows PowerShell can serialize non-stdout streams as CLIXML when
//! relayed over SSH. This decoder extracts human-readable error and
//! warning text while preserving the original text on malformed input.

const CLIXML_SENTINEL: &str = "#< CLIXML";
const MAX_DECODED_CHARS: usize = 800;

/// Return true when `text` starts with a CLIXML stream header after
/// leading whitespace.
#[must_use]
pub fn is_clixml(text: &str) -> bool {
    text.trim_start().starts_with(CLIXML_SENTINEL)
}

/// Decode a CLIXML stderr payload when possible.
///
/// Text that appears before the first CLIXML sentinel is preserved and
/// joined with decoded envelope messages. If no sentinel exists, or if
/// the envelope is malformed enough that no useful text can be recovered,
/// the original text is returned unchanged.
#[must_use]
pub fn maybe_decode_clixml(text: &str) -> String {
    if !is_clixml(text) && !text.contains(CLIXML_SENTINEL) {
        return text.to_owned();
    }

    let (prefix, xml_blob) = split_once_sentinel(text);
    let messages = extract_messages_from_blob(xml_blob);
    let decoded = dedupe_and_limit(messages);
    let parts = [prefix.trim(), decoded.trim()]
        .into_iter()
        .filter(|part| !part.is_empty())
        .collect::<Vec<_>>();

    if parts.is_empty() {
        text.to_owned()
    } else {
        parts.join("\n")
    }
}

fn split_once_sentinel(text: &str) -> (&str, &str) {
    let Some(index) = text.find(CLIXML_SENTINEL) else {
        return (text, "");
    };
    let prefix = &text[..index];
    let blob = text[index + CLIXML_SENTINEL.len()..].trim_start();
    (prefix, blob)
}

fn extract_messages_from_blob(xml_blob: &str) -> Vec<String> {
    split_objs(xml_blob)
        .into_iter()
        .flat_map(extract_messages)
        .collect()
}

fn split_objs(xml_blob: &str) -> Vec<&str> {
    let mut documents = Vec::new();
    let mut start = 0;

    while start < xml_blob.len() {
        let Some(open_rel) = xml_blob[start..].find("<Objs") else {
            break;
        };
        let open = start + open_rel;
        let Some(close_rel) = xml_blob[open..].find("</Objs>") else {
            break;
        };
        let close = open + close_rel + "</Objs>".len();
        documents.push(&xml_blob[open..close]);
        start = close;
    }

    documents
}

fn extract_messages(document: &str) -> Vec<String> {
    let mut messages = Vec::new();
    let mut start = 0;

    while let Some(tag_rel) = document[start..].find("<S") {
        let tag_start = start + tag_rel;
        let Some(tag_end_rel) = document[tag_start..].find('>') else {
            break;
        };
        let tag_end = tag_start + tag_end_rel;
        let tag = &document[tag_start..=tag_end];
        let content_start = tag_end + 1;
        let Some(close_rel) = document[content_start..].find("</S>") else {
            break;
        };
        let content_end = content_start + close_rel;

        if is_message_tag(tag) {
            messages.push(decode_powershell_escapes(&decode_xml_entities(
                &document[content_start..content_end],
            )));
        }

        start = content_end + "</S>".len();
    }

    messages
}

fn is_message_tag(tag: &str) -> bool {
    tag.contains(r#"S="Error""#)
        || tag.contains(r#"S="Warning""#)
        || tag.contains(r#"N="Message""#)
        || tag.contains(r#"N="Exception""#)
        || tag.contains(r#"N="FullyQualifiedErrorId""#)
}

fn decode_xml_entities(raw: &str) -> String {
    raw.replace("&lt;", "<")
        .replace("&gt;", ">")
        .replace("&quot;", "\"")
        .replace("&apos;", "'")
        .replace("&amp;", "&")
}

fn decode_powershell_escapes(raw: &str) -> String {
    let mut output = String::with_capacity(raw.len());
    let mut index = 0;

    while index < raw.len() {
        let rest = &raw[index..];
        if rest.starts_with("_x") && rest.len() >= 7 && &rest[6..7] == "_" {
            let hex = &rest[2..6];
            if let Ok(value) = u32::from_str_radix(hex, 16)
                && let Some(ch) = char::from_u32(value)
            {
                output.push(ch);
                index += 7;
                continue;
            }
        }
        let Some(ch) = rest.chars().next() else {
            break;
        };
        output.push(ch);
        index += ch.len_utf8();
    }

    output
}

fn dedupe_and_limit(messages: Vec<String>) -> String {
    let mut deduped: Vec<String> = Vec::new();

    for message in messages {
        let stripped = message.trim();
        if stripped.is_empty() {
            continue;
        }
        if deduped.last().is_some_and(|previous| previous == stripped) {
            continue;
        }
        deduped.push(stripped.to_owned());
    }

    let joined = deduped.join("\n");
    if joined.chars().count() <= MAX_DECODED_CHARS {
        return joined;
    }

    let tail = joined
        .chars()
        .rev()
        .take(MAX_DECODED_CHARS - 1)
        .collect::<Vec<_>>()
        .into_iter()
        .rev()
        .collect::<String>();
    format!("...{tail}")
}

#[cfg(test)]
mod tests {
    use super::{is_clixml, maybe_decode_clixml};

    #[test]
    fn plain_text_without_sentinel_is_unchanged() {
        assert_eq!(
            maybe_decode_clixml("just a regular error"),
            "just a regular error"
        );
    }

    #[test]
    fn detects_clixml_after_leading_whitespace() {
        assert!(is_clixml("  #< CLIXML\n<Objs></Objs>"));
        assert!(!is_clixml("error\n#< CLIXML\n<Objs></Objs>"));
    }

    #[test]
    fn decodes_error_stream_text() {
        let envelope = concat!(
            "#< CLIXML\n",
            r#"<Objs xmlns="http://schemas.microsoft.com/powershell/2004/04">"#,
            r#"<S S="Error">inner PS error</S>"#,
            "</Objs>"
        );
        assert_eq!(maybe_decode_clixml(envelope), "inner PS error");
    }

    #[test]
    fn decodes_message_properties_and_xml_entities() {
        let envelope = concat!(
            "#< CLIXML\n",
            "<Objs>",
            r#"<S N="Message">bad &lt;path&gt; &amp; reason</S>"#,
            "</Objs>"
        );
        assert_eq!(maybe_decode_clixml(envelope), "bad <path> & reason");
    }

    #[test]
    fn decodes_powershell_restricted_character_escapes() {
        let envelope = concat!(
            "#< CLIXML\n",
            "<Objs>",
            r#"<S S="Error">line1_x000D__x000A_line2_x0009_tab</S>"#,
            "</Objs>"
        );
        assert_eq!(maybe_decode_clixml(envelope), "line1\r\nline2\ttab");
    }

    #[test]
    fn surfaces_pre_sentinel_stderr_with_progress_only_body() {
        let envelope = concat!(
            "error: could not open 'C:/Users/alice/shipyard.bundle'\n",
            "#< CLIXML\n",
            r#"<Objs Version="1.1.0.1">"#,
            r#"<Obj S="progress"><TN><T>PSCustomObject</T></TN></Obj>"#,
            "</Objs>"
        );
        let decoded = maybe_decode_clixml(envelope);
        assert!(decoded.contains("could not open"));
        assert!(!decoded.contains("#< CLIXML"));
        assert!(!decoded.contains("<Objs"));
    }

    #[test]
    fn joins_pre_sentinel_stderr_and_error_stream() {
        let envelope = concat!(
            "error: outer context\n",
            "#< CLIXML\n",
            r#"<Objs xmlns="http://schemas.microsoft.com/powershell/2004/04">"#,
            r#"<S S="Error">inner PS error</S>"#,
            "</Objs>"
        );
        assert_eq!(
            maybe_decode_clixml(envelope),
            "error: outer context\ninner PS error"
        );
    }

    #[test]
    fn consecutive_duplicate_messages_are_deduped() {
        let envelope = concat!(
            "#< CLIXML\n",
            "<Objs>",
            r#"<S S="Error">same</S>"#,
            r#"<S N="Message">same</S>"#,
            r#"<S S="Warning">different</S>"#,
            "</Objs>"
        );
        assert_eq!(maybe_decode_clixml(envelope), "same\ndifferent");
    }
}
