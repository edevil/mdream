#![no_main]
use arbitrary::Arbitrary;
use libfuzzer_sys::fuzz_target;
use mdream::{MarkdownStreamProcessor, html_to_markdown, try_html_to_markdown, types::*};

#[derive(Arbitrary, Debug)]
struct FuzzInput {
    html: String,
    use_origin: bool,
    origin: String,
    clean_urls: bool,
    use_clean: bool,
    clean_fragments: bool,
    clean_empty_links: bool,
    clean_blank_lines: bool,
    clean_redundant_links: bool,
    clean_self_link_headings: bool,
    clean_empty_images: bool,
    clean_empty_link_text: bool,
    use_filter: bool,
    filter_exclude: Vec<String>,
    use_isolate_main: bool,
    use_frontmatter: bool,
    use_tailwind: bool,
    use_extraction: bool,
    extraction_selectors: Vec<String>,
    use_tag_override: bool,
    override_tag: String,
    alias_tag_id: u8,
    override_enter: Option<String>,
    override_exit: Option<String>,
    override_spacing: Option<[u8; 2]>,
    override_is_inline: Option<bool>,
    override_is_self_closing: Option<bool>,
    override_collapses_whitespace: Option<bool>,
}

fuzz_target!(|input: FuzzInput| {
    let clean = if input.use_clean {
        Some(CleanConfig {
            urls: input.clean_urls,
            fragments: input.clean_fragments,
            empty_links: input.clean_empty_links,
            blank_lines: input.clean_blank_lines,
            redundant_links: input.clean_redundant_links,
            self_link_headings: input.clean_self_link_headings,
            empty_images: input.clean_empty_images,
            empty_link_text: input.clean_empty_link_text,
        })
    } else {
        None
    };

    let plugins = {
        let filter = if input.use_filter {
            Some(FilterConfig {
                include: None,
                exclude: Some(input.filter_exclude),
                process_children: None,
            })
        } else {
            None
        };

        let isolate_main = if input.use_isolate_main {
            Some(IsolateMainConfig {})
        } else {
            None
        };

        let frontmatter = if input.use_frontmatter {
            Some(FrontmatterConfig::default())
        } else {
            None
        };

        let tailwind = if input.use_tailwind {
            Some(TailwindConfig {})
        } else {
            None
        };

        let extraction = if input.use_extraction && !input.extraction_selectors.is_empty() {
            Some(ExtractionConfig {
                selectors: input.extraction_selectors,
            })
        } else {
            None
        };

        let tag_overrides = input.use_tag_override.then(|| vec![(
            input.override_tag,
            TagOverrideConfig {
                enter: input.override_enter,
                exit: input.override_exit,
                spacing: input.override_spacing,
                is_inline: input.override_is_inline,
                is_self_closing: input.override_is_self_closing,
                collapses_inner_white_space: input.override_collapses_whitespace,
                alias_tag_id: Some(input.alias_tag_id),
            },
        )]);

        Some(PluginConfig {
            filter,
            isolate_main,
            frontmatter,
            tailwind,
            extraction,
            tag_overrides,
        })
    };

    let options = HTMLToMarkdownOptions {
        origin: if input.use_origin { Some(input.origin) } else { None },
        clean_urls: input.clean_urls,
        clean,
        plugins,
        ..Default::default()
    };

    let _ = try_html_to_markdown(&input.html, options.clone());
    let _ = MarkdownStreamProcessor::try_new(options.clone());
    let _ = html_to_markdown(&input.html, options);
});
