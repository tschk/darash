//! Deterministic main-content selection without a browser or extra dependencies.
//!
//! Semantic main/article landmarks win over a conservative prose heuristic.
//! This is extraction, not a universal readability classifier. The caller must
//! explicitly choose whether an unrecognized page falls back to the document.

use scraper::{ElementRef, Html, Selector};

/// What to do when no main-content candidate is recognized.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub enum MainContentFallback {
    #[default]
    Error,
    /// Use the original, unmodified HTML, including navigation and other clutter.
    FullDocument,
}

/// How content was selected; also makes fallback visible to callers.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum MainContentSource {
    Main,
    Article,
    Prose,
    FullDocument,
}

#[derive(Clone, Debug)]
pub struct MainContent {
    pub html: String,
    pub source: MainContentSource,
}

#[derive(Debug, thiserror::Error)]
#[error("no main content found; choose FullDocument fallback or render the full document")]
pub struct MainContentError;

/// Select main content and remove navigation, forms, complementary content and
/// explicitly hidden nodes. Links, headings, code, lists and tables retain their
/// HTML, so the existing [`crate::fetch`] renderers can preserve their structure.
/// Ties select the first candidate in document order. No scripts execute.
pub fn extract_main_content(
    html: &str,
    fallback: MainContentFallback,
) -> Result<MainContent, MainContentError> {
    let mut document = Html::parse_document(html);
    let all = Selector::parse("*").map_err(|_| MainContentError)?;
    let removed: Vec<_> = document
        .select(&all)
        .filter(|element| is_clutter(*element))
        .map(|element| element.id())
        .collect();
    for id in removed {
        if let Some(mut node) = document.tree.get_mut(id) {
            node.detach();
        }
    }

    let links = Selector::parse("a").map_err(|_| MainContentError)?;
    let paragraphs = Selector::parse("p").map_err(|_| MainContentError)?;

    for (selector, source) in [
        ("main, [role='main']", MainContentSource::Main),
        ("article, [role='article']", MainContentSource::Article),
        ("section, div", MainContentSource::Prose),
    ] {
        let selector = Selector::parse(selector).map_err(|_| MainContentError)?;
        let mut best = None;
        let mut best_score = 0;
        // Html::select visits detached nodes too; traverse only the live subtree.
        for candidate in document.root_element().select(&selector) {
            let text = candidate.text().collect::<String>();
            let length = text.split_whitespace().map(str::len).sum::<usize>();
            let link_length = candidate
                .select(&links)
                .flat_map(|link| link.text())
                .map(str::len)
                .sum::<usize>();
            let score = length.saturating_sub(link_length);
            // Semantic landmarks may contain a short title, code, or a table.
            // Unmarked content must contain substantial prose in two paragraphs.
            if source == MainContentSource::Prose
                && (score < 160 || candidate.select(&paragraphs).count() < 2)
            {
                continue;
            }
            if !text.trim().is_empty() && (best.is_none() || score > best_score) {
                best = Some(candidate);
                best_score = score;
            }
        }
        if let Some(candidate) = best {
            return Ok(MainContent {
                html: candidate.html(),
                source,
            });
        }
    }
    match fallback {
        MainContentFallback::Error => Err(MainContentError),
        MainContentFallback::FullDocument => Ok(MainContent {
            html: html.to_owned(),
            source: MainContentSource::FullDocument,
        }),
    }
}

fn is_clutter(element: ElementRef<'_>) -> bool {
    let value = element.value();
    if value.attr("hidden").is_some()
        || value
            .attr("aria-hidden")
            .is_some_and(|value| value.eq_ignore_ascii_case("true"))
    {
        return true;
    }
    let style = value.attr("style").unwrap_or_default();
    if style.split(';').any(|declaration| {
        declaration.split_once(':').is_some_and(|(name, value)| {
            let name = name.trim();
            let value = value.trim().trim_end_matches("!important").trim();
            (name.eq_ignore_ascii_case("display") && value.eq_ignore_ascii_case("none"))
                || (name.eq_ignore_ascii_case("visibility") && value.eq_ignore_ascii_case("hidden"))
        })
    }) {
        return true;
    }
    if matches!(
        value.name(),
        "nav"
            | "aside"
            | "footer"
            | "form"
            | "script"
            | "style"
            | "noscript"
            | "template"
            | "iframe"
            | "canvas"
            | "head"
    ) {
        return true;
    }
    if value.attr("role").is_some_and(|role| {
        matches!(
            role.to_ascii_lowercase().as_str(),
            "navigation" | "banner" | "contentinfo" | "complementary" | "search" | "dialog"
        )
    }) {
        return true;
    }
    // Keep article headings, including an article's own header.
    value.name() == "header"
        && !element
            .ancestors()
            .filter_map(ElementRef::wrap)
            .any(|parent| {
                matches!(parent.value().name(), "main" | "article")
                    || matches!(parent.value().attr("role"), Some("main" | "article"))
            })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::fetch::{to_markdown, to_text};

    #[test]
    fn landmarks_remove_clutter_and_preserve_article_structure() {
        let html = include_str!("../tests/fixtures/main-content.html");
        let content = extract_main_content(html, MainContentFallback::Error).unwrap();
        assert_eq!(content.source, MainContentSource::Main);
        let markdown = to_markdown(&content.html);
        for preserved in [
            "# A useful guide",
            "[reference](/reference)",
            "`cargo test`",
            "```\nlet x = 1;\n```",
            "| Name | Value |",
            "| A | 1 |",
        ] {
            assert!(
                markdown.contains(preserved),
                "missing {preserved}: {markdown}"
            );
        }
        for clutter in [
            "Navigation",
            "Sign in",
            "Related",
            "Hidden",
            "Footer",
            "Dialog",
        ] {
            assert!(!markdown.contains(clutter), "retained {clutter}");
        }
        assert!(
            to_markdown(html).contains("Navigation"),
            "legacy full output unchanged"
        );
        assert!(to_text(&content.html).contains("A useful guide"));
    }

    #[test]
    fn accessibility_landmarks_hidden_ancestors_and_ties() {
        let html = "<div hidden><main>Hidden main</main></div><div role='navigation'>Menu</div><div role='main'><h1>First</h1><p>Text</p></div><main><h1>Other</h1><p>Text</p></main>";
        let content = extract_main_content(html, MainContentFallback::Error).unwrap();
        assert!(content.html.contains("First"));
        assert!(!content.html.contains("Hidden main"));
        assert!(!content.html.contains("Other"));
    }

    #[test]
    fn article_and_conservative_prose_selection() {
        let article = extract_main_content(
            "<article><header><h1>Title</h1></header><p>Short</p></article>",
            MainContentFallback::Error,
        )
        .unwrap();
        assert_eq!(article.source, MainContentSource::Article);
        assert!(article.html.contains("Title"));
        let prose = format!(
            "<nav>Menu</nav><div><p>{}</p><p>{}</p></div>",
            "First sentence. ".repeat(10),
            "Second sentence. ".repeat(10)
        );
        assert_eq!(
            extract_main_content(&prose, MainContentFallback::Error)
                .unwrap()
                .source,
            MainContentSource::Prose
        );
    }

    #[test]
    fn fallback_is_explicit_and_preserves_the_original_document() {
        for html in ["", "<nav>Only links</nav>", "<div>Small shell</div>"] {
            assert!(extract_main_content(html, MainContentFallback::Error).is_err());
            let content = extract_main_content(html, MainContentFallback::FullDocument).unwrap();
            assert_eq!(content.source, MainContentSource::FullDocument);
            assert_eq!(content.html, html);
        }
    }
}
