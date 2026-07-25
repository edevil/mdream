use wasm_bindgen::prelude::*;

// ── Manual JsValue helpers (replaces serde) ──

fn get_prop(obj: &JsValue, key: &str) -> JsValue {
  js_sys::Reflect::get(obj, &JsValue::from_str(key)).unwrap_or(JsValue::UNDEFINED)
}

fn as_string(v: &JsValue) -> Option<String> {
  v.as_string()
}

fn as_bool(v: &JsValue) -> Option<bool> {
  v.as_bool()
}

fn as_string_vec(v: &JsValue) -> Option<Vec<String>> {
  if v.is_undefined() || v.is_null() || !js_sys::Array::is_array(v) {
    return None;
  }
  let arr = js_sys::Array::from(v);
  let mut out = Vec::with_capacity(arr.length() as usize);
  for i in 0..arr.length() {
    if let Some(s) = arr.get(i).as_string() {
      out.push(s);
    }
  }
  Some(out)
}

#[allow(clippy::cast_possible_truncation)]
fn as_spacing(v: &JsValue) -> Result<Option<[u8; 2]>, JsValue> {
  if v.is_undefined() || v.is_null() {
    return Ok(None);
  }
  if !js_sys::Array::is_array(v) {
    return Err(js_sys::TypeError::new("Tag override spacing must be a two-value array").into());
  }
  let arr = js_sys::Array::from(v);
  if arr.length() != 2 {
    return Err(
      js_sys::TypeError::new("Tag override spacing must contain exactly two values").into(),
    );
  }
  let mut out = [0; 2];
  for (index, value) in (0..2).zip(&mut out) {
    let Some(number) = arr.get(index).as_f64() else {
      return Err(js_sys::TypeError::new("Tag override spacing values must be numbers").into());
    };
    if !number.is_finite() || number.fract() != 0.0 || !(0.0..=255.0).contains(&number) {
      return Err(
        js_sys::TypeError::new("Tag override spacing values must be integers from 0 to 255").into(),
      );
    }
    *value = number as u8;
  }
  Ok(Some(out))
}

fn js_object_entries(v: &JsValue) -> Option<Vec<(String, JsValue)>> {
  if v.is_undefined() || v.is_null() {
    return None;
  }
  let entries = js_sys::Object::entries(&js_sys::Object::from(v.clone()));
  let mut out = Vec::with_capacity(entries.length() as usize);
  for i in 0..entries.length() {
    let pair = js_sys::Array::from(&entries.get(i));
    if let Some(key) = pair.get(0).as_string() {
      out.push((key, pair.get(1)));
    }
  }
  Some(out)
}

fn js_string_vec(v: &JsValue) -> Option<Vec<(String, String)>> {
  let entries = js_object_entries(v)?;
  let mut out = Vec::with_capacity(entries.len());
  for (k, v) in entries {
    if let Some(s) = v.as_string() {
      out.push((k, s));
    }
  }
  Some(out)
}

fn parse_clean(v: &JsValue) -> Option<mdream::types::CleanConfig> {
  if v.is_undefined() || v.is_null() {
    return None;
  }
  Some(mdream::types::CleanConfig {
    urls: as_bool(&get_prop(v, "urls")).unwrap_or(false),
    fragments: as_bool(&get_prop(v, "fragments")).unwrap_or(false),
    empty_links: as_bool(&get_prop(v, "emptyLinks")).unwrap_or(false),
    blank_lines: as_bool(&get_prop(v, "blankLines")).unwrap_or(false),
    redundant_links: as_bool(&get_prop(v, "redundantLinks")).unwrap_or(false),
    self_link_headings: as_bool(&get_prop(v, "selfLinkHeadings")).unwrap_or(false),
    empty_images: as_bool(&get_prop(v, "emptyImages")).unwrap_or(false),
    empty_link_text: as_bool(&get_prop(v, "emptyLinkText")).unwrap_or(false),
  })
}

// ── Options parsing ──

fn parse_options(
  options: &JsValue,
) -> Result<
  (
    mdream::types::HTMLToMarkdownOptions,
    mdream::types::OutputFormat,
  ),
  JsValue,
> {
  if options.is_undefined() || options.is_null() {
    return Ok((
      mdream::types::HTMLToMarkdownOptions::default(),
      mdream::types::OutputFormat::Markdown,
    ));
  }

  let origin = as_string(&get_prop(options, "origin"));
  let clean_urls = as_bool(&get_prop(options, "cleanUrls")).unwrap_or(false);
  let clean = parse_clean(&get_prop(options, "clean"));
  let url_policy = match as_string(&get_prop(options, "urlPolicy")).as_deref() {
    None | Some("preserve") => mdream::types::UrlPolicy::Preserve,
    Some("strict") => mdream::types::UrlPolicy::Strict,
    Some(value) => {
      return Err(js_sys::TypeError::new(&format!("Invalid urlPolicy: {value}")).into());
    }
  };

  let plugins_val = get_prop(options, "plugins");
  let plugins = if plugins_val.is_undefined() || plugins_val.is_null() {
    None
  } else {
    Some(parse_plugins(&plugins_val)?)
  };

  #[allow(clippy::cast_possible_truncation, clippy::cast_precision_loss)]
  let wrap_width = get_prop(options, "wrapWidth")
    .as_f64()
    .filter(|n| n.is_finite() && *n >= 0.0 && *n <= usize::MAX as f64)
    .map_or(0, |n| n as usize);
  let format = match as_string(&get_prop(options, "format")).as_deref() {
    Some("text") => mdream::types::OutputFormat::Text,
    _ => mdream::types::OutputFormat::Markdown,
  };

  let core_options = mdream::types::HTMLToMarkdownOptions {
    origin,
    url_policy,
    clean_urls,
    clean,
    plugins,
    wrap_width,
  };
  Ok((core_options, format))
}

fn parse_plugins(p: &JsValue) -> Result<mdream::types::PluginConfig, JsValue> {
  let filter_val = get_prop(p, "filter");
  let filter = if filter_val.is_undefined() || filter_val.is_null() {
    None
  } else {
    Some(mdream::types::FilterConfig {
      include: as_string_vec(&get_prop(&filter_val, "include")),
      exclude: as_string_vec(&get_prop(&filter_val, "exclude")),
      process_children: as_bool(&get_prop(&filter_val, "processChildren")),
    })
  };

  let isolate_val = get_prop(p, "isolateMain");
  let isolate_main = as_bool(&isolate_val).and_then(|v| {
    if v {
      Some(mdream::types::IsolateMainConfig {})
    } else {
      None
    }
  });

  let fm_val = get_prop(p, "frontmatter");
  let frontmatter = if fm_val.is_undefined() || fm_val.is_null() {
    None
  } else {
    Some(mdream::types::FrontmatterConfig {
      additional_fields: js_string_vec(&get_prop(&fm_val, "additionalFields")),
      meta_fields: as_string_vec(&get_prop(&fm_val, "metaFields")),
    })
  };

  let tailwind = as_bool(&get_prop(p, "tailwind")).and_then(|v| {
    if v {
      Some(mdream::types::TailwindConfig {})
    } else {
      None
    }
  });

  let ext_val = get_prop(p, "extraction");
  let extraction = if ext_val.is_undefined() || ext_val.is_null() {
    None
  } else {
    as_string_vec(&get_prop(&ext_val, "selectors"))
      .map(|selectors| mdream::types::ExtractionConfig { selectors })
  };

  let overrides_val = get_prop(p, "tagOverrides");
  let tag_overrides = if overrides_val.is_undefined() || overrides_val.is_null() {
    None
  } else {
    js_object_entries(&overrides_val)
      .map(|entries| {
        entries
          .into_iter()
          .map(|(tag_name, ov)| {
            let alias = as_string(&get_prop(&ov, "alias"));
            let alias_tag_id = alias
              .as_ref()
              .map(|alias| {
                let mut normalized = alias.clone();
                normalized.make_ascii_lowercase();
                mdream::consts::get_tag_id(&normalized).ok_or_else(|| {
                  JsValue::from(js_sys::TypeError::new(&format!(
                    "Unknown tag alias: {alias}"
                  )))
                })
              })
              .transpose()?;
            let enter = as_string(&get_prop(&ov, "enter"));
            let exit = as_string(&get_prop(&ov, "exit"));
            let spacing = as_spacing(&get_prop(&ov, "spacing"))?;
            let is_inline = as_bool(&get_prop(&ov, "isInline"));
            let is_self_closing = as_bool(&get_prop(&ov, "isSelfClosing"));
            let collapses_inner_white_space = as_bool(&get_prop(&ov, "collapsesInnerWhiteSpace"));
            if alias_tag_id.is_some()
              && (enter.is_some()
                || exit.is_some()
                || spacing.is_some()
                || is_inline.is_some()
                || is_self_closing.is_some()
                || collapses_inner_white_space.is_some())
            {
              return Err(
                js_sys::TypeError::new(&format!(
                  "Tag override {tag_name:?} cannot combine alias with override fields"
                ))
                .into(),
              );
            }
            let config = mdream::types::TagOverrideConfig {
              enter,
              exit,
              spacing,
              is_inline,
              is_self_closing,
              collapses_inner_white_space,
              alias_tag_id,
            };
            Ok((tag_name, config))
          })
          .collect::<Result<Vec<_>, JsValue>>()
      })
      .transpose()?
  };

  Ok(mdream::types::PluginConfig {
    filter,
    isolate_main,
    frontmatter,
    tailwind,
    extraction,
    tag_overrides,
  })
}

// ── WASM exports ──

#[wasm_bindgen(js_name = "htmlToMarkdown")]
#[allow(clippy::needless_pass_by_value)]
pub fn html_to_markdown(html: &str, options: JsValue) -> Result<String, JsValue> {
  let (opts, format) = parse_options(&options)?;
  mdream::try_html_to_format(html, opts, format)
    .map_err(|error| js_sys::TypeError::new(&error.to_string()).into())
}

#[wasm_bindgen(js_name = "htmlToMarkdownResult")]
#[allow(clippy::needless_pass_by_value)]
pub fn html_to_markdown_result(html: &str, options: JsValue) -> Result<JsValue, JsValue> {
  let (opts, format) = parse_options(&options)?;
  let result = mdream::try_html_to_format_result(html, opts, format)
    .map_err(|error| js_sys::TypeError::new(&error.to_string()))?;

  let obj = js_sys::Object::new();
  js_sys::Reflect::set(&obj, &"markdown".into(), &result.markdown.into()).unwrap_or_default();

  if let Some(extracted) = result.extracted {
    let arr = js_sys::Array::new();
    for e in extracted {
      let elem = js_sys::Object::new();
      js_sys::Reflect::set(&elem, &"selector".into(), &e.selector.into()).unwrap_or_default();
      js_sys::Reflect::set(&elem, &"tagName".into(), &e.tag_name.into()).unwrap_or_default();
      js_sys::Reflect::set(&elem, &"textContent".into(), &e.text_content.into())
        .unwrap_or_default();
      let attrs = js_sys::Object::new();
      for (k, v) in e.attributes {
        js_sys::Reflect::set(&attrs, &k.into(), &v.into()).unwrap_or_default();
      }
      js_sys::Reflect::set(&elem, &"attributes".into(), &attrs).unwrap_or_default();
      arr.push(&elem);
    }
    js_sys::Reflect::set(&obj, &"extracted".into(), &arr).unwrap_or_default();
  }

  if let Some(frontmatter) = result.frontmatter {
    let fm = js_sys::Object::new();
    for (k, v) in frontmatter {
      js_sys::Reflect::set(&fm, &k.into(), &v.into()).unwrap_or_default();
    }
    js_sys::Reflect::set(&obj, &"frontmatter".into(), &fm).unwrap_or_default();
  }

  Ok(obj.into())
}

#[wasm_bindgen]
pub struct MarkdownStream {
  inner: mdream::MarkdownStreamProcessor,
}

#[wasm_bindgen]
impl MarkdownStream {
  #[wasm_bindgen(constructor)]
  #[allow(clippy::needless_pass_by_value)]
  pub fn new(options: JsValue) -> Result<Self, JsValue> {
    let (opts, format) = parse_options(&options)?;
    Ok(Self {
      inner: mdream::MarkdownStreamProcessor::try_new_with_format(opts, format)
        .map_err(|error| js_sys::TypeError::new(&error.to_string()))?,
    })
  }

  #[wasm_bindgen(js_name = "processChunk")]
  pub fn process_chunk(&mut self, chunk: &str) -> String {
    self.inner.process_chunk(chunk)
  }

  pub fn finish(&mut self) -> String {
    self.inner.finish()
  }
}
