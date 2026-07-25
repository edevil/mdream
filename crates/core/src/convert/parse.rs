//! HTML parsing: tag scanning, node lifecycle, text-buffer processing.

use super::*;

// Firefox and Chromium flatten DOM trees beyond this practical depth. Nodes
// beyond this boundary are represented by a fixed-size overflow summary.
const MAX_ELEMENT_DEPTH: usize = 512;

/// Tags valid inside `<head>` per the HTML parser's "in head" insertion mode.
/// Any other start tag implies the end of `<head>` and the start of the body, so
/// an unclosed head is auto-closed when one appears. Unknown tags (`None`) are
/// treated as body content, matching the spec's "anything else" rule. `<head>` is
/// deliberately excluded: a second/nested head must close the first rather than
/// stack, so malformed `<head><head>...<p>` does not trap body flow under an
/// outer head.
fn is_head_content_tag(tag_id: Option<u8>) -> bool {
  matches!(
    tag_id,
    Some(
      TAG_TITLE
        | TAG_META
        | TAG_LINK
        | TAG_BASE
        | TAG_STYLE
        | TAG_SCRIPT
        | TAG_NOSCRIPT
        | TAG_TEMPLATE
    )
  )
}

/// Start tags that cannot appear inside a `<p>` and therefore imply its end
/// (HTML §13.1.2.4 optional tags + the "in body" insertion mode, where these
/// tags first close an open `<p>` in button scope). Block containers, headings,
/// lists, tables, and the list-item/definition tags all close an open `<p>`.
///
/// A compile-time `[bool; MAX_TAG_ID]` table, not a `match`: this is evaluated
/// for every start tag opened while a `<p>` is on the stack (most inline tags in
/// prose), so the hot path is a single indexed load, matching the table-driven
/// style of `TAG_HANDLERS` and `depth_map`.
const CLOSES_P: [bool; MAX_TAG_ID] = {
  let mut t = [false; MAX_TAG_ID];
  t[TAG_DIV as usize] = true;
  t[TAG_P as usize] = true;
  t[TAG_UL as usize] = true;
  t[TAG_OL as usize] = true;
  t[TAG_DL as usize] = true;
  t[TAG_LI as usize] = true;
  t[TAG_DD as usize] = true;
  t[TAG_DT as usize] = true;
  t[TAG_TABLE as usize] = true;
  t[TAG_H1 as usize] = true;
  t[TAG_H2 as usize] = true;
  t[TAG_H3 as usize] = true;
  t[TAG_H4 as usize] = true;
  t[TAG_H5 as usize] = true;
  t[TAG_H6 as usize] = true;
  t[TAG_BLOCKQUOTE as usize] = true;
  t[TAG_SECTION as usize] = true;
  t[TAG_ARTICLE as usize] = true;
  t[TAG_HEADER as usize] = true;
  t[TAG_FOOTER as usize] = true;
  t[TAG_NAV as usize] = true;
  t[TAG_ASIDE as usize] = true;
  t[TAG_PRE as usize] = true;
  t[TAG_HR as usize] = true;
  t[TAG_FORM as usize] = true;
  t[TAG_FIELDSET as usize] = true;
  t[TAG_FIGURE as usize] = true;
  t[TAG_FIGCAPTION as usize] = true;
  t[TAG_ADDRESS as usize] = true;
  t[TAG_MAIN as usize] = true;
  t[TAG_CENTER as usize] = true;
  t[TAG_DETAILS as usize] = true;
  t[TAG_SUMMARY as usize] = true;
  t[TAG_DIALOG as usize] = true;
  t
};

/// Start tags that can trigger any implied-end-tag recovery branch below. The
/// common inline tags (`code`, `em`, `span`, ...) otherwise skip the whole
/// recovery dispatch instead of loading unrelated depth counters and matching
/// through recovery-only cases on every start tag.
const NEEDS_IMPLIED_END_RECOVERY: [bool; MAX_TAG_ID] = {
  let mut t = CLOSES_P;
  t[TAG_A as usize] = true;
  t[TAG_TD as usize] = true;
  t[TAG_TH as usize] = true;
  t[TAG_TR as usize] = true;
  t[TAG_THEAD as usize] = true;
  t[TAG_TBODY as usize] = true;
  t[TAG_TFOOT as usize] = true;
  t[TAG_OPTION as usize] = true;
  t[TAG_OPTGROUP as usize] = true;
  t[TAG_SELECT as usize] = true;
  t
};

#[inline]
fn closes_p(tag_id: u8) -> bool {
  CLOSES_P[tag_id as usize]
}

#[inline]
fn needs_implied_end_recovery(tag_id: u8) -> bool {
  (tag_id as usize) < MAX_TAG_ID && NEEDS_IMPLIED_END_RECOVERY[tag_id as usize]
}

/// "Button scope" terminators for closing a `<p>`: scanning up the open stack
/// stops here so a `<p>` outside a table cell is never closed from inside one.
/// `UL`/`OL`/`DL`/`LI` are added (a deviation from the bare spec list) to keep
/// scans short; a `<p>` is always closed before any of these can become its
/// ancestor, so they never hide a closable `<p>`.
fn is_p_scope_boundary(tag_id: u8) -> bool {
  matches!(
    tag_id,
    TAG_BUTTON
      | TAG_TD
      | TAG_TH
      | TAG_CAPTION
      | TAG_TABLE
      | TAG_TEMPLATE
      | TAG_HTML
      | TAG_UL
      | TAG_OL
      | TAG_DL
      | TAG_LI
  )
}

/// "List item scope" terminators for closing a `<li>`: a new `<li>` closes the
/// previous one only within the same list, never across a nested list or table.
fn is_li_scope_boundary(tag_id: u8) -> bool {
  matches!(
    tag_id,
    TAG_UL | TAG_OL | TAG_TABLE | TAG_TD | TAG_TH | TAG_CAPTION | TAG_TEMPLATE | TAG_HTML
  )
}

/// Scope terminators for closing a `<dt>`/`<dd>`: each closes the other within
/// the same `<dl>`, never crossing into a nested list or table.
fn is_dl_scope_boundary(tag_id: u8) -> bool {
  matches!(
    tag_id,
    TAG_DL
      | TAG_UL
      | TAG_OL
      | TAG_LI
      | TAG_TABLE
      | TAG_TD
      | TAG_TH
      | TAG_CAPTION
      | TAG_TEMPLATE
      | TAG_HTML
  )
}

/// "Table cell scope" terminators: a new `<td>`/`<th>` closes the current cell
/// (and any inline content left open inside it), stopping at the row/section.
fn is_cell_scope_boundary(tag_id: u8) -> bool {
  matches!(
    tag_id,
    TAG_TR | TAG_THEAD | TAG_TBODY | TAG_TFOOT | TAG_TABLE | TAG_CAPTION | TAG_TEMPLATE | TAG_HTML
  )
}

/// Block-level terminators for closing an `<a>`: a nested `<a>` closes the open
/// one (HTML forbids nested anchors — the adoption agency closes the outer),
/// but only within the same block so a stray open `<a>` in another block is left
/// alone. Closing intervening inline formatting matches the spec's reconstruction.
fn is_a_scope_boundary(tag_id: u8) -> bool {
  matches!(
    tag_id,
    TAG_P
      | TAG_DIV
      | TAG_LI
      | TAG_UL
      | TAG_OL
      | TAG_DL
      | TAG_DD
      | TAG_DT
      | TAG_TABLE
      | TAG_TD
      | TAG_TH
      | TAG_TR
      | TAG_CAPTION
      | TAG_BLOCKQUOTE
      | TAG_SECTION
      | TAG_ARTICLE
      | TAG_HEADER
      | TAG_FOOTER
      | TAG_NAV
      | TAG_ASIDE
      | TAG_MAIN
      | TAG_FORM
      | TAG_FIELDSET
      | TAG_FIGURE
      | TAG_BUTTON
      | TAG_H1
      | TAG_H2
      | TAG_H3
      | TAG_H4
      | TAG_H5
      | TAG_H6
      | TAG_TEMPLATE
      | TAG_HTML
  )
}

/// Whether an element is visually hidden, so the filter should drop it and its
/// subtree: inline `display:none` / `visibility:hidden` / `position:absolute|fixed`,
/// or the `hidden` attribute (except `hidden="until-found"`).
///
/// Matches the actual `display:`/`visibility:`/`position:` declaration (not bare
/// keywords like `fixed`, which would false-match e.g. `background-attachment:fixed`)
/// and both unspaced and `: `-spaced forms. Allocation-free; uppercased
/// properties (rare in inline styles) are not handled.
fn is_hidden(node: &ElementNode) -> bool {
  if let Some(style) = node.attributes.get("style")
    && (style.contains("display:none")
      || style.contains("display: none")
      || style.contains("visibility:hidden")
      || style.contains("visibility: hidden")
      || style.contains("position:absolute")
      || style.contains("position: absolute")
      || style.contains("position:fixed")
      || style.contains("position: fixed"))
  {
    return true;
  }
  // The `hidden` attribute hides the element unless it's the revealable
  // `until-found` state (an enumerated keyword, so ASCII case-insensitive).
  matches!(node.attributes.get("hidden"), Some(v) if !v.eq_ignore_ascii_case("until-found"))
}

impl ConvertState {
  fn record_overflow_start(
    &mut self,
    tag_name: &str,
    tag_id: Option<u8>,
    tag_handler: Option<&TagHandler>,
  ) {
    if let Some(overflow) = &mut self.overflow {
      if overflow.root_name == tag_name {
        overflow.root_depth = overflow.root_depth.saturating_add(1);
      }
      if overflow.suppressed_name.as_deref() == Some(tag_name) {
        overflow.suppressed_depth = overflow.suppressed_depth.saturating_add(1);
      }
    } else {
      let Some(root_name) = self.retained_string_copy(tag_name, 0) else {
        return;
      };
      self.overflow = Some(OverflowState {
        root_name,
        root_depth: 1,
        suppressed_name: None,
        suppressed_depth: 0,
        raw_name: None,
        raw_mode: TextMode::Data,
        raw_is_script: false,
      });
    }

    let Some(handler) = tag_handler else { return };
    let raw_mode = self.text_mode_for_tag(tag_name, tag_id);
    let overflow = self.overflow.as_mut().expect("overflow was initialized");
    let suppresses_subtree = matches!(
      tag_id,
      Some(
        TAG_SCRIPT
          | TAG_STYLE
          | TAG_TEMPLATE
          | TAG_IFRAME
          | TAG_NOFRAMES
          | TAG_NOEMBED
          | TAG_NOSCRIPT
          | TAG_DATALIST
      )
    );
    let needs_suppressed_name = suppresses_subtree && overflow.suppressed_name.is_none();
    if needs_suppressed_name {
      let Some(suppressed_name) = self.retained_string_copy(tag_name, 0) else {
        return;
      };
      let overflow = self.overflow.as_mut().expect("overflow was initialized");
      overflow.suppressed_name = Some(suppressed_name);
      overflow.suppressed_depth = 1;
    }
    if handler.is_non_nesting {
      let Some(raw_name) = self.retained_string_copy(tag_name, 0) else {
        return;
      };
      let overflow = self.overflow.as_mut().expect("overflow was initialized");
      overflow.raw_name = Some(raw_name);
      overflow.raw_mode = raw_mode;
      overflow.raw_is_script = tag_id == Some(TAG_SCRIPT) && tag_name == "script";
      self.script_data_state = SCRIPT_DATA;
    }
  }

  pub(crate) fn process_overflow_closing_tag(
    &mut self,
    html_chunk: &str,
    position: usize,
  ) -> CloseTagResult {
    let bytes = html_chunk.as_bytes();
    let mut i = position + 2;
    let name_start = i;
    let mut name_end = bytes.len();
    let mut quote = 0;
    let mut complete = false;
    while i < bytes.len() {
      let c = bytes[i];
      if name_end == bytes.len() && (is_whitespace(c) || c == SLASH_CHAR || c == GT_CHAR) {
        name_end = i;
      }
      if quote != 0 {
        if c == quote {
          quote = 0;
        }
      } else if name_end != bytes.len() && (c == QUOTE_CHAR || c == APOS_CHAR) {
        quote = c;
      } else if c == GT_CHAR {
        complete = true;
        break;
      }
      i += 1;
    }
    if !complete {
      return CloseTagResult {
        complete: false,
        new_position: position,
      };
    }

    let raw_name = &html_chunk[name_start..name_end];
    let name: Cow<'_, str> =
      if let Some(id) = crate::consts::get_tag_id_ci_bytes(raw_name.as_bytes()) {
        Cow::Borrowed(TAG_NAMES[id as usize])
      } else if raw_name.bytes().any(|b| b.is_ascii_uppercase()) {
        Cow::Owned(raw_name.to_ascii_lowercase())
      } else {
        Cow::Borrowed(raw_name)
      };

    let mut leave_overflow = false;
    if let Some(overflow) = &mut self.overflow {
      if overflow.raw_name.as_deref() == Some(name.as_ref()) {
        overflow.raw_name = None;
        overflow.raw_mode = TextMode::Data;
        overflow.raw_is_script = false;
        self.script_data_state = SCRIPT_DATA;
      }
      if overflow.suppressed_name.as_deref() == Some(name.as_ref()) {
        overflow.suppressed_depth = overflow.suppressed_depth.saturating_sub(1);
        if overflow.suppressed_depth == 0 {
          overflow.suppressed_name = None;
        }
      }
      if overflow.root_name == name {
        overflow.root_depth = overflow.root_depth.saturating_sub(1);
        leave_overflow = overflow.root_depth == 0;
      }
    }
    if leave_overflow {
      self.overflow = None;
      self.script_data_state = SCRIPT_DATA;
    }
    self.just_closed_tag = true;
    CloseTagResult {
      complete: true,
      new_position: i + 1,
    }
  }

  pub(crate) fn process_text_buffer(&mut self, text_buffer: &mut String) {
    let contains_non_whitespace = self.text_buffer_contains_non_whitespace;
    let contains_whitespace = self.text_buffer_contains_whitespace;
    let has_inline_gfm_hazard =
      self.text_buffer_has_inline_gfm_hazard || self.has_encoded_html_entity;
    self.text_buffer_contains_non_whitespace = false;
    self.text_buffer_contains_whitespace = false;
    self.text_buffer_has_inline_gfm_hazard = false;

    // No parent element means this is a top-level (root) text node, e.g. the
    // leading `foo ` in the fragment `foo <sup>bar</sup>`. Such text must
    // still be emitted rather than dropped (issue #93).
    let mut excludes_text_nodes = self
      .stack
      .last()
      .is_some_and(|parent| parent.excludes_text_nodes || parent.excluded_from_markdown);

    if self.has_isolate_main {
      if self.isolate_main_found {
        if self.isolate_main_closed {
          excludes_text_nodes = true;
        }
      } else if self.isolate_first_header_depth.is_none() {
        if self.depth_map[TAG_HEAD as usize] == 0 {
          excludes_text_nodes = true;
        }
      } else if self.isolate_after_footer {
        excludes_text_nodes = true;
      }
    }

    if self.has_frontmatter
      && self.frontmatter_in_head
      && !excludes_text_nodes
      && self
        .stack
        .last()
        .is_some_and(|p| p.tag_id == Some(TAG_TITLE))
    {
      self.capture_frontmatter_title(text_buffer);
      text_buffer.clear();
      return;
    }

    let in_pre_tag = self.in_pre;
    let child_text_node_index = self.stack.last().map_or(0, |n| n.child_text_node_index);

    // Whitespace-only text before an element's first child is indentation and
    // can be dropped. At the fragment root it can instead separate adjacent
    // inline siblings, so emit it and let the output layer trim or absorb it at
    // leading/trailing and block boundaries.
    if !in_pre_tag
      && !contains_non_whitespace
      && child_text_node_index == 0
      && !self.stack.is_empty()
    {
      return;
    }
    if text_buffer.is_empty() {
      return;
    }

    let first_block_parent_index = self.first_block_parent_index;
    let first_block_child_text_count =
      first_block_parent_index.map_or(0, |idx| self.stack[idx].child_text_node_index);

    let mut text = std::mem::take(text_buffer);
    let mut tailwind_prefix = None;
    let mut tailwind_suffix = None;
    let is_first_text_in_block = first_block_child_text_count == 0
      && (first_block_parent_index.is_some()
        || self.buffer.is_empty()
        || self.buffer.as_bytes().last() == Some(&b'\n'));
    if contains_whitespace && is_first_text_in_block {
      let mut start = 0;
      let bytes = text.as_bytes();
      while start < bytes.len()
        && (if in_pre_tag {
          bytes[start] == NEWLINE_CHAR || bytes[start] == CARRIAGE_RETURN_CHAR
        } else {
          is_whitespace(bytes[start])
        })
      {
        start += 1;
      }
      if start > 0 {
        text.drain(..start);
      }
    }

    if self.has_encoded_html_entity {
      let protect_decoded_entity_references = !self.plain_text
        && self.depth_map[TAG_PRE as usize] == 0
        && self.depth_map[TAG_CODE as usize] == 0
        && !self.in_raw_html_block();
      let decoded = if protect_decoded_entity_references {
        decode_html_entities_for_markdown(&text)
      } else {
        decode_html_entities(&text)
      };
      if let Cow::Owned(decoded) = decoded {
        text = decoded;
      }
      self.has_encoded_html_entity = false;
    }

    if self.has_tailwind
      && let Some(parent) = self.stack.last()
      && let Some(tw) = &parent.tailwind
    {
      if tw.hidden {
        excludes_text_nodes = true;
      } else if !excludes_text_nodes {
        tailwind_prefix.clone_from(&tw.prefix);
        tailwind_suffix.clone_from(&tw.suffix);
      }
    }

    if !self.extraction_tracked.is_empty() {
      let current_depth = self.stack.len();
      for index in 0..self.extraction_tracked.len() {
        if self.extraction_tracked[index].stack_depth <= current_depth
          && !self.extraction_tracked[index].limit_exceeded
        {
          let Some(required) = self.extraction_tracked[index]
            .text_content
            .len()
            .checked_add(text.len())
          else {
            self.extraction_tracked[index].limit_exceeded = true;
            self.extraction_tracked[index].text_content = String::new();
            continue;
          };
          let retained_by_result = tracked_extraction_bytes(&self.extraction_tracked[index], true)
            .saturating_sub(self.extraction_tracked[index].text_content.capacity())
            .saturating_add(required);
          if retained_by_result > MAX_EXTRACTION_RESULT_BYTES {
            self.extraction_tracked[index].limit_exceeded = true;
            self.extraction_tracked[index].text_content = String::new();
            continue;
          }
          let retained = self.extraction_retained_capacity();
          let tracked = &mut self.extraction_tracked[index];
          if reserve_accounted(
            &mut tracked.text_content,
            required,
            retained,
            MAX_EXTRACTION_BYTES,
          )
          .is_err()
          {
            tracked.limit_exceeded = true;
            tracked.text_content = String::new();
          } else {
            tracked.text_content.push_str(&text);
            if tracked_extraction_bytes(tracked, true) > MAX_EXTRACTION_RESULT_BYTES {
              tracked.limit_exceeded = true;
              tracked.text_content = String::new();
            }
          }
        }
      }
    }

    if !excludes_text_nodes {
      let depth = self.depth;
      let index = self.stack.last().map_or(0, |n| n.current_walk_index);
      self.text_buffer_has_inline_gfm_hazard = has_inline_gfm_hazard;
      self.emit_text_with_generated_markdown(
        &text,
        contains_whitespace,
        depth,
        index,
        tailwind_prefix.as_deref(),
        tailwind_suffix.as_deref(),
      );
    }

    // Recover String allocation
    text.clear();
    *text_buffer = text;

    if let Some(parent) = self.stack.last_mut() {
      parent.current_walk_index += 1;
    }

    let up_to = first_block_parent_index.unwrap_or(0);
    for idx in up_to..self.stack.len() {
      self.stack[idx].child_text_node_index += 1;
    }
  }

  /// Close open nodes from the top of the stack up to and including the nearest
  /// node matching `target`, but only if it is found before `boundary` (in which
  /// case nothing is closed). Implements implied end tags for `<p>`, `<li>`, and
  /// `<dt>`/`<dd>`: the intervening unmatched nodes (inline formatting, unknown
  /// elements) are closed along the way, mirroring the spec's "generate implied
  /// end tags" step.
  fn close_implied_to(&mut self, target: fn(u8) -> bool, boundary: fn(u8) -> bool) {
    let mut close_count = 0usize;
    let mut found = false;
    for node in self.stack.iter().rev() {
      match node.tag_id {
        Some(id) if target(id) => {
          close_count += 1;
          found = true;
          break;
        }
        Some(id) if boundary(id) => break,
        _ => close_count += 1,
      }
    }
    if found {
      for _ in 0..close_count {
        self.close_node();
      }
    }
  }

  /// Close open table-internal nodes (cells, rows, sections) from the top while
  /// they match `closeable`, stopping at the first node that does not (e.g. the
  /// enclosing `<table>`). Implements implied end tags for `<tr>` (closes an open
  /// cell + row) and `<thead>`/`<tbody>`/`<tfoot>` (closes cell + row + section).
  fn close_table_context(&mut self, closeable: fn(u8) -> bool) {
    while let Some(top) = self.stack.last() {
      match top.tag_id {
        Some(id) if closeable(id) => self.close_node(),
        _ => break,
      }
    }
  }

  /// Close the nearest select-related `target_id`. Option/optgroup scans stop
  /// at their owning `<select>`; a select scan includes the select itself. This
  /// only runs for recovery tags, leaving the common path as one table lookup.
  fn close_select_to(&mut self, target_id: u8) -> bool {
    let mut target_index = None;
    for i in (0..self.stack.len()).rev() {
      match self.stack[i].tag_id {
        Some(id) if id == target_id => {
          target_index = Some(i);
          break;
        }
        Some(TAG_SELECT) if target_id != TAG_SELECT => break,
        Some(TAG_TEMPLATE) => break,
        _ => {}
      }
    }
    if let Some(index) = target_index {
      while self.stack.len() > index {
        self.close_node();
      }
      true
    } else {
      false
    }
  }

  fn discard_isolate_fallback(&mut self) {
    self.buffer.truncate(
      self
        .isolate_fallback_output_start
        .unwrap_or(self.buffer.len()),
    );
    self.last_content_start = None;
    self.last_text_node_contains_whitespace = false;
    self.has_last_text_node = false;
    self.last_node_is_inline = false;
    self.pending_inline_whitespace = false;
    self.last_yielded_length = 0;
    self.has_streamed_output = false;
    self.flushed_tail = [0; 2];
    self.buffer_start_column = 0;
    self.output_column = 0;
    self.clean_newline_run = 0;
    self.skip_current_link = false;
    self.link_bracket_pos = 0;
    self.open_markers.clear();
    self.code_spans.clear();
    self.code_fence = None;
    self.blockquotes.clear();
    self.heading_slugs.clear();
    self.fragment_links.clear();
    self.in_heading = false;
    self.heading_buffer_start = 0;
    self.self_link_heading = None;
  }

  pub(crate) fn process_opening_tag(
    &mut self,
    tag_name: &str,
    tag_id: Option<u8>,
    builtin_tag_id: Option<u8>,
    is_alias: bool,
    html_chunk: &str,
    position: usize,
  ) -> OpeningTagResult {
    let is_builtin = builtin_tag_id.is_some();
    let tag_handler = tag_id.and_then(get_tag_handler);
    let needs_attrs = tag_handler.is_some_and(|h| h.needs_attributes)
      || self.has_tailwind
      || self.has_filter
      || self.has_extraction
      || self.has_tag_overrides
      || self.has_frontmatter;
    let (complete, new_position, attributes, syntactic_self_closing) =
      process_tag_attributes(html_chunk, position, !needs_attrs);

    if !complete {
      return OpeningTagResult {
        complete: false,
        new_position: position,
        self_closing: false,
        skip: false,
      };
    }
    if attributes.limit_exceeded() && self.streaming_limit.is_some() {
      self.fail_streaming(StreamingError::ParserLimitExceeded);
      return OpeningTagResult {
        complete: true,
        new_position,
        self_closing: false,
        skip: true,
      };
    }

    let override_config = self
      .options
      .plugins
      .as_ref()
      .and_then(|plugins| plugins.tag_overrides.as_ref())
      .and_then(|overrides| overrides.iter().find(|(name, _)| name == tag_name))
      .map(|(_, config)| config.clone());
    let aliased_void = is_alias && tag_handler.is_some_and(|handler| handler.is_self_closing);
    let supported_foreign_self_close = syntactic_self_closing
      && (self.in_supported_svg_content() || builtin_tag_id == Some(TAG_SVG));
    let default_self_closing =
      (is_html_void_tag(builtin_tag_id) && !self.in_supported_svg_content()) || aliased_void;
    let self_closing = override_config
      .as_ref()
      .and_then(|config| config.is_self_closing)
      .unwrap_or(default_self_closing)
      || supported_foreign_self_close;

    if self.overflow.is_some() {
      if !self_closing {
        self.record_overflow_start(tag_name, tag_id, tag_handler);
      }
      return OpeningTagResult {
        complete: true,
        new_position,
        self_closing,
        skip: true,
      };
    }

    // Browser recovery: a non-head start tag while <head> is still open means the
    // page never closed its head (no </head>/<body>). Auto-close head (and anything
    // wrongly opened inside it) so body content parses as flow content with normal
    // block spacing instead of inheriting head's whitespace collapsing.
    if self.depth_map[TAG_HEAD as usize] > 0
      && self.depth_map[TAG_TEMPLATE as usize] == 0
      && !is_head_content_tag(tag_id)
    {
      while self
        .stack
        .last()
        .is_some_and(|n| n.tag_id != Some(TAG_HEAD))
      {
        self.close_node();
      }
      if self
        .stack
        .last()
        .is_some_and(|n| n.tag_id == Some(TAG_HEAD))
      {
        self.close_node();
      }
    }

    // Browser recovery: implied end tags (HTML §13.1.2.4 optional tags +
    // tree-construction). Common malformed-but-valid markup omits end tags
    // (`<p>a<p>b`, `<li>a<li>b`, `<td>a<td>b`, `<dt>t<dd>d`); auto-close the
    // open element so the new sibling is not wrongly nested. Runs after the
    // tag is confirmed complete (above) so a chunk-split start tag never
    // mutates parser state or emits a premature close.
    if let Some(id) = tag_id
      && needs_implied_end_recovery(id)
    {
      match id {
        // In the "in select" insertion mode a nested <select> acts as the end
        // of the open select; the incoming start tag itself is ignored.
        TAG_SELECT if self.depth_map[TAG_SELECT as usize] > 0 => {
          if self.close_select_to(TAG_SELECT) {
            return OpeningTagResult {
              complete: true,
              new_position,
              self_closing: false,
              skip: true,
            };
          }
        }
        TAG_OPTION => {
          if self.depth_map[TAG_SELECT as usize] > 0 {
            self.close_select_to(TAG_OPTION);
          } else if self
            .stack
            .last()
            .is_some_and(|node| node.tag_id == Some(TAG_OPTION))
          {
            self.close_node();
          }
        }
        TAG_OPTGROUP if self.depth_map[TAG_SELECT as usize] > 0 => {
          self.close_select_to(TAG_OPTION);
          self.close_select_to(TAG_OPTGROUP);
        }
        // A nested <a> closes the open one (anchors cannot nest), so the
        // markdown is two adjacent links rather than invalid nested `[..]`.
        TAG_A if self.depth_map[TAG_A as usize] > 0 => {
          self.close_implied_to(|t| t == TAG_A, is_a_scope_boundary);
        }
        TAG_TD | TAG_TH | TAG_TR | TAG_THEAD | TAG_TBODY | TAG_TFOOT
          if self.depth_map[TAG_TABLE as usize] > 0 =>
        {
          match id {
            TAG_TD | TAG_TH
              if self.depth_map[TAG_TD as usize] > 0 || self.depth_map[TAG_TH as usize] > 0 =>
            {
              self.close_implied_to(|t| t == TAG_TD || t == TAG_TH, is_cell_scope_boundary);
            }
            TAG_TR if self.depth_map[TAG_TR as usize] > 0 => {
              self.close_table_context(|t| matches!(t, TAG_TD | TAG_TH | TAG_TR));
            }
            TAG_THEAD | TAG_TBODY | TAG_TFOOT => {
              self.close_table_context(|t| {
                matches!(
                  t,
                  TAG_TD | TAG_TH | TAG_TR | TAG_THEAD | TAG_TBODY | TAG_TFOOT | TAG_CAPTION
                )
              });
            }
            _ => {}
          }
        }
        TAG_A | TAG_TD | TAG_TH | TAG_TR | TAG_THEAD | TAG_TBODY | TAG_TFOOT | TAG_OPTGROUP
        | TAG_SELECT => {}
        _ => {
          if self.depth_map[TAG_P as usize] > 0 {
            debug_assert!(closes_p(id));
            self.close_implied_to(|t| t == TAG_P, is_p_scope_boundary);
          }
          match id {
            // A heading start closes an open heading (they cannot nest); only
            // when one is the current node, matching the spec's "if the current
            // node is an h1–h6 element, pop it" step.
            TAG_H1 | TAG_H2 | TAG_H3 | TAG_H4 | TAG_H5 | TAG_H6
              if self.stack.last().is_some_and(|n| {
                matches!(
                  n.tag_id,
                  Some(TAG_H1 | TAG_H2 | TAG_H3 | TAG_H4 | TAG_H5 | TAG_H6)
                )
              }) =>
            {
              self.close_node();
            }
            TAG_LI if self.depth_map[TAG_LI as usize] > 0 => {
              self.close_implied_to(|t| t == TAG_LI, is_li_scope_boundary);
            }
            TAG_DT | TAG_DD
              if self.depth_map[TAG_DT as usize] > 0 || self.depth_map[TAG_DD as usize] > 0 =>
            {
              self.close_implied_to(|t| t == TAG_DT || t == TAG_DD, is_dl_scope_boundary);
            }
            _ => {}
          }
        }
      }
    }

    if !self_closing && self.stack.len() >= MAX_ELEMENT_DEPTH {
      self.record_overflow_start(tag_name, tag_id, tag_handler);
      return OpeningTagResult {
        complete: true,
        new_position,
        self_closing: false,
        skip: true,
      };
    }

    if let Some(id) = tag_id {
      debug_assert!(
        (id as usize) < MAX_TAG_ID,
        "tag_id {id} exceeds MAX_TAG_ID {MAX_TAG_ID}"
      );
      if (id as usize) < MAX_TAG_ID {
        self.depth_map[id as usize] = self.depth_map[id as usize].saturating_add(1);
      }
      if id == TAG_PRE {
        self.in_pre = true;
      }
    }
    self.depth += 1;

    let current_walk_index = self.stack.last().map_or(0, |n| n.current_walk_index);
    let custom_name = if is_builtin && !is_alias {
      None
    } else {
      Some(tag_name.to_string())
    };

    let (mut h_inline, h_excludes, h_non_nesting, mut h_collapses, mut h_spacing) =
      if let Some(h) = tag_handler {
        (
          h.is_inline,
          h.excludes_text_nodes,
          h.is_non_nesting,
          h.collapses_inner_white_space,
          h.spacing,
        )
      } else if tag_id.is_none() {
        // Truly unknown tag (not in dictionary, no override): treat as inline
        // with zero spacing so it doesn't fragment the surrounding paragraph.
        // `<p>before <ex>foo</ex> after</p>` becomes `before foo after`. Users
        // opt custom elements into block semantics via `tagOverrides`.
        (true, false, false, false, Some(NO_SPACING))
      } else {
        // Built-in tag without a dedicated handler (e.g. caption, span fallback):
        // keep previous block-default behaviour.
        (false, false, false, false, None)
      };
    if let Some(config) = override_config.as_ref() {
      h_inline = config.is_inline.unwrap_or(h_inline);
      h_collapses = config.collapses_inner_white_space.unwrap_or(h_collapses);
      h_spacing = config.spacing.or(h_spacing);
    }

    let mut tag = if let Some(mut pooled) = self.node_pool.pop() {
      pooled.custom_name = custom_name;
      pooled.attributes = attributes;
      pooled.tag_id = tag_id;
      pooled.depth = self.depth;
      pooled.index = current_walk_index;
      pooled.current_walk_index = 0;
      pooled.child_text_node_index = 0;
      pooled.list_number = 0;
      pooled.contains_whitespace = false;
      pooled.excluded_from_markdown = false;
      pooled.tailwind = None;
      pooled.is_inline = h_inline;
      pooled.excludes_text_nodes = h_excludes;
      pooled.is_non_nesting = h_non_nesting;
      pooled.collapses_inner_white_space = h_collapses;
      pooled.spacing = h_spacing;
      pooled
    } else {
      ElementNode {
        custom_name,
        attributes,
        tag_id,
        depth: self.depth,
        index: current_walk_index,
        current_walk_index: 0,
        child_text_node_index: 0,
        list_number: 0,
        contains_whitespace: false,
        excluded_from_markdown: false,
        tailwind: None,
        is_inline: h_inline,
        excludes_text_nodes: h_excludes,
        is_non_nesting: h_non_nesting,
        collapses_inner_white_space: h_collapses,
        spacing: h_spacing,
      }
    };

    let mut skip_node = false;
    let mut filter_excluded = false;
    let in_template = self.depth_map[TAG_TEMPLATE as usize] > 0;

    if self.has_plugins {
      if self.has_tailwind {
        let parent_hidden = self
          .stack
          .last()
          .and_then(|p| p.tailwind.as_ref())
          .is_some_and(|tw| tw.hidden);

        if let Some(class_attr) = tag.attributes.get("class") {
          let (mut prefix, mut suffix, hidden) = process_tailwind_classes(class_attr);
          if self.plain_text {
            prefix = None;
            suffix = None;
          }
          let hidden = hidden || parent_hidden;
          if prefix.is_some() || suffix.is_some() || hidden {
            tag.tailwind = Some(Box::new(TailwindData {
              prefix,
              suffix,
              hidden,
            }));
            if hidden {
              skip_node = true;
            }
          }
        } else if parent_hidden {
          tag.tailwind = Some(Box::new(TailwindData {
            prefix: None,
            suffix: None,
            hidden: true,
          }));
          skip_node = true;
        }
      }

      if self.has_filter {
        // Hidden elements (and their subtrees) are dropped — browsers never
        // render them. `hidden_since_depth` records the shallowest open hidden
        // element, so once inside a hidden subtree we skip O(1) without calling
        // is_hidden() again. Cleared in close_node at the matching depth.
        if self.hidden_since_depth.is_some() {
          skip_node = true;
          filter_excluded = true;
        } else if is_hidden(&tag) {
          skip_node = true;
          filter_excluded = true;
          self.hidden_since_depth = Some(self.depth);
        }
        if !skip_node {
          for (_, parsed) in &self.filter_exclude_parsed {
            if matches_selector(&tag, parsed) {
              skip_node = true;
              filter_excluded = true;
              break;
            }
          }
        }
        if !skip_node {
          for parent in &self.stack {
            for (_, parsed) in &self.filter_exclude_parsed {
              if matches_selector(parent, parsed) {
                skip_node = true;
                filter_excluded = true;
                break;
              }
            }
            if skip_node {
              break;
            }
          }
        }
        if !skip_node && !self.filter_include_parsed.is_empty() {
          let mut match_found = false;
          for (_, parsed) in &self.filter_include_parsed {
            if matches_selector(&tag, parsed) {
              match_found = true;
              break;
            }
          }
          if !match_found && self.filter_process_children {
            for parent in &self.stack {
              for (_, parsed) in &self.filter_include_parsed {
                if matches_selector(parent, parsed) {
                  match_found = true;
                  break;
                }
              }
              if match_found {
                break;
              }
            }
          }
          if !match_found {
            skip_node = true;
            filter_excluded = true;
          }
        }
      }

      if self.has_isolate_main && !in_template {
        let is_main = tag_id == Some(TAG_MAIN);
        if !self.isolate_main_found && is_main && self.depth <= 50 {
          self.isolate_main_found = true;
          self.discard_isolate_fallback();
        }
        if self.isolate_main_found {
          if self.isolate_main_closed {
            skip_node = true;
          }
        } else {
          let is_header = tag_id.is_some_and(|id| (TAG_H1..=TAG_H6).contains(&id));
          if self.isolate_first_header_depth.is_none()
            && is_header
            && self.depth_map[TAG_HEADER as usize] == 0
          {
            self.isolate_first_header_depth = Some(self.depth);
            self.isolate_fallback_output_start = Some(self.buffer.len());
          }
          if let Some(header_depth) = self.isolate_first_header_depth
            && !self.isolate_after_footer
            && tag_id == Some(TAG_FOOTER)
            && self.depth.saturating_sub(header_depth) <= 5
          {
            self.isolate_after_footer = true;
            skip_node = true;
          }
          if self.isolate_first_header_depth.is_none() {
            if tag_id != Some(TAG_HEAD) && self.depth_map[TAG_HEAD as usize] == 0 {
              skip_node = true;
            }
          } else if self.isolate_after_footer {
            skip_node = true;
          }
        }
      }

      if self.has_frontmatter && !in_template {
        if tag_id == Some(TAG_HEAD) {
          self.frontmatter_in_head = true;
        } else if self.frontmatter_in_head && tag_id == Some(TAG_META) {
          let name = tag
            .attributes
            .get("name")
            .or_else(|| tag.attributes.get("property"));
          let content = tag.attributes.get("content");
          if let (Some(n), Some(c)) = (name, content) {
            let n_str = n.as_str();
            let is_allowed = match n_str {
              "description"
              | "keywords"
              | "author"
              | "date"
              | "og:title"
              | "og:description"
              | "twitter:title"
              | "twitter:description" => true,
              _ => self
                .options
                .plugins
                .as_ref()
                .and_then(|p| p.frontmatter.as_ref())
                .and_then(|f| f.meta_fields.as_ref())
                .is_some_and(|allowed| allowed.iter().any(|a| a == n_str)),
            };
            if is_allowed {
              self.capture_frontmatter_meta(n, c);
            }
          }
        }
      }
    }

    tag.excluded_from_markdown = in_template
      || filter_excluded
      || (skip_node && (!self.has_isolate_main || self.isolate_main_found));

    if tag.collapses_inner_white_space && !tag.excluded_from_markdown {
      if tag.tag_id == Some(TAG_SPAN) {
        self.collapse_span_depth = self.collapse_span_depth.saturating_add(1);
      } else {
        self.collapse_non_span_depth = self.collapse_non_span_depth.saturating_add(1);
      }
    }

    if let Some(last) = self.stack.last_mut() {
      last.current_walk_index += 1;
    }

    if !tag.is_inline {
      if !self.reserve_block_parent() {
        return OpeningTagResult {
          complete: true,
          new_position,
          self_closing,
          skip: true,
        };
      }
      let idx = self.stack.len();
      self.first_block_parent_index = Some(idx);
      self.block_parent_indices.push(idx);
    }

    if !self.push_tokenizer_context(tag_name, tag_id) || !self.reserve_stack_node(&tag) {
      return OpeningTagResult {
        complete: true,
        new_position,
        self_closing,
        skip: true,
      };
    }
    self.stack.push(tag);

    // Extraction
    let mut new_extractions = Vec::new();
    if !self.extraction_parsed_selectors.is_empty()
      && let Some(element) = self.stack.last()
    {
      let stack_depth = self.stack.len();
      for (selector, parsed) in &self.extraction_parsed_selectors {
        if self.extraction_tracked.len() + new_extractions.len() >= MAX_ACTIVE_EXTRACTIONS
          || self.extraction_tracked.len() + self.extraction_results.len() + new_extractions.len()
            >= MAX_EXTRACTION_RESULTS
        {
          break;
        }
        if matches_selector(element, parsed) {
          let attrs: Vec<(String, String)> = element
            .attributes
            .iter()
            .map(|(k, v)| (k.clone(), v.clone()))
            .collect();
          let tracked = TrackedExtraction {
            selector: selector.clone(),
            stack_depth,
            text_content: String::new(),
            tag_name: element.name().to_string(),
            attributes: attrs,
            limit_exceeded: false,
          };
          let pending = new_extractions
            .iter()
            .map(|item| tracked_extraction_bytes(item, true))
            .sum::<usize>();
          if self
            .extraction_retained_capacity()
            .saturating_add(pending)
            .saturating_add(tracked_extraction_bytes(&tracked, true))
            <= MAX_EXTRACTION_BYTES
          {
            new_extractions.push(tracked);
          }
        }
      }
    }
    for tracked in new_extractions {
      self.push_tracked_extraction(tracked);
    }

    // Inline emit (no callback!)
    if !skip_node {
      self.emit_enter_element();
    }

    // After the LI prefix is emitted, push this LI's marker-width worth of
    // spaces to list_indent so subsequent continuation content (code blocks,
    // paragraphs, nested blocks) lands in the correct content column. The
    // width depends on the marker: "- " = 2, "N. " = digits(N) + 2.
    // Push for every LI open so close_node can pop unconditionally; width 0
    // when skipped or in a table cell keeps the stack balanced without
    // affecting the indent string.
    if tag_id == Some(TAG_LI)
      && let Some(li) = self.stack.last()
    {
      let width: usize = if !skip_node && !self.in_table_cell() && !self.plain_text {
        let stack_len = self.stack.len();
        let parent_is_ordered = stack_len >= 2 && self.stack[stack_len - 2].tag_id == Some(TAG_OL);
        if parent_is_ordered {
          li.list_number.to_string().len() + 2
        } else {
          2
        }
      } else {
        0
      };
      if !self.reserve_list_indent_width() {
        return OpeningTagResult {
          complete: true,
          new_position,
          self_closing,
          skip: false,
        };
      }
      self
        .list_indent_widths
        .push(u8::try_from(width).unwrap_or(u8::MAX));
      if !self.reserve_list_indent(width) {
        return OpeningTagResult {
          complete: true,
          new_position,
          self_closing,
          skip: false,
        };
      }
      for _ in 0..width {
        self.list_indent.push(' ');
      }
    }

    self.has_encoded_html_entity = false;

    let svg_title = self.is_supported_svg_integration_point();
    if self.stack.last().is_some_and(|n| n.is_non_nesting) && !svg_title && !self_closing {
      self.in_non_nesting = true;
    }

    if !self_closing {
      self.just_closed_tag = false;
    }

    OpeningTagResult {
      complete: true,
      new_position,
      self_closing,
      skip: false,
    }
  }

  pub(crate) fn close_node(&mut self) {
    if self.stack.is_empty() {
      return;
    }

    // Extraction finalize
    if !self.extraction_tracked.is_empty() {
      let current_depth = self.stack.len();
      let mut i = 0;
      while i < self.extraction_tracked.len() {
        if self.extraction_tracked[i].stack_depth == current_depth {
          let tracked = self.extraction_tracked.swap_remove(i);
          if !tracked.limit_exceeded && self.extraction_results.len() < MAX_EXTRACTION_RESULTS {
            let mut text_content = tracked.text_content;
            let start = text_content.len() - text_content.trim_start().len();
            let end = text_content.trim_end().len();
            text_content.truncate(end);
            if start > 0 {
              text_content.drain(..start);
            }
            self.push_extraction_result(ExtractedElement {
              selector: tracked.selector,
              tag_name: tracked.tag_name,
              text_content,
              attributes: tracked.attributes,
            });
          }
        } else {
          i += 1;
        }
      }
    }

    let popping_index = self.stack.len() - 1;
    // Guard already checked above, but avoid panic on edge cases
    let Some(node) = self.stack.pop() else { return };
    self.pop_tokenizer_context();

    // Leaving the element that opened the current hidden subtree (filter).
    if self.hidden_since_depth == Some(node.depth) {
      self.hidden_since_depth = None;
    }

    if self.first_block_parent_index == Some(popping_index) {
      self.block_parent_indices.pop();
      self.first_block_parent_index = self
        .block_parent_indices
        .last()
        .copied()
        .or(if self.stack.is_empty() { None } else { Some(0) });
    }

    if node.collapses_inner_white_space && !node.excluded_from_markdown {
      if node.tag_id == Some(TAG_SPAN) {
        self.collapse_span_depth = self.collapse_span_depth.saturating_sub(1);
      } else {
        self.collapse_non_span_depth = self.collapse_non_span_depth.saturating_sub(1);
      }
    }

    if self.has_isolate_main
      && node.tag_id == Some(TAG_MAIN)
      && !node.excluded_from_markdown
      && self.isolate_main_found
      && !self.isolate_main_closed
    {
      self.isolate_main_closed = true;
    }

    // Frontmatter generation on HEAD close
    if self.has_frontmatter
      && node.tag_id == Some(TAG_HEAD)
      && !node.excluded_from_markdown
      && self.frontmatter_in_head
    {
      self.frontmatter_in_head = false;
      self.generate_frontmatter_yaml();
    }

    // Special: empty links — synthesize text from title/aria-label
    if node.tag_id == Some(TAG_A) && node.child_text_node_index == 0 && !node.excluded_from_markdown
    {
      let prefix = node
        .attributes
        .get("title")
        .or_else(|| node.attributes.get("aria-label"))
        .cloned()
        .unwrap_or_default();
      if !prefix.is_empty() {
        let node_depth = node.depth;
        let node_tag_id = node.tag_id;
        let mut modified_node = node;
        modified_node.child_text_node_index = 1;
        let text_depth = node_depth + 1;
        self.stack.push(modified_node);
        // Emit synthetic text
        self.emit_text(&prefix, false, text_depth, 0);
        for prev in &mut self.stack {
          prev.child_text_node_index += 1;
        }
        let Some(modified_node2) = self.stack.pop() else {
          return;
        };
        // Emit exit
        self.emit_exit_element(&modified_node2);
        self.recycle_node(modified_node2);
        if let Some(id) = node_tag_id {
          debug_assert!(
            (id as usize) < MAX_TAG_ID,
            "tag_id {id} exceeds MAX_TAG_ID {MAX_TAG_ID}"
          );
          if (id as usize) < MAX_TAG_ID {
            self.depth_map[id as usize] = self.depth_map[id as usize].saturating_sub(1);
          }
          self.update_in_pre_on_close(id);
          if id == TAG_LI
            && let Some(w) = self.list_indent_widths.pop()
          {
            let new_len = self.list_indent.len().saturating_sub(w as usize);
            self.list_indent.truncate(new_len);
            if new_len == 0 && self.streaming_limit.is_some() {
              self.list_indent = String::new();
            }
          }
        }
        self.depth -= 1;
        self.has_encoded_html_entity = false;
        self.just_closed_tag = true;
        self.in_non_nesting = self.stack.last().is_some_and(|n| n.is_non_nesting)
          && !self.is_supported_svg_integration_point();
        return;
      }
    }

    // Inline emit exit (no callback!)
    self.emit_exit_element(&node);

    let node_tag_id = node.tag_id;
    self.recycle_node(node);

    if let Some(id) = node_tag_id {
      debug_assert!(
        (id as usize) < MAX_TAG_ID,
        "tag_id {id} exceeds MAX_TAG_ID {MAX_TAG_ID}"
      );
      if (id as usize) < MAX_TAG_ID {
        self.depth_map[id as usize] = self.depth_map[id as usize].saturating_sub(1);
      }
      self.update_in_pre_on_close(id);
      if id == TAG_LI
        && let Some(w) = self.list_indent_widths.pop()
      {
        let new_len = self.list_indent.len().saturating_sub(w as usize);
        self.list_indent.truncate(new_len);
        if new_len == 0 && self.streaming_limit.is_some() {
          self.list_indent = String::new();
        }
      }
    }

    self.in_non_nesting = self.stack.last().is_some_and(|n| n.is_non_nesting)
      && !self.is_supported_svg_integration_point();
    self.depth -= 1;
    self.has_encoded_html_entity = false;
    self.just_closed_tag = true;
  }

  pub(crate) fn process_closing_tag(
    &mut self,
    html_chunk: &str,
    position: usize,
  ) -> CloseTagResult {
    let mut i = position + 2;
    let tag_name_start = i;
    let bytes = html_chunk.as_bytes();
    let chunk_length = bytes.len();

    let mut tag_name_end = chunk_length;
    let mut quote = 0;
    let mut found_close = false;
    while i < chunk_length {
      let c = bytes[i];
      if tag_name_end == chunk_length && (is_whitespace(c) || c == SLASH_CHAR || c == GT_CHAR) {
        tag_name_end = i;
      }

      // End-tag attributes are a parse error, but the tokenizer still consumes
      // them. In particular, a `>` inside a quoted value does not complete the
      // token, which matters when the malformed tag spans stream chunks.
      if quote != 0 {
        if c == quote {
          quote = 0;
        }
      } else if tag_name_end != chunk_length && (c == QUOTE_CHAR || c == APOS_CHAR) {
        quote = c;
      } else if c == GT_CHAR {
        found_close = true;
        break;
      }
      i += 1;
    }

    if !found_close {
      return CloseTagResult {
        complete: false,
        new_position: position,
      };
    }

    // Only the initial name participates in matching. Whitespace, a trailing
    // solidus, and parse-error attributes belong to the rest of the end tag.
    let tag_name_raw = &html_chunk[tag_name_start..tag_name_end];
    let builtin_tag_id = crate::consts::get_tag_id_ci_bytes(tag_name_raw.as_bytes());
    let tag_name: Cow<str> = if let Some(id) = builtin_tag_id {
      Cow::Borrowed(TAG_NAMES[id as usize])
    } else if tag_name_raw.bytes().any(|b| b.is_ascii_uppercase()) {
      Cow::Owned(tag_name_raw.to_ascii_lowercase())
    } else {
      Cow::Borrowed(tag_name_raw)
    };
    // Closing tag may target an aliased custom element (e.g. </ex> where
    // tagOverrides: { ex: 'em' } opened a TAG_EM node). Resolve the alias
    // here so the close matches the open.
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

    if builtin_tag_id == Some(TAG_BR) {
      let open =
        self.process_opening_tag("br", tag_id, Some(TAG_BR), alias_tag_id.is_some(), ">", 0);
      if open.complete && !open.skip {
        self.close_node();
      }
      self.just_closed_tag = true;
      return CloseTagResult {
        complete: true,
        new_position: i + 1,
      };
    }

    if let Some(curr) = self.stack.last()
      && curr.is_non_nesting
      && !self.is_supported_svg_integration_point()
      && curr.tag_id != tag_id
    {
      return CloseTagResult {
        complete: false,
        new_position: position,
      };
    }

    // Non-built-in names must match the open node's custom name as well as its
    // resolved tag id. This keeps `</other>` from closing an unknown custom
    // element and keeps aliased `</ex>` from closing an unrelated built-in.
    let close_name: &str = tag_name.as_ref();
    let matches = |node: &ElementNode| -> bool {
      if node.tag_id != tag_id {
        return false;
      }
      if let Some(custom_name) = node.custom_name.as_deref() {
        return custom_name == close_name;
      }
      builtin_tag_id.is_some()
    };

    let mut matched = false;
    if let Some(top) = self.stack.last() {
      if matches(top) {
        matched = true;
        self.close_node();
      } else {
        let mut pop_count = 0;
        let mut found_match = false;
        for j in (0..self.stack.len()).rev() {
          // Template contents have their own tree-construction scope. An end
          // tag inside them cannot close an element in the outer document.
          if tag_id != Some(TAG_TEMPLATE) && self.stack[j].tag_id == Some(TAG_TEMPLATE) {
            break;
          }
          pop_count += 1;
          if matches(&self.stack[j]) {
            found_match = true;
            break;
          }
        }
        if found_match {
          matched = true;
          for _ in 0..pop_count {
            self.close_node();
          }
        }
      }
    }

    if !matched {
      // The ignored token does not create a semantic boundary between the
      // text nodes on either side. Avoid the output layer's conservative
      // separator for adjacent, independently flushed text nodes.
      self.last_node_is_inline = true;
    }
    // Keep the scanner's whitespace state aligned after consuming a tag token.
    self.just_closed_tag = true;
    CloseTagResult {
      complete: true,
      new_position: i + 1,
    }
  }

  /// Handle a CDATA section's inner content.
  ///
  /// CDATA is discarded by default (matching the HTML spec, where `<![CDATA[`
  /// outside foreign content is a bogus comment). Callers opt in by registering
  /// a `#cdata-section` entry in `tagOverrides`; the leading `#` makes the
  /// pseudo-tag impossible to collide with a real HTML element name. When an
  /// override exists the content is emitted as a synthetic `#cdata-section`
  /// element whose rendering follows the override (alias tag and/or
  /// enter/exit strings).
  pub(crate) fn process_cdata_section(&mut self, content: &str) {
    if !self.has_tag_overrides {
      return;
    }
    let Some(tag_id) = self
      .options
      .plugins
      .as_ref()
      .and_then(|p| p.tag_overrides.as_ref())
      .and_then(|ovs| ovs.iter().find(|(k, _)| k == "#cdata-section"))
      .map(|(_, ov)| ov.alias_tag_id)
    else {
      return;
    };

    let result = self.process_opening_tag("#cdata-section", tag_id, None, tag_id.is_some(), ">", 0);
    if !result.complete {
      return;
    }
    if result.skip {
      if self
        .overflow
        .as_ref()
        .is_some_and(|overflow| overflow.root_name == "#cdata-section")
      {
        self.overflow = None;
      }
      return;
    }

    if !result.self_closing && !content.is_empty() {
      let excluded = self
        .stack
        .last()
        .is_some_and(|n| n.excluded_from_markdown || n.excludes_text_nodes);
      if !excluded {
        let depth = self.depth;
        let index = self.stack.last().map_or(0, |n| n.current_walk_index);
        self.emit_text(content, false, depth, index);
      }
      if let Some(parent) = self.stack.last_mut() {
        parent.current_walk_index += 1;
        parent.child_text_node_index += 1;
      }
    }
    self.close_node();
  }

  /// Recycle a node into the pool, preserving its Attributes Vec allocation.
  #[inline]
  pub(crate) fn recycle_node(&mut self, mut node: ElementNode) {
    node.attributes.clear();
    node.custom_name = None;
    node.tailwind = None;
    if self.streaming_limit.is_none() && self.node_pool.len() < MAX_NODE_POOL_SIZE {
      self.node_pool.push(node);
    }
  }

  #[inline]
  pub(crate) fn update_in_pre_on_close(&mut self, id: u8) {
    if id == TAG_PRE && self.depth_map[TAG_PRE as usize] == 0 {
      self.in_pre = false;
    }
  }
}
