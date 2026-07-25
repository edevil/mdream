pub mod consts;
pub(crate) mod convert;
pub(crate) mod entities;
pub(crate) mod scan;
pub(crate) mod selector;
pub mod splitter;
pub(crate) mod tags;
pub(crate) mod tailwind;
pub mod types;
pub(crate) mod url;

use convert::ConvertState;
use scan::is_whitespace;

#[derive(Clone, Copy, Default)]
enum CarryState {
  #[default]
  Prefix,
  TagName {
    closing: bool,
  },
  Attributes {
    state: u8,
    quote: u8,
  },
  EndTagTail {
    quote: u8,
  },
  Comment {
    state: u8,
  },
  Cdata {
    matched: u8,
  },
  BogusComment,
  NamedEntity,
  NumericEntity {
    hex: bool,
  },
  EntityLookahead {
    length: usize,
  },
  GfmText,
}

#[derive(Default)]
struct CarryScanner {
  state: CarryState,
  scanned: usize,
}

impl CarryScanner {
  const ATTR_BEFORE_NAME: u8 = 0;
  const ATTR_NAME: u8 = 1;
  const ATTR_AFTER_NAME: u8 = 2;
  const ATTR_BEFORE_VALUE: u8 = 3;
  const ATTR_QUOTED_VALUE: u8 = 4;
  const ATTR_AFTER_QUOTED: u8 = 5;
  const ATTR_UNQUOTED: u8 = 6;

  fn reset(&mut self, carry: &str) {
    self.state = CarryState::Prefix;
    self.scanned = 0;
    let _ = self.advance(carry);
  }

  fn advance(&mut self, carry: &str) -> bool {
    let bytes = carry.as_bytes();
    while self.scanned < bytes.len() {
      match self.state {
        CarryState::Prefix => {
          if bytes[0] == b'&' {
            if bytes.get(1) == Some(&b'#') {
              if bytes.len() == 2 {
                self.scanned = 2;
                return false;
              }
              let hex = matches!(bytes.get(2), Some(b'x' | b'X'));
              self.state = CarryState::NumericEntity { hex };
              self.scanned = 2 + usize::from(hex);
            } else {
              self.state = CarryState::NamedEntity;
              self.scanned = 1;
            }
            continue;
          }
          if bytes[0] != b'<' {
            self.state = CarryState::GfmText;
            continue;
          }
          if bytes.len() == 1 {
            self.scanned = 1;
            return false;
          }
          if bytes.starts_with(b"<!--") {
            self.state = CarryState::Comment { state: 0 };
            self.scanned = 4;
          } else if bytes.starts_with(b"<![CDATA[") {
            self.state = CarryState::Cdata { matched: 0 };
            self.scanned = 9;
          } else if b"<!--".starts_with(bytes) || b"<![CDATA[".starts_with(bytes) {
            self.scanned = bytes.len();
            return false;
          } else if bytes.starts_with(b"<!") || bytes.starts_with(b"<?") {
            self.state = CarryState::BogusComment;
            self.scanned = 2;
          } else if bytes.starts_with(b"</") {
            self.state = CarryState::TagName { closing: true };
            self.scanned = 2;
          } else {
            self.state = CarryState::TagName { closing: false };
            self.scanned = 1;
          }
        }
        CarryState::TagName { closing } => {
          let byte = bytes[self.scanned];
          self.scanned += 1;
          if byte == b'>' {
            return true;
          }
          if byte == b'/' || is_whitespace(byte) {
            self.state = if closing {
              CarryState::EndTagTail { quote: 0 }
            } else {
              CarryState::Attributes {
                state: Self::ATTR_BEFORE_NAME,
                quote: 0,
              }
            };
          }
        }
        CarryState::Attributes { state, mut quote } => {
          let byte = bytes[self.scanned];
          self.scanned += 1;
          if byte == b'>' && state != Self::ATTR_QUOTED_VALUE {
            return true;
          }
          let next = match state {
            Self::ATTR_BEFORE_NAME if !is_whitespace(byte) => Self::ATTR_NAME,
            Self::ATTR_NAME if is_whitespace(byte) => Self::ATTR_AFTER_NAME,
            Self::ATTR_NAME if byte == b'=' => Self::ATTR_BEFORE_VALUE,
            Self::ATTR_AFTER_NAME if byte == b'=' => Self::ATTR_BEFORE_VALUE,
            Self::ATTR_AFTER_NAME if !is_whitespace(byte) => Self::ATTR_NAME,
            Self::ATTR_BEFORE_VALUE if matches!(byte, b'\'' | b'"') => {
              quote = byte;
              Self::ATTR_QUOTED_VALUE
            }
            Self::ATTR_BEFORE_VALUE if !is_whitespace(byte) => Self::ATTR_UNQUOTED,
            Self::ATTR_QUOTED_VALUE if byte == quote => Self::ATTR_AFTER_QUOTED,
            Self::ATTR_AFTER_QUOTED if is_whitespace(byte) => Self::ATTR_BEFORE_NAME,
            Self::ATTR_AFTER_QUOTED => Self::ATTR_NAME,
            Self::ATTR_UNQUOTED if is_whitespace(byte) => Self::ATTR_BEFORE_NAME,
            _ => state,
          };
          self.state = CarryState::Attributes { state: next, quote };
        }
        CarryState::EndTagTail { mut quote } => {
          let byte = bytes[self.scanned];
          self.scanned += 1;
          if quote != 0 {
            if byte == quote {
              quote = 0;
            }
          } else if matches!(byte, b'\'' | b'"') {
            quote = byte;
          } else if byte == b'>' {
            return true;
          }
          self.state = CarryState::EndTagTail { quote };
        }
        CarryState::Comment { mut state } => {
          let byte = bytes[self.scanned];
          self.scanned += 1;
          if matches!(state, 0 | 1 | 4 | 5) && byte == b'>' {
            return true;
          }
          state = match state {
            0 if byte == b'-' => 1,
            0 => 2,
            1 if byte == b'-' => 4,
            1 => 2,
            2 if byte == b'-' => 3,
            2 => 2,
            3 if byte == b'-' => 4,
            3 => 2,
            4 if byte == b'!' => 5,
            4 if byte == b'-' => 4,
            4 => 2,
            5 if byte == b'-' => 3,
            5 => 2,
            _ => state,
          };
          self.state = CarryState::Comment { state };
        }
        CarryState::Cdata { mut matched } => {
          let byte = bytes[self.scanned];
          self.scanned += 1;
          matched = match (matched, byte) {
            (0, b']') => 1,
            (1, b']') => 2,
            (2, b'>') => return true,
            (2, b']') => 2,
            (_, b']') => 1,
            _ => 0,
          };
          self.state = CarryState::Cdata { matched };
        }
        CarryState::BogusComment => {
          let byte = bytes[self.scanned];
          self.scanned += 1;
          if byte == b'>' {
            return true;
          }
        }
        CarryState::NamedEntity => {
          let byte = bytes[self.scanned];
          self.scanned += 1;
          if byte == b';' {
            self.state = CarryState::EntityLookahead { length: 0 };
          } else if !byte.is_ascii_alphanumeric()
            || self.scanned - 1 > entities::max_entity_name_length()
          {
            return true;
          }
        }
        CarryState::NumericEntity { hex } => {
          let byte = bytes[self.scanned];
          self.scanned += 1;
          if byte == b';' {
            self.state = CarryState::EntityLookahead { length: 0 };
          } else if !(byte.is_ascii_digit() || (hex && byte.is_ascii_hexdigit())) {
            return true;
          }
        }
        CarryState::EntityLookahead { mut length } => {
          let byte = bytes[self.scanned];
          self.scanned += 1;
          if byte == b';' {
            return true;
          }
          if !(byte.is_ascii_alphanumeric() || length == 0 && byte == b'#') {
            return true;
          }
          length += 1;
          if length > entities::max_entity_name_length() + 2 {
            return true;
          }
          self.state = CarryState::EntityLookahead { length };
        }
        CarryState::GfmText => return Self::gfm_text_complete(bytes),
      }
    }
    false
  }

  fn gfm_text_complete(bytes: &[u8]) -> bool {
    if bytes.len() >= 64 || bytes.contains(&b'<') {
      return true;
    }
    let mut index = 0;
    while index < bytes.len() && bytes[index] == b' ' && index < 3 {
      index += 1;
    }
    let Some(&marker) = bytes.get(index) else {
      return false;
    };
    match marker {
      b'#' => {
        let start = index;
        while bytes.get(index) == Some(&b'#') {
          index += 1;
        }
        index < bytes.len() || index - start > 6
      }
      b'+' => index + 1 < bytes.len(),
      b'-' => {
        let markers = bytes[index..].iter().filter(|&&byte| byte == b'-').count();
        markers >= 3
          || bytes[index..]
            .iter()
            .any(|byte| !matches!(byte, b'-' | b' ' | b'\t'))
      }
      b'0'..=b'9' => {
        let start = index;
        while bytes.get(index).is_some_and(u8::is_ascii_digit) && index - start < 9 {
          index += 1;
        }
        if index == bytes.len() {
          false
        } else if index - start > 9 || !matches!(bytes[index], b'.' | b')') {
          true
        } else {
          index + 1 < bytes.len()
        }
      }
      _ => false,
    }
  }

  fn compact_numeric(&mut self, carry: &mut String) {
    let CarryState::NumericEntity { hex } = self.state else {
      return;
    };
    let digit_start = 2 + usize::from(hex);
    if carry.len() <= digit_start + 16 {
      return;
    }
    let radix = if hex { 16 } else { 10 };
    let mut value = 0u32;
    for byte in carry.as_bytes()[digit_start..].iter().copied() {
      let digit = if byte.is_ascii_digit() {
        u32::from(byte - b'0')
      } else {
        u32::from((byte | 32) - b'a' + 10)
      };
      value = value
        .saturating_mul(radix)
        .saturating_add(digit)
        .min(0x11_0000);
    }
    *carry = if hex {
      format!("&#x{value:X}")
    } else {
      format!("&#{value}")
    };
    self.reset(carry);
  }
}

#[cfg(test)]
mod carry_scanner_tests {
  use super::CarryScanner;

  #[test]
  fn unfinished_quoted_tag_scans_only_appended_bytes() {
    let mut carry = String::from("<a title=\"");
    let mut scanner = CarryScanner::default();
    scanner.reset(&carry);

    for _ in 0..32 * 1024 {
      carry.push('x');
      assert!(!scanner.advance(&carry));
      assert_eq!(scanner.scanned, carry.len());
    }

    carry.push_str("\">text");
    assert!(scanner.advance(&carry));
  }

  #[test]
  fn oversized_numeric_entity_carry_is_compacted() {
    let mut carry = String::from("&#");
    let mut scanner = CarryScanner::default();
    scanner.reset(&carry);
    for _ in 0..1024 {
      carry.push('9');
      assert!(!scanner.advance(&carry));
      scanner.compact_numeric(&mut carry);
      assert!(carry.len() <= 20);
    }
    carry.push(';');
    assert!(!scanner.advance(&carry));
    carry.push(' ');
    assert!(scanner.advance(&carry));
  }
}

// Re-export the public option/config types at the crate root so `use mdream::*`
// pulls in everything needed to call `html_to_markdown` without reaching into
// the `types` module.
pub use types::{
  CleanConfig, ConfigurationError, ConversionMode, ExtractionConfig, FilterConfig,
  FrontmatterConfig, HTMLToMarkdownOptions, IsolateMainConfig, MdreamResult, OutputFormat,
  PluginConfig, StreamingError, StreamingLimits, TagOverrideConfig, TailwindConfig,
  UnsupportedStreamingOption, UrlPolicy,
};

// Re-export `get_tag_id` so callers can resolve tag names to IDs (for
// `TagOverrideConfig::alias_tag_id`) without reaching into `consts` directly.
pub use consts::get_tag_id;

fn tag_override_requires_pair(tag: &str) -> bool {
  matches!(
    consts::get_tag_id_ci_bytes(tag.as_bytes()),
    Some(consts::TAG_A | consts::TAG_BLOCKQUOTE | consts::TAG_CODE | consts::TAG_PRE)
  )
}

/// Validate converter options for one-shot or strict streaming use.
pub fn validate_options(
  options: &HTMLToMarkdownOptions,
  mode: ConversionMode,
) -> Result<(), ConfigurationError> {
  if mode == ConversionMode::Streaming {
    if options.clean.as_ref().is_some_and(|clean| clean.fragments) {
      return Err(ConfigurationError::UnsupportedStreamingOption(
        UnsupportedStreamingOption::CleanFragments,
      ));
    }
    if let Some(plugins) = &options.plugins {
      for (enabled, option) in [
        (
          plugins.isolate_main.is_some(),
          UnsupportedStreamingOption::IsolateMain,
        ),
        (
          plugins.frontmatter.is_some(),
          UnsupportedStreamingOption::Frontmatter,
        ),
        (
          plugins.extraction.is_some(),
          UnsupportedStreamingOption::Extraction,
        ),
      ] {
        if enabled {
          return Err(ConfigurationError::UnsupportedStreamingOption(option));
        }
      }
    }
  }

  if let Some(overrides) = options
    .plugins
    .as_ref()
    .and_then(|plugins| plugins.tag_overrides.as_ref())
  {
    for (tag, config) in overrides {
      if let Some(alias_tag_id) = config.alias_tag_id {
        if alias_tag_id as usize >= consts::MAX_TAG_ID {
          return Err(ConfigurationError::TagAliasOutOfRange {
            tag: tag.clone(),
            alias_tag_id,
          });
        }
        if config.enter.is_some()
          || config.exit.is_some()
          || config.spacing.is_some()
          || config.is_inline.is_some()
          || config.is_self_closing.is_some()
          || config.collapses_inner_white_space.is_some()
        {
          return Err(ConfigurationError::TagAliasWithOverrides { tag: tag.clone() });
        }
      }
      if tag_override_requires_pair(tag) && config.enter.is_some() != config.exit.is_some() {
        return Err(ConfigurationError::UnpairedTagOverride { tag: tag.clone() });
      }
    }
  }
  Ok(())
}

fn normalize_options(mut options: HTMLToMarkdownOptions) -> HTMLToMarkdownOptions {
  if let Some(overrides) = options
    .plugins
    .as_mut()
    .and_then(|plugins| plugins.tag_overrides.as_mut())
  {
    for (tag, _) in overrides {
      tag.make_ascii_lowercase();
    }
  }
  options
}

fn safe_options(mut options: HTMLToMarkdownOptions) -> HTMLToMarkdownOptions {
  options = normalize_options(options);
  if let Some(overrides) = options
    .plugins
    .as_mut()
    .and_then(|plugins| plugins.tag_overrides.as_mut())
  {
    for (tag, config) in overrides {
      if config
        .alias_tag_id
        .is_some_and(|alias_tag_id| alias_tag_id as usize >= consts::MAX_TAG_ID)
        || (config.alias_tag_id.is_some()
          && (config.enter.is_some()
            || config.exit.is_some()
            || config.spacing.is_some()
            || config.is_inline.is_some()
            || config.is_self_closing.is_some()
            || config.collapses_inner_white_space.is_some()))
      {
        config.alias_tag_id = None;
      }
      if tag_override_requires_pair(tag) && config.enter.is_some() != config.exit.is_some() {
        if config.enter.is_none() {
          config.enter = Some(String::new());
        }
        if config.exit.is_none() {
          config.exit = Some(String::new());
        }
      }
    }
  }
  options
}

fn requires_deferred_streaming(options: &HTMLToMarkdownOptions) -> bool {
  options.clean.as_ref().is_some_and(|clean| clean.fragments)
    || options
      .plugins
      .as_ref()
      .is_some_and(|plugins| plugins.isolate_main.is_some())
}

/// Convert HTML to Markdown in a single pass.
pub fn html_to_markdown(html: &str, options: HTMLToMarkdownOptions) -> String {
  html_to_format(html, safe_options(options), OutputFormat::Markdown)
}

/// Convert HTML to Markdown after validating the complete configuration.
pub fn try_html_to_markdown(
  html: &str,
  options: HTMLToMarkdownOptions,
) -> Result<String, ConfigurationError> {
  try_html_to_format(html, options, OutputFormat::Markdown)
}

/// Convert HTML to readable plain text in a single pass.
pub fn html_to_text(html: &str, options: HTMLToMarkdownOptions) -> String {
  html_to_format(html, safe_options(options), OutputFormat::Text)
}

/// Convert HTML to text after validating the complete configuration.
pub fn try_html_to_text(
  html: &str,
  options: HTMLToMarkdownOptions,
) -> Result<String, ConfigurationError> {
  try_html_to_format(html, options, OutputFormat::Text)
}

/// Convert HTML to the requested output format in a single pass.
pub fn html_to_format(html: &str, options: HTMLToMarkdownOptions, format: OutputFormat) -> String {
  html_to_format_result(html, options, format).markdown
}

/// Convert HTML to the requested format after validating the configuration.
pub fn try_html_to_format(
  html: &str,
  options: HTMLToMarkdownOptions,
  format: OutputFormat,
) -> Result<String, ConfigurationError> {
  Ok(try_html_to_format_result(html, options, format)?.markdown)
}

/// Convert HTML to Markdown with full results (extraction, frontmatter).
pub fn html_to_markdown_result(html: &str, options: HTMLToMarkdownOptions) -> MdreamResult {
  html_to_format_result(html, safe_options(options), OutputFormat::Markdown)
}

/// Convert HTML to Markdown with side data after validating the configuration.
pub fn try_html_to_markdown_result(
  html: &str,
  options: HTMLToMarkdownOptions,
) -> Result<MdreamResult, ConfigurationError> {
  try_html_to_format_result(html, options, OutputFormat::Markdown)
}

/// Convert HTML to plain text with full results (extraction, frontmatter).
pub fn html_to_text_result(html: &str, options: HTMLToMarkdownOptions) -> MdreamResult {
  html_to_format_result(html, safe_options(options), OutputFormat::Text)
}

/// Convert HTML to text with side data after validating the configuration.
pub fn try_html_to_text_result(
  html: &str,
  options: HTMLToMarkdownOptions,
) -> Result<MdreamResult, ConfigurationError> {
  try_html_to_format_result(html, options, OutputFormat::Text)
}

/// Convert HTML with side data after validating the configuration.
pub fn try_html_to_format_result(
  html: &str,
  options: HTMLToMarkdownOptions,
  format: OutputFormat,
) -> Result<MdreamResult, ConfigurationError> {
  validate_options(&options, ConversionMode::OneShot)?;
  Ok(html_to_format_result(
    html,
    normalize_options(options),
    format,
  ))
}

/// Convert HTML to the requested format with full results (extraction, frontmatter).
pub fn html_to_format_result(
  html: &str,
  options: HTMLToMarkdownOptions,
  format: OutputFormat,
) -> MdreamResult {
  let options = safe_options(options);
  let capacity = (html.len() / 3).clamp(1024, 256 * 1024);
  let mut state = ConvertState::new(options, capacity, format);
  let leftover = state.process_html(html);
  state.finalize(&leftover);

  let extracted = if state.has_extraction {
    let results = std::mem::take(&mut state.extraction_results);
    if results.is_empty() {
      None
    } else {
      Some(results)
    }
  } else {
    None
  };

  let frontmatter = state.frontmatter();

  MdreamResult {
    markdown: state.get_markdown(),
    extracted,
    frontmatter,
  }
}

/// Streaming HTML-to-Markdown converter.
///
/// Feed chunks of HTML via `process_chunk()`, then call `finish()` for remaining output.
pub struct MarkdownStreamProcessor {
  state: ConvertState,
  buffer: String,
  carry_scanner: CarryScanner,
  defer_output: bool,
}

impl MarkdownStreamProcessor {
  pub fn new(options: HTMLToMarkdownOptions) -> Self {
    Self::new_with_format(options, OutputFormat::Markdown)
  }

  /// Create a strict streaming converter after validating all options.
  pub fn try_new(options: HTMLToMarkdownOptions) -> Result<Self, ConfigurationError> {
    Self::try_new_with_format(options, OutputFormat::Markdown)
  }

  /// Create a streaming converter for the requested output format.
  pub fn new_with_format(options: HTMLToMarkdownOptions, format: OutputFormat) -> Self {
    let defer_output = requires_deferred_streaming(&options);
    let options = safe_options(options);
    let mut state = ConvertState::new(options, 4096, format);
    state.enable_incremental_lexing();
    Self {
      state,
      buffer: String::new(),
      carry_scanner: CarryScanner::default(),
      defer_output,
    }
  }

  /// Create a strict streaming converter for the requested output format.
  pub fn try_new_with_format(
    options: HTMLToMarkdownOptions,
    format: OutputFormat,
  ) -> Result<Self, ConfigurationError> {
    validate_options(&options, ConversionMode::Streaming)?;
    let mut state = ConvertState::new(normalize_options(options), 4096, format);
    state.enable_incremental_lexing();
    Ok(Self {
      state,
      buffer: String::new(),
      carry_scanner: CarryScanner::default(),
      defer_output: false,
    })
  }

  /// Like `new`, but with draining disabled (drain-transparency test only).
  #[cfg(test)]
  pub(crate) fn new_drain_disabled(options: HTMLToMarkdownOptions) -> Self {
    let mut me = Self::new(options);
    me.state.disable_drain = true;
    me
  }

  pub fn process_chunk(&mut self, chunk: &str) -> String {
    if self.buffer.is_empty() {
      self.buffer = self.state.process_html(chunk);
      self.carry_scanner.reset(&self.buffer);
    } else {
      self.buffer.push_str(chunk);
      if self.carry_scanner.advance(&self.buffer) {
        let full = std::mem::take(&mut self.buffer);
        self.buffer = self.state.process_html(&full);
        self.carry_scanner.reset(&self.buffer);
      } else {
        self.carry_scanner.compact_numeric(&mut self.buffer);
      }
    }
    if self.defer_output {
      String::new()
    } else {
      self.state.get_markdown_chunk()
    }
  }

  pub fn finish(&mut self) -> String {
    let leftover = if self.buffer.is_empty() {
      String::new()
    } else {
      let chunk = std::mem::take(&mut self.buffer);
      self.state.process_html(&chunk)
    };
    self.state.finalize(&leftover);
    if self.defer_output {
      self.state.get_markdown()
    } else {
      self.state.get_markdown_chunk()
    }
  }
}

/// Streaming converter with an enforceable retained dynamic-buffer ceiling.
///
/// Returned chunks are caller-owned and are not charged to the ceiling. If a
/// later call fails, chunks returned by earlier calls cannot be rolled back.
/// Parser stacks, attributes, tables, node pools, and mutable output windows
/// are charged. Configuration, returned output, and transient allocations are
/// outside the ceiling. Frontmatter and extraction remain unsupported because
/// this API has no result channel for their structured side data.
pub struct BoundedMarkdownStreamProcessor {
  state: ConvertState,
  buffer: String,
  carry_scanner: CarryScanner,
  terminal_error: Option<StreamingError>,
}

impl BoundedMarkdownStreamProcessor {
  pub fn new(
    options: HTMLToMarkdownOptions,
    limits: StreamingLimits,
  ) -> Result<Self, StreamingError> {
    Self::new_with_format(options, OutputFormat::Markdown, limits)
  }

  /// Create a bounded streaming converter for the requested output format.
  pub fn new_with_format(
    options: HTMLToMarkdownOptions,
    format: OutputFormat,
    limits: StreamingLimits,
  ) -> Result<Self, StreamingError> {
    if let Err(error) = validate_options(&options, ConversionMode::Streaming) {
      return Err(match error {
        ConfigurationError::UnsupportedStreamingOption(option) => {
          StreamingError::UnsupportedOption(option)
        }
        _ => StreamingError::InvalidConfiguration,
      });
    }

    let mut state = ConvertState::new_bounded(
      normalize_options(options),
      format,
      limits.max_buffered_bytes,
    );
    state.enable_incremental_lexing();
    Ok(Self {
      state,
      buffer: String::new(),
      carry_scanner: CarryScanner::default(),
      terminal_error: None,
    })
  }

  fn fail(&mut self, error: StreamingError) -> StreamingError {
    let error = *self.terminal_error.get_or_insert(error);
    self.buffer = String::new();
    self.state.release_retained_buffers();
    error
  }

  fn check_error(&mut self) -> Result<(), StreamingError> {
    if let Some(error) = self.terminal_error.or_else(|| self.state.streaming_error()) {
      return Err(self.fail(error));
    }
    Ok(())
  }

  fn retain_carry(&mut self, carry: &str) -> Result<(), StreamingError> {
    if carry.is_empty() {
      self.buffer = String::new();
      return Ok(());
    }
    if let Err(error) = self.state.reserve_external(&mut self.buffer, carry.len()) {
      return Err(self.fail(error));
    }
    self.buffer.push_str(carry);
    Ok(())
  }

  pub fn process_chunk(&mut self, chunk: &str) -> Result<String, StreamingError> {
    self.check_error()?;

    let carry = if self.buffer.is_empty() {
      self.state.process_html(chunk)
    } else {
      let required = self
        .buffer
        .len()
        .checked_add(chunk.len())
        .ok_or_else(|| self.fail(StreamingError::CapacityOverflow))?;
      if let Err(error) = self.state.reserve_external(&mut self.buffer, required) {
        return Err(self.fail(error));
      }
      self.buffer.push_str(chunk);
      if !self.carry_scanner.advance(&self.buffer) {
        self.carry_scanner.compact_numeric(&mut self.buffer);
        self.check_error()?;
        let output = self.state.get_markdown_chunk();
        self.state.release_closed_high_water();
        return Ok(output);
      }
      let full = std::mem::take(&mut self.buffer);
      self.state.process_html(&full)
    };

    self.check_error()?;
    self.retain_carry(&carry)?;
    self.carry_scanner.reset(&self.buffer);
    let output = self.state.get_markdown_chunk();
    self.check_error()?;
    self.state.release_closed_high_water();
    self.check_error()?;
    Ok(output)
  }

  pub fn finish(&mut self) -> Result<String, StreamingError> {
    self.check_error()?;
    let leftover = if self.buffer.is_empty() {
      String::new()
    } else {
      let chunk = std::mem::take(&mut self.buffer);
      self.state.process_html(&chunk)
    };
    self.check_error()?;
    self.state.finalize(&leftover);
    self.check_error()?;
    let output = self.state.get_markdown_chunk();
    self.check_error()?;
    self.state.release_closed_high_water();
    self.check_error()?;
    Ok(output)
  }

  /// Bytes currently retained by buffers covered by this API.
  pub fn buffered_bytes(&self) -> usize {
    self.buffer.len() + self.state.retained_buffered_bytes()
  }

  /// Allocated capacity currently charged to the retained-buffer ceiling.
  pub fn buffered_capacity(&self) -> usize {
    self.buffer.capacity() + self.state.retained_buffer_capacity()
  }
}

#[cfg(test)]
mod drain_equiv {
  //! Draining must be byte-transparent: same streamed output with it on or off,
  //! for any input at any chunk size. The corpus includes the rewrite-after-yield
  //! constructs (autolink text==url, self-link headings, redundant `[url](url)`)
  //! that diverge from one-shot but must stay drain-invariant.

  use super::MarkdownStreamProcessor;
  use super::types::{
    CleanConfig, ExtractionConfig, FrontmatterConfig, HTMLToMarkdownOptions, PluginConfig,
  };

  const CORPUS: &[&str] = &[
    // Breadth: chunk-invariant cases.
    "<h1>Title</h1><p>Para one.</p><p>Para <strong>two</strong>.</p>",
    "<ul><li>a</li><li>b<ul><li>b1</li><li>b2</li></ul></li></ul>",
    r#"<p>See <a href="https://example.com">Example</a> and <a href="https://x.io">the X site</a>.</p>"#,
    r#"<p>See <a href="https://example.com" title="Example site">Example</a>.</p>"#,
    "<blockquote><p>quote</p><blockquote><p>nested</p></blockquote></blockquote><p>after</p>",
    "<pre><code>let x = 1;\nlet y = 2;</code></pre><p>done</p>",
    "<table><tr><th>A</th><th>B</th></tr><tr><td>1</td><td>2</td></tr></table>",
    r#"<h2>Section</h2><p>text with a <a href="/rel">relative</a> link</p>"#,
    // Rewrite-after-yield constructs.
    r#"<p>Visit <a href="https://x.io">https://x.io</a> today.</p>"#,
    r##"<h2><a href="#section">Section</a></h2><p>body</p>"##,
    r#"<p>link <a href="https://example.com">https://example.com</a> end</p>"#,
  ];

  fn stream(html: &str, chunk: usize, opts: HTMLToMarkdownOptions, disable_drain: bool) -> String {
    let mut p = if disable_drain {
      MarkdownStreamProcessor::new_drain_disabled(opts)
    } else {
      MarkdownStreamProcessor::new(opts)
    };
    let mut out = String::new();
    for c in html.as_bytes().chunks(chunk) {
      out.push_str(&p.process_chunk(std::str::from_utf8(c).unwrap()));
    }
    out.push_str(&p.finish());
    out
  }

  fn safe_clean() -> CleanConfig {
    // Everything except `fragments`, which needs the whole buffer (drain gated off).
    CleanConfig {
      urls: true,
      fragments: false,
      empty_links: true,
      blank_lines: true,
      redundant_links: true,
      self_link_headings: true,
      empty_images: true,
      empty_link_text: true,
    }
  }

  #[test]
  fn drain_is_byte_transparent() {
    for &html in CORPUS {
      for opts in [
        HTMLToMarkdownOptions::default(),
        HTMLToMarkdownOptions::default().with_wrap_width(12),
        HTMLToMarkdownOptions {
          clean: Some(safe_clean()),
          ..Default::default()
        },
      ] {
        for chunk in [1usize, 3, 7, 64, html.len().max(1)] {
          let drained = stream(html, chunk, opts.clone(), false);
          let undrained = stream(html, chunk, opts.clone(), true);
          assert_eq!(
            drained, undrained,
            "drain changed output: chunk={chunk} html={html:?}"
          );
        }
      }
    }
  }

  #[test]
  fn closing_a_skipped_link_releases_the_yielded_prefix() {
    let options = HTMLToMarkdownOptions {
      clean: Some(safe_clean()),
      ..Default::default()
    };
    let mut processor = MarkdownStreamProcessor::new(options);

    for _ in 0..10_000 {
      let _ = processor.process_chunk(r##"<a href="#">x<span></span>"##);
      let _ = processor.process_chunk("</a>");
    }

    assert!(
      processor.state.buffer.len() < 1024,
      "yielded skipped links accumulated {} buffered bytes",
      processor.state.buffer.len()
    );
  }

  #[test]
  fn completed_links_do_not_retain_prior_output() {
    fn assert_released(html: &str, iterations: usize) {
      let mut processor = MarkdownStreamProcessor::new(HTMLToMarkdownOptions::default());

      for _ in 0..iterations {
        drop(processor.process_chunk(html));
      }

      assert!(
        processor.state.buffer.len() < 1024,
        "completed links accumulated {} buffered bytes",
        processor.state.buffer.len()
      );
      drop(processor.finish());
    }

    assert_released(r#"<a href="/x">x</a>"#, 350_000);
    assert_released(
      r#"<a href="https://example.com">https://example.com</a>"#,
      512,
    );
  }

  #[test]
  fn plugin_side_data_does_not_disable_output_draining() {
    for plugins in [
      PluginConfig {
        frontmatter: Some(FrontmatterConfig::default()),
        ..Default::default()
      },
      PluginConfig {
        extraction: Some(ExtractionConfig::new(&["p"])),
        ..Default::default()
      },
    ] {
      let mut processor = MarkdownStreamProcessor::new(HTMLToMarkdownOptions {
        plugins: Some(plugins),
        ..Default::default()
      });
      drop(processor.process_chunk(
        r#"<head><title>Title</title><meta name="description" content="Summary"></head>"#,
      ));
      for _ in 0..10_000 {
        drop(processor.process_chunk("<p>body</p>"));
        assert!(
          processor.state.buffer.len() < 1024,
          "plugin retained {} bytes of drained body output",
          processor.state.buffer.len()
        );
      }
      drop(processor.finish());
    }
  }
}
