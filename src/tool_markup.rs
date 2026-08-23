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
    let mut escaped = false;
    for (offset, byte) in input.as_bytes().iter().copied().enumerate() {
        if let Some(delimiter) = quote {
            if escaped {
                escaped = false;
            } else if byte == b'\\' {
                escaped = true;
            } else if byte == delimiter {
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
        let mut cursor = 0;
        while let Some(remaining) = input
            .get(cursor..)
            .filter(|remaining| !remaining.is_empty())
        {
            let scan = if self.markdown_disabled || self.active_tag.is_some() {
                // Suppressed markup is payload, not Markdown syntax.
                next_tag_without_markdown(remaining)
            } else {
                next_tag_outside_markdown(
                    remaining,
                    &mut self.markdown,
                    !self.replay_buffer.is_empty(),
                )
            };
            let token = match scan {
                ScanResult::Tag(token) => token,
                ScanResult::Pending { start } => {
                    if self.active_tag.is_some() {
                        self.pending = remaining.to_string();
                    } else {
                        output.push_str(&remaining[..start]);
                        self.pending = remaining[start..].to_string();
                    }
                    break;
                }
                ScanResult::Defer { start } => {
                    if self.active_tag.is_some() {
                        self.pending = remaining.to_string();
                    } else {
                        output.push_str(&remaining[..start]);
                        self.replay_buffer.push_str(&remaining[start..]);
                    }
                    break;
                }
                ScanResult::Release { end } => {
                    output.push_str(&std::mem::take(&mut self.replay_buffer));
                    output.push_str(&remaining[..end]);
                    cursor = cursor
                        .checked_add(end)
                        .expect("scanner remains within input");
                    continue;
                }
                ScanResult::Complete => {
                    if self.active_tag.is_some() {
                        self.pending = remaining.to_string();
                    } else {
                        output.push_str(remaining);
                    }
                    break;
                }
            };
            let start = match token {
                TagToken::Opening { start, .. } | TagToken::Closing { start, .. } => start,
            };

            if self.active_tag.is_none() {
                output.push_str(&remaining[..start]);
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
                            self.unterminated_tool_body = remaining[end..].to_string();
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
                        output.push_str(&remaining[start..end]);
                        self.markdown.consume(&remaining[start..end]);
                    }
                    end
                }
            };
            cursor = cursor
                .checked_add(end)
                .expect("token end remains within the input buffer");
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
            if self.markdown_disabled && !pending_was_suppressed {
                return replay_buffer + &pending;
            }
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
            if let Some(end) = closing_tag_end(&closing[tag.len()..]) {
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
                    TagAt::Complete(token) => return ScanResult::Tag(token.shifted(start)),
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
#[derive(Debug, Clone, PartialEq, Eq)]
struct MarkdownCodeState {
    fence: Option<Fence>,
    opening_backtick_fence: bool,
    inline_ticks: Option<usize>,
    pending_marker: Option<PendingMarker>,
    closing_fence: bool,
    leading_spaces: Option<usize>,
    indented_line: bool,
    escaped: bool,
}

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

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum MarkdownFinish {
    Complete,
    ReplayUnmatchedInline,
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
            indented_line: false,
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

    fn permits_markup(&mut self) -> bool {
        self.resolve_pending_marker();
        self.fence.is_none() && self.inline_ticks.is_none() && !self.indented_line && !self.escaped
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
            if self.inline_ticks.is_none() && self.indented_line {
                self.mark_nonspace();
                return;
            }
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
        } else if let Some(length) = self.inline_ticks {
            if pending.marker == b'`' && pending.length == length {
                self.inline_ticks = None;
            }
        } else if pending.leading_spaces.is_some_and(|spaces| spaces <= 3) && pending.length >= 3 {
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
        self.indented_line = false;
    }

    fn mark_nonspace(&mut self) {
        self.leading_spaces = None;
    }

    fn track_line_byte(&mut self, byte: u8) {
        if self.inline_ticks.is_some() {
            self.mark_nonspace();
            return;
        }
        if byte == b'\t' && self.leading_spaces.is_some() {
            self.indented_line = true;
        }
        if byte == b' ' {
            if !self.indented_line
                && let Some(spaces) = self.leading_spaces.as_mut()
            {
                *spaces = spaces
                    .checked_add(1)
                    .expect("Markdown indentation fits usize");
                self.indented_line = *spaces >= 4;
            }
        } else {
            self.mark_nonspace();
        }
    }
}

fn possible_tag_at_start(suffix: &str) -> bool {
    TAGS.into_iter()
        .find_map(|tag| {
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
                && closing_tag_end(&suffix[closing.len()..]).is_none()
                && suffix[closing.len()..].chars().all(char::is_whitespace);
            (opening_prefix || closing_prefix || closing_continues).then_some(())
        })
        .is_some()
}

#[cfg(test)]
mod tests {
    use super::Fence;
    use super::MarkdownCodeState;
    use super::Sanitizer;
    use super::ScanResult;
    use super::TAG_AT_CALLS;
    use super::TagAt;
    use super::TagToken;
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
    }

    #[test]
    fn unterminated_tool_fallback_preserves_body() {
        let mut sanitizer = Sanitizer::default();
        assert_eq!(
            sanitizer.push("<tool>body <parameter>duplicate</parameter>"),
            ""
        );
        assert_eq!(sanitizer.finish(), "body ");

        let mut fragmented = Sanitizer::default();
        assert_eq!(fragmented.push("<tool>first"), "");
        assert_eq!(fragmented.push(" second"), "");
        assert_eq!(fragmented.finish(), "first second");
    }

    #[test]
    fn tool_fallback_uses_original_markdown_position_and_marker_closing_rules() {
        let mut sanitizer = Sanitizer::default();
        assert_eq!(
            sanitizer.push("prefix <tool>~~~x <parameter>duplicate</parameter>"),
            "prefix "
        );
        assert_eq!(sanitizer.finish(), "~~~x ");

        let mut marker = Sanitizer::default();
        assert_eq!(marker.push("<tool>body <tool>literal</tool>"), "");
        assert_eq!(marker.finish(), "body literal");

        let mut unmatched_closing = Sanitizer::default();
        assert_eq!(unmatched_closing.push("<tool>body </function>"), "");
        assert_eq!(unmatched_closing.finish(), "body </function>");
    }

    #[test]
    fn terminal_replays_unmatched_inline_and_resets_markdown() {
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
    fn fences_require_line_position_and_valid_closing_line() {
        let mut sanitizer = Sanitizer::default();
        assert_eq!(
            sanitizer.push("text ``` code <tool>literal</tool>"),
            "text "
        );
        assert_eq!(sanitizer.finish(), "``` code ");

        let mut closing = Sanitizer::default();
        assert_eq!(
            closing.push("~~~\n<tool>literal</tool>\n~~~ trailing\n<tool>still-literal</tool>"),
            "~~~\n<tool>literal</tool>\n~~~ trailing\n<tool>still-literal</tool>"
        );
        assert_eq!(closing.finish(), "");
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
    fn sanitizer_reassembles_an_opening_tag_split_inside_a_quoted_attribute() {
        let mut sanitizer = Sanitizer::default();
        assert_eq!(sanitizer.push("Before <tool arg=\"a <"), "Before ");
        assert_eq!(sanitizer.push(" b\">duplicate</tool>After"), "After");
        assert_eq!(sanitizer.finish(), "");
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
    fn tag_classifier_retains_earliest_plausible_incomplete_tag() {
        assert_eq!(tag_at("<tool "), TagAt::Incomplete);
        assert_eq!(tag_at("<tool note=\"a >"), TagAt::Incomplete);
        assert_eq!(tag_at("</tool"), TagAt::Incomplete);
        assert_eq!(tag_at("</to"), TagAt::Incomplete);
        assert_eq!(tag_at("</tool extra"), TagAt::None);
        assert_eq!(tag_at("<toolbox"), TagAt::None);

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
    fn markup_detection_keeps_complete_and_split_recognized_tags() {
        assert!(Sanitizer::may_contain_markup("<tool>duplicate</tool>"));
        assert!(Sanitizer::may_contain_markup("before <parameter "));
        assert!(!Sanitizer::may_contain_markup("ordinary assistant text"));
    }

    #[test]
    fn markdown_state_tracks_inline_code_and_escapes() {
        let mut state = MarkdownCodeState::default();
        state.consume("`");
        state.resolve_pending_marker();
        assert!(!state.permits_markup());
        state.consume("`");
        state.resolve_pending_marker();
        assert!(state.permits_markup());

        state.consume("\\");
        assert!(!state.permits_markup());
        state.consume("\\");
        assert!(state.permits_markup());
        state.consume("\\\n");
        assert!(state.permits_markup());

        state.consume("\\```");
        assert!(!state.permits_markup());
        state.consume("~~");
        assert!(!state.permits_markup());
        state.consume("~");
        assert!(!state.permits_markup());
        state.consume("``");
        assert!(state.permits_markup());

        let mut inline = MarkdownCodeState::default();
        inline.consume("`x");
        assert!(!inline.permits_markup());
        inline.consume("~x");
        assert!(!inline.permits_markup());
        inline.consume("`x");
        assert!(inline.permits_markup());
    }

    #[test]
    fn markdown_state_requires_matching_fence_marker_and_length() {
        let mut state = MarkdownCodeState::default();
        state.consume("````");
        state.resolve_pending_marker();
        assert!(!state.permits_markup());
        state.consume("~~~");
        state.resolve_pending_marker();
        assert!(!state.permits_markup());
        state.consume("```");
        state.resolve_pending_marker();
        assert!(!state.permits_markup());
        state.consume("````");
        state.resolve_pending_marker();
        assert!(state.permits_markup());
    }

    #[test]
    fn markdown_state_reassembles_split_fence_runs() {
        let mut state = MarkdownCodeState::default();
        state.consume("``");
        state.consume("`");
        state.consume("xml\n");
        assert!(!state.permits_markup());
        state.consume("``");
        state.consume("`\n");
        assert!(state.permits_markup());

        let mut inline = MarkdownCodeState::default();
        inline.consume("`x");
        assert!(!inline.permits_markup());
        inline.consume("~x");
        assert!(!inline.permits_markup());
        inline.consume("`x");
        assert!(inline.permits_markup());

        let mut short_tilde = MarkdownCodeState::default();
        short_tilde.consume("~~x");
        assert!(short_tilde.permits_markup());

        let mut info_string = MarkdownCodeState::default();
        info_string.consume("```xml");
        assert_eq!(
            info_string.fence,
            Some(Fence {
                marker: b'`',
                length: 3,
            })
        );
        assert!(info_string.opening_backtick_fence);
        assert_eq!(info_string.inline_ticks, None);
    }

    #[test]
    fn sanitizer_preserves_escaped_and_markdown_literal_markup() {
        let mut sanitizer = Sanitizer::default();
        assert_eq!(
            sanitizer.push("Use ` <tool>literal</tool> ` and \\<parameter>escaped</parameter>."),
            "Use ` <tool>literal</tool> ` and \\<parameter>escaped</parameter>."
        );
        assert_eq!(sanitizer.finish(), "");
        let mut fenced = sanitizer.push("\n```xml\n<function>example</function>\n```\n");
        fenced.push_str(&sanitizer.finish());
        assert_eq!(fenced, "\n```xml\n<function>example</function>\n```\n");
        assert_eq!(sanitizer.push("<tool>duplicate</tool>After"), "After");
    }

    #[test]
    fn terminal_retains_escaped_and_unmatched_inline_literal_suffixes() {
        let mut escaped = Sanitizer::default();
        assert_eq!(escaped.push("Use \\<tool"), "Use \\<tool");
        assert_eq!(escaped.finish(), "");

        let mut inline = Sanitizer::default();
        assert_eq!(inline.push("Use `literal <tool"), "Use ");
        assert_eq!(inline.finish(), "`literal <tool");
    }

    #[test]
    fn sanitizer_preserves_fence_delimiters_split_across_chunks() {
        let mut sanitizer = Sanitizer::default();
        let mut output = sanitizer.push("Here is XML:\n``");
        output.push_str(&sanitizer.push("`xml\n<function>literal</function>\n"));
        output.push_str(&sanitizer.push("```\n<invoke>duplicate</invoke>After"));
        output.push_str(&sanitizer.finish());

        assert!(output.contains("<function>literal</function>"));
        assert!(!output.contains("duplicate"));
        assert!(output.ends_with("After"));
    }

    #[test]
    fn sanitizer_preserves_indented_code_and_resumes_after_newline() {
        let mut sanitizer = Sanitizer::default();
        let output = sanitizer.push(
            "    <parameter>spaces</parameter>\n\t<function>tab</function>\n<invoke>duplicate</invoke>After",
        ) + &sanitizer.finish();

        assert!(output.contains("<parameter>spaces</parameter>"));
        assert!(output.contains("<function>tab</function>"));
        assert!(!output.contains("duplicate"));
        assert!(output.ends_with("After"));
    }

    #[test]
    fn markdown_delimiters_inside_suppressed_markup_do_not_change_state() {
        let mut sanitizer = Sanitizer::default();
        assert_eq!(
            sanitizer.push("<tool note=\"```\">```<function>x</function>```</tool>After"),
            "After"
        );
        assert_eq!(
            sanitizer.push("<parameter>duplicate</parameter>Done"),
            "Done"
        );
    }

    #[test]
    fn sanitizer_handles_dense_and_incrementally_split_markup() {
        let mut sanitizer = Sanitizer::default();
        let dense = "<parameter/>".repeat(2_000) + "After";
        assert_eq!(sanitizer.push(&dense), "After");

        let mut split = Sanitizer::default();
        assert_eq!(split.push("<parameter name=\""), "");
        for _ in 0..2_000 {
            assert_eq!(split.push("x"), "");
        }
        assert_eq!(split.push("\"/>After"), "After");
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
}
