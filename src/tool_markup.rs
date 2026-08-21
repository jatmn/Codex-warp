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
    active_tag: Option<&'static str>,
    active_depth: usize,
}

#[allow(dead_code)] // wired by the following response-adapter layer
impl Sanitizer {
    /// Whether a fragment contains a complete recognized tag or a suffix that
    /// could become one in the next stream fragment.
    pub(crate) fn may_contain_markup(fragment: &str) -> bool {
        next_tag(fragment).is_some() || possible_tag_start(fragment).is_some()
    }

    pub(crate) fn push(&mut self, fragment: &str) -> String {
        let mut input = std::mem::take(&mut self.pending);
        input.push_str(fragment);
        let mut output = String::new();

        while !input.is_empty() {
            let Some(token) = next_tag(&input) else {
                let split_at = possible_tag_start(&input).unwrap_or(input.len());
                if self.active_tag.is_some() {
                    self.pending = input;
                } else {
                    output.push_str(&input[..split_at]);
                    self.pending = input[split_at..].to_string();
                }
                break;
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
                    if self.active_tag.is_none() && !self_closing {
                        self.active_tag = Some(tag);
                        self.active_depth = 1;
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
                        }
                    } else if self.active_tag.is_none() {
                        output.push_str(&input[start..end]);
                    }
                    end
                }
            };
            input = input[end..].to_string();
        }
        output
    }

    /// On a normal terminal, retain unclosed material rather than silently
    /// deleting user text. Complete elements have already been omitted.
    pub(crate) fn finish(&mut self) -> String {
        self.active_tag = None;
        self.active_depth = 0;
        std::mem::take(&mut self.pending)
    }
}

/// Tracks Markdown contexts in which XML-like text must remain literal.
/// Scanner integration is intentionally added by the next stack slice.
#[allow(dead_code)]
#[derive(Default)]
struct MarkdownCodeState {
    fence: Option<(u8, usize)>,
    inline_ticks: Option<usize>,
    escaped: bool,
}

#[allow(dead_code)]
impl MarkdownCodeState {
    fn permits_markup(&self) -> bool {
        self.fence.is_none() && self.inline_ticks.is_none() && !self.escaped
    }

    fn consume(&mut self, text: &str) {
        for run in text.as_bytes().chunk_by(|left, right| left == right) {
            let byte = run[0];
            if byte == b'\n' {
                self.escaped = false;
                continue;
            }
            if byte == b'\\' {
                if !run.len().is_multiple_of(2) {
                    self.escaped = !self.escaped;
                }
                continue;
            }
            if matches!(byte, b'`' | b'~') && !self.escaped {
                let count = run.len();
                if let Some((marker, length)) = self.fence {
                    if marker == byte && count >= length {
                        self.fence = None;
                    }
                } else if let Some(length) = self.inline_ticks {
                    if byte == b'`' && count == length {
                        self.inline_ticks = None;
                    }
                } else if count >= 3 {
                    self.fence = Some((byte, count));
                } else if byte == b'`' {
                    self.inline_ticks = Some(count);
                }
                self.escaped = false;
                continue;
            }
            self.escaped = false;
        }
    }
}

fn possible_tag_start(input: &str) -> Option<usize> {
    let start = input.rfind('<')?;
    let suffix = &input[start..];
    TAGS.into_iter().find_map(|tag| {
        let opening = format!("<{tag}");
        let closing = format!("</{tag}");
        let opening_prefix = opening
            .get(..suffix.len())
            .is_some_and(|prefix| prefix.eq_ignore_ascii_case(suffix));
        let closing_prefix = closing
            .get(..suffix.len())
            .is_some_and(|prefix| prefix.eq_ignore_ascii_case(suffix));
        let opening_continues = suffix
            .get(..opening.len())
            .is_some_and(|prefix| prefix.eq_ignore_ascii_case(&opening))
            && suffix
                .as_bytes()
                .get(opening.len())
                .is_some_and(|byte| byte.is_ascii_whitespace() || matches!(byte, b'>' | b'/'));
        let closing_continues = suffix
            .get(..closing.len())
            .is_some_and(|prefix| prefix.eq_ignore_ascii_case(&closing))
            && suffix
                .as_bytes()
                .get(closing.len())
                .is_some_and(|byte| byte.is_ascii_whitespace() || *byte == b'>');
        (opening_prefix || closing_prefix || opening_continues || closing_continues)
            .then_some(start)
    })
}

#[cfg(test)]
mod tests {
    use super::MarkdownCodeState;
    use super::Sanitizer;
    use super::TagToken;
    use super::closing_tag_end;
    use super::next_tag;
    use super::opening_tag;
    use super::possible_tag_start;
    use super::recognized_tag;
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
    fn sanitizer_tracks_nested_openings_and_ignores_mismatched_closings() {
        let mut sanitizer = Sanitizer::default();
        assert_eq!(
            sanitizer.push("<tool>outer <tool>inner</tool></function></tool>After"),
            "After"
        );
        assert_eq!(sanitizer.finish(), "");
    }

    #[test]
    fn possible_tag_start_retains_only_plausible_incomplete_tags() {
        assert_eq!(possible_tag_start("Before <tool "), Some(7));
        assert_eq!(possible_tag_start("Before </tool"), Some(7));
        assert_eq!(possible_tag_start("Before </tool>"), Some(7));
        assert_eq!(possible_tag_start("Before <toolbox"), None);
        assert_eq!(possible_tag_start("Before </toolbox"), None);
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
        assert!(!state.permits_markup());
        state.consume("`");
        assert!(state.permits_markup());

        state.consume("\\");
        assert!(!state.permits_markup());
        state.consume("\\");
        assert!(state.permits_markup());
        state.consume("\\\n");
        assert!(state.permits_markup());

        state.consume("\\```");
        assert!(state.permits_markup());
        state.consume("~~");
        assert!(state.permits_markup());
        state.consume("~");
        assert!(state.permits_markup());
    }

    #[test]
    fn markdown_state_requires_matching_fence_marker_and_length() {
        let mut state = MarkdownCodeState::default();
        state.consume("````");
        assert!(!state.permits_markup());
        state.consume("~~~");
        assert!(!state.permits_markup());
        state.consume("```");
        assert!(!state.permits_markup());
        state.consume("````");
        assert!(state.permits_markup());
    }
}
