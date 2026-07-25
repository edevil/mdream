use mdream::types::{ExtractionConfig, HTMLToMarkdownOptions, PluginConfig, TagOverrideConfig};
use mdream::{MarkdownStreamProcessor, html_to_markdown, html_to_markdown_result};

fn convert(html: &str) -> String {
  html_to_markdown(html, HTMLToMarkdownOptions::default())
}

fn stream(chunks: &[&str]) -> String {
  let mut processor = MarkdownStreamProcessor::new(HTMLToMarkdownOptions::default());
  let mut output = String::new();
  for chunk in chunks {
    output.push_str(&processor.process_chunk(chunk));
  }
  output.push_str(&processor.finish());
  output
}

fn alias_options(name: &str, target: &str) -> HTMLToMarkdownOptions {
  HTMLToMarkdownOptions {
    plugins: Some(PluginConfig {
      tag_overrides: Some(vec![(name.to_string(), TagOverrideConfig::alias(target))]),
      ..Default::default()
    }),
    ..Default::default()
  }
}

#[test]
fn html_solidus_only_closes_void_and_explicitly_overridden_elements() {
  assert_eq!(
    convert("<strong/>inside</strong><p>after</p>"),
    "**inside**\n\nafter"
  );
  assert_eq!(
    convert("before<br>after<img src=x>tail"),
    "before\\\nafter![](x)tail"
  );
  assert_eq!(convert(&("<keygen>".repeat(600) + "<p>after</p>")), "after");

  let options = HTMLToMarkdownOptions {
    plugins: Some(PluginConfig {
      tag_overrides: Some(vec![(
        "widget".to_string(),
        TagOverrideConfig {
          enter: Some("[".to_string()),
          exit: Some("]".to_string()),
          is_inline: Some(true),
          is_self_closing: Some(true),
          ..Default::default()
        },
      )]),
      ..Default::default()
    }),
    ..Default::default()
  };
  assert_eq!(html_to_markdown("<widget>after", options), "[]after");

  let options = HTMLToMarkdownOptions {
    plugins: Some(PluginConfig {
      tag_overrides: Some(vec![(
        "strong".to_string(),
        TagOverrideConfig {
          enter: Some("[".to_string()),
          exit: Some("]".to_string()),
          is_inline: Some(true),
          is_self_closing: Some(true),
          ..Default::default()
        },
      )]),
      ..Default::default()
    }),
    ..Default::default()
  };
  assert_eq!(html_to_markdown("<STRONG>after", options), "[]after");
}

#[test]
fn unquoted_attribute_solidus_is_data_until_whitespace() {
  let result = html_to_markdown_result(
    "<div data=x/>one</div><div data=x />two</div>",
    HTMLToMarkdownOptions {
      plugins: Some(PluginConfig {
        extraction: Some(ExtractionConfig::new(&["div"])),
        ..Default::default()
      }),
      ..Default::default()
    },
  );
  assert_eq!(result.markdown, "one\n\ntwo");
  let extracted = result.extracted.unwrap();
  assert_eq!(
    extracted[0].attributes,
    vec![("data".to_string(), "x/".to_string())]
  );
  assert_eq!(extracted[0].text_content, "one");
  assert_eq!(
    extracted[1].attributes,
    vec![("data".to_string(), "x".to_string())]
  );
  assert_eq!(extracted[1].text_content, "two");
  assert_eq!(convert("<svg><a href=/u =x/>after</svg>"), "[](/u)after");
}

#[test]
fn script_and_template_solidus_do_not_end_the_element() {
  assert_eq!(
    convert("<script/><p>hidden</p></script><p>shown</p>"),
    "shown"
  );

  let result = html_to_markdown_result(
    "<template/><strong class=target>hidden</strong></template><p>shown</p>",
    HTMLToMarkdownOptions {
      plugins: Some(PluginConfig {
        extraction: Some(ExtractionConfig::new(&[".target"])),
        ..Default::default()
      }),
      ..Default::default()
    },
  );
  assert_eq!(result.markdown, "shown");
  assert_eq!(result.extracted.unwrap()[0].text_content, "hidden");
  assert_eq!(
    convert("before<![CDATA[hidden>shown]]><p>after</p>"),
    "beforeshown]]>\n\nafter"
  );
}

#[test]
fn text_modes_apply_html_tokenization_and_safe_markdown_serialization() {
  assert_eq!(
    convert("<xmp>&amp; <b>literal</b></xmp>"),
    "\\&amp; \\<b>literal\\</b>"
  );
  assert_eq!(
    convert("<xmp><script>alert(1)</script></xmp>"),
    "\\<script>alert(1)\\</script>"
  );
  assert_eq!(
    convert("<textarea>A &amp; <b>x</b><!-- y --></textarea>"),
    "A & \\<b>x\\</b>\\<!-- y -->"
  );
  assert_eq!(
    convert("<title>A &amp; <b>x</b></title>"),
    "A & \\<b>x\\</b>"
  );
  assert_eq!(
    convert("<plaintext>A &amp; </plaintext><p>still text</p>"),
    "A \\&amp; \\</plaintext>\\<p>still text\\</p>"
  );
  assert_eq!(
    convert(
      "<style>&amp;<b>x</b></style><iframe>x</iframe><noframes>x</noframes><noembed>x</noembed><p>shown</p>"
    ),
    "shown"
  );
}

#[test]
fn text_modes_commit_leading_less_than_at_eof() {
  assert_eq!(convert("<xmp><"), "<");
  assert_eq!(convert("<textarea><"), "<");
  assert_eq!(convert("<plaintext><"), "<");
  assert_eq!(convert("<svg><title><"), "<");
  assert_eq!(convert("<svg><title><strong"), "");
  assert_eq!(convert("<svg><desc><"), "<");
  assert_eq!(convert("<svg><foreignObject><"), "<");
  assert_eq!(convert("<svg><desc><strong"), "");
  assert_eq!(convert("<svg><foreignObject><strong"), "");
}

#[test]
fn supported_svg_self_close_does_not_change_html_solidus_rules() {
  assert_eq!(
    convert("<svg><path /><text>foreign</text></svg><strong/>html</strong>"),
    "foreign**html**"
  );
  assert_eq!(convert("<svg />after"), "after");
  assert_eq!(
    convert("<svg><foreignObject><strong/>inside</strong></foreignObject></svg>"),
    "**inside**"
  );
  assert_eq!(
    convert("<svg><desc><strong/>inside</strong></desc><title><em/>title</em></title></svg>"),
    "**inside***title*"
  );
  assert_eq!(
    convert("<svg><title><strong>one</strong><em>two</em></title></svg>"),
    "**one***two*"
  );
  assert_eq!(
    convert("<svg><title>a</div><strong>b</strong></title></svg>"),
    "a**b**"
  );
  assert_eq!(
    convert("<svg><text><![CDATA[a<b>&amp;]]></text></svg>"),
    "a\\<b>\\&amp;"
  );
  assert_eq!(
    convert("<svg><text><![CDATA[&amp;]]></text></svg>"),
    "\\&amp;"
  );
  assert_eq!(convert("<svg><text>a<![CDATA[b]]>c</text></svg>"), "abc");
  assert_eq!(
    convert("<svg><text><![CDATA[&am]]><![CDATA[p;]]></text></svg>"),
    "\\&amp;"
  );
  assert_eq!(convert("<svg><text><![CDATA[a]]> b</text></svg>"), "a b");
  assert_eq!(convert("<svg>a</svg><svg><svg/>b</svg>"), "a b");
  assert_eq!(
    html_to_markdown(
      "<svg><text>a<![CDATA[b]]>c</text></svg>",
      HTMLToMarkdownOptions::default().with_wrap_width(80)
    ),
    "abc"
  );
  assert_eq!(convert("<svg><title><![CDATA[hidden]]></title></svg>"), "");

  let result = html_to_markdown_result(
    "<svg><source>inside</source></svg>",
    HTMLToMarkdownOptions {
      plugins: Some(PluginConfig {
        extraction: Some(ExtractionConfig::new(&["source"])),
        ..Default::default()
      }),
      ..Default::default()
    },
  );
  assert_eq!(result.extracted.unwrap()[0].text_content, "inside");
}

#[test]
fn aliases_use_target_text_modes_and_only_close_by_alias_name() {
  assert_eq!(
    html_to_markdown(
      "<x>hidden</iframe><p>still hidden</p></x><p>shown</p>",
      alias_options("x", "iframe")
    ),
    "shown"
  );

  let cases = [
    (
      "<x>&amp; <b>literal</b></x><p>after</p>",
      "\\&amp; \\<b>literal\\</b>\n\nafter",
      "x",
      "xmp",
    ),
    (
      "<x>A &amp; </x><p>still text</p>",
      "A \\&amp; \\</x>\\<p>still text\\</p>",
      "x",
      "plaintext",
    ),
    ("<x>&amp; <b>x</b></x>", "& \\<b>x\\</b>", "x", "textarea"),
    (
      "<strong>&amp; <b>literal</b></strong><p>after</p>",
      "\\&amp; \\<b>literal\\</b>\n\nafter",
      "strong",
      "xmp",
    ),
  ];
  for (html, expected, source, target) in cases {
    assert_eq!(
      html_to_markdown(html, alias_options(source, target)),
      expected
    );
    for split in 0..=html.len() {
      let mut processor = MarkdownStreamProcessor::new(alias_options(source, target));
      let mut output = processor.process_chunk(&html[..split]);
      output.push_str(&processor.process_chunk(&html[split..]));
      output.push_str(&processor.finish());
      assert_eq!(output, expected, "{source}->{target} split at byte {split}");
    }
  }

  let html = "<script><!--<script></script>--></script><p>after</p>";
  for split in 0..=html.len() {
    let mut processor = MarkdownStreamProcessor::new(alias_options("script", "script"));
    let mut output = processor.process_chunk(&html[..split]);
    output.push_str(&processor.process_chunk(&html[split..]));
    output.push_str(&processor.finish());
    assert_eq!(
      output, "after",
      "identity script alias split at byte {split}"
    );
  }
}

#[test]
fn text_modes_stream_identically_at_every_byte_boundary() {
  let html = "<p>before</p><xmp>&amp; <b>x</b></xmp><textarea>A &amp; <i>y</i><!--z--></textarea><style><p>hidden</p></style><script/><p>hidden</p></script><template/><b>hidden</b></template><svg><text>x<![CDATA[&am]]><![CDATA[p;]]> y</text><title>a</div><strong>b</strong></title></svg><p>after</p>";
  let expected = stream(&[html]);
  assert_eq!(expected, convert(html));
  for split in 0..=html.len() {
    assert_eq!(
      stream(&[&html[..split], &html[split..]]),
      expected,
      "split at byte {split}"
    );
  }
}

#[test]
fn end_br_recovers_as_a_start_tag() {
  assert_eq!(convert("before</br>after"), "before\\\nafter");

  assert_eq!(
    html_to_markdown("before</br>after", alias_options("br", "strong")),
    "beforeafter"
  );
  assert_eq!(
    html_to_markdown(
      "before<![CDATA[hidden]]>after",
      alias_options("#cdata-section", "br")
    ),
    "before\\\nafter"
  );
  assert_eq!(
    html_to_markdown(
      &("<![CDATA[hidden]]>".repeat(600) + "<p>after</p>"),
      alias_options("#cdata-section", "meta")
    ),
    "after"
  );
}
