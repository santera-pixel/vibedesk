//! Reading a model response while it is still arriving.
//!
//! The proxy has to answer one question mid-stream: is this tool call mine or
//! the client's? A client tool call must reach the client as soon as its name is
//! known, because the client is waiting to run it. A proxy tool call must never
//! reach the client at all, because the client has no such tool and would fail
//! the turn. Everything else — text, reasoning, usage — is forwarded untouched.
//!
//! Both dialects live here because the difference between them is only in how an
//! event is spelled, not in what the loop does with it.

use bytes::Bytes;
use serde_json::Value;

use crate::capabilities::compact::wire::Dialect;

/// What kind of content a forwarded event carries.
///
/// Only Anthropic needs this: its response is one message with numbered content
/// blocks, so a second round's text has to land in the block the first round
/// opened. OpenAI chunks are independent and the kind is ignored.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Forward {
    /// Assistant text.
    Text,
    /// The start of a text block.
    TextBlockStart,
    /// The end of a text block.
    TextBlockStop,
    /// Anything else: usage, message start, an event the proxy does not read.
    Other,
}

/// One thing the reader learned from the stream.
#[derive(Debug, Clone)]
pub enum Item {
    /// Bytes the client should receive as they are.
    Forward { raw: Bytes, kind: Forward },
    /// A tool call's name is now known. The loop decides who owns it.
    ToolCallNamed {
        index: usize,
        name: String,
        raw: Bytes,
    },
    /// More arguments for a tool call whose name is already known.
    ToolCallArgs { index: usize, raw: Bytes },
    /// The end of one tool call's block.
    ToolCallEnd { index: usize, raw: Bytes },
    /// The response is over. Held until the loop knows it is not re-requesting.
    Done(Bytes),
}

/// A tool call assembled across events.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct PartialCall {
    pub index: usize,
    pub id: String,
    pub name: String,
    pub arguments: String,
}

impl PartialCall {
    /// The arguments as JSON. A model that sent nothing gets an empty object
    /// rather than an error, because "no arguments" is a legal call.
    pub fn input(&self) -> Value {
        if self.arguments.trim().is_empty() {
            return Value::Object(Default::default());
        }
        serde_json::from_str(&self.arguments).unwrap_or_else(|_| Value::Object(Default::default()))
    }
}

/// Splits an SSE byte stream into events and classifies each one.
pub struct StreamReader {
    dialect: Dialect,
    buffer: Vec<u8>,
    calls: Vec<PartialCall>,
}

impl StreamReader {
    pub fn new(dialect: Dialect) -> Self {
        Self {
            dialect,
            buffer: Vec::new(),
            calls: Vec::new(),
        }
    }

    /// Every tool call seen so far, in the order the model started them.
    pub fn calls(&self) -> &[PartialCall] {
        &self.calls
    }

    /// Feed a chunk. Returns the items it completed; a partial event stays
    /// buffered until its terminator arrives.
    pub fn push(&mut self, chunk: &[u8]) -> Vec<Item> {
        self.buffer.extend_from_slice(chunk);
        let mut items = Vec::new();
        while let Some((at, width)) = next_boundary(&self.buffer) {
            let event: Vec<u8> = self.buffer.drain(..at + width).collect();
            self.classify(&event, &mut items);
        }
        items
    }

    /// Flush a trailing event that never got its terminator. Some providers end
    /// the stream without a final blank line.
    pub fn finish(&mut self) -> Vec<Item> {
        let mut items = Vec::new();
        if !self.buffer.is_empty() {
            let event = std::mem::take(&mut self.buffer);
            self.classify(&event, &mut items);
        }
        items
    }

    fn classify(&mut self, event: &[u8], items: &mut Vec<Item>) {
        let text = String::from_utf8_lossy(event);
        let raw = Bytes::copy_from_slice(event);

        let mut name: Option<&str> = None;
        let mut data: Option<&str> = None;
        for line in text.lines() {
            if let Some(rest) = line.strip_prefix("event:") {
                name = Some(rest.trim());
            } else if let Some(rest) = line.strip_prefix("data:") {
                data = Some(rest.trim());
            }
        }

        let Some(data) = data else {
            items.push(Item::Forward {
                raw,
                kind: Forward::Other,
            });
            return;
        };
        if data == "[DONE]" {
            items.push(Item::Done(raw));
            return;
        }
        let Ok(value) = serde_json::from_str::<Value>(data) else {
            items.push(Item::Forward {
                raw,
                kind: Forward::Other,
            });
            return;
        };

        match self.dialect {
            Dialect::OpenAi => self.classify_openai(&value, raw, items),
            Dialect::Anthropic => self.classify_anthropic(name, &value, raw, items),
        }
    }

    fn classify_openai(&mut self, value: &Value, raw: Bytes, items: &mut Vec<Item>) {
        let Some(choice) = value.get("choices").and_then(|choices| choices.get(0)) else {
            items.push(Item::Forward {
                raw,
                kind: Forward::Other,
            });
            return;
        };

        if let Some(calls) = choice
            .get("delta")
            .and_then(|delta| delta.get("tool_calls"))
            .and_then(Value::as_array)
        {
            for call in calls {
                let index = call.get("index").and_then(Value::as_u64).unwrap_or(0) as usize;
                let id = call
                    .get("id")
                    .and_then(Value::as_str)
                    .unwrap_or_default()
                    .to_string();
                let function = call.get("function");
                let name = function
                    .and_then(|f| f.get("name"))
                    .and_then(Value::as_str)
                    .unwrap_or_default();
                let arguments = function
                    .and_then(|f| f.get("arguments"))
                    .and_then(Value::as_str)
                    .unwrap_or_default();

                let slot = self.slot(index);
                if !id.is_empty() {
                    slot.id = id;
                }
                slot.arguments.push_str(arguments);

                if name.is_empty() {
                    items.push(Item::ToolCallArgs {
                        index,
                        raw: raw.clone(),
                    });
                } else {
                    self.slot(index).name = name.to_string();
                    items.push(Item::ToolCallNamed {
                        index,
                        name: name.to_string(),
                        raw: raw.clone(),
                    });
                }
            }
            return;
        }

        if choice
            .get("finish_reason")
            .map(|reason| !reason.is_null())
            .unwrap_or(false)
        {
            items.push(Item::Done(raw));
            return;
        }

        let kind = if choice
            .get("delta")
            .and_then(|delta| delta.get("content"))
            .and_then(Value::as_str)
            .map(|text| !text.is_empty())
            .unwrap_or(false)
        {
            Forward::Text
        } else {
            Forward::Other
        };
        items.push(Item::Forward { raw, kind });
    }

    fn classify_anthropic(
        &mut self,
        name: Option<&str>,
        value: &Value,
        raw: Bytes,
        items: &mut Vec<Item>,
    ) {
        let index = value.get("index").and_then(Value::as_u64).unwrap_or(0) as usize;
        match name.unwrap_or_default() {
            "content_block_start" => {
                let block = value.get("content_block");
                let kind = block
                    .and_then(|b| b.get("type"))
                    .and_then(Value::as_str)
                    .unwrap_or_default();
                if kind == "tool_use" {
                    let id = block
                        .and_then(|b| b.get("id"))
                        .and_then(Value::as_str)
                        .unwrap_or_default()
                        .to_string();
                    let tool = block
                        .and_then(|b| b.get("name"))
                        .and_then(Value::as_str)
                        .unwrap_or_default()
                        .to_string();
                    let slot = self.slot(index);
                    slot.id = id;
                    slot.name = tool.clone();
                    items.push(Item::ToolCallNamed {
                        index,
                        name: tool,
                        raw,
                    });
                } else {
                    let kind = if kind == "text" {
                        Forward::TextBlockStart
                    } else {
                        Forward::Other
                    };
                    items.push(Item::Forward { raw, kind });
                }
            }
            "content_block_delta" => {
                let delta = value.get("delta");
                let kind = delta
                    .and_then(|d| d.get("type"))
                    .and_then(Value::as_str)
                    .unwrap_or_default();
                if kind == "input_json_delta" {
                    let fragment = delta
                        .and_then(|d| d.get("partial_json"))
                        .and_then(Value::as_str)
                        .unwrap_or_default();
                    self.slot(index).arguments.push_str(fragment);
                    items.push(Item::ToolCallArgs { index, raw });
                } else {
                    let kind = if kind == "text_delta" {
                        Forward::Text
                    } else {
                        Forward::Other
                    };
                    items.push(Item::Forward { raw, kind });
                }
            }
            "content_block_stop" => items.push(Item::ToolCallEnd { index, raw }),
            "message_delta" | "message_stop" => items.push(Item::Done(raw)),
            _ => items.push(Item::Forward {
                raw,
                kind: Forward::Other,
            }),
        }
    }

    fn slot(&mut self, index: usize) -> &mut PartialCall {
        if let Some(at) = self.calls.iter().position(|call| call.index == index) {
            return &mut self.calls[at];
        }
        self.calls.push(PartialCall {
            index,
            ..Default::default()
        });
        self.calls.last_mut().expect("just pushed")
    }
}

/// Keeps the client's view of a multi-round response coherent.
///
/// OpenAI chunks are independent, so nothing is needed. Anthropic wraps the whole
/// response in one message with numbered content blocks, so a second round's text
/// has to land in the block the first round opened — or open one if the first
/// round never emitted text at all.
pub struct Emitter {
    dialect: Dialect,
    round: usize,
    text_block_open: bool,
}

impl Emitter {
    pub fn new(dialect: Dialect) -> Self {
        Self {
            dialect,
            round: 0,
            text_block_open: false,
        }
    }

    pub fn begin_round(&mut self) {
        self.round += 1;
    }

    /// The bytes to send for one forwarded event. Empty means "the client
    /// already has this".
    pub fn forward(&mut self, raw: &Bytes, kind: Forward) -> Vec<Bytes> {
        if self.dialect == Dialect::OpenAi || self.round <= 1 {
            if kind == Forward::TextBlockStart {
                self.text_block_open = true;
            } else if kind == Forward::TextBlockStop {
                self.text_block_open = false;
            }
            return vec![raw.clone()];
        }

        match kind {
            Forward::TextBlockStart => {
                // The client already has a text block open from round one.
                self.text_block_open = true;
                Vec::new()
            }
            Forward::TextBlockStop => {
                self.text_block_open = false;
                Vec::new()
            }
            Forward::Text => {
                let mut out = Vec::new();
                if !self.text_block_open {
                    out.push(text_block_start());
                    self.text_block_open = true;
                }
                out.push(raw.clone());
                out
            }
            Forward::Other => Vec::new(),
        }
    }
}

/// A `content_block_start` for a text block at index 0, in Anthropic's spelling.
fn text_block_start() -> Bytes {
    Bytes::from_static(
        b"event: content_block_start\ndata: {\"type\":\"content_block_start\",\"index\":0,\"content_block\":{\"type\":\"text\",\"text\":\"\"}}\n\n",
    )
}

/// The earliest blank-line terminator in the buffer, with its width.
fn next_boundary(buffer: &[u8]) -> Option<(usize, usize)> {
    let mut best: Option<(usize, usize)> = None;
    for (needle, width) in [(b"\r\n\r\n".as_slice(), 4usize), (b"\n\n".as_slice(), 2usize)] {
        if let Some(at) = find(buffer, needle) {
            if best.map(|(seen, _)| at < seen).unwrap_or(true) {
                best = Some((at, width));
            }
        }
    }
    best
}

fn find(haystack: &[u8], needle: &[u8]) -> Option<usize> {
    if needle.len() > haystack.len() {
        return None;
    }
    haystack.windows(needle.len()).position(|window| window == needle)
}

/// Tool calls from a non-streamed response body.
///
/// A client that asked for `stream: false` gets one JSON document, so there is
/// nothing to read incrementally — but the same question still has to be
/// answered.
pub fn calls_in_body(dialect: Dialect, body: &Value) -> Vec<PartialCall> {
    let mut calls = Vec::new();
    match dialect {
        Dialect::OpenAi => {
            let Some(choice) = body.get("choices").and_then(|choices| choices.get(0)) else {
                return calls;
            };
            let Some(list) = choice
                .get("message")
                .and_then(|message| message.get("tool_calls"))
                .and_then(Value::as_array)
            else {
                return calls;
            };
            for (index, call) in list.iter().enumerate() {
                calls.push(PartialCall {
                    index,
                    id: call
                        .get("id")
                        .and_then(Value::as_str)
                        .unwrap_or_default()
                        .to_string(),
                    name: call
                        .get("function")
                        .and_then(|f| f.get("name"))
                        .and_then(Value::as_str)
                        .unwrap_or_default()
                        .to_string(),
                    arguments: call
                        .get("function")
                        .and_then(|f| f.get("arguments"))
                        .and_then(Value::as_str)
                        .unwrap_or_default()
                        .to_string(),
                });
            }
        }
        Dialect::Anthropic => {
            let Some(blocks) = body.get("content").and_then(Value::as_array) else {
                return calls;
            };
            for (index, block) in blocks.iter().enumerate() {
                if block.get("type").and_then(Value::as_str) != Some("tool_use") {
                    continue;
                }
                calls.push(PartialCall {
                    index,
                    id: block
                        .get("id")
                        .and_then(Value::as_str)
                        .unwrap_or_default()
                        .to_string(),
                    name: block
                        .get("name")
                        .and_then(Value::as_str)
                        .unwrap_or_default()
                        .to_string(),
                    arguments: block
                        .get("input")
                        .map(|input| input.to_string())
                        .unwrap_or_default(),
                });
            }
        }
    }
    calls
}

#[cfg(test)]
mod tests {
    use super::*;

    fn openai_chunk(delta: &str) -> String {
        format!("data: {{\"choices\":[{{\"delta\":{delta}}}]}}\n\n")
    }

    #[test]
    fn text_deltas_are_forwarded_as_they_arrive() {
        let mut reader = StreamReader::new(Dialect::OpenAi);
        let items = reader.push(openai_chunk("{\"content\":\"hello\"}").as_bytes());
        assert_eq!(items.len(), 1);
        assert!(matches!(
            items[0],
            Item::Forward {
                kind: Forward::Text,
                ..
            }
        ));
    }

    #[test]
    fn an_event_split_across_chunks_is_held_until_it_is_whole() {
        let mut reader = StreamReader::new(Dialect::OpenAi);
        let whole = openai_chunk("{\"content\":\"hello\"}");
        let (head, tail) = whole.split_at(20);
        assert!(reader.push(head.as_bytes()).is_empty());
        let items = reader.push(tail.as_bytes());
        assert_eq!(items.len(), 1);
    }

    #[test]
    fn a_multibyte_character_split_across_chunks_survives() {
        let mut reader = StreamReader::new(Dialect::OpenAi);
        let whole = openai_chunk("{\"content\":\"héllo\"}");
        let bytes = whole.as_bytes();
        let cut = whole.find('é').unwrap() + 1;
        assert!(reader.push(&bytes[..cut]).is_empty());
        let items = reader.push(&bytes[cut..]);
        assert_eq!(items.len(), 1);
        match &items[0] {
            Item::Forward { raw, .. } => {
                assert!(String::from_utf8_lossy(raw).contains("héllo"));
            }
            other => panic!("expected a forward, got {other:?}"),
        }
    }

    #[test]
    fn an_openai_tool_call_is_named_then_filled_in() {
        let mut reader = StreamReader::new(Dialect::OpenAi);
        let first = openai_chunk(
            "{\"tool_calls\":[{\"index\":0,\"id\":\"call_1\",\"function\":{\"name\":\"compress\",\"arguments\":\"\"}}]}",
        );
        let items = reader.push(first.as_bytes());
        assert!(matches!(
            items[0],
            Item::ToolCallNamed {
                index: 0,
                ref name,
                ..
            } if name == "compress"
        ));

        let second = openai_chunk(
            "{\"tool_calls\":[{\"index\":0,\"function\":{\"arguments\":\"{\\\"content\\\":\"}}]}",
        );
        let items = reader.push(second.as_bytes());
        assert!(matches!(items[0], Item::ToolCallArgs { index: 0, .. }));

        let third = openai_chunk(
            "{\"tool_calls\":[{\"index\":0,\"function\":{\"arguments\":\"\\\"m1-m2 x\\\"}\"}}]}",
        );
        reader.push(third.as_bytes());

        let calls = reader.calls();
        assert_eq!(calls.len(), 1);
        assert_eq!(calls[0].id, "call_1");
        assert_eq!(calls[0].name, "compress");
        assert_eq!(calls[0].input()["content"], "m1-m2 x");
    }

    #[test]
    fn an_anthropic_tool_use_block_is_assembled_from_three_events() {
        let mut reader = StreamReader::new(Dialect::Anthropic);
        let start = "event: content_block_start\ndata: {\"type\":\"content_block_start\",\"index\":1,\"content_block\":{\"type\":\"tool_use\",\"id\":\"toolu_1\",\"name\":\"decompress\",\"input\":{}}}\n\n";
        let items = reader.push(start.as_bytes());
        assert!(matches!(
            items[0],
            Item::ToolCallNamed {
                index: 1,
                ref name,
                ..
            } if name == "decompress"
        ));

        let delta = "event: content_block_delta\ndata: {\"type\":\"content_block_delta\",\"index\":1,\"delta\":{\"type\":\"input_json_delta\",\"partial_json\":\"{\\\"blockId\\\":\\\"b1\\\"}\"}}\n\n";
        let items = reader.push(delta.as_bytes());
        assert!(matches!(items[0], Item::ToolCallArgs { index: 1, .. }));

        let stop = "event: content_block_stop\ndata: {\"type\":\"content_block_stop\",\"index\":1}\n\n";
        let items = reader.push(stop.as_bytes());
        assert!(matches!(items[0], Item::ToolCallEnd { index: 1, .. }));

        assert_eq!(reader.calls()[0].input()["blockId"], "b1");
    }

    #[test]
    fn the_end_of_a_response_is_held_rather_than_forwarded() {
        let mut reader = StreamReader::new(Dialect::OpenAi);
        let items = reader.push(b"data: [DONE]\n\n");
        assert!(matches!(items[0], Item::Done(_)));
    }

    #[test]
    fn an_anthropic_message_stop_is_held_rather_than_forwarded() {
        let mut reader = StreamReader::new(Dialect::Anthropic);
        let items = reader.push(b"event: message_stop\ndata: {\"type\":\"message_stop\"}\n\n");
        assert!(matches!(items[0], Item::Done(_)));
    }

    #[test]
    fn a_second_round_opens_a_text_block_the_first_round_never_opened() {
        let mut emitter = Emitter::new(Dialect::Anthropic);
        emitter.begin_round();
        emitter.begin_round();
        let delta = Bytes::from_static(
            b"event: content_block_delta\ndata: {\"type\":\"content_block_delta\",\"index\":0,\"delta\":{\"type\":\"text_delta\",\"text\":\"hi\"}}\n\n",
        );
        let out = emitter.forward(&delta, Forward::Text);
        assert_eq!(out.len(), 2, "a start must be synthesised first");
        assert!(String::from_utf8_lossy(&out[0]).contains("content_block_start"));
    }

    #[test]
    fn a_second_round_does_not_reopen_a_text_block_the_first_round_opened() {
        let mut emitter = Emitter::new(Dialect::Anthropic);
        emitter.begin_round();
        let start = Bytes::from_static(
            b"event: content_block_start\ndata: {\"type\":\"content_block_start\",\"index\":0,\"content_block\":{\"type\":\"text\",\"text\":\"\"}}\n\n",
        );
        assert_eq!(emitter.forward(&start, Forward::TextBlockStart).len(), 1);

        emitter.begin_round();
        assert!(emitter
            .forward(&start, Forward::TextBlockStart)
            .is_empty());
        let delta = Bytes::from_static(
            b"event: content_block_delta\ndata: {\"type\":\"content_block_delta\",\"index\":0,\"delta\":{\"type\":\"text_delta\",\"text\":\"hi\"}}\n\n",
        );
        assert_eq!(emitter.forward(&delta, Forward::Text).len(), 1);
    }

    #[test]
    fn a_non_streamed_body_yields_its_tool_calls() {
        let body = serde_json::json!({
            "choices": [{"message": {"tool_calls": [
                {"id": "c1", "function": {"name": "compress", "arguments": "{\"content\":\"m1-m2 x\"}"}}
            ]}}]
        });
        let calls = calls_in_body(Dialect::OpenAi, &body);
        assert_eq!(calls.len(), 1);
        assert_eq!(calls[0].name, "compress");
        assert_eq!(calls[0].input()["content"], "m1-m2 x");
    }

    #[test]
    fn an_anthropic_body_yields_its_tool_use_blocks() {
        let body = serde_json::json!({
            "content": [
                {"type": "text", "text": "thinking"},
                {"type": "tool_use", "id": "t1", "name": "acp_retrieve", "input": {"ref": "s1"}}
            ]
        });
        let calls = calls_in_body(Dialect::Anthropic, &body);
        assert_eq!(calls.len(), 1);
        assert_eq!(calls[0].name, "acp_retrieve");
        assert_eq!(calls[0].input()["ref"], "s1");
    }
}
