//! The wire layer: OpenAI and Anthropic message shapes, the `compress` tool,
//! and the ref tags the model cites.
//!
//! Ported from billion-context's `kernel/src/wire/`, `compress-tools.ts` and
//! `render-refs.ts`. The proxy sits between the agent and the provider, so it
//! has to speak both dialects: decode a request into [`CoreMessage`]s, let the
//! kernel work, and encode the result back into the same dialect.
//!
//! Two things here are load-bearing and easy to get wrong:
//!
//! 1. **Ref tags.** The model can only cite a range if it can see the refs, so
//!    every addressable message gets a `<acp tokens="2.1K" type="tool:bash">
//!    m00175</acp>` prefix, and the system prompt explains what they are. The
//!    prompt also tells the model never to echo them.
//! 2. **Tool pairing.** A tool result must never be separated from the call
//!    that produced it. Decoding splits an assistant turn into separate
//!    tool-call messages; encoding has to merge them back, or the provider
//!    rejects the request.

use serde_json::{json, Map, Value};

use super::types::{ContentType, CoreMessage, Role};

/// The tool the model calls to fold a range.
pub const COMPRESS_TOOL: &str = "compress";
/// The tool the model calls to bring a folded range back.
pub const DECOMPRESS_TOOL: &str = "decompress";
/// The tool the model calls to read a stored tool result.
pub const RETRIEVE_TOOL: &str = "acp_retrieve";

const COMPRESS_DESCRIPTION: &str = "Replace consumed conversation ranges with self-contained summaries you write, identified by their refs. content = ONE plain string holding ALL ranges — each block starts with its 'm00150–m00220 optional topic' header line followed by that block's summary. A range may also name blocks ('b3–b7') to distil existing summaries into one. Batch multiple ranges into ONE call — do not split into one call per range. Use when content is genuinely consumed.";

const COMPRESS_CONTENT_DESCRIPTION: &str = "ONE plain string holding ALL ranges to compress — each block starts with its 'm00150–m00220 optional topic' header line followed by that block's summary markdown. A range of message refs folds raw messages; a range of block refs ('b3–b7') distils the summaries those blocks hold. Batch multiple ranges into ONE call — do not split into one call per range.";

/// The system-prompt section that explains the ref tags.
///
/// Without this the model sees `<acp ...>m00175</acp>` and either ignores it or
/// echoes it back. Both are failures: the first means nothing ever gets folded,
/// the second pollutes the transcript with metadata.
pub const ACP_TAGS_SECTION: &str = "ACP TAGS

Addressable messages in the conversation are annotated with a <acp tokens=\"2.1K\" type=\"tool:bash\">m00175</acp> tag showing its reference ID, approximate token size, and content type. These tags are system metadata injected by the proxy. NEVER echo, repeat, or reference these XML tags in your responses — the tags must not appear in your output. Use only the ref ID (e.g. m00005) inside compress calls, never the XML wrapper. The token size is approximate — treat it as a relative guide, not an exact count.";

fn compress_parameters() -> Value {
    json!({
        "type": "object",
        "properties": {
            "content": {
                "type": "string",
                "description": COMPRESS_CONTENT_DESCRIPTION,
            }
        }
    })
}

/// The `compress` tool in OpenAI function-calling shape.
pub fn compress_tool_openai() -> Value {
    json!({
        "type": "function",
        "function": {
            "name": COMPRESS_TOOL,
            "description": COMPRESS_DESCRIPTION,
            "parameters": compress_parameters(),
        }
    })
}

/// The `compress` tool in Anthropic tool shape.
pub fn compress_tool_anthropic() -> Value {
    json!({
        "name": COMPRESS_TOOL,
        "description": COMPRESS_DESCRIPTION,
        "input_schema": compress_parameters(),
    })
}

/// The `decompress` tool in OpenAI function-calling shape.
pub fn decompress_tool_openai() -> Value {
    json!({
        "type": "function",
        "function": {
            "name": DECOMPRESS_TOOL,
            "description": "Restore a compressed block's original messages to the live context. Use when a summary is not enough and you need the exact bytes back.",
            "parameters": {
                "type": "object",
                "properties": {
                    "blockId": {
                        "type": "string",
                        "description": "Block ID to decompress (e.g. b5)",
                    }
                },
                "required": ["blockId"]
            }
        }
    })
}

/// The `decompress` tool in Anthropic tool shape.
pub fn decompress_tool_anthropic() -> Value {
    json!({
        "name": DECOMPRESS_TOOL,
        "description": "Restore a compressed block's original messages to the live context. Use when a summary is not enough and you need the exact bytes back.",
        "input_schema": {
            "type": "object",
            "properties": {
                "blockId": {
                    "type": "string",
                    "description": "Block ID to decompress (e.g. b5)",
                }
            },
            "required": ["blockId"]
        }
    })
}

/// The `acp_retrieve` tool in OpenAI function-calling shape.
pub fn retrieve_tool_openai() -> Value {
    json!({
        "type": "function",
        "function": {
            "name": RETRIEVE_TOOL,
            "description": "Read the full original of a tool result that was stored to save context. The message shows a [acp-stored <ref> ...] pointer naming the ref.",
            "parameters": {
                "type": "object",
                "properties": {
                    "ref": {
                        "type": "string",
                        "description": "The ref named in the [acp-stored ...] pointer (e.g. m00042)",
                    }
                },
                "required": ["ref"]
            }
        }
    })
}

/// The `acp_retrieve` tool in Anthropic tool shape.
pub fn retrieve_tool_anthropic() -> Value {
    json!({
        "name": RETRIEVE_TOOL,
        "description": "Read the full original of a tool result that was stored to save context. The message shows a [acp-stored <ref> ...] pointer naming the ref.",
        "input_schema": {
            "type": "object",
            "properties": {
                "ref": {
                    "type": "string",
                    "description": "The ref named in the [acp-stored ...] pointer (e.g. m00042)",
                }
            },
            "required": ["ref"]
        }
    })
}

/// The `<acp>` tag that makes a message addressable.
pub fn ref_tag(message: &CoreMessage, reference: &str) -> String {
    let kind = match message.content_type {
        ContentType::Text => "text".to_string(),
        ContentType::Reasoning => "reasoning".to_string(),
        ContentType::ToolCall => format!(
            "tool:{}",
            message.tool_name.as_deref().unwrap_or("unknown")
        ),
        ContentType::ToolResult => format!(
            "tool-result:{}",
            message.tool_name.as_deref().unwrap_or("unknown")
        ),
    };
    let tokens = message.tokens();
    let tokens = if tokens >= 1000 {
        format!("{:.1}K", tokens as f64 / 1000.0)
    } else {
        tokens.to_string()
    };
    format!("<acp tokens=\"{tokens}\" type=\"{kind}\">{reference}</acp>")
}

/// Prefix a message's text with its ref tag.
pub fn with_ref_tag(message: &CoreMessage, reference: &str) -> String {
    format!("{} {}", ref_tag(message, reference), message.text)
}

/// Parse the `compress` tool's arguments into ranges.
///
/// The advertised shape is one plain string holding every range, each block
/// starting with its `m00150–m00220 topic` header line. Models being models,
/// this also accepts a JSON array of range objects, a JSON-encoded array inside
/// the string, and the flat single-range call. Anything it cannot read is
/// dropped rather than guessed at.
pub fn parse_compress_input(input: &Value) -> Vec<super::CompressRange> {
    let mut ranges = Vec::new();

    let content = input.get("content");
    match content {
        Some(Value::String(text)) => {
            // A JSON-encoded array is a common gateway artefact.
            if let Ok(Value::Array(items)) = serde_json::from_str::<Value>(text) {
                for item in &items {
                    if let Some(range) = range_from_object(item) {
                        ranges.push(range);
                    }
                }
                if !ranges.is_empty() {
                    return ranges;
                }
            }
            ranges.extend(parse_line_form(text));
        }
        Some(Value::Array(items)) => {
            for item in items {
                match item {
                    Value::String(text) => ranges.extend(parse_line_form(text)),
                    other => {
                        if let Some(range) = range_from_object(other) {
                            ranges.push(range);
                        }
                    }
                }
            }
        }
        _ => {}
    }

    // The flat single-range call: {startRef, endRef, summary}.
    if ranges.is_empty() {
        if let Some(range) = range_from_object(input) {
            ranges.push(range);
        }
    }
    ranges
}

fn range_from_object(value: &Value) -> Option<super::CompressRange> {
    let object = value.as_object()?;
    let pick = |keys: &[&str]| -> Option<String> {
        keys.iter()
            .find_map(|key| object.get(*key).and_then(Value::as_str))
            .map(str::to_string)
    };
    let start_ref = pick(&["startRef", "startId"])?;
    let end_ref = pick(&["endRef", "endId"])?;
    let summary = pick(&["summary"])?;
    Some(super::CompressRange {
        start_ref,
        end_ref,
        summary,
        topic: pick(&["topic"]).filter(|topic| !topic.trim().is_empty()),
    })
}

/// Parse the line form: a header line per block, then that block's summary.
fn parse_line_form(text: &str) -> Vec<super::CompressRange> {
    let mut ranges: Vec<super::CompressRange> = Vec::new();
    let mut current: Option<(String, String, Option<String>, Vec<&str>)> = None;

    for line in text.lines() {
        if let Some((start, end, topic)) = parse_header(line) {
            if let Some((start_ref, end_ref, topic, body)) = current.take() {
                push_range(&mut ranges, start_ref, end_ref, topic, &body);
            }
            current = Some((start, end, topic, Vec::new()));
            continue;
        }
        if let Some((_, _, _, body)) = current.as_mut() {
            body.push(line);
        }
    }
    if let Some((start_ref, end_ref, topic, body)) = current.take() {
        push_range(&mut ranges, start_ref, end_ref, topic, &body);
    }
    ranges
}

fn push_range(
    ranges: &mut Vec<super::CompressRange>,
    start_ref: String,
    end_ref: String,
    topic: Option<String>,
    body: &[&str],
) {
    let summary = body.join("\n").trim().to_string();
    if summary.is_empty() {
        return;
    }
    ranges.push(super::CompressRange {
        start_ref,
        end_ref,
        summary,
        topic,
    });
}

/// `m00150–m00220 optional topic`, with any of the dashes a model might emit.
///
/// A header may also name blocks — `b3–b7` — which is how the model asks for a
/// distillation. The prefix of the end ref is taken from the start ref, so a
/// header that opens with `b` and closes with a bare number still reads as a
/// block range.
fn parse_header(line: &str) -> Option<(String, String, Option<String>)> {
    let trimmed = line.trim();
    let dash = trimmed.find(['–', '—', '-'])?;
    let start = trimmed[..dash].trim();
    let prefix = ref_prefix(start)?;
    // The dash may be multi-byte, so advance by its own length, not by one.
    let dash_len = trimmed[dash..].chars().next()?.len_utf8();
    let rest = trimmed[dash + dash_len..]
        .trim_start_matches(['–', '—', '-'])
        .trim_start();
    let rest = rest.strip_prefix(prefix).unwrap_or(rest);
    let end_len = rest.chars().take_while(|ch| ch.is_ascii_digit()).count();
    if end_len == 0 {
        return None;
    }
    let end = &rest[..end_len];
    let topic = rest[end_len..].trim();
    Some((
        start.to_string(),
        format!("{prefix}{end}"),
        if topic.is_empty() {
            None
        } else {
            Some(topic.to_string())
        },
    ))
}

/// The prefix of a ref, if the text is one: `m` for a message, `b` for a block.
fn ref_prefix(text: &str) -> Option<char> {
    let mut chars = text.chars();
    let prefix = chars.next()?;
    if prefix != 'm' && prefix != 'b' {
        return None;
    }
    let digits: String = chars.collect();
    if digits.is_empty() || !digits.chars().all(|ch| ch.is_ascii_digit()) {
        return None;
    }
    Some(prefix)
}

/// Which dialect a request speaks.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Dialect {
    OpenAi,
    Anthropic,
}

/// Decode a request body into kernel messages.
pub fn decode(dialect: Dialect, body: &Value) -> Vec<CoreMessage> {
    match dialect {
        Dialect::OpenAi => decode_openai(body),
        Dialect::Anthropic => decode_anthropic(body),
    }
}

/// Encode kernel messages back into a request body, replacing `messages`.
pub fn encode(dialect: Dialect, body: &Value, messages: &[CoreMessage]) -> Value {
    match dialect {
        Dialect::OpenAi => encode_openai(body, messages),
        Dialect::Anthropic => encode_anthropic(body, messages),
    }
}

fn decode_openai(body: &Value) -> Vec<CoreMessage> {
    let mut out = Vec::new();
    let Some(messages) = body.get("messages").and_then(Value::as_array) else {
        return out;
    };
    for message in messages {
        let role = message.get("role").and_then(Value::as_str).unwrap_or("user");
        let text = message
            .get("content")
            .map(content_to_text)
            .unwrap_or_default();

        if let Some(calls) = message.get("tool_calls").and_then(Value::as_array) {
            if !text.trim().is_empty() {
                out.push(CoreMessage::text(Role::Assistant, text));
            }
            for call in calls {
                let id = call.get("id").and_then(Value::as_str).unwrap_or_default();
                let function = call.get("function");
                let name = function
                    .and_then(|f| f.get("name"))
                    .and_then(Value::as_str)
                    .unwrap_or("unknown");
                let arguments = function
                    .and_then(|f| f.get("arguments"))
                    .map(|value| match value {
                        Value::String(text) => text.clone(),
                        other => other.to_string(),
                    })
                    .unwrap_or_default();
                out.push(CoreMessage {
                    id: String::new(),
                    role: Role::Assistant,
                    content_type: ContentType::ToolCall,
                    text: arguments,
                    tool_name: Some(name.to_string()),
                    tool_call_id: Some(id.to_string()),
                    blocks: None,
                });
            }
            continue;
        }

        let parsed_role = match role {
            "system" | "developer" => Role::System,
            "assistant" => Role::Assistant,
            "tool" => Role::Tool,
            _ => Role::User,
        };
        let content_type = if parsed_role == Role::Tool {
            ContentType::ToolResult
        } else {
            ContentType::Text
        };
        out.push(CoreMessage {
            id: String::new(),
            role: parsed_role,
            content_type,
            text,
            tool_name: message
                .get("name")
                .and_then(Value::as_str)
                .map(str::to_string),
            tool_call_id: message
                .get("tool_call_id")
                .and_then(Value::as_str)
                .map(str::to_string),
            blocks: content_blocks(message),
        });
    }
    out
}

/// What goes back on the wire as this message's `content`.
///
/// The original array when the message still has one, so blocks the kernel
/// cannot read — an image — survive the round trip. The text projection
/// otherwise, which is what a message the kernel built or rewrote has.
fn content_of(message: &CoreMessage) -> Value {
    message
        .blocks
        .clone()
        .unwrap_or_else(|| Value::String(message.text.clone()))
}

/// The message's `content` array, when flattening it would lose something.
///
/// A plain string carries nothing the text projection loses, so it is not kept.
/// Neither is an array of text blocks: `content_to_text` joins those without
/// dropping a byte, and re-emitting the array would change the request bytes
/// for no gain — which is exactly what the provider's cache is keyed on.
///
/// An image has no `text` key, so it is dropped by the projection. That is the
/// case this exists for, and the only one.
fn content_blocks(message: &Value) -> Option<Value> {
    message.get("content").and_then(lossy_blocks)
}

/// The array, when at least one block in it survives no other way.
///
/// The test mirrors `content_to_text`'s own filter: a block contributes text if
/// it is a string, or carries `text`, or carries `content`. Anything else is
/// invisible to the kernel and has to be carried verbatim.
fn lossy_blocks(content: &Value) -> Option<Value> {
    let blocks = content.as_array()?;
    let lossy = blocks.iter().any(|block| {
        !(block.is_string() || block.get("text").is_some() || block.get("content").is_some())
    });
    lossy.then(|| Value::Array(blocks.clone()))
}

fn encode_openai(body: &Value, messages: &[CoreMessage]) -> Value {
    let mut out: Vec<Value> = Vec::new();
    let mut index = 0;

    while index < messages.len() {
        let message = &messages[index];
        match message.content_type {
            ContentType::ToolCall => {
                // Gather the whole run of calls into one assistant turn, and
                // fold in a preceding assistant text message if there is one.
                let mut calls = Vec::new();
                while index < messages.len()
                    && messages[index].content_type == ContentType::ToolCall
                {
                    let call = &messages[index];
                    calls.push(json!({
                        "id": call.tool_call_id.clone().unwrap_or_default(),
                        "type": "function",
                        "function": {
                            "name": call.tool_name.clone().unwrap_or_default(),
                            "arguments": call.text,
                        }
                    }));
                    index += 1;
                }
                let mut entry = Map::new();
                entry.insert("role".into(), json!("assistant"));
                entry.insert("content".into(), json!(Value::Null));
                entry.insert("tool_calls".into(), Value::Array(calls));
                if let Some(previous) = out.last_mut() {
                    if previous.get("role").and_then(Value::as_str) == Some("assistant")
                        && previous.get("tool_calls").is_none()
                    {
                        let text = previous.get("content").cloned().unwrap_or(json!(""));
                        entry.insert("content".into(), text);
                        out.pop();
                    }
                }
                out.push(Value::Object(entry));
            }
            ContentType::ToolResult => {
                out.push(json!({
                    "role": "tool",
                    "tool_call_id": message.tool_call_id.clone().unwrap_or_default(),
                    "content": content_of(message),
                }));
                index += 1;
            }
            _ => {
                out.push(json!({
                    "role": message.role.as_str(),
                    "content": content_of(message),
                }));
                index += 1;
            }
        }
    }

    let mut next = body.clone();
    if let Some(object) = next.as_object_mut() {
        object.insert("messages".into(), Value::Array(out));
    }
    next
}

fn decode_anthropic(body: &Value) -> Vec<CoreMessage> {
    let mut out = Vec::new();
    if let Some(system) = body.get("system") {
        let text = content_to_text(system);
        if !text.trim().is_empty() {
            out.push(CoreMessage::text(Role::System, text));
        }
    }
    let Some(messages) = body.get("messages").and_then(Value::as_array) else {
        return out;
    };
    for message in messages {
        let role = match message.get("role").and_then(Value::as_str) {
            Some("assistant") => Role::Assistant,
            _ => Role::User,
        };
        let Some(blocks) = message.get("content").and_then(Value::as_array) else {
            let text = message
                .get("content")
                .map(content_to_text)
                .unwrap_or_default();
            out.push(CoreMessage::text(role, text));
            continue;
        };
        for block in blocks {
            match block.get("type").and_then(Value::as_str) {
                Some("text") => out.push(CoreMessage::text(
                    role,
                    block.get("text").and_then(Value::as_str).unwrap_or_default(),
                )),
                Some("thinking") => out.push(CoreMessage {
                    id: String::new(),
                    role,
                    content_type: ContentType::Reasoning,
                    text: block
                        .get("thinking")
                        .and_then(Value::as_str)
                        .unwrap_or_default()
                        .to_string(),
                    tool_name: None,
                    tool_call_id: None,
                    blocks: None,
                }),
                Some("tool_use") => out.push(CoreMessage {
                    id: String::new(),
                    role: Role::Assistant,
                    content_type: ContentType::ToolCall,
                    text: block
                        .get("input")
                        .map(|input| input.to_string())
                        .unwrap_or_default(),
                    tool_name: block
                        .get("name")
                        .and_then(Value::as_str)
                        .map(str::to_string),
                    tool_call_id: block
                        .get("id")
                        .and_then(Value::as_str)
                        .map(str::to_string),
                    blocks: None,
                }),
                Some("tool_result") => out.push(CoreMessage {
                    id: String::new(),
                    role: Role::User,
                    content_type: ContentType::ToolResult,
                    text: block
                        .get("content")
                        .map(content_to_text)
                        .unwrap_or_default(),
                    tool_name: None,
                    tool_call_id: block
                        .get("tool_use_id")
                        .and_then(Value::as_str)
                        .map(str::to_string),
                    blocks: block.get("content").and_then(lossy_blocks),
                }),
                // A block the kernel cannot read. It carries no text, so it
                // rides along as an empty text message whose `blocks` is the
                // block itself, and encode puts it back in place.
                Some("image") => out.push(CoreMessage {
                    id: String::new(),
                    role,
                    content_type: ContentType::Text,
                    text: String::new(),
                    tool_name: None,
                    tool_call_id: None,
                    blocks: Some(Value::Array(vec![block.clone()])),
                }),
                _ => {}
            }
        }
    }
    out
}

fn encode_anthropic(body: &Value, messages: &[CoreMessage]) -> Value {
    let mut system_parts: Vec<String> = Vec::new();
    let mut out: Vec<Value> = Vec::new();
    let mut index = 0;

    while index < messages.len() {
        let message = &messages[index];
        if message.role == Role::System && message.content_type == ContentType::Text {
            system_parts.push(message.text.clone());
            index += 1;
            continue;
        }

        let role = if message.role == Role::Assistant {
            "assistant"
        } else {
            "user"
        };
        let mut blocks: Vec<Value> = Vec::new();

        // One turn's blocks: text, then any run of tool calls or results.
        while index < messages.len() {
            let current = &messages[index];
            let current_role = if current.role == Role::Assistant {
                "assistant"
            } else {
                "user"
            };
            if current_role != role || current.role == Role::System {
                break;
            }
            match current.content_type {
                ContentType::Text => match &current.blocks {
                    // The original blocks, in order, so an image the kernel
                    // could not read is still there when the turn goes out.
                    Some(stored) => {
                        blocks.extend(stored.as_array().cloned().unwrap_or_default())
                    }
                    None => blocks.push(json!({
                        "type": "text",
                        "text": current.text,
                    })),
                },
                ContentType::Reasoning => blocks.push(json!({
                    "type": "thinking",
                    "thinking": current.text,
                })),
                ContentType::ToolCall => blocks.push(json!({
                    "type": "tool_use",
                    "id": current.tool_call_id.clone().unwrap_or_default(),
                    "name": current.tool_name.clone().unwrap_or_default(),
                    "input": serde_json::from_str::<Value>(&current.text)
                        .unwrap_or_else(|_| json!({})),
                })),
                ContentType::ToolResult => blocks.push(json!({
                    "type": "tool_result",
                    "tool_use_id": current.tool_call_id.clone().unwrap_or_default(),
                    "content": content_of(current),
                })),
            }
            index += 1;
        }

        if !blocks.is_empty() {
            out.push(json!({ "role": role, "content": blocks }));
        }
    }

    let mut next = body.clone();
    if let Some(object) = next.as_object_mut() {
        object.insert("messages".into(), Value::Array(out));
        // The kernel's system messages came from this body's `system` field in
        // the first place, so they replace it rather than being appended to it.
        if !system_parts.is_empty() {
            object.insert("system".into(), json!(system_parts.join("\n\n")));
        }
    }
    next
}

/// Flatten a `content` field, which may be a string or an array of blocks.
fn content_to_text(value: &Value) -> String {
    match value {
        Value::String(text) => text.clone(),
        Value::Array(blocks) => blocks
            .iter()
            .filter_map(|block| match block {
                Value::String(text) => Some(text.clone()),
                other => other
                    .get("text")
                    .and_then(Value::as_str)
                    .map(str::to_string)
                    .or_else(|| {
                        other
                            .get("content")
                            .map(content_to_text)
                            .filter(|text| !text.is_empty())
                    }),
            })
            .collect::<Vec<_>>()
            .join("\n"),
        Value::Null => String::new(),
        other => other.to_string(),
    }
}

/// Which dialect a request path speaks.
pub fn dialect_for_path(path: &str) -> Dialect {
    if path.contains("/v1/messages") {
        Dialect::Anthropic
    } else {
        Dialect::OpenAi
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_line_form_parses_one_block_per_header() {
        let text = "m00150–m00220 auth refactor\nchose X over Y because Z\n\nm00230-m00250\nsecond summary";
        let ranges = parse_line_form(text);
        assert_eq!(ranges.len(), 2);
        assert_eq!(ranges[0].start_ref, "m00150");
        assert_eq!(ranges[0].end_ref, "m00220");
        assert_eq!(ranges[0].topic.as_deref(), Some("auth refactor"));
        assert_eq!(ranges[0].summary, "chose X over Y because Z");
        assert_eq!(ranges[1].start_ref, "m00230");
        assert_eq!(ranges[1].end_ref, "m00250");
        assert_eq!(ranges[1].topic, None);
        assert_eq!(ranges[1].summary, "second summary");
    }

    #[test]
    fn a_plain_hyphen_header_is_accepted() {
        let ranges = parse_line_form("m00001-m00005 topic\nsummary");
        assert_eq!(ranges.len(), 1);
        assert_eq!(ranges[0].end_ref, "m00005");
    }

    #[test]
    fn a_block_with_no_summary_is_dropped_rather_than_guessed() {
        let ranges = parse_line_form("m00001–m00005 topic\n\nm00006–m00009\nreal summary");
        assert_eq!(ranges.len(), 1);
        assert_eq!(ranges[0].start_ref, "m00006");
    }

    #[test]
    fn a_block_header_becomes_a_block_range() {
        let ranges = parse_line_form("b3–b7 distilled
the summaries, condensed");
        assert_eq!(ranges.len(), 1);
        assert_eq!(ranges[0].start_ref, "b3");
        assert_eq!(ranges[0].end_ref, "b7");
        assert_eq!(ranges[0].topic.as_deref(), Some("distilled"));
    }

    #[test]
    fn a_block_header_with_a_bare_end_number_still_reads_as_blocks() {
        // Models drop the prefix on the right-hand side constantly. Taking it
        // from the start ref is what keeps `b3–7` a block range rather than a
        // message range that silently names the wrong thing.
        let ranges = parse_line_form("b3-7
the summaries, condensed");
        assert_eq!(ranges.len(), 1);
        assert_eq!(ranges[0].start_ref, "b3");
        assert_eq!(ranges[0].end_ref, "b7");
    }

    #[test]
    fn a_json_array_of_range_objects_is_accepted() {
        let input = json!({
            "content": [
                {"startRef": "m00001", "endRef": "m00005", "summary": "one", "topic": "t"},
                {"startId": "m00006", "endId": "m00009", "summary": "two"}
            ]
        });
        let ranges = parse_compress_input(&input);
        assert_eq!(ranges.len(), 2);
        assert_eq!(ranges[0].topic.as_deref(), Some("t"));
        assert_eq!(ranges[1].start_ref, "m00006");
    }

    #[test]
    fn a_json_encoded_array_inside_the_string_is_accepted() {
        let inner = json!([{"startRef": "m00001", "endRef": "m00005", "summary": "one"}]);
        let input = json!({ "content": inner.to_string() });
        let ranges = parse_compress_input(&input);
        assert_eq!(ranges.len(), 1);
        assert_eq!(ranges[0].summary, "one");
    }

    #[test]
    fn the_flat_single_range_call_is_accepted() {
        let input = json!({"startRef": "m00001", "endRef": "m00005", "summary": "one"});
        let ranges = parse_compress_input(&input);
        assert_eq!(ranges.len(), 1);
    }

    #[test]
    fn unparseable_input_yields_no_ranges_rather_than_a_guess() {
        assert!(parse_compress_input(&json!({"content": "no headers here"})).is_empty());
        assert!(parse_compress_input(&json!({})).is_empty());
        assert!(parse_compress_input(&json!(null)).is_empty());
    }

    #[test]
    fn a_ref_tag_names_the_ref_its_size_and_its_kind() {
        let message = CoreMessage {
            id: "x".into(),
            role: Role::Assistant,
            content_type: ContentType::ToolCall,
            text: "{}".into(),
            tool_name: Some("bash".into()),
            tool_call_id: Some("c1".into()),
            blocks: None,
        };
        assert_eq!(
            ref_tag(&message, "m00175"),
            "<acp tokens=\"1\" type=\"tool:bash\">m00175</acp>"
        );
    }

    #[test]
    fn a_large_message_reports_its_size_in_k() {
        let message = CoreMessage::text(Role::User, "x".repeat(8400));
        assert!(ref_tag(&message, "m00001").contains("tokens=\"2.1K\""));
    }

    #[test]
    fn an_openai_request_decodes_into_kernel_messages() {
        let body = json!({
            "model": "m",
            "messages": [
                {"role": "system", "content": "be good"},
                {"role": "user", "content": "run it"},
                {"role": "assistant", "content": null, "tool_calls": [
                    {"id": "c1", "type": "function", "function": {"name": "exec", "arguments": "{\"cmd\":\"ls\"}"}}
                ]},
                {"role": "tool", "tool_call_id": "c1", "content": "a.txt"}
            ]
        });
        let messages = decode_openai(&body);
        assert_eq!(messages.len(), 4);
        assert_eq!(messages[0].role, Role::System);
        assert_eq!(messages[2].content_type, ContentType::ToolCall);
        assert_eq!(messages[2].tool_name.as_deref(), Some("exec"));
        assert_eq!(messages[3].content_type, ContentType::ToolResult);
        assert_eq!(messages[3].tool_call_id.as_deref(), Some("c1"));
    }

    #[test]
    fn an_openai_request_round_trips_through_encode() {
        let body = json!({
            "model": "m",
            "messages": [
                {"role": "user", "content": "run it"},
                {"role": "assistant", "content": null, "tool_calls": [
                    {"id": "c1", "type": "function", "function": {"name": "exec", "arguments": "{}"}}
                ]},
                {"role": "tool", "tool_call_id": "c1", "content": "a.txt"}
            ]
        });
        let messages = decode_openai(&body);
        let encoded = encode_openai(&body, &messages);
        let out = encoded["messages"].as_array().unwrap();
        assert_eq!(out.len(), 3);
        assert_eq!(out[1]["tool_calls"][0]["id"], "c1");
        assert_eq!(out[2]["role"], "tool");
    }

    #[test]
    fn an_assistant_text_and_its_tool_calls_merge_into_one_turn() {
        let messages = vec![
            CoreMessage::text(Role::Assistant, "let me check"),
            CoreMessage {
                id: String::new(),
                role: Role::Assistant,
                content_type: ContentType::ToolCall,
                text: "{}".into(),
                tool_name: Some("exec".into()),
                tool_call_id: Some("c1".into()),
                blocks: None,
            },
        ];
        let encoded = encode_openai(&json!({}), &messages);
        let out = encoded["messages"].as_array().unwrap();
        assert_eq!(out.len(), 1);
        assert_eq!(out[0]["content"], "let me check");
        assert_eq!(out[0]["tool_calls"][0]["id"], "c1");
    }

    #[test]
    fn an_anthropic_request_decodes_into_kernel_messages() {
        let body = json!({
            "model": "m",
            "system": "be good",
            "messages": [
                {"role": "user", "content": [{"type": "text", "text": "run it"}]},
                {"role": "assistant", "content": [
                    {"type": "text", "text": "checking"},
                    {"type": "tool_use", "id": "t1", "name": "exec", "input": {"cmd": "ls"}}
                ]},
                {"role": "user", "content": [
                    {"type": "tool_result", "tool_use_id": "t1", "content": "a.txt"}
                ]}
            ]
        });
        let messages = decode_anthropic(&body);
        assert_eq!(messages[0].role, Role::System);
        assert_eq!(messages[1].text, "run it");
        assert_eq!(messages[3].content_type, ContentType::ToolCall);
        assert_eq!(messages[4].content_type, ContentType::ToolResult);
    }

    #[test]
    fn an_anthropic_request_round_trips_through_encode() {
        let body = json!({
            "model": "m",
            "system": "be good",
            "messages": [
                {"role": "user", "content": [{"type": "text", "text": "run it"}]},
                {"role": "assistant", "content": [
                    {"type": "tool_use", "id": "t1", "name": "exec", "input": {"cmd": "ls"}}
                ]},
                {"role": "user", "content": [
                    {"type": "tool_result", "tool_use_id": "t1", "content": "a.txt"}
                ]}
            ]
        });
        let messages = decode_anthropic(&body);
        let encoded = encode_anthropic(&body, &messages);
        assert_eq!(encoded["system"], "be good");
        let out = encoded["messages"].as_array().unwrap();
        assert_eq!(out.len(), 3);
        assert_eq!(out[1]["content"][0]["type"], "tool_use");
        assert_eq!(out[2]["content"][0]["type"], "tool_result");
    }

    #[test]
    fn an_anthropic_system_message_lands_in_the_top_level_field() {
        let messages = vec![
            CoreMessage::text(Role::System, "base"),
            CoreMessage::text(Role::System, "extra instruction"),
            CoreMessage::text(Role::User, "hi"),
        ];
        let encoded = encode_anthropic(&json!({"system": "base"}), &messages);
        assert_eq!(encoded["system"], "base\n\nextra instruction");
        assert_eq!(encoded["messages"].as_array().unwrap().len(), 1);
    }

    #[test]
    fn the_path_decides_the_dialect() {
        assert_eq!(dialect_for_path("/v1/messages"), Dialect::Anthropic);
        assert_eq!(dialect_for_path("/v1/chat/completions"), Dialect::OpenAi);
    }

    #[test]
    fn the_compress_tool_is_advertised_in_both_dialects() {
        let openai = compress_tool_openai();
        assert_eq!(openai["function"]["name"], COMPRESS_TOOL);
        assert_eq!(openai["function"]["parameters"]["properties"]["content"]["type"], "string");
        let anthropic = compress_tool_anthropic();
        assert_eq!(anthropic["name"], COMPRESS_TOOL);
        assert_eq!(anthropic["input_schema"]["properties"]["content"]["type"], "string");
    }

    #[test]
    fn the_tags_section_tells_the_model_not_to_echo_the_tags() {
        assert!(ACP_TAGS_SECTION.contains("NEVER echo"));
        assert!(ACP_TAGS_SECTION.contains("m00175"));
    }

    /// The kernel reads text, so an image has no representation in
    /// `CoreMessage`. It still has to reach the model: a user who attaches a
    /// screenshot and is told "nothing arrived" has been lied to by the proxy.
    #[test]
    fn an_image_block_survives_the_round_trip() {
        let body = json!({
            "model": "m",
            "messages": [{
                "role": "user",
                "content": [
                    { "type": "text", "text": "can you see that?" },
                    { "type": "image", "data": "iVBORw0KGgo", "mimeType": "image/png" }
                ]
            }]
        });
        let messages = decode(Dialect::OpenAi, &body);
        let out = encode(Dialect::OpenAi, &body, &messages);
        let content = out["messages"][0]["content"]
            .as_array()
            .expect("the content array was flattened to a string");
        assert_eq!(content.len(), 2);
        assert_eq!(content[0]["text"], "can you see that?");
        assert_eq!(content[1]["type"], "image");
        assert_eq!(content[1]["data"], "iVBORw0KGgo");
    }

    /// The cache is keyed on request bytes. A text-only array flattens
    /// losslessly, so it has to keep flattening: preserving it would rewrite
    /// the prefix of every conversation on upgrade and cost a cache miss for
    /// nothing.
    #[test]
    fn a_text_only_array_still_flattens_to_a_string() {
        let body = json!({
            "model": "m",
            "messages": [{
                "role": "user",
                "content": [
                    { "type": "text", "text": "first" },
                    { "type": "text", "text": "second" }
                ]
            }]
        });
        let messages = decode(Dialect::OpenAi, &body);
        let out = encode(Dialect::OpenAi, &body, &messages);
        assert_eq!(out["messages"][0]["content"], "first\nsecond");
    }

    /// The same for a tool result, which is how `read_file` hands back a PNG.
    /// Its text block is metadata; the pixels are the second block.
    #[test]
    fn a_tool_result_keeps_its_image_block() {
        let body = json!({
            "model": "m",
            "messages": [{
                "role": "tool",
                "tool_call_id": "c1",
                "content": [
                    { "type": "text", "text": "{\"content\":\"Image file (image/png, 394745 bytes).\"}" },
                    { "type": "image", "data": "iVBORw0KGgo", "mimeType": "image/png" }
                ]
            }]
        });
        let messages = decode(Dialect::OpenAi, &body);
        let out = encode(Dialect::OpenAi, &body, &messages);
        let content = out["messages"][0]["content"]
            .as_array()
            .expect("the content array was flattened to a string");
        assert_eq!(content[1]["type"], "image");
    }

    #[test]
    fn an_anthropic_image_block_survives_the_round_trip() {
        let body = json!({
            "model": "m",
            "messages": [{
                "role": "user",
                "content": [
                    { "type": "text", "text": "can you see that?" },
                    { "type": "image", "source": {
                        "type": "base64", "media_type": "image/png", "data": "iVBORw0KGgo"
                    }}
                ]
            }]
        });
        let messages = decode(Dialect::Anthropic, &body);
        let out = encode(Dialect::Anthropic, &body, &messages);
        let content = out["messages"][0]["content"]
            .as_array()
            .expect("the content array was flattened to a string");
        assert_eq!(content.len(), 2);
        assert_eq!(content[1]["type"], "image");
        assert_eq!(content[1]["source"]["data"], "iVBORw0KGgo");
    }

    /// The invariant that keeps the fix from undoing compaction: once the text
    /// is a marker, the blocks are stale and must not go back on the wire.
    #[test]
    fn a_rewritten_message_does_not_resurrect_its_blocks() {
        let body = json!({
            "model": "m",
            "messages": [{
                "role": "user",
                "content": [
                    { "type": "text", "text": "can you see that?" },
                    { "type": "image", "data": "iVBORw0KGgo", "mimeType": "image/png" }
                ]
            }]
        });
        let mut messages = decode(Dialect::OpenAi, &body);
        messages[0].text = "[stored m00001]".into();
        messages[0].blocks = None;
        let out = encode(Dialect::OpenAi, &body, &messages);
        assert_eq!(out["messages"][0]["content"], "[stored m00001]");
    }
}
