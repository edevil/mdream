use mdream::{
  BoundedMarkdownStreamProcessor, CleanConfig, ConfigurationError, ExtractionConfig,
  FrontmatterConfig, HTMLToMarkdownOptions, IsolateMainConfig, MarkdownStreamProcessor,
  PluginConfig, StreamingError, StreamingLimits, TagOverrideConfig, UnsupportedStreamingOption,
  html_to_markdown, try_html_to_markdown,
};

fn with_plugins(plugins: PluginConfig) -> HTMLToMarkdownOptions {
  HTMLToMarkdownOptions {
    plugins: Some(plugins),
    ..Default::default()
  }
}

fn strict_error(options: HTMLToMarkdownOptions) -> ConfigurationError {
  MarkdownStreamProcessor::try_new(options).err().unwrap()
}

#[test]
fn whole_document_option_capability_matrix() {
  let cases = [
    (
      HTMLToMarkdownOptions {
        clean: Some(CleanConfig {
          fragments: true,
          ..Default::default()
        }),
        ..Default::default()
      },
      UnsupportedStreamingOption::CleanFragments,
    ),
    (
      with_plugins(PluginConfig {
        isolate_main: Some(IsolateMainConfig),
        ..Default::default()
      }),
      UnsupportedStreamingOption::IsolateMain,
    ),
    (
      with_plugins(PluginConfig {
        frontmatter: Some(FrontmatterConfig::default()),
        ..Default::default()
      }),
      UnsupportedStreamingOption::Frontmatter,
    ),
    (
      with_plugins(PluginConfig {
        extraction: Some(ExtractionConfig::new(&["p"])),
        ..Default::default()
      }),
      UnsupportedStreamingOption::Extraction,
    ),
  ];

  for (options, expected) in cases {
    assert!(try_html_to_markdown("<p>body</p>", options.clone()).is_ok());
    assert_eq!(
      strict_error(options.clone()),
      ConfigurationError::UnsupportedStreamingOption(expected)
    );
    assert!(matches!(
      BoundedMarkdownStreamProcessor::new(options, StreamingLimits::new(4096)),
      Err(StreamingError::UnsupportedOption(option)) if option == expected
    ));
  }
}

#[test]
fn legacy_stream_defers_whole_document_output_and_keeps_semantics() {
  let fragments = HTMLToMarkdownOptions {
    clean: Some(CleanConfig {
      fragments: true,
      ..Default::default()
    }),
    ..Default::default()
  };
  let html = r##"<p><a href="#missing">missing</a></p><h2 id="present">Present</h2>"##;
  let expected = html_to_markdown(html, fragments.clone());
  let mut stream = MarkdownStreamProcessor::new(fragments);
  assert_eq!(stream.process_chunk(html), "");
  assert_eq!(stream.finish(), expected);

  let isolate = with_plugins(PluginConfig {
    isolate_main: Some(IsolateMainConfig),
    ..Default::default()
  });
  let prefix = format!("<h1>Fallback</h1><p>{}</p>", "prefix ".repeat(20_000));
  let html = format!("{prefix}<main><h1>Real</h1><p>body</p></main>");
  assert_eq!(html_to_markdown(&html, isolate.clone()), "# Real\n\nbody");
  let mut stream = MarkdownStreamProcessor::new(isolate);
  assert_eq!(stream.process_chunk(&prefix), "");
  assert_eq!(stream.process_chunk("<main><h1>Real</h1>"), "");
  assert_eq!(stream.process_chunk("<p>body</p></main>"), "");
  assert_eq!(stream.finish(), "# Real\n\nbody");
}

#[test]
fn partial_and_literal_overrides_are_consistent() {
  let options = with_plugins(
    PluginConfig::default()
      .with_tag_override(
        "strong",
        TagOverrideConfig {
          enter: Some("[".into()),
          ..Default::default()
        },
      )
      .with_tag_override(
        "em",
        TagOverrideConfig {
          exit: Some("]".into()),
          ..Default::default()
        },
      )
      .with_tag_override(
        "x",
        TagOverrideConfig {
          enter: Some(String::new()),
          exit: Some(String::new()),
          spacing: Some([0, 0]),
          is_inline: Some(true),
          collapses_inner_white_space: Some(true),
          ..Default::default()
        },
      ),
  );
  assert_eq!(
    try_html_to_markdown(
      "<p><strong>a<em>b</em></strong><x>  c   d  </x></p>",
      options
    )
    .unwrap(),
    "[a*b]** c d"
  );

  let literal_anchor = with_plugins(PluginConfig::default().with_tag_override(
    "a",
    TagOverrideConfig {
      enter: Some("<literal>".into()),
      exit: Some("</literal>".into()),
      ..Default::default()
    },
  ));
  assert_eq!(
    try_html_to_markdown(
      r#"<a href="https://example.com"><strong>x</strong></a>"#,
      literal_anchor
    )
    .unwrap(),
    "<literal>**x**</literal>"
  );
}

#[test]
fn invalid_aliases_and_combinations_are_fallible_and_never_panic() {
  assert!(matches!(
    TagOverrideConfig::try_alias("not-a-real-tag"),
    Err(ConfigurationError::UnknownTagAlias { .. })
  ));

  let mixed = with_plugins(PluginConfig::default().with_tag_override(
    "x",
    TagOverrideConfig {
      enter: Some("[".into()),
      alias_tag_id: Some(mdream::consts::TAG_EM),
      ..Default::default()
    },
  ));
  assert!(matches!(
    try_html_to_markdown("<x>body</x>", mixed),
    Err(ConfigurationError::TagAliasWithOverrides { .. })
  ));

  let partial_anchor = with_plugins(PluginConfig::default().with_tag_override(
    "a",
    TagOverrideConfig {
      enter: Some("[".into()),
      ..Default::default()
    },
  ));
  assert!(matches!(
    try_html_to_markdown(r#"<a href="/x">body</a>"#, partial_anchor),
    Err(ConfigurationError::UnpairedTagOverride { .. })
  ));

  for alias_tag_id in 0..=u8::MAX {
    let options = with_plugins(PluginConfig::default().with_tag_override(
      "x",
      TagOverrideConfig {
        alias_tag_id: Some(alias_tag_id),
        ..Default::default()
      },
    ));
    let outcome = std::panic::catch_unwind(|| {
      let _ = try_html_to_markdown("<x>body</x>", options.clone());
      let _ = html_to_markdown("<x>body</x>", options);
    });
    assert!(outcome.is_ok(), "alias id {alias_tag_id} panicked");
  }
}
