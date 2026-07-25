use crate::consts::*;
use crate::entities::{
  decode_html_entities, decode_html_entities_for_markdown, is_entity_reference_after_ampersand,
  max_entity_name_length,
};
use crate::scan::{
  is_whitespace, process_bogus_comment, process_comment_or_doctype, process_tag_attributes,
};
use crate::selector::{matches_selector, parse_css_selector};
use crate::tags::get_tag_handler;
use crate::tailwind::process_tailwind_classes;
use crate::types::{
  ElementNode, ExtractedElement, HTMLToMarkdownOptions, OutputFormat, ParsedSelector,
  StreamingError, TagHandler, TailwindData,
};
use crate::url::{is_autolink_uri, normalize_fragment, resolve_url_with_policy, slugify_heading};
use std::borrow::Cow;
use std::collections::TryReserveError;

mod output;
mod parse;
mod plugins;

/// Tracked element during extraction — maps stack depth to accumulator
pub(crate) struct TrackedExtraction {
  pub(crate) selector: String,
  pub(crate) stack_depth: usize,
  pub(crate) text_content: String,
  pub(crate) tag_name: String,
  pub(crate) attributes: Vec<(String, String)>,
  pub(crate) limit_exceeded: bool,
}

const MAX_TABLE_COLUMNS: usize = 256;
const MAX_POOLED_TABLE_COLUMNS: usize = 64;
const MAX_NODE_POOL_SIZE: usize = 64;
const MAX_EXTRACTION_SELECTORS: usize = 64;
const MAX_ACTIVE_EXTRACTIONS: usize = 64;
const MAX_EXTRACTION_RESULTS: usize = 256;
const MAX_EXTRACTION_RESULT_BYTES: usize = 64 * 1024;
const MAX_EXTRACTION_BYTES: usize = 256 * 1024;
const MAX_FRONTMATTER_FIELDS: usize = 64;
const MAX_FRONTMATTER_VALUE_BYTES: usize = 64 * 1024;
const MAX_FRONTMATTER_BYTES: usize = 256 * 1024;

#[inline(always)]
fn is_inline_gfm_hazard(byte: u8) -> bool {
  const LOW: u64 = (1 << b'&') | (1 << b'*') | (1 << b'<');
  const HIGH: u64 = (1 << (b'[' - 64))
    | (1 << (b'\\' - 64))
    | (1 << (b'_' - 64))
    | (1 << (b'`' - 64))
    | (1 << (b'~' - 64));
  let mask = if byte < 64 { LOW } else { HIGH };
  (mask >> (byte & 63)) & 1 != 0
}

struct CodeSpanState {
  output_start: usize,
  content_start: usize,
}

struct CodeFenceState {
  output_start: usize,
  marker_offset: usize,
  content_start: usize,
  indent: String,
  language: String,
}

struct SelfLinkHeadingState {
  bracket_start: usize,
  text_start: usize,
  text_len: usize,
  link_end: usize,
  fragment: String,
}

#[derive(Clone)]
struct BlockquoteFrame {
  content_start: usize,
  list_indent: String,
}

fn reserve_accounted_with<F>(
  value: &mut String,
  required_len: usize,
  retained_capacity: usize,
  limit: usize,
  reserve: F,
) -> Result<(), StreamingError>
where
  F: FnOnce(&mut String, usize) -> Result<(), TryReserveError>,
{
  if required_len <= value.capacity() {
    return Ok(());
  }
  if required_len > isize::MAX as usize {
    return Err(StreamingError::CapacityOverflow);
  }
  let old_capacity = value.capacity();
  let requested_growth = required_len
    .checked_sub(old_capacity)
    .ok_or(StreamingError::CapacityOverflow)?;
  let requested_total = retained_capacity
    .checked_add(requested_growth)
    .ok_or(StreamingError::CapacityOverflow)?;
  if requested_total > limit {
    return Err(StreamingError::BufferLimitExceeded);
  }

  let additional = required_len
    .checked_sub(value.len())
    .ok_or(StreamingError::CapacityOverflow)?;
  reserve(value, additional).map_err(|_| StreamingError::AllocationFailed)?;

  let Some(actual_total) = retained_capacity
    .checked_sub(old_capacity)
    .and_then(|total| total.checked_add(value.capacity()))
  else {
    *value = String::new();
    return Err(StreamingError::CapacityOverflow);
  };
  if actual_total > limit {
    *value = String::new();
    return Err(StreamingError::BufferLimitExceeded);
  }
  Ok(())
}

pub(crate) fn reserve_accounted(
  value: &mut String,
  required_len: usize,
  retained_capacity: usize,
  limit: usize,
) -> Result<(), StreamingError> {
  reserve_accounted_with(
    value,
    required_len,
    retained_capacity,
    limit,
    String::try_reserve_exact,
  )
}

fn vector_capacity_bytes<T>(value: &Vec<T>) -> usize {
  value.capacity().saturating_mul(std::mem::size_of::<T>())
}

fn string_pair_bytes(values: &[(String, String)], allocated_slots: usize, capacity: bool) -> usize {
  values
    .iter()
    .map(|(key, value)| {
      if capacity {
        key.capacity() + value.capacity()
      } else {
        key.len() + value.len()
      }
    })
    .sum::<usize>()
    + if capacity {
      allocated_slots * std::mem::size_of::<(String, String)>()
    } else {
      std::mem::size_of_val(values)
    }
}

fn element_dynamic_bytes(node: &ElementNode, capacity: bool) -> usize {
  let attributes = if capacity {
    node.attributes.retained_capacity()
  } else {
    node.attributes.retained_bytes()
  };
  let custom_name = node.custom_name.as_ref().map_or(0, |name| {
    if capacity {
      name.capacity()
    } else {
      name.len()
    }
  });
  let tailwind = node.tailwind.as_ref().map_or(0, |data| {
    std::mem::size_of::<TailwindData>()
      + data.prefix.as_ref().map_or(0, |value| {
        if capacity {
          value.capacity()
        } else {
          value.len()
        }
      })
      + data.suffix.as_ref().map_or(0, |value| {
        if capacity {
          value.capacity()
        } else {
          value.len()
        }
      })
  });
  attributes + custom_name + tailwind
}

fn extracted_element_bytes(element: &ExtractedElement, capacity: bool) -> usize {
  let string_bytes = |value: &String| {
    if capacity {
      value.capacity()
    } else {
      value.len()
    }
  };
  string_bytes(&element.selector)
    + string_bytes(&element.tag_name)
    + string_bytes(&element.text_content)
    + string_pair_bytes(&element.attributes, element.attributes.capacity(), capacity)
}

fn tracked_extraction_bytes(extraction: &TrackedExtraction, capacity: bool) -> usize {
  let string_bytes = |value: &String| {
    if capacity {
      value.capacity()
    } else {
      value.len()
    }
  };
  string_bytes(&extraction.selector)
    + string_bytes(&extraction.tag_name)
    + string_bytes(&extraction.text_content)
    + string_pair_bytes(
      &extraction.attributes,
      extraction.attributes.capacity(),
      capacity,
    )
}

fn reserve_vec_accounted_with<T, F>(
  value: &mut Vec<T>,
  required_len: usize,
  retained_capacity: usize,
  limit: usize,
  reserve: F,
) -> Result<(), StreamingError>
where
  F: FnOnce(&mut Vec<T>, usize) -> Result<(), TryReserveError>,
{
  if required_len <= value.capacity() {
    return Ok(());
  }
  let item_size = std::mem::size_of::<T>();
  if item_size != 0 && required_len > isize::MAX as usize / item_size {
    return Err(StreamingError::CapacityOverflow);
  }
  let old_capacity = vector_capacity_bytes(value);
  let requested_capacity = required_len
    .checked_mul(item_size)
    .ok_or(StreamingError::CapacityOverflow)?;
  let requested_growth = requested_capacity
    .checked_sub(old_capacity)
    .ok_or(StreamingError::CapacityOverflow)?;
  if retained_capacity
    .checked_add(requested_growth)
    .ok_or(StreamingError::CapacityOverflow)?
    > limit
  {
    return Err(StreamingError::BufferLimitExceeded);
  }

  reserve(value, required_len - value.len()).map_err(|_| StreamingError::AllocationFailed)?;
  let Some(actual_total) = retained_capacity
    .checked_sub(old_capacity)
    .and_then(|retained| retained.checked_add(vector_capacity_bytes(value)))
  else {
    *value = Vec::new();
    return Err(StreamingError::CapacityOverflow);
  };
  if actual_total > limit {
    *value = Vec::new();
    return Err(StreamingError::BufferLimitExceeded);
  }
  Ok(())
}

fn reserve_vec_accounted<T>(
  value: &mut Vec<T>,
  required_len: usize,
  retained_capacity: usize,
  limit: usize,
) -> Result<(), StreamingError> {
  reserve_vec_accounted_with(
    value,
    required_len,
    retained_capacity,
    limit,
    Vec::try_reserve_exact,
  )
}

static HEADING_PREFIXES: [&str; 6] = ["# ", "## ", "### ", "#### ", "##### ", "###### "];

// Clean mode bitmask flags
const CLEAN_EMPTY_LINKS: u8 = 1;
const CLEAN_FRAGMENTS: u8 = 2;
const CLEAN_REDUNDANT_LINKS: u8 = 4;
const CLEAN_SELF_LINK_HEADINGS: u8 = 8;
const CLEAN_EMPTY_IMAGES: u8 = 16;
const CLEAN_EMPTY_LINK_TEXT: u8 = 32;
const CLEAN_BLANK_LINES: u8 = 64;

const SCRIPT_DATA: u8 = 0;
const SCRIPT_DATA_ESCAPED: u8 = 1;
const SCRIPT_DATA_ESCAPED_DASH: u8 = 2;
const SCRIPT_DATA_ESCAPED_DASH_DASH: u8 = 3;
const SCRIPT_DATA_DOUBLE_ESCAPED: u8 = 4;
const SCRIPT_DATA_DOUBLE_ESCAPED_DASH: u8 = 5;
const SCRIPT_DATA_DOUBLE_ESCAPED_DASH_DASH: u8 = 6;
const TEXT_LOOKBEHIND: usize = 64;

#[derive(Clone, Copy, PartialEq, Eq)]
enum TextMode {
  Data,
  RawText,
  RcData,
  PlainText,
}

struct OverflowState {
  // A matching root end tag closes every ignored inner node. Only nested roots
  // of the same name need a counter to recover at the correct close.
  root_name: String,
  root_depth: usize,
  suppressed_name: Option<String>,
  suppressed_depth: usize,
  raw_name: Option<String>,
  raw_mode: TextMode,
  raw_is_script: bool,
}

#[derive(Clone, Copy)]
struct TokenizerContext {
  text_mode: TextMode,
  in_supported_svg: bool,
  svg_integration_point: bool,
}

#[inline]
fn is_html_void_tag(tag_id: Option<u8>) -> bool {
  matches!(
    tag_id,
    Some(
      TAG_AREA
        | TAG_BASE
        | TAG_BR
        | TAG_COL
        | TAG_EMBED
        | TAG_HR
        | TAG_IMG
        | TAG_INPUT
        | TAG_KEYGEN
        | TAG_LINK
        | TAG_META
        | TAG_PARAM
        | TAG_SOURCE
        | TAG_TRACK
        | TAG_WBR
    )
  )
}

#[derive(Clone, Copy)]
enum ScriptSequenceEnd {
  Match(usize),
  NoMatch,
  Incomplete,
}

enum ScriptScanBoundary {
  Close(usize),
  Pending(usize),
  Complete,
}

/// Outcome of feeding one chunk to the script-data scanner.
enum ScriptChunk {
  /// The `</script` close tag was found; resume normal parsing at this index.
  Closed(usize),
  /// The chunk ended inside script data; carry the raw tail from this offset
  /// into the next chunk (`chunk.len()` when everything was consumed, i.e.
  /// nothing to carry).
  Carry(usize),
}

struct ScriptScanResult {
  boundary: ScriptScanBoundary,
  state: u8,
}

#[inline(always)]
fn script_sequence_end(bytes: &[u8], name_start: usize) -> ScriptSequenceEnd {
  const SCRIPT_NAME: &[u8; 6] = b"script";
  for (offset, expected) in SCRIPT_NAME.iter().enumerate() {
    let index = name_start + offset;
    if index >= bytes.len() {
      return ScriptSequenceEnd::Incomplete;
    }
    if bytes[index] | 32 != *expected {
      return ScriptSequenceEnd::NoMatch;
    }
  }
  let delimiter_index = name_start + 6;
  if delimiter_index >= bytes.len() {
    return ScriptSequenceEnd::Incomplete;
  }
  let delimiter = bytes[delimiter_index];
  if delimiter == GT_CHAR || delimiter == SLASH_CHAR || is_whitespace(delimiter) {
    ScriptSequenceEnd::Match(delimiter_index + 1)
  } else {
    ScriptSequenceEnd::NoMatch
  }
}

/// Find the first script end tag that the HTML tokenizer would emit.
///
/// Only an incomplete `<...` boundary candidate is returned to the streaming
/// caller. All completed script data advances the persisted tokenizer state.
fn find_script_end_tag(bytes: &[u8], start: usize, initial_state: u8) -> ScriptScanResult {
  let mut state = initial_state;
  let mut i = start;

  while i < bytes.len() {
    if state == SCRIPT_DATA {
      while i < bytes.len() && bytes[i] != LT_CHAR {
        i += 1;
      }
    } else if state == SCRIPT_DATA_ESCAPED || state == SCRIPT_DATA_DOUBLE_ESCAPED {
      while i < bytes.len() && bytes[i] != LT_CHAR && bytes[i] != b'-' {
        i += 1;
      }
    }
    if i == bytes.len() {
      break;
    }

    let c = bytes[i];

    if c == LT_CHAR {
      if i + 1 == bytes.len() {
        return ScriptScanResult {
          boundary: ScriptScanBoundary::Pending(i),
          state,
        };
      }

      let next = bytes[i + 1];
      if state == SCRIPT_DATA {
        if next == SLASH_CHAR {
          match script_sequence_end(bytes, i + 2) {
            ScriptSequenceEnd::Match(_) => {
              return ScriptScanResult {
                boundary: ScriptScanBoundary::Close(i),
                state,
              };
            }
            ScriptSequenceEnd::Incomplete => {
              return ScriptScanResult {
                boundary: ScriptScanBoundary::Pending(i),
                state,
              };
            }
            ScriptSequenceEnd::NoMatch => {}
          }
        } else if next == EXCLAMATION_CHAR {
          let available = (bytes.len() - i).min(4);
          if b"<!--"[..available] == bytes[i..i + available] {
            if available < 4 {
              return ScriptScanResult {
                boundary: ScriptScanBoundary::Pending(i),
                state,
              };
            }
            state = SCRIPT_DATA_ESCAPED_DASH_DASH;
            i += 4;
            continue;
          }
        }
      } else {
        let escaped = state < SCRIPT_DATA_DOUBLE_ESCAPED;
        state = if escaped {
          SCRIPT_DATA_ESCAPED
        } else {
          SCRIPT_DATA_DOUBLE_ESCAPED
        };
        let is_end_tag = next == SLASH_CHAR;

        if is_end_tag || escaped {
          match script_sequence_end(bytes, i + if is_end_tag { 2 } else { 1 }) {
            ScriptSequenceEnd::Match(sequence_end) => {
              if is_end_tag {
                if escaped {
                  return ScriptScanResult {
                    boundary: ScriptScanBoundary::Close(i),
                    state,
                  };
                }
                state = SCRIPT_DATA_ESCAPED;
              } else {
                state = SCRIPT_DATA_DOUBLE_ESCAPED;
              }
              i = sequence_end;
              continue;
            }
            ScriptSequenceEnd::Incomplete => {
              return ScriptScanResult {
                boundary: ScriptScanBoundary::Pending(i),
                state,
              };
            }
            ScriptSequenceEnd::NoMatch => {}
          }
        }
      }
    } else {
      state = match state {
        SCRIPT_DATA_ESCAPED if c == b'-' => SCRIPT_DATA_ESCAPED_DASH,
        SCRIPT_DATA_ESCAPED_DASH if c == b'-' => SCRIPT_DATA_ESCAPED_DASH_DASH,
        SCRIPT_DATA_ESCAPED_DASH => SCRIPT_DATA_ESCAPED,
        SCRIPT_DATA_ESCAPED_DASH_DASH if c == GT_CHAR => SCRIPT_DATA,
        SCRIPT_DATA_ESCAPED_DASH_DASH if c != b'-' => SCRIPT_DATA_ESCAPED,
        SCRIPT_DATA_DOUBLE_ESCAPED if c == b'-' => SCRIPT_DATA_DOUBLE_ESCAPED_DASH,
        SCRIPT_DATA_DOUBLE_ESCAPED_DASH if c == b'-' => SCRIPT_DATA_DOUBLE_ESCAPED_DASH_DASH,
        SCRIPT_DATA_DOUBLE_ESCAPED_DASH => SCRIPT_DATA_DOUBLE_ESCAPED,
        SCRIPT_DATA_DOUBLE_ESCAPED_DASH_DASH if c == GT_CHAR => SCRIPT_DATA,
        SCRIPT_DATA_DOUBLE_ESCAPED_DASH_DASH if c != b'-' => SCRIPT_DATA_DOUBLE_ESCAPED,
        _ => state,
      };
    }

    i += 1;
  }

  ScriptScanResult {
    boundary: ScriptScanBoundary::Complete,
    state,
  }
}

/// Unified single-pass HTML-to-Markdown converter.
/// Merges parser state and markdown output state to eliminate callback overhead,
/// duplicate state tracking, and enable full inlining of tag handler logic.
pub struct ConvertState {
  // === Parser state ===
  pub depth_map: [u16; MAX_TAG_ID],
  pub depth: usize,
  has_encoded_html_entity: bool,
  last_char_was_whitespace: bool,
  text_buffer_contains_whitespace: bool,
  text_buffer_contains_non_whitespace: bool,
  text_buffer_has_inline_gfm_hazard: bool,
  just_closed_tag: bool,
  is_first_text_in_element: bool,
  in_non_nesting: bool,
  script_data_state: u8,
  in_pre: bool,
  overflow: Option<OverflowState>,
  /// Filter: depth of the shallowest currently-open visually-hidden element, or
  /// None. Lets the parser skip a hidden subtree in O(1) without re-checking
  /// styles per node, and keeps this state off the public `ElementNode`.
  hidden_since_depth: Option<usize>,
  /// Unified collapse depth counter (replaces separate counters in ParseState + MarkdownState)
  collapse_non_span_depth: u16,
  collapse_span_depth: u16,
  first_block_parent_index: Option<usize>,
  block_parent_indices: Vec<usize>,
  parse_text_buffer: String,
  script_text_buffer: String,
  pub stack: Vec<ElementNode>,
  tokenizer_contexts: Vec<TokenizerContext>,
  node_pool: Vec<ElementNode>,

  // Plugin flags
  has_plugins: bool,
  has_tailwind: bool,
  has_isolate_main: bool,
  pub has_frontmatter: bool,
  has_filter: bool,
  pub has_extraction: bool,
  has_tag_overrides: bool,

  // Plugin tracking
  isolate_main_found: bool,
  isolate_main_closed: bool,
  isolate_first_header_depth: Option<usize>,
  isolate_fallback_output_start: Option<usize>,
  isolate_after_footer: bool,

  frontmatter_in_head: bool,
  pub frontmatter_title: Option<String>,
  pub frontmatter_meta: Vec<(String, String)>,

  extraction_parsed_selectors: Vec<(String, ParsedSelector)>,
  extraction_tracked: Vec<TrackedExtraction>,
  pub extraction_results: Vec<ExtractedElement>,

  filter_include_parsed: Vec<(String, ParsedSelector)>,
  filter_exclude_parsed: Vec<(String, ParsedSelector)>,
  filter_process_children: bool,

  // === Markdown output state ===
  pub options: HTMLToMarkdownOptions,
  pub buffer: String,
  last_content_start: Option<usize>,
  table_rendered_table: bool,
  table_current_row_cells: usize,
  table_row_emitted_cells: usize,
  table_width: usize,
  table_current_cell_start: usize,
  table_current_cell_colspan: usize,
  table_current_cell_rowspan: usize,
  table_cell_suppressed: bool,
  table_rowspans: [u16; MAX_TABLE_COLUMNS],
  // 0=none, 1=left, 2=center, 3=right
  table_column_alignments: Vec<u8>,
  last_text_node_contains_whitespace: bool,
  last_text_node_depth: usize,
  last_text_node_index: usize,
  text_run_generation: usize,
  last_text_run_generation: usize,
  has_last_text_node: bool,
  last_node_is_inline: bool,
  /// A collapsed trailing space trimmed from the end of an inline element.
  /// It stays deferred until later visible inline content appears so Markdown
  /// delimiters close before the separator and streaming output has no
  /// speculative trailing whitespace.
  pending_inline_whitespace: bool,

  // Streaming
  last_yielded_length: usize,
  /// Whether any non-leading output has already been returned to the caller.
  /// Once streaming starts, whitespace at the front of a drained buffer is
  /// content, not document-leading whitespace.
  has_streamed_output: bool,
  /// Last two bytes flushed out of the front of the buffer by draining. When a
  /// later rewrite trims the retained buffer empty, spacing and newline counts
  /// still need the same two-byte context one-shot conversion sees.
  flushed_tail: [u8; 2],
  /// Rendered column immediately before the retained output buffer when wrapping.
  buffer_start_column: usize,
  /// Rendered column at the end of `buffer` when wrapping.
  output_column: usize,
  /// Test-only: disables draining to prove it never alters streamed bytes.
  #[cfg(test)]
  pub(crate) disable_drain: bool,

  /// Hard-wrap width in characters; 0 disables wrapping. Code, tables, and
  /// headings are exempt.
  wrap_width: usize,
  plain_text: bool,
  preserve_leading_whitespace: bool,

  // Clean mode — bitmask for zero-cost when disabled
  clean_flags: u8,
  clean_newline_run: u8,
  /// Set when current TAG_A has a meaningless href and should be rendered as plain text
  skip_current_link: bool,
  /// Buffer position of the `[` character written for TAG_A enter
  link_bracket_pos: usize,
  /// Open inline markers as (kind, output start, content start); lets the exit drop empty pairs.
  open_markers: Vec<(u8, usize, usize)>,
  /// Open code spans and fenced blocks stay buffered until their closing
  /// delimiter can be chosen from the complete literal content.
  code_spans: Vec<CodeSpanState>,
  code_fence: Option<CodeFenceState>,
  /// Open blockquotes stay buffered until all child line boundaries are known.
  blockquotes: Vec<BlockquoteFrame>,
  /// Heading slugs collected during conversion for fragment validation
  heading_slugs: Vec<String>,
  /// Fragment link locations: (bracket_start, link_end)
  /// Fragment slug is derived from buffer at fixup time
  fragment_links: Vec<(usize, usize)>,
  /// Whether we're inside a heading (for slug collection)
  in_heading: bool,
  /// Buffer position at heading start (for extracting heading text)
  heading_buffer_start: usize,
  /// A fragment link that may span the complete current heading.
  self_link_heading: Option<SelfLinkHeadingState>,

  /// Cumulative indent string for list-item continuation content. Grows by
  /// each ancestor `<li>`'s marker width (`"- "` = 2, `"N. "` = digits(N)+2),
  /// so code blocks, paragraphs, and nested blocks inside a list item land
  /// in the content column that CommonMark requires. Pushed on `<li>` enter,
  /// popped on `<li>` close.
  list_indent: String,
  /// Per-`<li>` contribution width stack, parallel to `list_indent`. Used to
  /// truncate the correct number of bytes on close without re-walking ancestors.
  list_indent_widths: Vec<u8>,

  /// `<pre>` fenced-code deferral (issue #97). A bare `<pre>` (no `<code>`
  /// child) becomes a fenced code block, but the opening fence is deferred
  /// until the first non-whitespace child so empty/whitespace-only blocks emit
  /// nothing. `pre_fence_pending`: inside a `<pre>` whose fence is undecided.
  /// `pre_fence_lang`: language resolved from the `<pre>`'s own class.
  /// `pre_own_fence`: the `<pre>` opened its own fence (so a nested `<code>`
  /// must not, and the `<pre>` exit emits the closing fence).
  pre_fence_pending: bool,
  pre_fence_lang: String,
  pre_own_fence: bool,
  streaming_limit: Option<usize>,
  streaming_error: Option<StreamingError>,
  incremental_lexing: bool,
  #[cfg(test)]
  gfm_escape_slow_path_calls: usize,
}

impl ConvertState {
  #[inline]
  fn text_mode(&self) -> TextMode {
    self
      .tokenizer_contexts
      .last()
      .map_or(TextMode::Data, |context| context.text_mode)
  }

  #[inline]
  fn text_mode_for_tag(&self, tag_name: &str, tag_id: Option<u8>) -> TextMode {
    let parent_in_svg = self.in_supported_svg_content();
    let svg_integration_point = parent_in_svg
      && (tag_name.eq_ignore_ascii_case("foreignobject")
        || tag_name.eq_ignore_ascii_case("desc")
        || tag_name.eq_ignore_ascii_case("title"));
    match tag_id {
      Some(TAG_STYLE | TAG_XMP | TAG_IFRAME | TAG_NOFRAMES | TAG_NOEMBED) => TextMode::RawText,
      Some(TAG_TITLE) if svg_integration_point => TextMode::Data,
      Some(TAG_TITLE | TAG_TEXTAREA) => TextMode::RcData,
      Some(TAG_PLAINTEXT) => TextMode::PlainText,
      _ => TextMode::Data,
    }
  }

  #[inline]
  pub(crate) fn in_supported_svg_content(&self) -> bool {
    self
      .tokenizer_contexts
      .last()
      .is_some_and(|context| context.in_supported_svg)
  }

  #[inline]
  pub(crate) fn is_supported_svg_integration_point(&self) -> bool {
    self
      .tokenizer_contexts
      .last()
      .is_some_and(|context| context.svg_integration_point)
  }

  pub(crate) fn push_tokenizer_context(&mut self, tag_name: &str, tag_id: Option<u8>) -> bool {
    let parent_in_svg = self.in_supported_svg_content();
    let svg_integration_point = parent_in_svg
      && (tag_name.eq_ignore_ascii_case("foreignobject")
        || tag_name.eq_ignore_ascii_case("desc")
        || tag_name.eq_ignore_ascii_case("title"));
    let in_supported_svg = tag_id == Some(TAG_SVG) || (parent_in_svg && !svg_integration_point);
    let text_mode = self.text_mode_for_tag(tag_name, tag_id);
    if !self.reserve_tokenizer_context() {
      return false;
    }
    self.tokenizer_contexts.push(TokenizerContext {
      text_mode,
      in_supported_svg,
      svg_integration_point,
    });
    true
  }

  pub(crate) fn pop_tokenizer_context(&mut self) {
    self.tokenizer_contexts.pop();
  }

  /// Check if we're inside a table cell (either `<td>` or `<th>`).
  #[inline]
  pub(crate) fn in_table_cell(&self) -> bool {
    self.depth_map[TAG_TD as usize] > 0 || self.depth_map[TAG_TH as usize] > 0
  }

  pub fn new(options: HTMLToMarkdownOptions, capacity: usize, format: OutputFormat) -> Self {
    Self::new_inner(options, capacity, format, None)
  }

  pub(crate) fn new_bounded(
    options: HTMLToMarkdownOptions,
    format: OutputFormat,
    max_buffered_bytes: usize,
  ) -> Self {
    Self::new_inner(options, 0, format, Some(max_buffered_bytes))
  }

  fn new_inner(
    options: HTMLToMarkdownOptions,
    capacity: usize,
    format: OutputFormat,
    streaming_limit: Option<usize>,
  ) -> Self {
    // Read wrap width before `options` is moved into the struct below.
    let options_wrap_width = options.wrap_width;
    let plain_text = format == OutputFormat::Text;
    let mut s = Self {
      depth_map: [0; MAX_TAG_ID],
      depth: 0,
      has_encoded_html_entity: false,
      last_char_was_whitespace: true,
      text_buffer_contains_whitespace: false,
      text_buffer_contains_non_whitespace: false,
      text_buffer_has_inline_gfm_hazard: false,
      just_closed_tag: false,
      is_first_text_in_element: false,
      in_non_nesting: false,
      script_data_state: SCRIPT_DATA,
      in_pre: false,
      overflow: None,
      hidden_since_depth: None,
      collapse_non_span_depth: 0,
      collapse_span_depth: 0,
      first_block_parent_index: None,
      block_parent_indices: if streaming_limit.is_some() {
        Vec::new()
      } else {
        Vec::with_capacity(16)
      },
      parse_text_buffer: String::new(),
      script_text_buffer: String::new(),
      stack: if streaming_limit.is_some() {
        Vec::new()
      } else {
        Vec::with_capacity(32)
      },
      tokenizer_contexts: if streaming_limit.is_some() {
        Vec::new()
      } else {
        Vec::with_capacity(32)
      },
      node_pool: if streaming_limit.is_some() {
        Vec::new()
      } else {
        Vec::with_capacity(32)
      },

      has_plugins: false,
      has_tailwind: false,
      has_isolate_main: false,
      has_frontmatter: false,
      has_filter: false,
      has_extraction: false,
      has_tag_overrides: false,

      isolate_main_found: false,
      isolate_main_closed: false,
      isolate_first_header_depth: None,
      isolate_fallback_output_start: None,
      isolate_after_footer: false,

      frontmatter_in_head: false,
      frontmatter_title: None,
      frontmatter_meta: Vec::new(),

      extraction_parsed_selectors: Vec::new(),
      extraction_tracked: Vec::new(),
      extraction_results: Vec::new(),

      filter_include_parsed: Vec::new(),
      filter_exclude_parsed: Vec::new(),
      filter_process_children: true,

      options,
      buffer: if streaming_limit.is_some() {
        String::new()
      } else {
        String::with_capacity(capacity.max(1024))
      },
      last_content_start: None,
      table_rendered_table: false,
      table_current_row_cells: 0,
      table_row_emitted_cells: 0,
      table_width: 0,
      table_current_cell_start: 0,
      table_current_cell_colspan: 1,
      table_current_cell_rowspan: 1,
      table_cell_suppressed: false,
      table_rowspans: [0; MAX_TABLE_COLUMNS],
      table_column_alignments: Vec::new(),
      last_text_node_contains_whitespace: false,
      last_text_node_depth: 0,
      last_text_node_index: 0,
      text_run_generation: 0,
      last_text_run_generation: 0,
      has_last_text_node: false,
      last_node_is_inline: false,
      pending_inline_whitespace: false,
      last_yielded_length: 0,
      has_streamed_output: false,
      flushed_tail: [0; 2],
      buffer_start_column: 0,
      output_column: 0,
      #[cfg(test)]
      disable_drain: false,

      wrap_width: options_wrap_width,
      plain_text,
      preserve_leading_whitespace: false,
      clean_flags: 0,
      clean_newline_run: 0,
      skip_current_link: false,
      link_bracket_pos: 0,
      open_markers: Vec::new(),
      code_spans: Vec::new(),
      code_fence: None,
      blockquotes: if streaming_limit.is_some() {
        Vec::new()
      } else {
        Vec::with_capacity(4)
      },
      heading_slugs: Vec::new(),
      fragment_links: Vec::new(),
      in_heading: false,
      heading_buffer_start: 0,
      self_link_heading: None,

      list_indent: String::new(),
      list_indent_widths: if streaming_limit.is_some() {
        Vec::new()
      } else {
        Vec::with_capacity(8)
      },

      pre_fence_pending: false,
      pre_fence_lang: String::new(),
      pre_own_fence: false,
      streaming_limit,
      streaming_error: None,
      incremental_lexing: false,
      #[cfg(test)]
      gfm_escape_slow_path_calls: 0,
    };
    // Resolve clean config into bitmask
    let effective_clean_urls;
    if let Some(ref clean) = s.options.clean {
      effective_clean_urls = clean.urls;
      let mut flags = 0u8;
      if clean.empty_links {
        flags |= CLEAN_EMPTY_LINKS;
      }
      if clean.fragments {
        flags |= CLEAN_FRAGMENTS;
      }
      if clean.redundant_links {
        flags |= CLEAN_REDUNDANT_LINKS;
      }
      if clean.self_link_headings {
        flags |= CLEAN_SELF_LINK_HEADINGS;
      }
      if clean.empty_images {
        flags |= CLEAN_EMPTY_IMAGES;
      }
      if clean.empty_link_text {
        flags |= CLEAN_EMPTY_LINK_TEXT;
      }
      if clean.blank_lines {
        flags |= CLEAN_BLANK_LINES;
      }
      s.clean_flags = flags;
    } else {
      effective_clean_urls = s.options.clean_urls;
    }
    s.options.clean_urls = effective_clean_urls;

    if let Some(plugins) = &s.options.plugins {
      s.has_plugins = true;
      s.has_tailwind = plugins.tailwind.is_some();
      s.has_isolate_main = plugins.isolate_main.is_some();
      s.has_frontmatter = plugins.frontmatter.is_some();
      s.has_tag_overrides = plugins.tag_overrides.is_some();
      if let Some(extraction) = &plugins.extraction {
        s.has_extraction = true;
        s.extraction_parsed_selectors = extraction
          .selectors
          .iter()
          .filter(|selector| selector.len() <= MAX_EXTRACTION_RESULT_BYTES)
          .take(MAX_EXTRACTION_SELECTORS)
          .map(|sel| (sel.clone(), parse_css_selector(sel)))
          .collect();
      }
      if let Some(filter) = &plugins.filter {
        s.has_filter = true;
        if let Some(incl) = &filter.include {
          s.filter_include_parsed = incl
            .iter()
            .map(|sel| (sel.clone(), parse_css_selector(sel)))
            .collect();
        }
        if let Some(excl) = &filter.exclude {
          s.filter_exclude_parsed = excl
            .iter()
            .map(|sel| (sel.clone(), parse_css_selector(sel)))
            .collect();
        }
        s.filter_process_children = filter.process_children.unwrap_or(true);
      }
    }
    s
  }

  pub(crate) fn streaming_error(&self) -> Option<StreamingError> {
    self.streaming_error
  }

  pub(crate) fn enable_incremental_lexing(&mut self) {
    self.incremental_lexing = true;
  }

  fn flush_stable_text_prefix(&mut self, text_buffer: &mut String) -> bool {
    if !self.incremental_lexing || text_buffer.len() < TEXT_LOOKBEHIND {
      return false;
    }
    self.process_text_buffer(text_buffer);
    text_buffer.clear();
    true
  }

  pub(crate) fn retained_buffered_bytes(&self) -> usize {
    self.buffer.len()
      + self.parse_text_buffer.len()
      + self.script_text_buffer.len()
      + self.list_indent.len()
      + self.pre_fence_lang.len()
      + self.overflow.as_ref().map_or(0, |overflow| {
        overflow.root_name.len()
          + overflow.suppressed_name.as_ref().map_or(0, String::len)
          + overflow.raw_name.as_ref().map_or(0, String::len)
      })
      + self
        .code_fence
        .as_ref()
        .map_or(0, |fence| fence.indent.len() + fence.language.len())
      + self
        .blockquotes
        .iter()
        .map(|frame| frame.list_indent.len())
        .sum::<usize>()
      + self.open_markers.len() * std::mem::size_of::<(u8, usize, usize)>()
      + self.code_spans.len() * std::mem::size_of::<CodeSpanState>()
      + self.blockquotes.len() * std::mem::size_of::<BlockquoteFrame>()
      + self.list_indent_widths.len() * std::mem::size_of::<u8>()
      + self.heading_slugs.len() * std::mem::size_of::<String>()
      + self.heading_slugs.iter().map(String::len).sum::<usize>()
      + self.fragment_links.len() * std::mem::size_of::<(usize, usize)>()
      + self
        .self_link_heading
        .as_ref()
        .map_or(0, |link| link.fragment.len())
      + self.stack.len() * std::mem::size_of::<ElementNode>()
      + self
        .stack
        .iter()
        .map(|node| element_dynamic_bytes(node, false))
        .sum::<usize>()
      + self.tokenizer_contexts.len() * std::mem::size_of::<TokenizerContext>()
      + self.block_parent_indices.len() * std::mem::size_of::<usize>()
      + self.node_pool.len() * std::mem::size_of::<ElementNode>()
      + self
        .node_pool
        .iter()
        .map(|node| element_dynamic_bytes(node, false))
        .sum::<usize>()
      + self.table_column_alignments.len() * std::mem::size_of::<u8>()
      + self.frontmatter_title.as_ref().map_or(0, String::len)
      + string_pair_bytes(
        &self.frontmatter_meta,
        self.frontmatter_meta.capacity(),
        false,
      )
      + self
        .extraction_tracked
        .iter()
        .map(|tracked| tracked_extraction_bytes(tracked, false))
        .sum::<usize>()
      + self
        .extraction_results
        .iter()
        .map(|result| extracted_element_bytes(result, false))
        .sum::<usize>()
  }

  pub(crate) fn retained_buffer_capacity(&self) -> usize {
    self.buffer.capacity()
      + self.parse_text_buffer.capacity()
      + self.script_text_buffer.capacity()
      + self.list_indent.capacity()
      + self.pre_fence_lang.capacity()
      + self.overflow.as_ref().map_or(0, |overflow| {
        overflow.root_name.capacity()
          + overflow
            .suppressed_name
            .as_ref()
            .map_or(0, String::capacity)
          + overflow.raw_name.as_ref().map_or(0, String::capacity)
      })
      + self.code_fence.as_ref().map_or(0, |fence| {
        fence.indent.capacity() + fence.language.capacity()
      })
      + self
        .blockquotes
        .iter()
        .map(|frame| frame.list_indent.capacity())
        .sum::<usize>()
      + vector_capacity_bytes(&self.open_markers)
      + vector_capacity_bytes(&self.code_spans)
      + vector_capacity_bytes(&self.blockquotes)
      + vector_capacity_bytes(&self.list_indent_widths)
      + vector_capacity_bytes(&self.heading_slugs)
      + self
        .heading_slugs
        .iter()
        .map(String::capacity)
        .sum::<usize>()
      + vector_capacity_bytes(&self.fragment_links)
      + self
        .self_link_heading
        .as_ref()
        .map_or(0, |link| link.fragment.capacity())
      + vector_capacity_bytes(&self.stack)
      + self
        .stack
        .iter()
        .map(|node| element_dynamic_bytes(node, true))
        .sum::<usize>()
      + vector_capacity_bytes(&self.tokenizer_contexts)
      + vector_capacity_bytes(&self.block_parent_indices)
      + vector_capacity_bytes(&self.node_pool)
      + self
        .node_pool
        .iter()
        .map(|node| element_dynamic_bytes(node, true))
        .sum::<usize>()
      + vector_capacity_bytes(&self.table_column_alignments)
      + self.frontmatter_title.as_ref().map_or(0, String::capacity)
      + string_pair_bytes(
        &self.frontmatter_meta,
        self.frontmatter_meta.capacity(),
        true,
      )
      + vector_capacity_bytes(&self.extraction_tracked)
      + self
        .extraction_tracked
        .iter()
        .map(|tracked| tracked_extraction_bytes(tracked, true))
        .sum::<usize>()
      + vector_capacity_bytes(&self.extraction_results)
      + self
        .extraction_results
        .iter()
        .map(|result| extracted_element_bytes(result, true))
        .sum::<usize>()
  }

  pub(crate) fn extraction_retained_capacity(&self) -> usize {
    vector_capacity_bytes(&self.extraction_tracked)
      + self
        .extraction_tracked
        .iter()
        .map(|tracked| tracked_extraction_bytes(tracked, true))
        .sum::<usize>()
      + vector_capacity_bytes(&self.extraction_results)
      + self
        .extraction_results
        .iter()
        .map(|result| extracted_element_bytes(result, true))
        .sum::<usize>()
  }

  pub(crate) fn push_tracked_extraction(&mut self, tracked: TrackedExtraction) {
    if self.extraction_tracked.len() >= MAX_ACTIVE_EXTRACTIONS
      || self.extraction_tracked.len() + self.extraction_results.len() >= MAX_EXTRACTION_RESULTS
    {
      return;
    }
    if tracked_extraction_bytes(&tracked, true) > MAX_EXTRACTION_RESULT_BYTES {
      return;
    }
    let retained = self
      .extraction_retained_capacity()
      .saturating_add(tracked_extraction_bytes(&tracked, true));
    let required = self.extraction_tracked.len() + 1;
    if reserve_vec_accounted(
      &mut self.extraction_tracked,
      required,
      retained,
      MAX_EXTRACTION_BYTES,
    )
    .is_ok()
    {
      self.extraction_tracked.push(tracked);
    }
  }

  pub(crate) fn push_extraction_result(&mut self, result: ExtractedElement) {
    if self.extraction_results.len() >= MAX_EXTRACTION_RESULTS {
      return;
    }
    let retained = self
      .extraction_retained_capacity()
      .saturating_add(extracted_element_bytes(&result, true));
    let required = self.extraction_results.len() + 1;
    if reserve_vec_accounted(
      &mut self.extraction_results,
      required,
      retained,
      MAX_EXTRACTION_BYTES,
    )
    .is_ok()
    {
      self.extraction_results.push(result);
    }
  }

  pub(crate) fn frontmatter_retained_capacity(&self) -> usize {
    self.frontmatter_title.as_ref().map_or(0, String::capacity)
      + string_pair_bytes(
        &self.frontmatter_meta,
        self.frontmatter_meta.capacity(),
        true,
      )
  }

  pub(crate) fn capture_frontmatter_title(&mut self, value: &str) {
    let value = value.trim();
    if value.is_empty() || value.len() > MAX_FRONTMATTER_VALUE_BYTES {
      return;
    }
    let retained = self
      .frontmatter_retained_capacity()
      .saturating_sub(self.frontmatter_title.as_ref().map_or(0, String::capacity));
    let mut copy = String::new();
    if reserve_accounted(&mut copy, value.len(), retained, MAX_FRONTMATTER_BYTES).is_ok() {
      copy.push_str(value);
      self.frontmatter_title = Some(copy);
    }
  }

  pub(crate) fn capture_frontmatter_meta(&mut self, key: &str, value: &str) {
    if key.len() > MAX_FRONTMATTER_VALUE_BYTES || value.len() > MAX_FRONTMATTER_VALUE_BYTES {
      return;
    }
    if let Some(index) = self
      .frontmatter_meta
      .iter()
      .position(|(existing, _)| existing == key)
    {
      let retained = self
        .frontmatter_retained_capacity()
        .saturating_sub(self.frontmatter_meta[index].1.capacity());
      let mut copy = String::new();
      if reserve_accounted(&mut copy, value.len(), retained, MAX_FRONTMATTER_BYTES).is_ok() {
        copy.push_str(value);
        self.frontmatter_meta[index].1 = copy;
      }
      return;
    }
    if self.frontmatter_meta.len() >= MAX_FRONTMATTER_FIELDS.saturating_sub(1) {
      return;
    }

    let retained = self.frontmatter_retained_capacity();
    let mut key_copy = String::new();
    if reserve_accounted(&mut key_copy, key.len(), retained, MAX_FRONTMATTER_BYTES).is_err() {
      return;
    }
    key_copy.push_str(key);
    let retained = self
      .frontmatter_retained_capacity()
      .saturating_add(key_copy.capacity());
    let mut value_copy = String::new();
    if reserve_accounted(
      &mut value_copy,
      value.len(),
      retained,
      MAX_FRONTMATTER_BYTES,
    )
    .is_err()
    {
      return;
    }
    value_copy.push_str(value);
    let retained = self
      .frontmatter_retained_capacity()
      .saturating_add(key_copy.capacity())
      .saturating_add(value_copy.capacity());
    let required = self.frontmatter_meta.len() + 1;
    if reserve_vec_accounted(
      &mut self.frontmatter_meta,
      required,
      retained,
      MAX_FRONTMATTER_BYTES,
    )
    .is_ok()
    {
      self.frontmatter_meta.push((key_copy, value_copy));
    }
  }

  fn fail_streaming(&mut self, error: StreamingError) {
    if self.streaming_error.is_none() {
      self.streaming_error = Some(error);
    }
  }

  pub(crate) fn release_retained_buffers(&mut self) {
    self.buffer = String::new();
    self.parse_text_buffer = String::new();
    self.script_text_buffer = String::new();
    self.list_indent = String::new();
    self.pre_fence_lang = String::new();
    self.overflow = None;
    self.code_fence = None;
    self.open_markers = Vec::new();
    self.code_spans = Vec::new();
    self.blockquotes = Vec::new();
    self.list_indent_widths = Vec::new();
    self.heading_slugs = Vec::new();
    self.fragment_links = Vec::new();
    self.self_link_heading = None;
    self.stack = Vec::new();
    self.tokenizer_contexts = Vec::new();
    self.block_parent_indices = Vec::new();
    self.node_pool = Vec::new();
    self.table_column_alignments = Vec::new();
    self.frontmatter_title = None;
    self.frontmatter_meta = Vec::new();
    self.extraction_tracked = Vec::new();
    self.extraction_results = Vec::new();
    self.buffer_start_column = 0;
    self.output_column = 0;
  }

  fn reserve_tokenizer_context(&mut self) -> bool {
    let Some(limit) = self.streaming_limit else {
      return true;
    };
    let retained = self.retained_buffer_capacity();
    let Some(required) = self.tokenizer_contexts.len().checked_add(1) else {
      self.fail_streaming(StreamingError::CapacityOverflow);
      return false;
    };
    if let Err(error) =
      reserve_vec_accounted(&mut self.tokenizer_contexts, required, retained, limit)
    {
      self.fail_streaming(error);
      return false;
    }
    true
  }

  pub(crate) fn reserve_stack_node(&mut self, node: &ElementNode) -> bool {
    let Some(limit) = self.streaming_limit else {
      return true;
    };
    let retained = self.retained_buffer_capacity();
    let Some(required) = self.stack.len().checked_add(1) else {
      self.fail_streaming(StreamingError::CapacityOverflow);
      return false;
    };
    if let Err(error) = reserve_vec_accounted(&mut self.stack, required, retained, limit) {
      self.fail_streaming(error);
      return false;
    }
    let Some(total) = self
      .retained_buffer_capacity()
      .checked_add(element_dynamic_bytes(node, true))
    else {
      self.fail_streaming(StreamingError::CapacityOverflow);
      return false;
    };
    if total > limit {
      self.fail_streaming(StreamingError::BufferLimitExceeded);
      return false;
    }
    true
  }

  pub(crate) fn reserve_block_parent(&mut self) -> bool {
    let Some(limit) = self.streaming_limit else {
      return true;
    };
    let retained = self.retained_buffer_capacity();
    let Some(required) = self.block_parent_indices.len().checked_add(1) else {
      self.fail_streaming(StreamingError::CapacityOverflow);
      return false;
    };
    if let Err(error) =
      reserve_vec_accounted(&mut self.block_parent_indices, required, retained, limit)
    {
      self.fail_streaming(error);
      return false;
    }
    true
  }

  pub(crate) fn push_table_alignment(&mut self, alignment: u8) -> bool {
    if self.table_column_alignments.len() >= MAX_TABLE_COLUMNS {
      if self.streaming_limit.is_some() {
        self.fail_streaming(StreamingError::ParserLimitExceeded);
      }
      return false;
    }
    if let Some(limit) = self.streaming_limit {
      let retained = self.retained_buffer_capacity();
      let required = self.table_column_alignments.len() + 1;
      if let Err(error) =
        reserve_vec_accounted(&mut self.table_column_alignments, required, retained, limit)
      {
        self.fail_streaming(error);
        return false;
      }
    }
    self.table_column_alignments.push(alignment);
    true
  }

  fn reserve_retained(
    value: &mut String,
    required_len: usize,
    retained_capacity: usize,
    limit: usize,
  ) -> Result<(), StreamingError> {
    reserve_accounted(value, required_len, retained_capacity, limit)
  }

  pub(crate) fn reserve_output_to(&mut self, required_len: usize) -> bool {
    let Some(limit) = self.streaming_limit else {
      return true;
    };
    if self.streaming_error.is_some() {
      return false;
    }
    let retained = self.retained_buffer_capacity();
    if let Err(error) = Self::reserve_retained(&mut self.buffer, required_len, retained, limit) {
      self.fail_streaming(error);
      return false;
    }
    true
  }

  pub(crate) fn reserve_external(
    &self,
    value: &mut String,
    required_len: usize,
  ) -> Result<(), StreamingError> {
    let Some(limit) = self.streaming_limit else {
      return Ok(());
    };
    let retained = self
      .retained_buffer_capacity()
      .checked_add(value.capacity())
      .ok_or(StreamingError::CapacityOverflow)?;
    reserve_accounted(value, required_len, retained, limit)
  }

  pub(crate) fn reserve_output(&mut self, additional: usize) -> bool {
    let Some(required_len) = self.buffer.len().checked_add(additional) else {
      self.fail_streaming(StreamingError::CapacityOverflow);
      return false;
    };
    self.reserve_output_to(required_len)
  }

  pub(crate) fn push_output_str(&mut self, value: &str) -> bool {
    if !self.reserve_output(value.len()) {
      return false;
    }
    self.buffer.push_str(value);
    if self.wrap_width != 0 {
      if let Some(last_newline) = value.rfind('\n') {
        self.output_column = value[last_newline + 1..].chars().count();
      } else {
        self.output_column = self.output_column.saturating_add(value.chars().count());
      }
    }
    true
  }

  pub(crate) fn push_output_char(&mut self, value: char) -> bool {
    if !self.reserve_output(value.len_utf8()) {
      return false;
    }
    self.buffer.push(value);
    if self.wrap_width != 0 {
      if value == '\n' {
        self.output_column = 0;
      } else {
        self.output_column = self.output_column.saturating_add(1);
      }
    }
    true
  }

  fn column_at(&self, offset: usize) -> usize {
    let prefix = &self.buffer[..offset];
    prefix.rfind('\n').map_or_else(
      || {
        self
          .buffer_start_column
          .saturating_add(prefix.chars().count())
      },
      |newline| prefix[newline + 1..].chars().count(),
    )
  }

  fn advance_buffer_start_column(&mut self, drain_end: usize) {
    if self.wrap_width != 0 {
      self.buffer_start_column = self.column_at(drain_end);
    }
  }

  pub(crate) fn reserve_list_indent(&mut self, additional: usize) -> bool {
    let Some(limit) = self.streaming_limit else {
      return true;
    };
    let Some(required_len) = self.list_indent.len().checked_add(additional) else {
      self.fail_streaming(StreamingError::CapacityOverflow);
      return false;
    };
    let retained = self.retained_buffer_capacity();
    if let Err(error) = reserve_accounted(&mut self.list_indent, required_len, retained, limit) {
      self.fail_streaming(error);
      return false;
    }
    true
  }

  pub(crate) fn reserve_open_marker(&mut self) -> bool {
    let Some(limit) = self.streaming_limit else {
      return true;
    };
    let retained = self.retained_buffer_capacity();
    let Some(required) = self.open_markers.len().checked_add(1) else {
      self.fail_streaming(StreamingError::CapacityOverflow);
      return false;
    };
    if let Err(error) = reserve_vec_accounted(&mut self.open_markers, required, retained, limit) {
      self.fail_streaming(error);
      return false;
    }
    true
  }

  pub(crate) fn reserve_code_span(&mut self) -> bool {
    let Some(limit) = self.streaming_limit else {
      return true;
    };
    let retained = self.retained_buffer_capacity();
    let Some(required) = self.code_spans.len().checked_add(1) else {
      self.fail_streaming(StreamingError::CapacityOverflow);
      return false;
    };
    if let Err(error) = reserve_vec_accounted(&mut self.code_spans, required, retained, limit) {
      self.fail_streaming(error);
      return false;
    }
    true
  }

  pub(crate) fn reserve_blockquote(&mut self) -> bool {
    let Some(limit) = self.streaming_limit else {
      return true;
    };
    let retained = self.retained_buffer_capacity();
    let Some(required) = self.blockquotes.len().checked_add(1) else {
      self.fail_streaming(StreamingError::CapacityOverflow);
      return false;
    };
    if let Err(error) = reserve_vec_accounted(&mut self.blockquotes, required, retained, limit) {
      self.fail_streaming(error);
      return false;
    }
    true
  }

  pub(crate) fn reserve_list_indent_width(&mut self) -> bool {
    let Some(limit) = self.streaming_limit else {
      return true;
    };
    let retained = self.retained_buffer_capacity();
    let Some(required) = self.list_indent_widths.len().checked_add(1) else {
      self.fail_streaming(StreamingError::CapacityOverflow);
      return false;
    };
    if let Err(error) =
      reserve_vec_accounted(&mut self.list_indent_widths, required, retained, limit)
    {
      self.fail_streaming(error);
      return false;
    }
    true
  }

  fn set_self_link_heading(&mut self, link: SelfLinkHeadingState) -> bool {
    self.self_link_heading = None;
    if let Some(limit) = self.streaming_limit {
      let Some(retained) = self
        .retained_buffer_capacity()
        .checked_add(link.fragment.capacity())
      else {
        self.fail_streaming(StreamingError::CapacityOverflow);
        return false;
      };
      if retained > limit {
        self.fail_streaming(StreamingError::BufferLimitExceeded);
        return false;
      }
    }
    self.self_link_heading = Some(link);
    true
  }

  fn push_heading_slug(&mut self, slug: String) -> bool {
    if let Some(limit) = self.streaming_limit {
      let Some(retained) = self.retained_buffer_capacity().checked_add(slug.capacity()) else {
        self.fail_streaming(StreamingError::CapacityOverflow);
        return false;
      };
      let Some(required) = self.heading_slugs.len().checked_add(1) else {
        self.fail_streaming(StreamingError::CapacityOverflow);
        return false;
      };
      if let Err(error) = reserve_vec_accounted(&mut self.heading_slugs, required, retained, limit)
      {
        self.fail_streaming(error);
        return false;
      }
    }
    self.heading_slugs.push(slug);
    true
  }

  pub(crate) fn set_pre_fence_language(&mut self, language: &str) -> bool {
    if self.streaming_limit.is_none() {
      self.pre_fence_lang.clear();
      self.pre_fence_lang.push_str(language);
      return true;
    }
    self.pre_fence_lang = String::new();
    let Some(value) = self.retained_string_copy(language, 0) else {
      return false;
    };
    self.pre_fence_lang = value;
    true
  }

  pub(crate) fn release_closed_high_water(&mut self) {
    if self.streaming_limit.is_none() {
      return;
    }
    if self.depth_map[TAG_SCRIPT as usize] == 0 {
      self.script_text_buffer = String::new();
    }
    if self.open_markers.is_empty() {
      self.open_markers = Vec::new();
    }
    if self.code_spans.is_empty() {
      self.code_spans = Vec::new();
    }
    if self.blockquotes.is_empty() {
      self.blockquotes = Vec::new();
    }
    if self.list_indent_widths.is_empty() {
      self.list_indent_widths = Vec::new();
    }
    if self.stack.is_empty() {
      self.stack = Vec::new();
      self.tokenizer_contexts = Vec::new();
      self.block_parent_indices = Vec::new();
    }
    if self.depth_map[TAG_TABLE as usize] == 0 {
      self.table_column_alignments = Vec::new();
    }
    if self.depth_map[TAG_A as usize] > 0
      || !self.open_markers.is_empty()
      || !self.code_spans.is_empty()
      || self.code_fence.is_some()
      || !self.blockquotes.is_empty()
      || (self.in_heading && self.clean_flags & CLEAN_SELF_LINK_HEADINGS != 0)
    {
      return;
    }
    if self.last_yielded_length > 2 {
      let mut drain_end = self.last_yielded_length - 2;
      while drain_end > 0 && !self.buffer.is_char_boundary(drain_end) {
        drain_end -= 1;
      }
      if drain_end > 0 {
        let bytes = self.buffer.as_bytes();
        self.flushed_tail = if drain_end >= 2 {
          [bytes[drain_end - 2], bytes[drain_end - 1]]
        } else {
          [self.flushed_tail[1], bytes[0]]
        };
        self.advance_buffer_start_column(drain_end);
        self.buffer.drain(..drain_end);
        self.last_yielded_length -= drain_end;
        self.link_bracket_pos = self.link_bracket_pos.saturating_sub(drain_end);
        self.last_content_start = None;
      }
    }
    let threshold = self.buffer.len().saturating_mul(4).max(256);
    if self.buffer.capacity() <= threshold {
      return;
    }
    let old = std::mem::take(&mut self.buffer);
    let replacement = self.retained_string_copy(&old, 0);
    drop(old);
    if let Some(replacement) = replacement {
      self.buffer = replacement;
    }
  }

  pub(crate) fn replace_output_range(
    &mut self,
    start: usize,
    end: usize,
    replacement: &str,
  ) -> bool {
    let Some(required_len) = self
      .buffer
      .len()
      .checked_sub(end.saturating_sub(start))
      .and_then(|len| len.checked_add(replacement.len()))
    else {
      self.fail_streaming(StreamingError::CapacityOverflow);
      return false;
    };
    if !self.reserve_output_to(required_len) {
      return false;
    }
    if self.wrap_width != 0 {
      let suffix = &self.buffer[end..];
      self.output_column = if suffix.contains('\n') {
        self.output_column
      } else if let Some(last_newline) = replacement.rfind('\n') {
        replacement[last_newline + 1..]
          .chars()
          .count()
          .saturating_add(suffix.chars().count())
      } else {
        let removed = &self.buffer[start..end];
        if removed.contains('\n') {
          self
            .column_at(start)
            .saturating_add(replacement.chars().count())
            .saturating_add(suffix.chars().count())
        } else {
          self
            .output_column
            .saturating_sub(removed.chars().count())
            .saturating_add(replacement.chars().count())
        }
      };
    }
    self.buffer.replace_range(start..end, replacement);
    true
  }

  pub(crate) fn truncate_output(&mut self, new_len: usize) {
    if new_len >= self.buffer.len() {
      return;
    }
    if self.wrap_width != 0 {
      let removed = &self.buffer[new_len..];
      self.output_column = if removed.contains('\n') {
        self.column_at(new_len)
      } else {
        self.output_column.saturating_sub(removed.chars().count())
      };
    }
    self.buffer.truncate(new_len);
  }

  pub(crate) fn retained_string_copy(
    &mut self,
    value: &str,
    additional_retained_capacity: usize,
  ) -> Option<String> {
    if self.streaming_error.is_some() {
      return None;
    }
    let Some(limit) = self.streaming_limit else {
      return Some(value.to_string());
    };
    let Some(retained) = self
      .retained_buffer_capacity()
      .checked_add(additional_retained_capacity)
    else {
      self.fail_streaming(StreamingError::CapacityOverflow);
      return None;
    };
    let mut copy = String::new();
    if let Err(error) = reserve_accounted(&mut copy, value.len(), retained, limit) {
      self.fail_streaming(error);
      return None;
    }
    copy.push_str(value);
    Some(copy)
  }

  fn reserve_parse_scratch(&mut self, value: &mut String, additional: usize) -> bool {
    let Some(limit) = self.streaming_limit else {
      return true;
    };
    let Some(required_len) = value.len().checked_add(additional) else {
      self.fail_streaming(StreamingError::CapacityOverflow);
      return false;
    };
    let Some(retained) = self
      .retained_buffer_capacity()
      .checked_add(value.capacity())
    else {
      self.fail_streaming(StreamingError::CapacityOverflow);
      return false;
    };
    if let Err(error) = reserve_accounted(value, required_len, retained, limit) {
      self.fail_streaming(error);
      return false;
    }
    true
  }

  fn push_parse_str(&mut self, value: &mut String, text: &str) -> bool {
    if !self.reserve_parse_scratch(value, text.len()) {
      return false;
    }
    value.push_str(text);
    true
  }

  fn push_parse_char(&mut self, value: &mut String, character: char) -> bool {
    if !self.reserve_parse_scratch(value, character.len_utf8()) {
      return false;
    }
    value.push(character);
    true
  }

  #[inline]
  fn push_script_text(&mut self, text: &str) {
    if text.is_empty() || !self.has_extraction {
      return;
    }
    if let Some(limit) = self.streaming_limit {
      let Some(required_len) = self.script_text_buffer.len().checked_add(text.len()) else {
        self.fail_streaming(StreamingError::CapacityOverflow);
        return;
      };
      let retained = self.retained_buffer_capacity();
      if let Err(error) =
        reserve_accounted(&mut self.script_text_buffer, required_len, retained, limit)
      {
        self.fail_streaming(error);
        return;
      }
    }
    self.script_text_buffer.push_str(text);
    self.text_buffer_contains_non_whitespace = true;
    self.last_char_was_whitespace = false;
    self.just_closed_tag = false;
  }

  fn flush_script_text(&mut self) {
    if self.script_text_buffer.is_empty() {
      if self.streaming_limit.is_some() {
        self.script_text_buffer = String::new();
      }
      return;
    }
    let mut script_text = std::mem::take(&mut self.script_text_buffer);
    self.process_text_buffer(&mut script_text);
    if self.streaming_limit.is_none() {
      self.script_text_buffer = script_text;
    }
  }

  fn process_script_chunk(&mut self, chunk: &str, start: usize) -> ScriptChunk {
    let scan = find_script_end_tag(chunk.as_bytes(), start, self.script_data_state);
    self.script_data_state = scan.state;
    match scan.boundary {
      ScriptScanBoundary::Close(close_index) => {
        self.push_script_text(&chunk[start..close_index]);
        self.flush_script_text();
        self.script_data_state = SCRIPT_DATA;
        ScriptChunk::Closed(close_index)
      }
      ScriptScanBoundary::Pending(pending_start) => {
        // Consume the script text before the partial `</scr…` and carry only
        // that raw tail. Carrying from the script start instead would re-feed
        // the text just pushed here, which the resume path would push again.
        self.push_script_text(&chunk[start..pending_start]);
        ScriptChunk::Carry(pending_start)
      }
      ScriptScanBoundary::Complete => {
        self.push_script_text(&chunk[start..]);
        ScriptChunk::Carry(chunk.len())
      }
    }
  }

  pub fn process_html(&mut self, chunk: &str) -> String {
    // Reuse text_buffer allocation from previous call if available
    let mut text_buffer = std::mem::take(&mut self.parse_text_buffer);
    text_buffer.clear();
    if self.streaming_limit.is_none() && text_buffer.capacity() == 0 {
      text_buffer.reserve(256);
    }
    let bytes = chunk.as_bytes();
    let chunk_length = bytes.len();
    let mut i = 0;
    // Raw start of the text/tag run currently held in `text_buffer`. Any tail
    // carried to the next chunk is returned raw from here, never the decoded
    // and escaped `text_buffer` (which would be re-escaped, multiplying `\`).
    let mut run_start = 0usize;
    let mut carry = false;

    if self.overflow.is_none()
      && self
        .stack
        .last()
        .is_some_and(|node| node.tag_id == Some(TAG_SCRIPT) && node.custom_name.is_none())
    {
      match self.process_script_chunk(chunk, i) {
        ScriptChunk::Closed(close_index) => i = close_index,
        ScriptChunk::Carry(from) => {
          run_start = from;
          carry = true;
          i = chunk_length;
        }
      }
    }

    while i < chunk_length && self.streaming_error.is_none() {
      if text_buffer.is_empty() {
        run_start = i;
      }

      if self
        .overflow
        .as_ref()
        .is_some_and(|overflow| overflow.raw_is_script)
      {
        let scan = find_script_end_tag(bytes, i, self.script_data_state);
        self.script_data_state = scan.state;
        match scan.boundary {
          ScriptScanBoundary::Close(close_index) => i = close_index,
          ScriptScanBoundary::Pending(pending_start) => {
            run_start = pending_start;
            carry = true;
            break;
          }
          ScriptScanBoundary::Complete => {
            i = chunk_length;
            continue;
          }
        }
      }

      let cc = bytes[i];

      if cc != LT_CHAR
        && self
          .overflow
          .as_ref()
          .is_some_and(|overflow| overflow.suppressed_name.is_some())
      {
        while i < chunk_length && bytes[i] != LT_CHAR {
          i += 1;
        }
        continue;
      }

      if cc != LT_CHAR {
        // FAST PATH: batch contiguous plain ASCII text (>32, <128, not & or <)
        // Skip when: non-nesting mode or pre tag
        if cc > 32
          && cc < 0x80
          && cc != AMPERSAND_CHAR
          && !is_inline_gfm_hazard(cc)
          && !self.in_non_nesting
          && !self.in_pre
        {
          let start = i;
          i += 1;
          while i < chunk_length && (!self.incremental_lexing || i - start < TEXT_LOOKBEHIND) {
            let c = bytes[i];
            if c <= 32
              || c >= 0x80
              || c == LT_CHAR
              || c == AMPERSAND_CHAR
              || is_inline_gfm_hazard(c)
            {
              break;
            }
            i += 1;
          }
          if !self.push_parse_str(&mut text_buffer, &chunk[start..i]) {
            break;
          }
          self.text_buffer_contains_non_whitespace = true;
          self.last_char_was_whitespace = false;
          self.just_closed_tag = false;
          if self.flush_stable_text_prefix(&mut text_buffer) {
            run_start = i;
          }
          continue;
        }

        // Script/style rawtext is excluded from output. Scan directly to the
        // next potential tag instead of routing every byte through the general
        // text path. Quotes are ordinary rawtext bytes; HTML closes these
        // elements at the first matching end tag (issue #132).
        if self.in_non_nesting
          && (self.depth_map[TAG_SCRIPT as usize] > 0 || self.depth_map[TAG_STYLE as usize] > 0)
        {
          let start = i;
          while i < chunk_length
            && bytes[i] != LT_CHAR
            && (!self.incremental_lexing || i - start < TEXT_LOOKBEHIND)
          {
            i += 1;
          }
          if !self.push_parse_str(&mut text_buffer, &chunk[start..i]) {
            break;
          }
          self.text_buffer_contains_non_whitespace = true;
          self.last_char_was_whitespace = false;
          self.just_closed_tag = false;
          if self.flush_stable_text_prefix(&mut text_buffer) {
            run_start = i;
          }
          continue;
        }

        if cc == AMPERSAND_CHAR {
          let text_mode = self
            .overflow
            .as_ref()
            .and_then(|overflow| overflow.raw_name.as_ref().map(|_| overflow.raw_mode))
            .unwrap_or_else(|| self.text_mode());
          if text_mode != TextMode::RawText && text_mode != TextMode::PlainText {
            if Self::incomplete_entity_at_end(&bytes[i..]) {
              if !text_buffer.is_empty() {
                self.process_text_buffer(&mut text_buffer);
                text_buffer.clear();
              }
              run_start = i;
              carry = true;
              break;
            }
            self.has_encoded_html_entity = true;
          }
        }
        if cc > 32 && cc < 0x80 && is_inline_gfm_hazard(cc) {
          self.text_buffer_has_inline_gfm_hazard = true;
        }

        if is_whitespace(cc) {
          if self.just_closed_tag {
            self.just_closed_tag = false;
            self.last_char_was_whitespace = false;
          }
          if !self.in_pre && self.last_char_was_whitespace {
            i += 1;
            continue;
          }
          if self.in_pre {
            if !self.push_parse_char(&mut text_buffer, cc as char) {
              break;
            }
          } else if (cc == SPACE_CHAR || !self.last_char_was_whitespace)
            && !self.push_parse_char(&mut text_buffer, ' ')
          {
            break;
          }
          self.last_char_was_whitespace = true;
          self.text_buffer_contains_whitespace = true;
        } else {
          self.text_buffer_contains_non_whitespace = true;
          self.last_char_was_whitespace = false;
          self.just_closed_tag = false;

          // Structural GFM escaping (|, [, ], > in table/link/blockquote
          // context) is applied at output time in escape_gfm_text, which also
          // covers characters produced by decoded entities that never pass
          // through this parse loop.
          if cc < 0x80 {
            if !self.push_parse_char(&mut text_buffer, cc as char) {
              break;
            }
          } else if let Some(ch) = chunk[i..].chars().next() {
            if !self.push_parse_char(&mut text_buffer, ch) {
              break;
            }
            i += ch.len_utf8();
            if self.flush_stable_text_prefix(&mut text_buffer) {
              run_start = i;
            }
            continue;
          }
        }
        i += 1;
        if self.flush_stable_text_prefix(&mut text_buffer) {
          run_start = i;
        }
        continue;
      }

      // Processing '<'
      if i + 1 >= chunk_length {
        carry = true;
        break;
      }

      // Non-nesting guard: inside script/style/title/textarea, only the
      // matching closing tag exits. All other '<' patterns (comments,
      // non-matching closing tags, opening tags) are treated as literal text.
      if self.in_non_nesting
        || self
          .overflow
          .as_ref()
          .is_some_and(|overflow| overflow.raw_name.is_some())
      {
        let (text_mode, raw_name, suppressed) = if let Some(overflow) = &self.overflow
          && let Some(raw_name) = &overflow.raw_name
        {
          (
            overflow.raw_mode,
            Some(raw_name.as_str()),
            overflow.suppressed_name.is_some(),
          )
        } else {
          (self.text_mode(), None, false)
        };
        let next = bytes[i + 1];
        if text_mode != TextMode::PlainText && next == SLASH_CHAR {
          let peek_start = i + 2;
          let mut peek_end = peek_start;
          while peek_end < chunk_length {
            let c = bytes[peek_end];
            if c == GT_CHAR || c == SLASH_CHAR || is_whitespace(c) {
              break;
            }
            peek_end += 1;
          }
          let peek_name = &chunk[peek_start..peek_end];
          if peek_end == chunk_length
            && raw_name.is_some_and(|name| name.starts_with(&peek_name.to_ascii_lowercase()))
          {
            carry = true;
            break;
          }
          let raw_matches = raw_name.is_some_and(|name| name.eq_ignore_ascii_case(peek_name));
          let stack_matches = raw_name.is_none() && {
            let peek_tag_id = crate::consts::get_tag_id_ci_bytes(peek_name.as_bytes());
            self.stack.last().is_some_and(|curr| {
              curr.custom_name.as_deref().map_or_else(
                || curr.tag_id == peek_tag_id,
                |name| name.eq_ignore_ascii_case(peek_name),
              )
            })
          };
          if raw_matches || stack_matches {
            // Matching closing tag: fall through to normal closing tag processing
            if !text_buffer.is_empty() {
              self.process_text_buffer(&mut text_buffer);
              text_buffer.clear();
              run_start = i;
            }
            let result = if self.overflow.is_some() {
              self.process_overflow_closing_tag(chunk, i)
            } else {
              self.process_closing_tag(chunk, i)
            };
            if result.complete {
              i = result.new_position;
            } else {
              carry = true;
              break;
            }
            continue;
          }
        }
        // Not a matching closing tag: treat '<' as literal text
        if suppressed {
          i += 1;
          continue;
        }
        self.text_buffer_has_inline_gfm_hazard = true;
        if !self.push_parse_char(&mut text_buffer, '<') {
          break;
        }
        self.text_buffer_contains_non_whitespace = true;
        self.last_char_was_whitespace = false;
        self.just_closed_tag = false;
        i += 1;
        continue;
      }

      let next = bytes[i + 1];

      if next == EXCLAMATION_CHAR {
        let remaining = &chunk[i..];
        let has_cdata_override = self
          .options
          .plugins
          .as_ref()
          .and_then(|plugins| plugins.tag_overrides.as_ref())
          .is_some_and(|overrides| overrides.iter().any(|(name, _)| name == "#cdata-section"));
        let recognizes_cdata = has_cdata_override || self.in_supported_svg_content();
        // CDATA is dropped by default but can be surfaced via
        // tagOverrides["#cdata-section"]. Handle it before the generic
        // comment/doctype scan, which would otherwise stop at the first
        // `>` inside `]]>` and discard the content. We already matched
        // `<!`, so only the `[CDATA[` tail is checked; `strip_prefix`
        // short-circuits on the third byte for the common comment and
        // doctype cases.
        if recognizes_cdata && let Some(after_open) = chunk[i + 2..].strip_prefix("[CDATA[") {
          if let Some(rel) = after_open.find("]]>") {
            if !text_buffer.is_empty() {
              self.process_text_buffer(&mut text_buffer);
              text_buffer.clear();
              run_start = i;
            }
            let content = &after_open[..rel];
            if has_cdata_override {
              self.process_cdata_section(content);
            } else if self.in_supported_svg_content() && !content.is_empty() {
              for &byte in content.as_bytes() {
                if is_whitespace(byte) {
                  self.text_buffer_contains_whitespace = true;
                } else {
                  self.text_buffer_contains_non_whitespace = true;
                }
                if byte < 0x80 && is_inline_gfm_hazard(byte) {
                  self.text_buffer_has_inline_gfm_hazard = true;
                }
              }
              if !self.push_parse_str(&mut text_buffer, content) {
                break;
              }
              self.process_text_buffer(&mut text_buffer);
              text_buffer.clear();
              if let Some(&last) = content.as_bytes().last() {
                self.last_char_was_whitespace = is_whitespace(last);
                self.just_closed_tag = false;
              }
            }
            i += "<![CDATA[".len() + rel + 3;
            continue;
          }
          // Unterminated CDATA: re-parse from '<' in the next chunk.
          carry = true;
          break;
        }
        if recognizes_cdata
          && remaining.len() < "<![CDATA[".len()
          && "<![CDATA[".starts_with(remaining)
        {
          // Chunk boundary fell inside the `<![CDATA[` opener.
          carry = true;
          break;
        }
        if !text_buffer.is_empty() {
          self.process_text_buffer(&mut text_buffer);
          text_buffer.clear();
          run_start = i;
        }
        let result = process_comment_or_doctype(chunk, i);
        if result.complete {
          i = result.new_position;
        } else {
          carry = true;
          break;
        }
      } else if next == b'?' {
        if !text_buffer.is_empty() {
          self.process_text_buffer(&mut text_buffer);
          text_buffer.clear();
          run_start = i;
        }
        let result = process_bogus_comment(chunk, i);
        if result.complete {
          i = result.new_position;
          run_start = i;
        } else {
          carry = true;
          break;
        }
      } else if next == SLASH_CHAR {
        if i + 2 >= chunk_length {
          if !text_buffer.is_empty() {
            self.process_text_buffer(&mut text_buffer);
            text_buffer.clear();
            run_start = i;
          }
          carry = true;
          break;
        }
        let end_tag_start = bytes[i + 2];
        if end_tag_start == GT_CHAR {
          if !text_buffer.is_empty() {
            self.process_text_buffer(&mut text_buffer);
            text_buffer.clear();
          }
          i += 3;
          run_start = i;
          continue;
        }
        if !end_tag_start.is_ascii_alphabetic() {
          if !text_buffer.is_empty() {
            self.process_text_buffer(&mut text_buffer);
            text_buffer.clear();
            run_start = i;
          }
          let result = process_bogus_comment(chunk, i);
          if result.complete {
            i = result.new_position;
            run_start = i;
          } else {
            carry = true;
            break;
          }
          continue;
        }
        if !text_buffer.is_empty() {
          self.process_text_buffer(&mut text_buffer);
          text_buffer.clear();
          run_start = i;
        }
        let result = if self.overflow.is_some() {
          self.process_overflow_closing_tag(chunk, i)
        } else {
          self.process_closing_tag(chunk, i)
        };
        if result.complete {
          i = result.new_position;
        } else {
          carry = true;
          break;
        }
      } else if next.is_ascii_alphabetic() {
        let mut i2 = i + 1;
        let tag_name_start = i2;
        let mut tag_name_end = None;
        while i2 < chunk_length {
          let c = bytes[i2];
          if is_whitespace(c) || c == SLASH_CHAR || c == GT_CHAR {
            tag_name_end = Some(i2);
            break;
          }
          i2 += 1;
        }
        let Some(tag_name_end) = tag_name_end else {
          if !text_buffer.is_empty() {
            self.process_text_buffer(&mut text_buffer);
            text_buffer.clear();
            run_start = i;
          }
          carry = true;
          break;
        };
        let tag_name_raw = &chunk[tag_name_start..tag_name_end];

        // CI lookup first: built-in tags (the common case) skip the
        // lowercase allocation entirely. Only fall back to a Cow when
        // the override path actually needs the lowercased name.
        let builtin_tag_id = crate::consts::get_tag_id_ci_bytes(tag_name_raw.as_bytes());
        let tag_name: Cow<str> = if let Some(id) = builtin_tag_id {
          Cow::Borrowed(TAG_NAMES[id as usize])
        } else if tag_name_raw.bytes().any(|b| b.is_ascii_uppercase()) {
          Cow::Owned(tag_name_raw.to_ascii_lowercase())
        } else {
          Cow::Borrowed(tag_name_raw)
        };
        let alias_tag_id = self
          .options
          .plugins
          .as_ref()
          .and_then(|p| p.tag_overrides.as_ref())
          .and_then(|ovs| {
            ovs
              .iter()
              .find(|(k, _)| k == tag_name.as_ref())
              .map(|(_, v)| v)
          })
          .and_then(|ov| ov.alias_tag_id);
        let tag_id = alias_tag_id.or(builtin_tag_id);
        let is_alias = alias_tag_id.is_some_and(|alias| Some(alias) != builtin_tag_id);
        i2 = tag_name_end;

        if tag_name_raw.is_empty() {
          // `<` followed by whitespace or `>` is not a tag: treat as literal text
          if !self.push_parse_char(&mut text_buffer, bytes[i] as char) {
            break;
          }
          self.text_buffer_contains_non_whitespace = true;
          self.text_buffer_has_inline_gfm_hazard = true;
          self.last_char_was_whitespace = false;
          self.just_closed_tag = false;
          i += 1;
          continue;
        }

        if !text_buffer.is_empty() {
          self.process_text_buffer(&mut text_buffer);
          text_buffer.clear();
          run_start = i;
        }

        let result =
          self.process_opening_tag(&tag_name, tag_id, builtin_tag_id, is_alias, chunk, i2);
        if result.skip {
          i = result.new_position;
        } else if result.complete {
          i = result.new_position;
          if result.self_closing {
            self.close_node();
            self.just_closed_tag = true;
          } else {
            self.is_first_text_in_element = true;
            if builtin_tag_id == Some(TAG_SCRIPT) && tag_id == Some(TAG_SCRIPT) {
              match self.process_script_chunk(chunk, i) {
                ScriptChunk::Closed(close_index) => i = close_index,
                ScriptChunk::Carry(from) => {
                  // Carry the raw script tail (from the partial close tag, or
                  // nothing when fully consumed) into the next chunk.
                  run_start = from;
                  carry = true;
                  break;
                }
              }
            }
          }
        } else {
          // Incomplete opening tag: re-parse from '<' in the next chunk.
          carry = true;
          break;
        }
      } else {
        if !self.push_parse_char(&mut text_buffer, '<') {
          break;
        }
        self.text_buffer_contains_non_whitespace = true;
        self.text_buffer_has_inline_gfm_hazard = true;
        self.last_char_was_whitespace = false;
        self.just_closed_tag = false;
        i += 1;
      }
    }

    // Carry the chunk's unfinished tail (incomplete tag/entity, or text a later
    // chunk may extend) RAW from `run_start`, never the decoded+escaped
    // `text_buffer`; re-processing re-derives it so nothing is double-applied.
    // Otherwise reuse the allocation (the common non-streaming case).
    if !carry && self.incremental_lexing && !text_buffer.is_empty() {
      carry = true;
    }

    if carry {
      let leftover = chunk[run_start..].to_string();
      text_buffer.clear();
      self.parse_text_buffer = if self.streaming_limit.is_some() && text_buffer.is_empty() {
        String::new()
      } else {
        text_buffer
      };
      if leftover
        .as_bytes()
        .first()
        .is_some_and(|&c| is_whitespace(c))
      {
        self.last_char_was_whitespace = false;
      }
      leftover
    } else {
      if !text_buffer.is_empty() {
        self.process_text_buffer(&mut text_buffer);
        text_buffer.clear();
      }
      self.parse_text_buffer = if self.streaming_limit.is_some() && text_buffer.is_empty() {
        String::new()
      } else {
        text_buffer
      };
      String::new()
    }
  }

  fn incomplete_entity_at_end(bytes: &[u8]) -> bool {
    debug_assert_eq!(bytes.first(), Some(&AMPERSAND_CHAR));
    if bytes.len() == 1 {
      return true;
    }
    if bytes[1] == b'#' {
      let mut index = 2;
      let hex = matches!(bytes.get(index), Some(b'x' | b'X'));
      if hex {
        index += 1;
      }
      while let Some(&byte) = bytes.get(index) {
        if byte.is_ascii_digit() || (hex && byte.is_ascii_hexdigit()) {
          index += 1;
        } else {
          return byte == b';' && index + 1 == bytes.len();
        }
      }
      return true;
    }
    let mut index = 1;
    while bytes.get(index).is_some_and(u8::is_ascii_alphanumeric) {
      index += 1;
      if index - 1 > max_entity_name_length() {
        return false;
      }
    }
    index == bytes.len() || (bytes.get(index) == Some(&b';') && index + 1 == bytes.len())
  }

  pub fn get_markdown(&mut self) -> String {
    let trimmed_end_len = self.buffer.trim_end().len();
    self.buffer.truncate(trimmed_end_len);
    let start = if self.preserve_leading_whitespace {
      0
    } else {
      self.buffer.len() - self.buffer.trim_start().len()
    };
    if start > 0 {
      self.buffer.drain(..start);
    }

    // Apply clean.fragments using recorded positions
    // Build new string copying segments, replacing broken links with text only
    if self.clean_flags & CLEAN_FRAGMENTS != 0 && !self.fragment_links.is_empty() {
      let trim_offset = start;
      let mut result = String::with_capacity(self.buffer.len());
      let mut cursor = 0usize;

      for &(bracket_start, link_end) in &self.fragment_links {
        let adj_start = bracket_start.saturating_sub(trim_offset);
        let adj_end = link_end.saturating_sub(trim_offset);
        if adj_end > self.buffer.len() || adj_start >= adj_end {
          continue;
        }

        // Extract fragment from buffer: [text](#fragment) → find ](#
        let range = &self.buffer[adj_start..adj_end];
        let is_valid = if let Some(hash_pos) = range.find("](#") {
          let frag_start = hash_pos + 3; // skip ](#
          let frag_end = range.len().saturating_sub(1); // skip trailing )
          if frag_start < frag_end {
            let fragment = &range[frag_start..frag_end];
            !self.heading_slugs.is_empty() && self.heading_slugs.iter().any(|s| s == fragment)
          } else {
            false
          }
        } else {
          true // not a fragment link pattern, keep as-is
        };

        if is_valid {
          continue; // keep original, will be copied by cursor
        }

        // Copy everything before this link
        if cursor < adj_start {
          result.push_str(&self.buffer[cursor..adj_start]);
        }
        // Extract and copy just the text (between [ and ])
        if let Some(close_bracket) = range.find("](#") {
          result.push_str(&self.buffer[adj_start + 1..adj_start + close_bracket]);
        }
        cursor = adj_end;
      }

      // Only rebuild if we actually replaced something
      if cursor > 0 {
        if cursor < self.buffer.len() {
          result.push_str(&self.buffer[cursor..]);
        }
        self.buffer = result;
      }
    }
    let output = std::mem::take(&mut self.buffer);
    self.clean_blank_lines(output)
  }

  fn clean_blank_lines(&mut self, content: String) -> String {
    if self.plain_text || self.clean_flags & CLEAN_BLANK_LINES == 0 {
      return content;
    }

    let mut output = String::with_capacity(content.len());
    let mut run = self.clean_newline_run;
    for character in content.chars() {
      if character == '\n' {
        run = run.saturating_add(1);
        if run <= 2 {
          output.push(character);
        }
      } else {
        run = 0;
        output.push(character);
      }
    }
    self.clean_newline_run = run;
    output
  }

  /// Commit end-of-input state: flush trailing buffered text and close any
  /// elements left open. The streaming parser keeps trailing text and unclosed
  /// elements pending because a later chunk might continue them; at true EOF
  /// they must be committed so trailing content is not dropped (e.g. a document
  /// that ends mid-paragraph like `<p>a<p>b`, or any unclosed fragment).
  ///
  /// `leftover` is the residual returned by the final `process_html`. Pure
  /// trailing text (no leading `<`) is emitted; a residual that is an
  /// incomplete start tag (leading `<`) is dropped, matching the browser
  /// tokenizer's EOF-in-tag behaviour. The text-buffer flags set while the
  /// trailing text was scanned persist on `self`, so `process_text_buffer`
  /// commits it exactly as if the next tag had triggered the flush.
  pub fn finalize(&mut self, leftover: &str) {
    let overflow_suppressed = self
      .overflow
      .as_ref()
      .is_some_and(|overflow| overflow.suppressed_name.is_some());
    let in_script = self
      .stack
      .last()
      .is_some_and(|node| node.tag_id == Some(TAG_SCRIPT) && node.custom_name.is_none());
    if in_script {
      self.push_script_text(leftover);
      self.flush_script_text();
      self.script_data_state = SCRIPT_DATA;
    } else if !overflow_suppressed
      && !leftover.is_empty()
      && (leftover.as_bytes()[0] != LT_CHAR
        || self.overflow.is_some()
        || self.in_non_nesting
        || self.text_mode() != TextMode::Data
        || matches!(leftover, "<" | "</")
        || leftover.as_bytes().get(1).is_some_and(|next| {
          !next.is_ascii_alphabetic() && !matches!(*next, EXCLAMATION_CHAR | SLASH_CHAR | b'?')
        })
        || (self.is_supported_svg_integration_point() && leftover == "<"))
    {
      if leftover.as_bytes()[0] == LT_CHAR {
        self.text_buffer_contains_non_whitespace = true;
        self.text_buffer_has_inline_gfm_hazard = true;
        self.last_char_was_whitespace = false;
      }
      let mut buf = leftover.to_string();
      self.process_text_buffer(&mut buf);
    }
    while !self.stack.is_empty() && self.streaming_error.is_none() {
      self.close_node();
    }
  }

  pub fn get_markdown_chunk(&mut self) -> String {
    if self.streaming_error.is_some() {
      return String::new();
    }
    self.flush_streaming_blockquote_lines();
    if self.streaming_error.is_some() {
      return String::new();
    }
    let buf_len = self.buffer.len();
    // Trailing spaces at the buffer end are never final outside <pre>: a later
    // block close (or a dropped empty element followed by a block) trims them,
    // and an inline close arriving next chunk can trim a text node's trailing
    // space. Yielding them would let that later trim silently remove an
    // already-sent byte and shift every byte after it. Always hold them back;
    // they are re-yielded once real content follows, or dropped at finalize.
    let in_pre = self.depth_map[TAG_PRE as usize] != 0;
    let mut stable_end = self.buffer.trim_end_matches(' ').len();
    if in_pre {
      if self.last_text_node_contains_whitespace {
        // A trailing whitespace run in the current text node stays mutable
        // until its inline/code element closes. That close trims ASCII
        // whitespace, so hold the whole run rather than yielding bytes it may
        // retract later.
        stable_end = self
          .buffer
          .trim_end_matches(|c: char| c.is_ascii_whitespace())
          .len();
      } else if stable_end < buf_len {
        // Other trailing spaces inside <pre> are significant code. A
        // line-leading run is the exception: list continuation indentation is
        // emitted before the next sibling is known and can still be replaced
        // by its list marker.
        let line_leading = stable_end == 0 || self.buffer.as_bytes()[stable_end - 1] == b'\n';
        if !line_leading {
          stable_end = buf_len;
        }
      }
    } else {
      // A block close or document finalization may still trim trailing block
      // spacing. Keep newlines buffered until following content makes them
      // stable, since yielded bytes cannot be retracted.
      stable_end = stable_end.min(self.buffer.trim_end_matches(['\n', ' ']).len());
    }
    let leading = if self.preserve_leading_whitespace || self.has_streamed_output {
      0
    } else {
      buf_len - self.buffer.trim_start().len()
    };
    // Hold output before every active rewrite owner. Whitespace immediately
    // before an owner can also be trimmed if that construct becomes empty.
    if let Some(rewrite_start) = self.earliest_rewrite_start() {
      stable_end = stable_end.min(
        self.buffer[..rewrite_start]
          .trim_end_matches(['\n', ' '])
          .len(),
      );
    }
    // `last_yielded_length` is an absolute buffer offset (see drain below).
    let mut start = self.last_yielded_length.max(leading);
    if start >= stable_end {
      self.drain_streamed_prefix();
      return String::new();
    }
    // Offsets here derive from marker positions and drain rebasing that need not
    // fall on a UTF-8 boundary; slicing mid-codepoint panics. Clamp both bounds
    // down to a boundary, holding any partial codepoint for the next chunk.
    while stable_end > start && !self.buffer.is_char_boundary(stable_end) {
      stable_end -= 1;
    }
    while start > 0 && !self.buffer.is_char_boundary(start) {
      start -= 1;
    }
    if start >= stable_end {
      self.drain_streamed_prefix();
      return String::new();
    }
    let new_content = self.buffer[start..stable_end].to_string();
    self.has_streamed_output = true;
    self.last_yielded_length = stable_end;
    self.drain_streamed_prefix();
    self.clean_blank_lines(new_content)
  }

  fn earliest_rewrite_start(&self) -> Option<usize> {
    let mut start = (self.depth_map[TAG_A as usize] > 0).then_some(self.link_bracket_pos);
    for candidate in [
      (self.in_heading && self.clean_flags & CLEAN_SELF_LINK_HEADINGS != 0)
        .then_some(self.heading_buffer_start),
      self.open_markers.first().map(|marker| marker.1),
      self.code_spans.first().map(|span| span.output_start),
      self.code_fence.as_ref().map(|fence| fence.output_start),
      self.blockquotes.first().map(|frame| frame.content_start),
    ]
    .into_iter()
    .flatten()
    {
      start = Some(start.map_or(candidate, |current| current.min(candidate)));
    }
    start
  }

  /// Free already-yielded output so streaming memory stays O(window), not
  /// O(document). Skipped when a whole-document feature still needs the full
  /// buffer.
  ///
  /// Must never change emitted bytes. Every syntax rewrite exposes its exact
  /// start through `earliest_rewrite_start`; the trailing content start covers
  /// whitespace cleanup. Both offsets are rebased when a prefix is removed.
  fn drain_streamed_prefix(&mut self) {
    #[cfg(test)]
    if self.disable_drain {
      return;
    }
    if self.clean_flags & CLEAN_FRAGMENTS != 0 {
      return;
    }
    // Keep the tail a late rewrite may still touch, and never drop the `[` of an
    // open link (its close can rewrite from that offset).
    // Formatting also inspects the last two bytes to count existing newlines.
    // Keep both, moving to a UTF-8 boundary if the tail starts in a code point.
    let mut retained_tail_start = self
      .buffer
      .len()
      .saturating_sub(2)
      .min(self.last_content_start.unwrap_or(self.buffer.len()));
    while !self.buffer.is_char_boundary(retained_tail_start) {
      retained_tail_start -= 1;
    }
    let mut drain_end = self.last_yielded_length.min(retained_tail_start);
    // An empty link or inline marker closing in a later chunk truncates the
    // buffer back to its reach-back point (`link_bracket_pos` / `output_start`).
    // The next block then counts its leading newlines from the two bytes ending
    // there (see `write_output`, which inspects only the last two), so keep
    // those two bytes; otherwise a dropped element leaks an extra newline in
    // streaming.
    let keep_two_before = |buf: &str, at: usize| {
      let mut i = at.saturating_sub(2);
      while i > 0 && !buf.is_char_boundary(i) {
        i -= 1;
      }
      i
    };
    if let Some(rewrite_start) = self.earliest_rewrite_start() {
      drain_end = drain_end.min(keep_two_before(&self.buffer, rewrite_start));
    }
    if drain_end == 0 {
      return;
    }
    let bytes = self.buffer.as_bytes();
    self.flushed_tail = if drain_end >= 2 {
      [bytes[drain_end - 2], bytes[drain_end - 1]]
    } else {
      [self.flushed_tail[1], bytes[0]]
    };
    self.advance_buffer_start_column(drain_end);
    self.buffer.drain(..drain_end);
    self.last_yielded_length -= drain_end;
    self.link_bracket_pos = self.link_bracket_pos.saturating_sub(drain_end);
    self.last_content_start = self
      .last_content_start
      .map(|start| start.saturating_sub(drain_end));
    for (_, output_start, content_start) in &mut self.open_markers {
      *output_start -= drain_end;
      *content_start -= drain_end;
    }
    for span in &mut self.code_spans {
      span.output_start -= drain_end;
      span.content_start -= drain_end;
    }
    if let Some(fence) = &mut self.code_fence {
      fence.output_start -= drain_end;
      fence.content_start -= drain_end;
    }
    for frame in &mut self.blockquotes {
      frame.content_start -= drain_end;
    }
    if self.in_heading {
      self.heading_buffer_start -= drain_end;
    }
    if let Some(link) = &mut self.self_link_heading {
      link.bracket_start -= drain_end;
      link.text_start -= drain_end;
      link.link_end -= drain_end;
    }
  }
}

// Internal result structs
// ========================================================================

pub(crate) struct OpeningTagResult {
  complete: bool,
  new_position: usize,
  self_closing: bool,
  skip: bool,
}

pub(crate) struct CloseTagResult {
  complete: bool,
  new_position: usize,
}

#[cfg(test)]
mod bounded_buffer_tests {
  use super::*;
  use std::cell::Cell;

  #[test]
  fn requested_capacity_is_charged_before_allocating() {
    let called = Cell::new(false);
    let mut value = String::new();
    let error = reserve_accounted_with(&mut value, 1, 0, 0, |_, _| {
      called.set(true);
      unreachable!()
    })
    .unwrap_err();

    assert_eq!(error, StreamingError::BufferLimitExceeded);
    assert!(!called.get());
    assert_eq!(value.capacity(), 0);
  }

  #[test]
  fn allocator_over_capacity_is_reconciled_and_dropped() {
    let mut value = String::new();
    let error = reserve_accounted_with(&mut value, 1, 0, 8, |value, additional| {
      value.try_reserve_exact(additional)?;
      value.reserve(64);
      Ok(())
    })
    .unwrap_err();

    assert_eq!(error, StreamingError::BufferLimitExceeded);
    assert_eq!(value.capacity(), 0);
  }

  #[test]
  fn allocation_failure_and_capacity_overflow_are_distinct() {
    let mut value = String::new();
    let allocation_error = reserve_accounted_with(&mut value, 1, 0, usize::MAX, |value, _| {
      value.try_reserve_exact(usize::MAX)
    })
    .unwrap_err();
    assert_eq!(allocation_error, StreamingError::AllocationFailed);

    let overflow =
      reserve_accounted(&mut value, isize::MAX as usize + 1, 0, usize::MAX).unwrap_err();
    assert_eq!(overflow, StreamingError::CapacityOverflow);
  }

  #[test]
  fn vector_capacity_is_charged_and_reconciled() {
    let called = Cell::new(false);
    let mut value = Vec::<usize>::new();
    let error = reserve_vec_accounted_with(&mut value, 1, 0, 0, |_, _| {
      called.set(true);
      unreachable!()
    })
    .unwrap_err();
    assert_eq!(error, StreamingError::BufferLimitExceeded);
    assert!(!called.get());

    let mut value = Vec::<u8>::new();
    let error = reserve_vec_accounted_with(&mut value, 1, 0, 8, |value, additional| {
      value.try_reserve_exact(additional)?;
      value.reserve(64);
      Ok(())
    })
    .unwrap_err();
    assert_eq!(error, StreamingError::BufferLimitExceeded);
    assert_eq!(value.capacity(), 0);

    let mut value = Vec::<usize>::new();
    let overflow =
      reserve_vec_accounted(&mut value, isize::MAX as usize, 0, usize::MAX).unwrap_err();
    assert_eq!(overflow, StreamingError::CapacityOverflow);
  }

  #[test]
  fn excessive_nesting_keeps_the_real_stacks_capped() {
    let html = "<div>".repeat(100_000);
    let mut state = ConvertState::new(HTMLToMarkdownOptions::default(), 0, OutputFormat::Markdown);
    state.process_html(&html);

    assert_eq!(state.stack.len(), 512);
    assert_eq!(state.tokenizer_contexts.len(), 512);
    assert_eq!(
      state.overflow.as_ref().map(|overflow| overflow.root_depth),
      Some(100_000 - 512)
    );
  }

  #[test]
  fn rewrites_restore_columns_before_a_drained_newline() {
    let mut state = ConvertState::new(
      HTMLToMarkdownOptions::default().with_wrap_width(40),
      64,
      OutputFormat::Markdown,
    );
    assert!(state.push_output_str("abcdefghij"));
    state.advance_buffer_start_column(10);
    state.buffer.drain(..10);

    assert!(state.push_output_str("abc\ndef"));
    state.truncate_output(3);
    assert_eq!(state.output_column, 13);

    assert!(state.push_output_str("\nghi"));
    assert!(state.replace_output_range(3, 4, ""));
    assert_eq!(state.output_column, 16);
  }
}
