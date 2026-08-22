/// The end of a complete XML-like opening tag.
#[allow(dead_code)] // consumed by the following incremental sanitizer layer
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct OpeningTag {
    pub(crate) end: usize,
    pub(crate) self_closing: bool,
}

#[allow(dead_code)] // consumed by the following incremental sanitizer layer
pub(crate) const TAGS: [&str; 7] = [
    "function_call",
    "tool_calls",
    "parameter",
    "function",
    "invoke",
    "think",
    "tool",
];

#[allow(dead_code)] // wired when the state-machine layer lands
pub(crate) fn recognized_tag(input: &str) -> Option<&'static str> {
    TAGS.into_iter().find(|tag| {
        let prefix = format!("<{tag}");
        input
            .get(..prefix.len())
            .is_some_and(|value| value.eq_ignore_ascii_case(&prefix))
            && input
                .as_bytes()
                .get(prefix.len())
                .is_some_and(|byte| byte.is_ascii_whitespace() || matches!(byte, b'>' | b'/'))
    })
}

/// Finds an opening tag delimiter without treating `>` in a quoted attribute
/// as structural. This is deliberately independent of response conversion so
/// stream and non-stream adapters share exactly the same token contract.
#[allow(dead_code)] // consumed by the following incremental sanitizer layer
pub(crate) fn opening_tag(input: &str) -> Option<OpeningTag> {
    let mut quote = None;
    for (offset, byte) in input.as_bytes().iter().copied().enumerate() {
        if let Some(delimiter) = quote {
            if byte == delimiter {
                quote = None;
            }
        } else if matches!(byte, b'\'' | b'\"') {
            quote = Some(byte);
        } else if byte == b'>' {
            let end = offset + 1;
            return Some(OpeningTag {
                end,
                self_closing: input[..offset].trim_end().ends_with('/'),
            });
        }
    }
    None
}

/// A complete recognized tool-markup tag found in a content fragment.
#[allow(dead_code)] // consumed by the following incremental sanitizer layer
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum TagToken {
    Opening {
        tag: &'static str,
        start: usize,
        end: usize,
        self_closing: bool,
    },
    Closing {
        tag: &'static str,
        start: usize,
        end: usize,
    },
}

impl TagToken {
    fn shifted(self, offset: usize) -> Self {
        match self {
            Self::Opening {
                tag,
                start,
                end,
                self_closing,
            } => Self::Opening {
                tag,
                start: start
                    .checked_add(offset)
                    .expect("tag start remains in input"),
                end: end.checked_add(offset).expect("tag end remains in input"),
                self_closing,
            },
            Self::Closing { tag, start, end } => Self::Closing {
                tag,
                start: start
                    .checked_add(offset)
                    .expect("tag start remains in input"),
                end: end.checked_add(offset).expect("tag end remains in input"),
            },
        }
    }
}

/// Finds the next complete recognized tag, preserving quoted `>` bytes inside
/// opening-tag attributes. Incomplete tags intentionally return `None` so a
/// streaming caller can retain them for the next content chunk.
#[allow(dead_code)] // consumed by the following incremental sanitizer layer
pub(crate) fn next_tag(input: &str) -> Option<TagToken> {
    for (start, _) in input.match_indices('<') {
        let candidate = &input[start..];
        if let Some(tag) = recognized_tag(candidate) {
            let opening = opening_tag(candidate)?;
            return Some(TagToken::Opening {
                tag,
                start,
                end: start + opening.end,
                self_closing: opening.self_closing,
            });
        }

        let Some(closing) = candidate.strip_prefix("</") else {
            continue;
        };
        for tag in TAGS {
            if !closing
                .get(..tag.len())
                .is_some_and(|value| value.eq_ignore_ascii_case(tag))
            {
                continue;
            }
            let rest = &closing[tag.len()..];
            let Some(end) = closing_tag_end(rest) else {
                continue;
            };
            return Some(TagToken::Closing {
                tag,
                start,
                end: start + 2 + tag.len() + end,
            });
        }
    }
    None
}

fn closing_tag_end(rest: &str) -> Option<usize> {
    let delimiter = rest.trim_start();
    delimiter
        .starts_with('>')
        .then_some(rest.len() - delimiter.len() + 1)
}
/// Incrementally removes complete recognized markup elements while retaining
/// ordinary content and enough trailing input to finish a split tag later.
#[allow(dead_code)] // wired by the following response-adapter layer
#[derive(Default)]
pub(crate) struct Sanitizer {
    pending: String,
    replay_buffer: String,
    active_tag: Option<&'static str>,
    active_depth: usize,
    unterminated_tool_body: String,
    unterminated_tool_markdown: Option<MarkdownCodeState>,
    tool_is_marker: bool,
    markdown: MarkdownCodeState,
    markdown_disabled: bool,
}

#[allow(dead_code)] // wired by the following response-adapter layer
impl Sanitizer {
    /// Whether a fragment contains a complete recognized tag or a suffix that
    /// could become one in the next stream fragment.
    pub(crate) fn may_contain_markup(fragment: &str) -> bool {
        matches!(
            next_tag_without_markdown(fragment),
            ScanResult::Tag(_) | ScanResult::Pending { .. }
        )
    }

    pub(crate) fn push(&mut self, fragment: &str) -> String {
        if self.active_tag == Some("tool") {
            self.unterminated_tool_body.push_str(fragment);
        }
        let mut input = std::mem::take(&mut self.pending);
        input.push_str(fragment);
        let mut output = String::new();

        while !input.is_empty() {
            let scan = if self.markdown_disabled {
                next_tag_without_markdown(&input)
            } else {
                next_tag_outside_markdown(
                    &input,
                    &mut self.markdown,
                    !self.replay_buffer.is_empty(),
                )
            };
            let token = match scan {
                ScanResult::Tag(token) => token,
                ScanResult::Pending { start } => {
                    if self.active_tag.is_some() {
                        self.pending = input;
                    } else {
                        output.push_str(&input[..start]);
                        self.pending = input[start..].to_string();
                    }
                    break;
                }
                ScanResult::Defer { start } => {
                    if self.active_tag.is_some() {
                        self.pending = input;
                    } else {
                        output.push_str(&input[..start]);
                        self.replay_buffer.push_str(&input[start..]);
                    }
                    break;
                }
                ScanResult::Release { end } => {
                    debug_assert!(self.active_tag.is_none());
                    output.push_str(&std::mem::take(&mut self.replay_buffer));
                    output.push_str(&input[..end]);
                    input = input[end..].to_string();
                    continue;
                }
                ScanResult::Complete => {
                    if self.active_tag.is_some() {
                        self.pending = input;
                    } else {
                        output.push_str(&input);
                    }
                    break;
                }
            };
            let start = match token {
                TagToken::Opening { start, .. } | TagToken::Closing { start, .. } => start,
            };

            if self.active_tag.is_none() {
                output.push_str(&input[..start]);
            }
            let end = match token {
                TagToken::Opening {
                    tag,
                    end,
                    self_closing,
                    ..
                } => {
                    if self.active_tag.is_none()
                        && !self_closing
                        && !(tag == "tool" && self.tool_is_marker)
                    {
                        self.active_tag = Some(tag);
                        self.active_depth = 1;
                        if tag == "tool" {
                            self.unterminated_tool_body = input[end..].to_string();
                            self.unterminated_tool_markdown = Some(self.markdown.clone());
                        }
                    } else if self.active_tag == Some(tag) && !self_closing {
                        self.active_depth += 1;
                    } else if self.active_tag.is_none() {
                        // A complete self-closing recognized tag is itself a
                        // duplicate element, so omit it without entering state.
                    }
                    end
                }
                TagToken::Closing { tag, end, .. } => {
                    if self.active_tag == Some(tag) {
                        self.active_depth = self.active_depth.saturating_sub(1);
                        if self.active_depth == 0 {
                            self.active_tag = None;
                            self.unterminated_tool_body.clear();
                            self.unterminated_tool_markdown = None;
                        }
                    } else if self.active_tag.is_none() && !(tag == "tool" && self.tool_is_marker) {
                        output.push_str(&input[start..end]);
                    }
                    end
                }
            };
            self.markdown.consume(&input[start..end]);
            input = input[end..].to_string();
        }
        output
    }

    /// On a normal terminal, retain unclosed material rather than silently
    /// deleting user text. Complete elements have already been omitted.
    pub(crate) fn finish(&mut self) -> String {
        let disposition = self.markdown.finish();
        let pending = std::mem::take(&mut self.pending);
        let replay_buffer = std::mem::take(&mut self.replay_buffer);
        let pending_is_recognized_tag = recognized_tag(&pending).is_some();
        let pending_was_suppressed = self.active_tag.is_some();
        let (unterminated_tool_body, unterminated_tool_markdown) =
            if self.active_tag == Some("tool") {
                (
                    std::mem::take(&mut self.unterminated_tool_body),
                    self.unterminated_tool_markdown
                        .take()
                        .expect("active tool records its output Markdown state"),
                )
            } else {
                self.unterminated_tool_body.clear();
                self.unterminated_tool_markdown = None;
                (String::new(), MarkdownCodeState::default())
            };
        self.active_tag = None;
        self.active_depth = 0;
        if !unterminated_tool_body.is_empty() {
            let mut fallback = Self {
                tool_is_marker: true,
                markdown: unterminated_tool_markdown,
                ..Self::default()
            };
            let mut output = fallback.push(&unterminated_tool_body);
            output.push_str(&fallback.finish());
            return output;
        }
        if disposition == MarkdownFinish::Complete {
            if pending_is_recognized_tag && !pending_was_suppressed && !self.tool_is_marker {
                return replay_buffer + &pending;
            }
            return replay_buffer;
        }

        let mut terminal_buffer = replay_buffer;
        if !pending_was_suppressed {
            terminal_buffer.push_str(&pending);
        }
        let mut replay = Self {
            tool_is_marker: self.tool_is_marker,
            markdown_disabled: true,
            ..Self::default()
        };
        let mut output = replay.push(&terminal_buffer);
        output.push_str(&replay.finish());
        output
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum TagAt {
    Complete(TagToken),
    Incomplete,
    None,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ScanResult {
    Tag(TagToken),
    Pending { start: usize },
    Defer { start: usize },
    Release { end: usize },
    Complete,
}

#[cfg(test)]
std::thread_local! {
    static TAG_AT_CALLS: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
}

fn tag_at(input: &str) -> TagAt {
    #[cfg(test)]
    TAG_AT_CALLS.set(TAG_AT_CALLS.get() + 1);

    if let Some(tag) = recognized_tag(input) {
        return opening_tag(input).map_or(TagAt::Incomplete, |opening| {
            TagAt::Complete(TagToken::Opening {
                tag,
                start: 0,
                end: opening.end,
                self_closing: opening.self_closing,
            })
        });
    }

    if let Some(closing) = input.strip_prefix("</") {
        for tag in TAGS {
            if !closing
                .get(..tag.len())
                .is_some_and(|value| value.eq_ignore_ascii_case(tag))
            {
                continue;
            }
            let rest = &closing[tag.len()..];
            if let Some(end) = closing_tag_end(rest) {
                return TagAt::Complete(TagToken::Closing {
                    tag,
                    start: 0,
                    end: 2 + tag.len() + end,
                });
            }
        }
    }

    if possible_tag_at_start(input) {
        TagAt::Incomplete
    } else {
        TagAt::None
    }
}

fn next_tag_without_markdown(input: &str) -> ScanResult {
    for (start, _) in input.match_indices('<') {
        match tag_at(&input[start..]) {
            TagAt::Complete(token) => return ScanResult::Tag(token.shifted(start)),
            TagAt::Incomplete => return ScanResult::Pending { start },
            TagAt::None => {}
        }
    }
    ScanResult::Complete
}

fn next_tag_outside_markdown(
    input: &str,
    markdown: &mut MarkdownCodeState,
    buffering_replay: bool,
) -> ScanResult {
    let mut deferred_start = buffering_replay.then_some(0);
    let mut characters = input.char_indices().peekable();
    while let Some((start, character)) = characters.next() {
        let needed_replay = markdown.needs_replay_buffer();
        if character == '<' {
            let permits_markup = markdown.permits_markup();
            let needs_replay = markdown.needs_replay_buffer();
            if needed_replay && !needs_replay {
                if buffering_replay {
                    return ScanResult::Release { end: start };
                }
                deferred_start = None;
            }
            if permits_markup {
                match tag_at(&input[start..]) {
                    TagAt::Complete(token) => {
                        return ScanResult::Tag(token.shifted(start));
                    }
                    TagAt::Incomplete => return ScanResult::Pending { start },
                    TagAt::None => {}
                }
            }
        }
        if matches!(character, '`' | '~') {
            while characters
                .peek()
                .is_some_and(|(_, candidate)| *candidate == character)
            {
                characters.next();
            }
        }
        let end = characters
            .peek()
            .map(|(offset, _)| *offset)
            .unwrap_or(input.len());
        markdown.consume(&input[start..end]);
        let needs_replay = markdown.needs_replay_buffer();
        if !needed_replay && needs_replay && deferred_start.is_none() {
            deferred_start = Some(start);
        } else if needed_replay && !needs_replay {
            if buffering_replay {
                return ScanResult::Release { end };
            }
            deferred_start = None;
        }
    }
    deferred_start.map_or(ScanResult::Complete, |start| ScanResult::Defer { start })
}

/// Tracks Markdown contexts in which XML-like text must remain literal.
/// The sanitizer retains provisional inline content until this state reports
/// whether it is code or literal text at a lexical boundary or terminal.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct Fence {
    marker: u8,
    length: usize,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct PendingMarker {
    marker: u8,
    length: usize,
    leading_spaces: Option<usize>,
}

/// A caller that suppresses markup while an inline-code candidate is active
/// must retain that candidate until the terminal disposition is known. An
/// unmatched candidate is literal Markdown and therefore needs to be replayed.
#[allow(dead_code)] // consumed by the following incremental sanitizer layer
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum MarkdownFinish {
    Complete,
    ReplayUnmatchedInline,
}

#[allow(dead_code)]
#[derive(Debug, Clone, PartialEq, Eq)]
struct MarkdownCodeState {
    fence: Option<Fence>,
    opening_backtick_fence: bool,
    inline_ticks: Option<usize>,
    pending_marker: Option<PendingMarker>,
    closing_fence: bool,
    leading_spaces: Option<usize>,
    escaped: bool,
}

impl Default for MarkdownCodeState {
    fn default() -> Self {
        Self {
            fence: None,
            opening_backtick_fence: false,
            inline_ticks: None,
            pending_marker: None,
            closing_fence: false,
            leading_spaces: Some(0),
            escaped: false,
        }
    }
}

#[allow(dead_code)]
impl MarkdownCodeState {
    fn needs_replay_buffer(&self) -> bool {
        self.inline_ticks.is_some()
            || self.opening_backtick_fence
            || (self.fence.is_none()
                && self
                    .pending_marker
                    .is_some_and(|pending| pending.marker == b'`'))
    }

    /// Resolve a marker run at the lexical boundary before a possible markup
    /// tag, then report whether the tag is outside Markdown code.
    fn permits_markup(&mut self) -> bool {
        self.resolve_pending_marker();
        self.fence.is_none() && self.inline_ticks.is_none() && !self.escaped
    }

    fn consume(&mut self, text: &str) {
        for byte in text.bytes() {
            self.consume_byte(byte);
        }
    }

    fn finish(&mut self) -> MarkdownFinish {
        self.resolve_pending_marker();
        if self.closing_fence {
            self.fence = None;
            self.closing_fence = false;
        }
        let disposition = if self.inline_ticks.is_some() {
            MarkdownFinish::ReplayUnmatchedInline
        } else {
            MarkdownFinish::Complete
        };
        *self = Self::default();
        disposition
    }

    fn consume_byte(&mut self, byte: u8) {
        if self
            .pending_marker
            .is_some_and(|pending| pending.marker != byte)
        {
            self.resolve_pending_marker();
        }

        if self.closing_fence {
            match byte {
                b' ' | b'\t' => return,
                b'\n' | b'\r' => {
                    self.fence = None;
                    self.closing_fence = false;
                    self.start_line();
                    return;
                }
                _ => self.closing_fence = false,
            }
        }

        if matches!(byte, b'\n' | b'\r') {
            self.pending_marker = None;
            self.opening_backtick_fence = false;
            self.escaped = false;
            self.start_line();
            return;
        }

        if self.opening_backtick_fence && byte == b'`' {
            let opener = self.fence.take().expect("opening fence is present");
            self.opening_backtick_fence = false;
            self.inline_ticks = Some(opener.length);
        }

        if self.fence.is_none() && self.inline_ticks.is_none() && self.escaped {
            self.escaped = false;
            if byte.is_ascii_punctuation() {
                self.mark_nonspace();
                return;
            }
        }

        if self.fence.is_none() && self.inline_ticks.is_none() && byte == b'\\' {
            self.escaped = true;
            self.mark_nonspace();
            return;
        }

        if matches!(byte, b'`' | b'~') {
            if let Some(pending) = self.pending_marker.as_mut() {
                pending.length += 1;
            } else {
                self.pending_marker = Some(PendingMarker {
                    marker: byte,
                    length: 1,
                    leading_spaces: self.leading_spaces,
                });
            }
            return;
        }

        self.track_line_byte(byte);
    }

    fn resolve_pending_marker(&mut self) {
        let Some(pending) = self.pending_marker.take() else {
            return;
        };
        self.mark_nonspace();

        if let Some(fence) = self.fence {
            if pending.marker == fence.marker
                && pending.length >= fence.length
                && pending.leading_spaces.is_some_and(|spaces| spaces <= 3)
            {
                self.closing_fence = true;
            }
            return;
        }

        if let Some(length) = self.inline_ticks {
            if pending.marker == b'`' && pending.length == length {
                self.inline_ticks = None;
            }
            return;
        }

        let fence_position = pending.leading_spaces.is_some_and(|spaces| spaces <= 3);
        if fence_position && pending.length >= 3 {
            self.fence = Some(Fence {
                marker: pending.marker,
                length: pending.length,
            });
            self.opening_backtick_fence = pending.marker == b'`';
        } else if pending.marker == b'`' {
            self.inline_ticks = Some(pending.length);
        }
    }

    fn start_line(&mut self) {
        self.leading_spaces = Some(0);
    }

    fn mark_nonspace(&mut self) {
        self.leading_spaces = None;
    }

    fn track_line_byte(&mut self, byte: u8) {
        if byte == b' ' {
            if let Some(spaces) = self.leading_spaces.as_mut() {
                *spaces += 1;
            }
        } else {
            self.mark_nonspace();
        }
    }
}

fn possible_tag_at_start(suffix: &str) -> bool {
    TAGS.into_iter().any(|tag| {
        let opening = format!("<{tag}");
        let closing = format!("</{tag}");
        let opening_prefix = opening
            .get(..suffix.len())
            .is_some_and(|prefix| prefix.eq_ignore_ascii_case(suffix));
        let closing_prefix = closing
            .get(..suffix.len())
            .is_some_and(|prefix| prefix.eq_ignore_ascii_case(suffix));
        let closing_continues = suffix
            .get(..closing.len())
            .is_some_and(|prefix| prefix.eq_ignore_ascii_case(&closing))
            && {
                let rest = &suffix[closing.len()..];
                closing_tag_end(rest).is_none() && rest.chars().all(char::is_whitespace)
            };
        opening_prefix || closing_prefix || closing_continues
    })
}

#[cfg(test)]
mod tests {
    use super::MarkdownCodeState;
    use super::MarkdownFinish;
    use super::Sanitizer;
    use super::ScanResult;
    use super::TAG_AT_CALLS;
    use super::TagAt;
    use super::TagToken;
    use super::closing_tag_end;
    use super::next_tag;
    use super::next_tag_outside_markdown;
    use super::opening_tag;
    use super::recognized_tag;
    use super::tag_at;
    #[test]
    fn quoted_attribute_delimiter_is_not_a_tag_delimiter() {
        let tag = opening_tag("<parameter note=\"a > b\"/>After").expect("complete tag");
        assert_eq!(tag.end, "<parameter note=\"a > b\"/>".len());
        assert!(tag.self_closing);
    }

    #[test]
    fn incomplete_quoted_attribute_stays_incomplete() {
        assert!(opening_tag("<parameter note=\"a >").is_none());
    }
    #[test]
    fn backslash_before_quote_does_not_escape_xml_quote() {
        let tag = opening_tag(r#"<parameter path="C:\">After"#).expect("complete tag");
        assert_eq!(tag.end, r#"<parameter path="C:\">"#.len());
        assert!(!tag.self_closing);
    }

    #[test]
    fn recognized_tags_are_case_insensitive_and_require_a_tag_boundary() {
        assert_eq!(
            recognized_tag("<FUNCTION_CALL name=\"run\">"),
            Some("function_call")
        );
        assert_eq!(recognized_tag("<parameter/>"), Some("parameter"));
        assert_eq!(recognized_tag("<toolbox>"), None);
        assert_eq!(recognized_tag("<functionality>"), None);
        assert_eq!(recognized_tag("<invoke"), None);
    }
    #[test]
    fn next_tag_keeps_quoted_delimiters_and_validates_closing_tags() {
        assert_eq!(
            next_tag("before <parameter note=\"a > b\"/>After"),
            Some(TagToken::Opening {
                tag: "parameter",
                start: 7,
                end: 32,
                self_closing: true,
            })
        );
        assert_eq!(
            next_tag("</FUNCTION >After"),
            Some(TagToken::Closing {
                tag: "function",
                start: 0,
                end: 12,
            })
        );
        assert_eq!(next_tag("</function extra>"), None);
        assert_eq!(next_tag("<parameter note=\"unterminated"), None);
    }
    #[test]
    fn closing_tag_end_requires_the_delimiter_after_whitespace() {
        assert_eq!(closing_tag_end(">After"), Some(1));
        assert_eq!(closing_tag_end(" \t>After"), Some(3));
        assert_eq!(closing_tag_end("\u{2003}>After"), Some(4));
        assert_eq!(closing_tag_end(" extra>"), None);
        assert_eq!(closing_tag_end(" <tool>"), None);
    }

    #[test]
    fn sanitizer_removes_nested_elements_without_losing_quoted_self_closing_suffixes() {
        let mut sanitizer = Sanitizer::default();
        assert_eq!(
            sanitizer.push("Before <tool_calls><tool_calls note=\"a > b\"/></tool_calls>After"),
            "Before After"
        );
        assert_eq!(sanitizer.finish(), "");
    }

    #[test]
    fn sanitizer_reassembles_split_tags_and_retains_unclosed_content_at_finish() {
        let mut sanitizer = Sanitizer::default();
        assert_eq!(sanitizer.push("Before <para"), "Before ");
        assert_eq!(sanitizer.push("meter>duplicate</parameter>After"), "After");
        assert_eq!(sanitizer.finish(), "");

        assert_eq!(sanitizer.push("<tool>working"), "");
        assert_eq!(sanitizer.finish(), "working");
    }

    #[test]
    fn sanitizer_drops_confirmed_incomplete_tag_prefixes_at_finish() {
        let mut sanitizer = Sanitizer::default();
        assert_eq!(sanitizer.push("Before <para"), "Before ");
        assert_eq!(sanitizer.finish(), "");

        assert_eq!(sanitizer.push("<parameter>duplicate"), "");
        assert_eq!(sanitizer.finish(), "");

        assert_eq!(sanitizer.push("Before <parameter "), "Before ");
        assert_eq!(sanitizer.finish(), "<parameter ");

        assert_eq!(sanitizer.push("<parameter><tool "), "");
        assert_eq!(sanitizer.finish(), "");

        assert_eq!(sanitizer.push("<parameter>duplicate `"), "");
        assert_eq!(sanitizer.finish(), "");
    }

    #[test]
    fn unterminated_tool_fallback_preserves_body_and_nested_tool_literal() {
        let mut sanitizer = Sanitizer::default();
        assert_eq!(
            sanitizer.push("<tool>body <parameter>duplicate</parameter>"),
            ""
        );
        assert_eq!(sanitizer.finish(), "body ");

        let mut nested = Sanitizer::default();
        assert_eq!(nested.push("<tool>body <tool>literal</tool>"), "");
        assert_eq!(nested.finish(), "body literal");

        let mut fragmented = Sanitizer::default();
        assert_eq!(fragmented.push("<tool>first"), "");
        assert_eq!(fragmented.push(" second"), "");
        assert_eq!(fragmented.finish(), "first second");
    }

    #[test]
    fn unterminated_tool_fallback_uses_the_recovered_output_markdown_position() {
        let mut tilde = Sanitizer::default();
        assert_eq!(
            tilde.push("prefix <tool>~~~x <parameter>duplicate</parameter>"),
            "prefix "
        );
        assert_eq!(tilde.finish(), "~~~x ");

        let mut backtick = Sanitizer::default();
        assert_eq!(backtick.push("prefix <tool>``"), "prefix ");
        assert_eq!(backtick.push("`x <parameter>duplicate</parameter>"), "");
        assert_eq!(backtick.finish(), "```x ");

        let mut line_start = Sanitizer::default();
        assert_eq!(
            line_start.push("before\n<tool>```text\n<parameter>literal</parameter>"),
            "before\n"
        );
        assert_eq!(
            line_start.finish(),
            "```text\n<parameter>literal</parameter>"
        );
    }

    #[test]
    fn unterminated_tool_fallback_drops_incomplete_nested_openings() {
        let mut plain = Sanitizer::default();
        assert_eq!(plain.push("<tool>body <parameter "), "");
        assert_eq!(plain.finish(), "body ");

        let mut attributed = Sanitizer::default();
        assert_eq!(
            attributed.push("<tool>body <parameter note=\"unterminated"),
            ""
        );
        assert_eq!(attributed.finish(), "body ");

        let mut fragmented = Sanitizer::default();
        assert_eq!(fragmented.push("<tool>body <para"), "");
        assert_eq!(fragmented.push("meter "), "");
        assert_eq!(fragmented.finish(), "body ");

        let mut unmatched_inline = Sanitizer::default();
        assert_eq!(
            unmatched_inline.push("<tool>body `candidate <parameter "),
            ""
        );
        assert_eq!(unmatched_inline.finish(), "body `candidate ");

        let mut fragmented_inline = Sanitizer::default();
        assert_eq!(fragmented_inline.push("<tool>body `candidate <para"), "");
        assert_eq!(fragmented_inline.push("meter note=\"unterminated"), "");
        assert_eq!(fragmented_inline.finish(), "body `candidate ");
    }

    #[test]
    fn sanitizer_reassembles_an_opening_tag_split_after_its_name() {
        let mut sanitizer = Sanitizer::default();
        assert_eq!(sanitizer.push("Before <tool "), "Before ");
        assert_eq!(
            sanitizer.push("name=\"run\">duplicate</tool>After"),
            "After"
        );
        assert_eq!(sanitizer.finish(), "");
    }

    #[test]
    fn sanitizer_scans_markup_after_a_closed_single_backtick_run() {
        let mut sanitizer = Sanitizer::default();
        assert_eq!(
            sanitizer.push("before `literal` <tool>duplicate</tool> after"),
            "before `literal`  after"
        );
        assert_eq!(sanitizer.finish(), "");
    }

    #[test]
    fn sanitizer_preserves_an_escaped_tag_split_across_fragments() {
        let mut sanitizer = Sanitizer::default();
        assert_eq!(sanitizer.push(r"\<tool"), r"\<tool");
        assert_eq!(
            sanitizer.push(">duplicate</tool> after"),
            ">duplicate</tool> after"
        );
        assert_eq!(sanitizer.finish(), "");
    }

    #[test]
    fn sanitizer_stops_at_the_first_incomplete_recognized_opening() {
        let mut sanitizer = Sanitizer::default();
        assert_eq!(
            sanitizer.push("before <tool note=\"unterminated <parameter>literal</parameter> after"),
            "before "
        );
        assert_eq!(
            sanitizer.finish(),
            "<tool note=\"unterminated <parameter>literal</parameter> after"
        );
    }

    #[test]
    fn sanitizer_replays_unmatched_inline_content_and_resets_after_finish() {
        let mut sanitizer = Sanitizer::default();
        assert_eq!(
            sanitizer.push("before `literal <tool>duplicate</tool> tail"),
            "before "
        );
        assert_eq!(sanitizer.finish(), "`literal  tail");

        assert_eq!(
            sanitizer.push("next <tool>duplicate</tool> response"),
            "next  response"
        );
        assert_eq!(sanitizer.finish(), "");
    }

    #[test]
    fn sanitizer_releases_a_fragmented_closed_inline_candidate_as_literal() {
        let mut sanitizer = Sanitizer::default();
        assert_eq!(sanitizer.push("before `literal <tool>duplicate"), "before ");
        assert_eq!(
            sanitizer.push("</tool>` after <tool>removed</tool>"),
            "`literal <tool>duplicate</tool>` after "
        );
        assert_eq!(sanitizer.finish(), "");
    }

    #[test]
    fn markdown_scanner_checks_dense_candidates_once() {
        let mut input = "<x".repeat(4_096);
        input.push_str("<tool/>");
        let mut markdown = MarkdownCodeState::default();
        TAG_AT_CALLS.set(0);

        assert!(matches!(
            next_tag_outside_markdown(&input, &mut markdown, false),
            ScanResult::Tag(TagToken::Opening {
                tag: "tool",
                start: 8_192,
                self_closing: true,
                ..
            })
        ));
        assert_eq!(TAG_AT_CALLS.get(), 4_097);
    }

    #[test]
    fn sanitizer_tracks_nested_openings_and_ignores_mismatched_closings() {
        let mut sanitizer = Sanitizer::default();
        assert_eq!(
            sanitizer.push("<tool>outer <tool>inner</tool></function></tool>After"),
            "After"
        );
        assert_eq!(sanitizer.finish(), "");
    }

    #[test]
    fn tag_classifier_retains_only_plausible_incomplete_tags() {
        assert_eq!(tag_at("<tool "), TagAt::Incomplete);
        assert_eq!(tag_at("<tool note=\"a >"), TagAt::Incomplete);
        assert_eq!(tag_at("</tool"), TagAt::Incomplete);
        assert_eq!(tag_at("</tool \u{2003}"), TagAt::Incomplete);
        assert!(matches!(tag_at("</tool>"), TagAt::Complete(_)));
        assert!(matches!(tag_at("</tool >"), TagAt::Complete(_)));
        assert_eq!(tag_at("</tool extra"), TagAt::None);
        assert_eq!(tag_at("<toolbox"), TagAt::None);
        assert_eq!(tag_at("</toolbox"), TagAt::None);
    }

    #[test]
    fn markup_detection_keeps_complete_and_split_recognized_tags() {
        assert!(Sanitizer::may_contain_markup("<tool>duplicate</tool>"));
        assert!(Sanitizer::may_contain_markup("before <parameter "));
        assert!(Sanitizer::may_contain_markup(
            "before <parameter note=\"a >"
        ));
        assert!(Sanitizer::may_contain_markup(
            "before <tool note=\"unterminated <parameter>literal</parameter>"
        ));
        assert!(!Sanitizer::may_contain_markup("ordinary assistant text"));
    }

    #[test]
    fn sanitizer_releases_a_closing_tag_as_soon_as_its_tail_is_impossible() {
        let mut sanitizer = Sanitizer::default();
        assert_eq!(sanitizer.push("Before </tool "), "Before ");
        assert_eq!(sanitizer.push("extra"), "</tool extra");
        assert_eq!(sanitizer.finish(), "");
    }

    #[test]
    fn markdown_state_tracks_inline_code_and_escapes_byte_by_byte() {
        let mut state = MarkdownCodeState::default();
        state.consume("`code");
        assert!(!state.permits_markup());
        state.consume("` after");
        assert!(state.permits_markup());

        state.consume("\\");
        assert!(!state.permits_markup());
        state.consume("\\");
        assert!(state.permits_markup());
        state.consume("\\\n");
        assert!(state.permits_markup());

        state.consume("\\```");
        state.resolve_pending_marker();
        assert!(!state.permits_markup());
        assert_eq!(state.inline_ticks, Some(2));
        state.consume("code`` after");
        assert!(state.permits_markup());

        let mut inline = MarkdownCodeState::default();
        inline.consume("`foo\\` after");
        assert!(inline.permits_markup());
        assert_eq!(inline.inline_ticks, None);

        inline.consume("`x");
        assert!(!inline.permits_markup());
        inline.consume("~x");
        assert!(!inline.permits_markup());
        inline.consume("` after");
        assert!(inline.permits_markup());
    }

    #[test]
    fn markdown_state_requires_matching_fence_marker_length_and_line_placement() {
        let mut state = MarkdownCodeState::default();
        state.consume("````\n");
        assert!(!state.permits_markup());
        state.consume("~~~\n");
        assert!(!state.permits_markup());
        state.consume("```\n");
        assert!(!state.permits_markup());
        state.consume("text ```` still code\n");
        assert!(!state.permits_markup());
        state.consume("```` trailing text\n");
        assert!(!state.permits_markup());
        state.consume("   ````  \n");
        assert!(state.permits_markup());

        let mut midline = MarkdownCodeState::default();
        midline.consume("text ``` code");
        assert_eq!(midline.fence, None);
        assert_eq!(midline.inline_ticks, Some(3));
        midline.consume("``` after");
        assert!(midline.permits_markup());

        let mut midline_tildes = MarkdownCodeState::default();
        midline_tildes.consume("text ~~~ code");
        assert_eq!(midline_tildes.fence, None);
        assert!(midline_tildes.permits_markup());

        let mut indented = MarkdownCodeState::default();
        indented.consume("    ~~~\n");
        assert_eq!(indented.fence, None);
        assert!(indented.permits_markup());
    }

    #[test]
    fn markdown_state_reinterprets_invalid_backtick_fence_info_as_inline_code() {
        let mut valid = MarkdownCodeState::default();
        valid.consume("```lang\nbody with ` tick\n");
        assert!(valid.fence.is_some());
        assert_eq!(valid.inline_ticks, None);
        valid.consume("```\nafter");
        assert!(valid.permits_markup());

        let mut unmatched = MarkdownCodeState::default();
        unmatched.consume("```lang`oops\n<tool>outside</tool>");
        assert_eq!(unmatched.fence, None);
        assert_eq!(unmatched.inline_ticks, Some(3));
        assert_eq!(unmatched.finish(), MarkdownFinish::ReplayUnmatchedInline);

        let mut matched = MarkdownCodeState::default();
        matched.consume("```lang`oops``` after");
        assert_eq!(matched.fence, None);
        assert!(matched.permits_markup());
        assert_eq!(matched.finish(), MarkdownFinish::Complete);
    }

    #[test]
    fn markdown_state_is_invariant_to_fragment_boundaries() {
        for input in [
            "```xml\n<tool>literal</tool>\n```\nafter",
            "before `inline <tool>` after",
            "\\```two ticks`` after",
            "~~~\nbody\n~~~",
            "```lang`oops\n<tool>outside</tool>",
        ] {
            let mut whole = MarkdownCodeState::default();
            whole.consume(input);

            let mut fragmented = MarkdownCodeState::default();
            for byte in input.as_bytes() {
                fragmented.consume(std::str::from_utf8(std::slice::from_ref(byte)).expect("ASCII"));
            }
            assert_eq!(fragmented, whole, "one-byte fragments for {input:?}");

            for split in 0..=input.len() {
                let mut split_state = MarkdownCodeState::default();
                split_state.consume(&input[..split]);
                split_state.consume(&input[split..]);
                assert_eq!(split_state, whole, "split {split} for {input:?}");
            }
        }
    }

    #[test]
    fn markdown_state_reports_unmatched_inline_candidates_at_finish() {
        let mut unmatched = MarkdownCodeState::default();
        unmatched.consume("before `literal <tool> text");
        assert!(!unmatched.permits_markup());
        assert_eq!(unmatched.finish(), MarkdownFinish::ReplayUnmatchedInline);
        assert!(unmatched.permits_markup());

        let mut matched = MarkdownCodeState::default();
        matched.consume("before `literal` after");
        assert_eq!(matched.finish(), MarkdownFinish::Complete);

        let mut terminal_fence = MarkdownCodeState::default();
        terminal_fence.consume("~~~\nbody\n~~~");
        assert_eq!(terminal_fence.finish(), MarkdownFinish::Complete);
        assert!(terminal_fence.permits_markup());
    }
}
