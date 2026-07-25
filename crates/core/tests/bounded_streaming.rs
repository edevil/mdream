use mdream::{
  BoundedMarkdownStreamProcessor, CleanConfig, ExtractionConfig, FrontmatterConfig,
  HTMLToMarkdownOptions, MarkdownStreamProcessor, OutputFormat, PluginConfig, StreamingError,
  StreamingLimits, UnsupportedStreamingOption,
};

fn limits(max_buffered_bytes: usize) -> StreamingLimits {
  StreamingLimits { max_buffered_bytes }
}

fn run_bounded(
  chunks: &[&str],
  max_buffered_bytes: usize,
) -> Result<(String, usize), StreamingError> {
  let mut processor = BoundedMarkdownStreamProcessor::new(
    HTMLToMarkdownOptions::default(),
    limits(max_buffered_bytes),
  )?;
  let mut output = String::new();
  let mut peak = processor.buffered_capacity();
  for chunk in chunks {
    output.push_str(&processor.process_chunk(chunk)?);
    peak = peak.max(processor.buffered_capacity());
    assert!(processor.buffered_capacity() <= max_buffered_bytes);
  }
  output.push_str(&processor.finish()?);
  peak = peak.max(processor.buffered_capacity());
  assert!(processor.buffered_capacity() <= max_buffered_bytes);
  Ok((output, peak))
}

fn run_legacy(chunks: &[&str]) -> String {
  let mut processor = MarkdownStreamProcessor::new(HTMLToMarkdownOptions::default());
  let mut output = String::new();
  for chunk in chunks {
    output.push_str(&processor.process_chunk(chunk));
  }
  output.push_str(&processor.finish());
  output
}

fn assert_exact_boundary(chunks: &[&str]) {
  let expected = run_legacy(chunks);
  let exact = (0..=16 * 1024)
    .find(|&limit| run_bounded(chunks, limit).is_ok())
    .expect("test case should fit within search range");

  if exact > 0 {
    assert_eq!(
      run_bounded(chunks, exact - 1).unwrap_err(),
      StreamingError::BufferLimitExceeded,
      "one byte below the allocator capacity boundary must fail"
    );
  }
  let (actual, peak) = run_bounded(chunks, exact).unwrap();
  assert_eq!(actual, expected);
  assert!(peak <= exact);
  assert_eq!(run_bounded(chunks, exact + 1).unwrap().0, expected);
}

#[test]
fn zero_tiny_and_exact_limits() {
  let mut empty =
    BoundedMarkdownStreamProcessor::new(HTMLToMarkdownOptions::default(), limits(0)).unwrap();
  assert_eq!(empty.process_chunk("").unwrap(), "");
  assert_eq!(empty.finish().unwrap(), "");
  assert_eq!(empty.buffered_capacity(), 0);

  let mut zero =
    BoundedMarkdownStreamProcessor::new(HTMLToMarkdownOptions::default(), limits(0)).unwrap();
  assert_eq!(
    zero.process_chunk("x").unwrap_err(),
    StreamingError::BufferLimitExceeded
  );

  assert_exact_boundary(&["x"]);
  assert_exact_boundary(&["<", "p>x</p>"]);
}

#[test]
fn open_constructs_obey_capacity_boundaries() {
  for chunks in [
    &["<p>before <code>", "a ` b", "</code> after</p>"][..],
    &["<pre><code>", "line\n```wide", "</code></pre>"][..],
    &["<blockquote>", "<p>one</p><p>two</p>", "</blockquote>"][..],
    &["<p><a href=\"/destination\">", "link text", "</a></p>"][..],
    &["<xmp>", "&amp;<b>x</b>", "</xmp>"][..],
    &["<plaintext>", "&amp;</plaintext><p>x</p>"][..],
  ] {
    assert_exact_boundary(chunks);
  }
}

#[test]
fn terminal_error_is_sticky_and_emits_nothing_later() {
  let mut processor =
    BoundedMarkdownStreamProcessor::new(HTMLToMarkdownOptions::default(), limits(512)).unwrap();
  let prior = processor.process_chunk("<p>ok</p>").unwrap();
  assert_eq!(prior, "ok");

  let first = processor
    .process_chunk(&format!("<a href=\"/x\">{}", "x".repeat(1024)))
    .unwrap_err();
  assert_eq!(first, StreamingError::BufferLimitExceeded);
  assert_eq!(processor.buffered_capacity(), 0);
  assert_eq!(processor.buffered_bytes(), 0);
  assert_eq!(processor.process_chunk("</a>").unwrap_err(), first);
  assert_eq!(processor.finish().unwrap_err(), first);
  assert_eq!(processor.buffered_capacity(), 0);
}

#[test]
fn unsupported_whole_document_options_fail_at_construction() {
  let fragments = HTMLToMarkdownOptions {
    clean: Some(CleanConfig {
      fragments: true,
      ..Default::default()
    }),
    ..Default::default()
  };
  assert!(matches!(
    BoundedMarkdownStreamProcessor::new(fragments, limits(1024)),
    Err(StreamingError::UnsupportedOption(
      UnsupportedStreamingOption::CleanFragments
    ))
  ));

  for (plugins, expected) in [
    (
      PluginConfig {
        frontmatter: Some(FrontmatterConfig::default()),
        ..Default::default()
      },
      UnsupportedStreamingOption::Frontmatter,
    ),
    (
      PluginConfig {
        extraction: Some(ExtractionConfig::new(&["h1"])),
        ..Default::default()
      },
      UnsupportedStreamingOption::Extraction,
    ),
  ] {
    let options = HTMLToMarkdownOptions {
      plugins: Some(plugins),
      ..Default::default()
    };
    assert!(matches!(
      BoundedMarkdownStreamProcessor::new(options, limits(1024)),
      Err(StreamingError::UnsupportedOption(option)) if option == expected
    ));
  }
}

#[test]
fn closed_construct_releases_high_water_capacity() {
  let mut processor =
    BoundedMarkdownStreamProcessor::new(HTMLToMarkdownOptions::default(), limits(64 * 1024))
      .unwrap();
  assert_eq!(processor.process_chunk("<a href=\"/x\">").unwrap(), "");
  assert_eq!(processor.process_chunk(&"x".repeat(8192)).unwrap(), "");
  assert_eq!(processor.process_chunk("<span></span>").unwrap(), "");
  let high_water = processor.buffered_capacity();
  assert!(high_water >= 8192);

  assert!(!processor.process_chunk("</a>").unwrap().is_empty());
  assert!(
    processor.buffered_capacity() < 256,
    "closed link retained {} bytes after a {high_water}-byte high-water mark",
    processor.buffered_capacity()
  );
}

#[test]
fn active_rewrite_window_does_not_pin_prior_output() {
  const LIMIT: usize = 1024;
  let mut processor =
    BoundedMarkdownStreamProcessor::new(HTMLToMarkdownOptions::default(), limits(LIMIT)).unwrap();

  for _ in 0..256 {
    drop(processor.process_chunk("<p>completed prefix</p>").unwrap());
    assert!(processor.buffered_capacity() <= LIMIT);
  }

  drop(processor.process_chunk("<a href=\"/x\">").unwrap());
  assert_eq!(processor.process_chunk(&"x".repeat(256)).unwrap(), "");
  assert!(
    processor.buffered_bytes() < 512,
    "open link pinned {} bytes of completed output",
    processor.buffered_bytes()
  );

  let closed = processor.process_chunk("</a>").unwrap();
  assert!(closed.ends_with(&format!("[{}](/x)", "x".repeat(256))));
  assert!(!closed.contains("completed prefix"));
  assert!(processor.buffered_capacity() < 512);
}

#[test]
fn closed_script_releases_parser_scratch_capacity() {
  let mut processor =
    BoundedMarkdownStreamProcessor::new(HTMLToMarkdownOptions::default(), limits(16 * 1024))
      .unwrap();
  assert_eq!(processor.process_chunk("<script>").unwrap(), "");
  assert_eq!(processor.process_chunk(&"x".repeat(8192)).unwrap(), "");
  assert_eq!(processor.process_chunk("</script>").unwrap(), "");
  assert!(
    processor.buffered_capacity() < 256,
    "closed script retained {} bytes of scratch capacity",
    processor.buffered_capacity()
  );
  assert_eq!(processor.process_chunk("<p>ok</p>").unwrap(), "ok");
  assert_eq!(processor.finish().unwrap(), "");
}

#[test]
fn one_byte_text_streams_through_a_bounded_window() {
  const INPUT_LEN: usize = 1024 * 1024;
  const LIMIT: usize = 512;
  let mut processor =
    BoundedMarkdownStreamProcessor::new(HTMLToMarkdownOptions::default(), limits(LIMIT)).unwrap();
  let mut output_len = 0usize;

  for _ in 0..INPUT_LEN {
    output_len += processor.process_chunk("x").unwrap().len();
    assert!(processor.buffered_capacity() <= LIMIT);
  }
  output_len += processor.finish().unwrap().len();

  assert_eq!(output_len, INPUT_LEN);
  assert!(processor.buffered_capacity() <= LIMIT);
}

#[test]
fn excluded_raw_text_does_not_retain_its_body() {
  const LIMIT: usize = 512;
  for tag in ["script", "style"] {
    let mut processor =
      BoundedMarkdownStreamProcessor::new(HTMLToMarkdownOptions::default(), limits(LIMIT)).unwrap();
    assert_eq!(processor.process_chunk(&format!("<{tag}>")).unwrap(), "");
    for _ in 0..256 {
      assert_eq!(processor.process_chunk(&"x".repeat(8192)).unwrap(), "");
      assert!(
        processor.buffered_capacity() <= LIMIT,
        "{tag} retained {} bytes",
        processor.buffered_capacity()
      );
    }
    assert_eq!(
      processor
        .process_chunk(&format!("</{tag}><p>ok</p>"))
        .unwrap(),
      "ok"
    );
    assert_eq!(processor.finish().unwrap(), "");
  }
}

#[test]
fn claimed_retained_state_is_observable() {
  let mut carry =
    BoundedMarkdownStreamProcessor::new(HTMLToMarkdownOptions::default(), limits(4096)).unwrap();
  assert_eq!(carry.process_chunk("<tag").unwrap(), "");
  assert!(carry.buffered_bytes() >= 4);
  assert!(carry.buffered_capacity() >= carry.buffered_bytes());

  let mut code =
    BoundedMarkdownStreamProcessor::new(HTMLToMarkdownOptions::default(), limits(4096)).unwrap();
  assert_eq!(code.process_chunk("<code>literal").unwrap(), "");
  assert!(
    code.buffered_bytes() >= "literal".len() + 1 + 2 * std::mem::size_of::<usize>(),
    "an open code span must charge its output and rewrite anchor"
  );
  assert!(code.buffered_capacity() >= code.buffered_bytes());

  let mut quote =
    BoundedMarkdownStreamProcessor::new(HTMLToMarkdownOptions::default(), limits(4096)).unwrap();
  assert_eq!(quote.process_chunk("<blockquote>").unwrap(), "");
  assert!(
    quote.buffered_bytes() >= std::mem::size_of::<String>() + std::mem::size_of::<usize>(),
    "an open blockquote frame must be charged even before it emits text"
  );
  assert!(quote.buffered_capacity() >= quote.buffered_bytes());

  let mut marker =
    BoundedMarkdownStreamProcessor::new(HTMLToMarkdownOptions::default(), limits(4096)).unwrap();
  assert_eq!(marker.process_chunk("<strong>").unwrap(), "");
  assert!(
    marker.buffered_bytes() >= 2 + std::mem::size_of::<(u8, usize, usize)>(),
    "an open inline marker must charge its output and rewrite anchor"
  );

  let mut list =
    BoundedMarkdownStreamProcessor::new(HTMLToMarkdownOptions::default(), limits(4096)).unwrap();
  let _ = list.process_chunk("<ul><li>").unwrap();
  assert!(
    list.buffered_bytes() >= 3,
    "list indent text and width stack must both be charged"
  );
}

#[test]
fn bounded_output_matches_legacy_for_formats_and_chunking() {
  const CORPUS: &[&str] = &[
    "<h1>Title</h1><p>plain <strong>text</strong></p>",
    "<blockquote><p>quote</p><ul><li>one</li><li>two</li></ul></blockquote>",
    "<pre><code>let x = `value`;</code></pre>",
    r#"<p><a href="/relative" title="Title">link</a></p>"#,
  ];

  for &format in &[OutputFormat::Markdown, OutputFormat::Text] {
    for &html in CORPUS {
      for chunk_size in [1, 3, 17, html.len()] {
        let mut legacy =
          MarkdownStreamProcessor::new_with_format(HTMLToMarkdownOptions::default(), format);
        let mut bounded = BoundedMarkdownStreamProcessor::new_with_format(
          HTMLToMarkdownOptions::default(),
          format,
          limits(64 * 1024),
        )
        .unwrap();
        let mut expected = String::new();
        let mut actual = String::new();
        for chunk in html.as_bytes().chunks(chunk_size) {
          let chunk = std::str::from_utf8(chunk).unwrap();
          expected.push_str(&legacy.process_chunk(chunk));
          actual.push_str(&bounded.process_chunk(chunk).unwrap());
        }
        expected.push_str(&legacy.finish());
        actual.push_str(&bounded.finish().unwrap());
        assert_eq!(actual, expected, "format={format:?} chunk={chunk_size}");
      }
    }
  }
}

#[test]
fn attributes_and_tables_enforce_parser_cardinality_limits() {
  fn assert_parser_limit(html: &str) {
    let mut processor =
      BoundedMarkdownStreamProcessor::new(HTMLToMarkdownOptions::default(), limits(1024 * 1024))
        .unwrap();
    assert_eq!(
      processor.process_chunk(html).unwrap_err(),
      StreamingError::ParserLimitExceeded
    );
    assert_eq!(
      processor.finish().unwrap_err(),
      StreamingError::ParserLimitExceeded
    );
    assert_eq!(processor.buffered_capacity(), 0);
  }

  let attributes = (0..256)
    .map(|index| format!("a{index}=x"))
    .collect::<Vec<_>>()
    .join(" ");
  let mut at_limit =
    BoundedMarkdownStreamProcessor::new(HTMLToMarkdownOptions::default(), limits(1024 * 1024))
      .unwrap();
  drop(
    at_limit
      .process_chunk(&format!("<a {attributes}>x</a>"))
      .unwrap(),
  );
  drop(at_limit.finish().unwrap());

  let cells = "<td>x</td>".repeat(256);
  let mut at_limit =
    BoundedMarkdownStreamProcessor::new(HTMLToMarkdownOptions::default(), limits(1024 * 1024))
      .unwrap();
  drop(
    at_limit
      .process_chunk(&format!("<table><tr>{cells}</tr></table>"))
      .unwrap(),
  );
  drop(at_limit.finish().unwrap());

  assert_parser_limit(&format!(r#"<a href="{}">x</a>"#, "x".repeat(64 * 1024)));

  let attributes = (0..257)
    .map(|index| format!("a{index}=x"))
    .collect::<Vec<_>>()
    .join(" ");
  assert_parser_limit(&format!("<a {attributes}>x</a>"));

  let cells = "<td>x</td>".repeat(257);
  assert_parser_limit(&format!("<table><tr>{cells}</tr></table>"));
}

#[test]
fn parser_owned_attributes_are_charged_and_released() {
  let value = "x".repeat(8192);
  let mut processor =
    BoundedMarkdownStreamProcessor::new(HTMLToMarkdownOptions::default(), limits(64 * 1024))
      .unwrap();

  assert_eq!(
    processor
      .process_chunk(&format!(r#"<a title="{value}">"#))
      .unwrap(),
    ""
  );
  assert!(processor.buffered_capacity() >= value.len());
  drop(processor.process_chunk("x</a>").unwrap());
  assert!(processor.buffered_capacity() < 512);
}

#[test]
fn depth_overflow_summary_obeys_the_capacity_limit() {
  const LIMIT: usize = 256 * 1024;
  let mut processor =
    BoundedMarkdownStreamProcessor::new(HTMLToMarkdownOptions::default(), limits(LIMIT)).unwrap();
  for _ in 0..512 {
    assert_eq!(processor.process_chunk("<div>").unwrap(), "");
  }
  assert!(processor.buffered_capacity() < LIMIT);

  let oversized_name = format!("<x{}>", "x".repeat(LIMIT));
  assert_eq!(
    processor.process_chunk(&oversized_name).unwrap_err(),
    StreamingError::BufferLimitExceeded
  );
  assert_eq!(processor.buffered_capacity(), 0);
}
