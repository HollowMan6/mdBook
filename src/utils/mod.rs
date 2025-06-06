//! Various helpers and utilities.

pub mod fs;
mod string;
pub(crate) mod toml_ext;
use crate::errors::Error;
use log::error;
use pulldown_cmark::{html, CodeBlockKind, CowStr, Event, LinkType, Options, Parser, Tag, TagEnd};
use regex::Regex;

use std::borrow::Cow;
use std::collections::HashMap;
use std::fmt::Write;
use std::path::{Component, Path, PathBuf};
use std::sync::LazyLock;

pub use self::string::{
    take_anchored_lines, take_lines, take_rustdoc_include_anchored_lines,
    take_rustdoc_include_lines,
};

/// Replaces multiple consecutive whitespace characters with a single space character.
pub fn collapse_whitespace(text: &str) -> Cow<'_, str> {
    static RE: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"\s\s+").unwrap());
    RE.replace_all(text, " ")
}

/// Convert the given string to a valid HTML element ID.
/// The only restriction is that the ID must not contain any ASCII whitespace.
pub fn normalize_id(content: &str) -> String {
    content
        .chars()
        .filter_map(|ch| {
            if ch.is_alphanumeric() || ch == '_' || ch == '-' {
                Some(ch.to_ascii_lowercase())
            } else if ch.is_whitespace() {
                Some('-')
            } else {
                None
            }
        })
        .collect::<String>()
}

/// Generate an ID for use with anchors which is derived from a "normalised"
/// string.
// This function should be made private when the deprecation expires.
#[deprecated(since = "0.4.16", note = "use unique_id_from_content instead")]
pub fn id_from_content(content: &str) -> String {
    let mut content = content.to_string();

    // Skip any tags or html-encoded stuff
    static HTML: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"(<.*?>)").unwrap());
    content = HTML.replace_all(&content, "").into();
    const REPL_SUB: &[&str] = &["&lt;", "&gt;", "&amp;", "&#39;", "&quot;"];
    for sub in REPL_SUB {
        content = content.replace(sub, "");
    }

    // Remove spaces and hashes indicating a header
    let trimmed = content.trim().trim_start_matches('#').trim();
    normalize_id(trimmed)
}

/// Generate an ID for use with anchors which is derived from a "normalised"
/// string.
///
/// Each ID returned will be unique, if the same `id_counter` is provided on
/// each call.
pub fn unique_id_from_content(content: &str, id_counter: &mut HashMap<String, usize>) -> String {
    let id = {
        #[allow(deprecated)]
        id_from_content(content)
    };

    // If we have headers with the same normalized id, append an incrementing counter
    let id_count = id_counter.entry(id.clone()).or_insert(0);
    let unique_id = match *id_count {
        0 => id,
        id_count => format!("{id}-{id_count}"),
    };
    *id_count += 1;
    unique_id
}

/// Improve the path to try remove and solve .. token,
/// This assumes that `a/b/../c` is `a/c`.
///
/// This function ensures a given path ending with '/' will also
/// end with '/' after normalization.
/// <https://stackoverflow.com/a/68233480>
fn normalize_path<P: AsRef<Path>>(path: P) -> String {
    let ends_with_slash = path.as_ref().to_str().map_or(false, |s| s.ends_with('/'));
    let mut normalized = PathBuf::new();
    for component in path.as_ref().components() {
        match &component {
            Component::ParentDir => {
                if !normalized.pop() {
                    normalized.push(component);
                }
            }
            Component::CurDir => {}
            _ => {
                normalized.push(component);
            }
        }
    }
    if ends_with_slash {
        normalized.push("");
    }
    normalized
        .to_str()
        .unwrap()
        .replace("\\", "/")
        .trim_start_matches('/')
        .to_string()
}

/// Converts a relative URL path to a reference ID for the print page.
fn normalize_print_page_id(mut path: String) -> String {
    path = path
        .replace("/", "-")
        .replace(".html#", "-")
        .replace("#", "-")
        .to_ascii_lowercase();
    if path.ends_with(".html") {
        path.truncate(path.len() - 5);
    }
    path
}

/// Fix links to the correct location.
///
/// This adjusts links, such as turning `.md` extensions to `.html`.
///
/// See [`render_markdown_with_path_and_redirects`] for a description of
/// `path` and `redirects`.
fn adjust_links<'a>(
    event: Event<'a>,
    path: Option<&Path>,
    redirects: &HashMap<String, String>,
) -> Event<'a> {
    static SCHEME_LINK: LazyLock<Regex> =
        LazyLock::new(|| Regex::new(r"^[a-z][a-z0-9+.-]*:").unwrap());
    static HTML_MD_LINK: LazyLock<Regex> =
        LazyLock::new(|| Regex::new(r"(?P<link>.*)\.(html|md)(?P<anchor>#.*)?").unwrap());

    fn add_base(path: Option<&Path>) -> String {
        let mut fixed_link = String::new();
        if let Some(path) = path {
            let base = path
                .parent()
                .expect("path can't be empty")
                .to_str()
                .expect("utf-8 paths only");
            if !base.is_empty() {
                write!(fixed_link, "{base}/").unwrap();
            }
        }
        fixed_link.to_string()
    }

    fn fix_print_page_link<'a>(
        mut normalized_path: String,
        redirects: &HashMap<String, String>,
    ) -> CowStr<'a> {
        // Fix redirect links
        let (path_no_fragment, fragment) = match normalized_path.split_once('#') {
            Some((a, b)) => (a, Some(b)),
            None => (normalized_path.as_str(), None),
        };
        for (original, redirect) in redirects {
            if !normalize_path(original.trim_start_matches('/'))
                .eq_ignore_ascii_case(&normalized_path)
                && !normalize_path(original.trim_start_matches('/'))
                    .eq_ignore_ascii_case(&path_no_fragment)
            {
                continue;
            }

            let mut unnormalized_path = String::new();
            if SCHEME_LINK.is_match(&redirect) {
                unnormalized_path = redirect.to_string();
            } else {
                let base = PathBuf::from(path_no_fragment)
                    .parent()
                    .expect("path can't be empty")
                    .to_str()
                    .expect("utf-8 paths only")
                    .to_owned();

                let normalized_base = normalize_path(base).trim_matches('/').to_owned();
                if !normalized_base.is_empty() {
                    write!(unnormalized_path, "{normalized_base}/{redirect}").unwrap();
                } else {
                    unnormalized_path = redirect.to_string().trim_start_matches('/').to_string();
                }
            }

            // original without anchors, need to append link anchors
            if !original.contains("#") {
                if let Some(fragment) = fragment {
                    if !unnormalized_path.contains("#") {
                        unnormalized_path.push('#');
                    } else {
                        unnormalized_path.push('-');
                    }
                    unnormalized_path.push_str(fragment);
                }
            }

            if SCHEME_LINK.is_match(&redirect) {
                return CowStr::from(unnormalized_path);
            } else {
                normalized_path = normalize_path(unnormalized_path);
            }
            break;
        }

        // Check again to make sure anchors are the html links inside the book.
        if normalized_path.starts_with("../") || normalized_path.contains("/../") {
            return CowStr::from(normalized_path);
        }

        let mut fixed_anchor_for_print = String::new();
        fixed_anchor_for_print.push_str("#");
        fixed_anchor_for_print.push_str(&normalize_print_page_id(normalized_path));
        CowStr::from(fixed_anchor_for_print)
    }

    /// Fix resource links like img to the correct location.
    fn fix_resource_links<'a>(dest: CowStr<'a>, path: Option<&Path>) -> CowStr<'a> {
        // Don't modify links with schemes like `https`.
        // Only fix relative links
        if SCHEME_LINK.is_match(&dest) || dest.starts_with('/') {
            return dest;
        }

        // This is a relative link, adjust it as necessary.
        let mut fixed_link = add_base(path);
        fixed_link.push_str(&dest);
        CowStr::from(fixed_link)
    }

    fn fix_a_links_with_type<'a>(
        dest: CowStr<'a>,
        path: Option<&Path>,
        redirects: &HashMap<String, String>,
        link_type: LinkType,
    ) -> CowStr<'a> {
        if link_type == LinkType::Email {
            return dest;
        }
        fix_a_links(dest, path, redirects)
    }

    /// Adjust markdown file to correct point in the html file.
    fn fix_a_links<'a>(
        dest: CowStr<'a>,
        path: Option<&Path>,
        redirects: &HashMap<String, String>,
    ) -> CowStr<'a> {
        if dest.starts_with('#') {
            // Fragment-only link.
            return match path {
                Some(path) => {
                    let mut base = path.display().to_string();
                    if base.ends_with(".md") {
                        base.truncate(base.len() - 3);
                    }
                    format!(
                        "#{}{}",
                        normalize_print_page_id(normalize_path(base)),
                        dest.replace("#", "-")
                    )
                    .into()
                }
                None => dest,
            };
        }

        // Don't modify links with schemes like `https`.
        if SCHEME_LINK.is_match(&dest) {
            return dest;
        }

        let mut fixed_link = if dest.starts_with('/') {
            String::new()
        } else {
            // This is a relative link, adjust it as necessary.
            add_base(path)
        };

        if let Some(caps) = HTML_MD_LINK.captures(&dest) {
            fixed_link.push_str(&caps["link"]);
            fixed_link.push_str(".html");
            if let Some(anchor) = caps.name("anchor") {
                fixed_link.push_str(anchor.as_str());
            }
        } else {
            fixed_link.push_str(&dest);
        };

        let normalized_path = normalize_path(&fixed_link);

        // Judge if the html link is inside the book.
        if !normalized_path.starts_with("../") && !normalized_path.contains("/../") {
            // In `print.html`, print page links would all link to anchors on the print page.
            return match path {
                Some(_) => fix_print_page_link(normalized_path, redirects),
                None => CowStr::from(fixed_link),
            };
        }
        // In normal page rendering, links to anchors on another page.
        CowStr::from(fixed_link)
    }

    fn fix_html<'a>(
        html: CowStr<'a>,
        path: Option<&Path>,
        redirects: &HashMap<String, String>,
    ) -> CowStr<'a> {
        // This is a terrible hack, but should be reasonably reliable. Nobody
        // should ever parse a tag with a regex. However, there isn't anything
        // in Rust that I know of that is suitable for handling partial html
        // fragments like those generated by pulldown_cmark.
        //
        // There are dozens of HTML tags/attributes that contain paths, so
        // feel free to add more tags if desired; these are the only ones I
        // care about right now.
        static A_LINK: LazyLock<Regex> =
            LazyLock::new(|| Regex::new(r#"(<a [^>]*?href=")([^"]+?)""#).unwrap());
        static A_NAME: LazyLock<Regex> =
            LazyLock::new(|| Regex::new(r#"(<a [^>]*?name=")([^"]+?)""#).unwrap());
        static IMG_LINK: LazyLock<Regex> =
            LazyLock::new(|| Regex::new(r#"(<img [^>]*?src=")([^"]+?)""#).unwrap());

        let img_link_fixed_html = IMG_LINK.replace_all(&html, |caps: &regex::Captures<'_>| {
            let fixed = fix_resource_links(caps[2].into(), path);
            format!("{}{}\"", &caps[1], fixed)
        });

        let a_name_fixed_html =
            A_NAME.replace_all(&img_link_fixed_html, |caps: &regex::Captures<'_>| {
                // This is a relative link, adjust it as necessary.
                let origin_name = &caps[2].to_string();
                format!(
                    "{}{}\"",
                    &caps[1],
                    CowStr::from(match path {
                        Some(path) => {
                            let mut base = path.display().to_string();
                            if base.ends_with(".md") {
                                base.truncate(base.len() - 3);
                            }
                            format!(
                                "{}-{}",
                                normalize_print_page_id(normalize_path(base)),
                                origin_name.to_string()
                            )
                        }
                        None => origin_name.to_string(),
                    })
                )
            });

        A_LINK
            .replace_all(&a_name_fixed_html, |caps: &regex::Captures<'_>| {
                let fixed = fix_a_links(caps[2].into(), path, &redirects);
                format!("{}{}\"", &caps[1], fixed)
            })
            .into_owned()
            .into()
    }

    match event {
        Event::Start(Tag::Link {
            link_type,
            dest_url,
            title,
            id,
        }) => Event::Start(Tag::Link {
            link_type,
            dest_url: fix_a_links_with_type(dest_url, path, redirects, link_type),
            title,
            id,
        }),
        Event::Start(Tag::Image {
            link_type,
            dest_url,
            title,
            id,
        }) => Event::Start(Tag::Image {
            link_type,
            dest_url: fix_resource_links(dest_url, path),
            title,
            id,
        }),
        Event::Html(html) => Event::Html(fix_html(html, path, redirects)),
        Event::InlineHtml(html) => Event::InlineHtml(fix_html(html, path, redirects)),
        _ => event,
    }
}

/// Wrapper around the pulldown-cmark parser for rendering markdown to HTML.
pub fn render_markdown(text: &str, smart_punctuation: bool) -> String {
    render_markdown_with_path(text, smart_punctuation, None)
}

/// Wrapper around for API compatibility.
pub fn render_markdown_with_path(
    text: &str,
    smart_punctuation: bool,
    path: Option<&Path>,
) -> String {
    render_markdown_with_path_and_redirects(text, smart_punctuation, path, &HashMap::new())
}

/// Creates a new pulldown-cmark parser of the given text.
pub fn new_cmark_parser(text: &str, smart_punctuation: bool) -> Parser<'_> {
    let mut opts = Options::empty();
    opts.insert(Options::ENABLE_TABLES);
    opts.insert(Options::ENABLE_FOOTNOTES);
    opts.insert(Options::ENABLE_STRIKETHROUGH);
    opts.insert(Options::ENABLE_TASKLISTS);
    opts.insert(Options::ENABLE_HEADING_ATTRIBUTES);
    if smart_punctuation {
        opts.insert(Options::ENABLE_SMART_PUNCTUATION);
    }
    Parser::new_ext(text, opts)
}

/// Renders markdown to HTML.
///
/// `path` is the path to the page being rendered relative to the root of the
/// book. This is used for the `print.html` page so that links on the print
/// page go to the anchors that has a path id prefix. Normal page rendering
/// sets `path` to None.
///
/// `redirects` is also only for the print page. It's for adjusting links to
/// a redirected location to go to the correct spot on the `print.html` page.
pub(crate) fn render_markdown_with_path_and_redirects(
    text: &str,
    smart_punctuation: bool,
    path: Option<&Path>,
    redirects: &HashMap<String, String>,
) -> String {
    let mut body = String::with_capacity(text.len() * 3 / 2);

    // Based on
    // https://github.com/pulldown-cmark/pulldown-cmark/blob/master/pulldown-cmark/examples/footnote-rewrite.rs

    // This handling of footnotes is a two-pass process. This is done to
    // support linkbacks, little arrows that allow you to jump back to the
    // footnote reference. The first pass collects the footnote definitions.
    // The second pass modifies those definitions to include the linkbacks,
    // and inserts the definitions back into the `events` list.

    // This is a map of name -> (number, count)
    // `name` is the name of the footnote.
    // `number` is the footnote number displayed in the output.
    // `count` is the number of references to this footnote (used for multiple
    // linkbacks, and checking for unused footnotes).
    let mut footnote_numbers = HashMap::new();
    // This is a map of name -> Vec<Event>
    // `name` is the name of the footnote.
    // The events list is the list of events needed to build the footnote definition.
    let mut footnote_defs = HashMap::new();

    // The following are used when currently processing a footnote definition.
    //
    // This is the name of the footnote (escaped).
    let mut in_footnote_name = String::new();
    // This is the list of events to build the footnote definition.
    let mut in_footnote = Vec::new();

    let events = new_cmark_parser(text, smart_punctuation)
        .map(clean_codeblock_headers)
        .map(|event| adjust_links(event, path, &redirects))
        .flat_map(|event| {
            let (a, b) = wrap_tables(event);
            a.into_iter().chain(b)
        })
        // Footnote rewriting must go last to ensure inner definition contents
        // are processed (since they get pulled out of the initial stream).
        .filter_map(|event| {
            match event {
                Event::Start(Tag::FootnoteDefinition(name)) => {
                    if !in_footnote.is_empty() {
                        log::warn!("internal bug: nested footnote not expected in {path:?}");
                    }
                    in_footnote_name = special_escape(&name);
                    None
                }
                Event::End(TagEnd::FootnoteDefinition) => {
                    let def_events = std::mem::take(&mut in_footnote);
                    let name = std::mem::take(&mut in_footnote_name);

                    if footnote_defs.contains_key(&name) {
                        log::warn!(
                            "footnote `{name}` in {} defined multiple times - \
                             not updating to new definition",
                            path.map_or_else(|| Cow::from("<unknown>"), |p| p.to_string_lossy())
                        );
                    } else {
                        footnote_defs.insert(name, def_events);
                    }
                    None
                }
                Event::FootnoteReference(name) => {
                    let name = special_escape(&name);
                    let len = footnote_numbers.len() + 1;
                    let (n, count) = footnote_numbers.entry(name.clone()).or_insert((len, 0));
                    *count += 1;
                    let html = Event::Html(
                        format!(
                            "<sup class=\"footnote-reference\" id=\"fr-{name}-{count}\">\
                                <a href=\"#footnote-{name}\">{n}</a>\
                             </sup>"
                        )
                        .into(),
                    );
                    if in_footnote_name.is_empty() {
                        Some(html)
                    } else {
                        // While inside a footnote, we need to accumulate.
                        in_footnote.push(html);
                        None
                    }
                }
                // While inside a footnote, accumulate all events into a local.
                _ if !in_footnote_name.is_empty() => {
                    in_footnote.push(event);
                    None
                }
                _ => Some(event),
            }
        });

    html::push_html(&mut body, events);

    if !footnote_defs.is_empty() {
        add_footnote_defs(
            &mut body,
            path,
            footnote_defs.into_iter().collect(),
            &footnote_numbers,
        );
    }

    body
}

/// Adds all footnote definitions into `body`.
fn add_footnote_defs(
    body: &mut String,
    path: Option<&Path>,
    mut defs: Vec<(String, Vec<Event<'_>>)>,
    numbers: &HashMap<String, (usize, u32)>,
) {
    // Remove unused.
    defs.retain(|(name, _)| {
        if !numbers.contains_key(name) {
            log::warn!(
                "footnote `{name}` in `{}` is defined but not referenced",
                path.map_or_else(|| Cow::from("<unknown>"), |p| p.to_string_lossy())
            );
            false
        } else {
            true
        }
    });

    let prefix = if let Some(path) = path {
        let mut base = path.display().to_string();
        if base.ends_with(".md") {
            base.truncate(base.len() - 3);
        }
        base = normalize_print_page_id(normalize_path(base));

        if base.is_empty() {
            String::new()
        } else {
            format!("{}-", base)
        }
    } else {
        String::new()
    };

    defs.sort_by_cached_key(|(name, _)| numbers[name].0);

    body.push_str(
        "<hr>\n\
         <ol class=\"footnote-definition\">",
    );

    // Insert the backrefs to the definition, and put the definitions in the output.
    for (name, mut fn_events) in defs {
        let count = numbers[&name].1;
        fn_events.insert(
            0,
            Event::Html(format!("<li id=\"footnote-{name}\">").into()),
        );
        // Generate the linkbacks.
        for usage in 1..=count {
            let nth = if usage == 1 {
                String::new()
            } else {
                usage.to_string()
            };
            let backlink =
                Event::Html(format!(" <a href=\"#{prefix}fr-{name}-{usage}\">↩{nth}</a>").into());
            if matches!(fn_events.last(), Some(Event::End(TagEnd::Paragraph))) {
                // Put the linkback at the end of the last paragraph instead
                // of on a line by itself.
                fn_events.insert(fn_events.len() - 1, backlink);
            } else {
                // Not a clear place to put it in this circumstance, so put it
                // at the end.
                fn_events.push(backlink);
            }
        }
        fn_events.push(Event::Html("</li>\n".into()));
        html::push_html(body, fn_events.into_iter());
    }

    body.push_str("</ol>");
}

/// Wraps tables in a `.table-wrapper` class to apply overflow-x rules to.
fn wrap_tables(event: Event<'_>) -> (Option<Event<'_>>, Option<Event<'_>>) {
    match event {
        Event::Start(Tag::Table(_)) => (
            Some(Event::Html(r#"<div class="table-wrapper">"#.into())),
            Some(event),
        ),
        Event::End(TagEnd::Table) => (Some(event), Some(Event::Html(r#"</div>"#.into()))),
        _ => (Some(event), None),
    }
}

fn clean_codeblock_headers(event: Event<'_>) -> Event<'_> {
    match event {
        Event::Start(Tag::CodeBlock(CodeBlockKind::Fenced(ref info))) => {
            let info: String = info
                .chars()
                .map(|x| match x {
                    ' ' | '\t' => ',',
                    _ => x,
                })
                .filter(|ch| !ch.is_whitespace())
                .collect();

            Event::Start(Tag::CodeBlock(CodeBlockKind::Fenced(CowStr::from(info))))
        }
        _ => event,
    }
}

/// Prints a "backtrace" of some `Error`.
pub fn log_backtrace(e: &Error) {
    error!("Error: {}", e);

    for cause in e.chain().skip(1) {
        error!("\tCaused By: {}", cause);
    }
}

pub(crate) fn special_escape(mut s: &str) -> String {
    let mut escaped = String::with_capacity(s.len());
    let needs_escape: &[char] = &['<', '>', '\'', '"', '\\', '&'];
    while let Some(next) = s.find(needs_escape) {
        escaped.push_str(&s[..next]);
        match s.as_bytes()[next] {
            b'<' => escaped.push_str("&lt;"),
            b'>' => escaped.push_str("&gt;"),
            b'\'' => escaped.push_str("&#39;"),
            b'"' => escaped.push_str("&quot;"),
            b'\\' => escaped.push_str("&#92;"),
            b'&' => escaped.push_str("&amp;"),
            _ => unreachable!(),
        }
        s = &s[next + 1..];
    }
    escaped.push_str(s);
    escaped
}

pub(crate) fn bracket_escape(mut s: &str) -> String {
    let mut escaped = String::with_capacity(s.len());
    let needs_escape: &[char] = &['<', '>'];
    while let Some(next) = s.find(needs_escape) {
        escaped.push_str(&s[..next]);
        match s.as_bytes()[next] {
            b'<' => escaped.push_str("&lt;"),
            b'>' => escaped.push_str("&gt;"),
            _ => unreachable!(),
        }
        s = &s[next + 1..];
    }
    escaped.push_str(s);
    escaped
}

#[cfg(test)]
mod tests {
    use super::{bracket_escape, special_escape};

    mod render_markdown {
        use super::super::render_markdown;

        #[test]
        fn preserves_external_links() {
            assert_eq!(
                render_markdown("[example](https://www.rust-lang.org/)", false),
                "<p><a href=\"https://www.rust-lang.org/\">example</a></p>\n"
            );
        }

        #[test]
        fn it_can_adjust_markdown_links() {
            assert_eq!(
                render_markdown("[example](example.md)", false),
                "<p><a href=\"example.html\">example</a></p>\n"
            );
            assert_eq!(
                render_markdown("[example_anchor](example.md#anchor)", false),
                "<p><a href=\"example.html#anchor\">example_anchor</a></p>\n"
            );

            // this anchor contains 'md' inside of it
            assert_eq!(
                render_markdown("[phantom data](foo.html#phantomdata)", false),
                "<p><a href=\"foo.html#phantomdata\">phantom data</a></p>\n"
            );
        }

        #[test]
        fn it_can_wrap_tables() {
            let src = r#"
| Original        | Punycode        | Punycode + Encoding |
|-----------------|-----------------|---------------------|
| føø             | f-5gaa          | f_5gaa              |
"#;
            let out = r#"
<div class="table-wrapper"><table><thead><tr><th>Original</th><th>Punycode</th><th>Punycode + Encoding</th></tr></thead><tbody>
<tr><td>føø</td><td>f-5gaa</td><td>f_5gaa</td></tr>
</tbody></table>
</div>
"#.trim();
            assert_eq!(render_markdown(src, false), out);
        }

        #[test]
        fn it_can_keep_quotes_straight() {
            assert_eq!(render_markdown("'one'", false), "<p>'one'</p>\n");
        }

        #[test]
        fn it_can_make_quotes_curly_except_when_they_are_in_code() {
            let input = r#"
'one'
```
'two'
```
`'three'` 'four'"#;
            let expected = r#"<p>‘one’</p>
<pre><code>'two'
</code></pre>
<p><code>'three'</code> ‘four’</p>
"#;
            assert_eq!(render_markdown(input, true), expected);
        }

        #[test]
        fn whitespace_outside_of_codeblock_header_is_preserved() {
            let input = r#"
some text with spaces
```rust
fn main() {
// code inside is unchanged
}
```
more text with spaces
"#;

            let expected = r#"<p>some text with spaces</p>
<pre><code class="language-rust">fn main() {
// code inside is unchanged
}
</code></pre>
<p>more text with spaces</p>
"#;
            assert_eq!(render_markdown(input, false), expected);
            assert_eq!(render_markdown(input, true), expected);
        }

        #[test]
        fn rust_code_block_properties_are_passed_as_space_delimited_class() {
            let input = r#"
```rust,no_run,should_panic,property_3
```
"#;

            let expected = r#"<pre><code class="language-rust,no_run,should_panic,property_3"></code></pre>
"#;
            assert_eq!(render_markdown(input, false), expected);
            assert_eq!(render_markdown(input, true), expected);
        }

        #[test]
        fn rust_code_block_properties_with_whitespace_are_passed_as_space_delimited_class() {
            let input = r#"
```rust,    no_run,,,should_panic , ,property_3
```
"#;

            let expected = r#"<pre><code class="language-rust,,,,,no_run,,,should_panic,,,,property_3"></code></pre>
"#;
            assert_eq!(render_markdown(input, false), expected);
            assert_eq!(render_markdown(input, true), expected);
        }

        #[test]
        fn rust_code_block_without_properties_has_proper_html_class() {
            let input = r#"
```rust
```
"#;

            let expected = r#"<pre><code class="language-rust"></code></pre>
"#;
            assert_eq!(render_markdown(input, false), expected);
            assert_eq!(render_markdown(input, true), expected);

            let input = r#"
```rust
```
"#;
            assert_eq!(render_markdown(input, false), expected);
            assert_eq!(render_markdown(input, true), expected);
        }
    }

    #[allow(deprecated)]
    mod id_from_content {
        use super::super::id_from_content;

        #[test]
        fn it_generates_anchors() {
            assert_eq!(
                id_from_content("## Method-call expressions"),
                "method-call-expressions"
            );
            assert_eq!(id_from_content("## **Bold** title"), "bold-title");
            assert_eq!(id_from_content("## `Code` title"), "code-title");
            assert_eq!(
                id_from_content("## title <span dir=rtl>foo</span>"),
                "title-foo"
            );
        }

        #[test]
        fn it_generates_anchors_from_non_ascii_initial() {
            assert_eq!(
                id_from_content("## `--passes`: add more rustdoc passes"),
                "--passes-add-more-rustdoc-passes"
            );
            assert_eq!(
                id_from_content("## 中文標題 CJK title"),
                "中文標題-cjk-title"
            );
            assert_eq!(id_from_content("## Über"), "Über");
        }
    }

    mod html_munging {
        use super::super::{normalize_id, unique_id_from_content};

        #[test]
        fn it_normalizes_ids() {
            assert_eq!(
                normalize_id("`--passes`: add more rustdoc passes"),
                "--passes-add-more-rustdoc-passes"
            );
            assert_eq!(
                normalize_id("Method-call 🐙 expressions \u{1f47c}"),
                "method-call--expressions-"
            );
            assert_eq!(normalize_id("_-_12345"), "_-_12345");
            assert_eq!(normalize_id("12345"), "12345");
            assert_eq!(normalize_id("中文"), "中文");
            assert_eq!(normalize_id("にほんご"), "にほんご");
            assert_eq!(normalize_id("한국어"), "한국어");
            assert_eq!(normalize_id(""), "");
        }

        #[test]
        fn it_generates_unique_ids_from_content() {
            // Same id if not given shared state
            assert_eq!(
                unique_id_from_content("## 中文標題 CJK title", &mut Default::default()),
                "中文標題-cjk-title"
            );
            assert_eq!(
                unique_id_from_content("## 中文標題 CJK title", &mut Default::default()),
                "中文標題-cjk-title"
            );

            // Different id if given shared state
            let mut id_counter = Default::default();
            assert_eq!(unique_id_from_content("## Über", &mut id_counter), "Über");
            assert_eq!(
                unique_id_from_content("## 中文標題 CJK title", &mut id_counter),
                "中文標題-cjk-title"
            );
            assert_eq!(unique_id_from_content("## Über", &mut id_counter), "Über-1");
            assert_eq!(unique_id_from_content("## Über", &mut id_counter), "Über-2");
        }
    }

    #[test]
    fn escaped_brackets() {
        assert_eq!(bracket_escape(""), "");
        assert_eq!(bracket_escape("<"), "&lt;");
        assert_eq!(bracket_escape(">"), "&gt;");
        assert_eq!(bracket_escape("<>"), "&lt;&gt;");
        assert_eq!(bracket_escape("<test>"), "&lt;test&gt;");
        assert_eq!(bracket_escape("a<test>b"), "a&lt;test&gt;b");
        assert_eq!(bracket_escape("'"), "'");
        assert_eq!(bracket_escape("\\"), "\\");
    }

    #[test]
    fn escaped_special() {
        assert_eq!(special_escape(""), "");
        assert_eq!(special_escape("<"), "&lt;");
        assert_eq!(special_escape(">"), "&gt;");
        assert_eq!(special_escape("<>"), "&lt;&gt;");
        assert_eq!(special_escape("<test>"), "&lt;test&gt;");
        assert_eq!(special_escape("a<test>b"), "a&lt;test&gt;b");
        assert_eq!(special_escape("'"), "&#39;");
        assert_eq!(special_escape("\\"), "&#92;");
        assert_eq!(special_escape("&"), "&amp;");
    }
}
