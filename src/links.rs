//! Pull the anchors out of an HTML page, so a fetch whose reader drops links
//! (Exa returns plain text) can still show where a page's downloads point.

use std::collections::HashSet;

/// Most links appended to a fetch; a navigation-heavy page can carry hundreds.
pub const MAX_LINKS: usize = 300;

/// Anchors of `html` as (text, absolute URL), in page order, one per URL.
/// Relative hrefs resolve against `base` (the page's final URL, or its
/// `<base href>` when present).
/// Fragment-only, `javascript:`, `mailto:`, `tel:` and `data:` targets are
/// dropped, as is the fragment of every kept URL.
pub fn html_links(html: &str, base: &str) -> Vec<(String, String)> {
    let lower = html.to_ascii_lowercase();
    let base = base_href(html, &lower)
        .and_then(|b| url::Url::parse(base).ok()?.join(&b).ok())
        .or_else(|| url::Url::parse(base).ok());
    let mut out = Vec::new();
    let mut seen = HashSet::new();
    let mut pos = 0;
    while let Some(i) = lower[pos..].find("<a") {
        let after = pos + i + 2;
        // `<a ` or `<a>` only, not `<abbr>` or `<area>`.
        if !lower[after..].starts_with(|c: char| c.is_ascii_whitespace() || c == '>') {
            pos = after;
            continue;
        }
        let Some(rel) = lower[after..].find('>') else { break };
        let tag_end = after + rel;
        let close = lower[tag_end..].find("</a").map_or(tag_end + 1, |j| tag_end + j);
        pos = close.max(tag_end + 1);
        let Some(href) = attr(&html[after..tag_end], "href") else {
            continue;
        };
        let Some(target) = resolve(base.as_ref(), &decode_entities(href.trim())) else {
            continue;
        };
        if seen.insert(target.clone()) {
            let inner = html.get(tag_end + 1..close).unwrap_or("");
            out.push((collapse(&decode_entities(&strip_tags(inner))), target));
        }
    }
    out
}

/// Render links as a markdown section to append to a fetched page, capped at
/// `MAX_LINKS`; empty when there is nothing to add.
pub fn links_section(links: &[(String, String)]) -> String {
    if links.is_empty() {
        return String::new();
    }
    let mut out = String::from("\n\n## Links on the page\n\n");
    for (text, href) in links.iter().take(MAX_LINKS) {
        if text.is_empty() {
            out.push_str(&format!("- <{href}>\n"));
        } else {
            out.push_str(&format!("- [{}]({href})\n", text.replace(['[', ']'], "")));
        }
    }
    if links.len() > MAX_LINKS {
        out.push_str(&format!(
            "\n[{} more links not shown; `web fetch --raw` has the full HTML.]\n",
            links.len() - MAX_LINKS
        ));
    }
    out
}

fn base_href(html: &str, lower: &str) -> Option<String> {
    let i = lower.find("<base")?;
    let end = i + lower[i..].find('>')?;
    attr(&html[i + 5..end], "href").map(|h| decode_entities(h.trim()))
}

fn resolve(base: Option<&url::Url>, href: &str) -> Option<String> {
    let scheme = href.split(':').next().unwrap_or("").to_ascii_lowercase();
    if href.is_empty()
        || href.starts_with('#')
        || matches!(scheme.as_str(), "javascript" | "mailto" | "tel" | "data")
    {
        return None;
    }
    let mut target = match base {
        Some(b) => b.join(href).ok()?,
        None => url::Url::parse(href).ok()?,
    };
    if !matches!(target.scheme(), "http" | "https") {
        return None;
    }
    target.set_fragment(None);
    Some(target.to_string())
}

/// Value of attribute `name` inside a tag's attribute text, quoted or bare.
fn attr<'a>(tag: &'a str, name: &str) -> Option<&'a str> {
    let lower = tag.to_ascii_lowercase();
    let mut from = 0;
    while let Some(i) = lower[from..].find(name) {
        let s = from + i;
        from = s + name.len();
        let preceded = s == 0 || lower.as_bytes()[s - 1].is_ascii_whitespace();
        let rest = lower[from..].trim_start();
        if !preceded || !rest.starts_with('=') {
            continue;
        }
        let value = tag[lower.len() - rest.len() + 1..].trim_start();
        let start = tag.len() - value.len();
        return match value.chars().next() {
            Some(q @ ('"' | '\'')) => tag[start + 1..]
                .find(q)
                .map(|e| &tag[start + 1..start + 1 + e]),
            Some(_) => value.split(|c: char| c.is_ascii_whitespace()).next(),
            None => None,
        };
    }
    None
}

fn strip_tags(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut in_tag = false;
    for c in s.chars() {
        match c {
            '<' => in_tag = true,
            '>' if in_tag => {
                in_tag = false;
                out.push(' ');
            }
            _ if !in_tag => out.push(c),
            _ => {}
        }
    }
    out
}

fn decode_entities(s: &str) -> String {
    s.replace("&nbsp;", " ")
        .replace("&lt;", "<")
        .replace("&gt;", ">")
        .replace("&quot;", "\"")
        .replace("&#39;", "'")
        .replace("&#x27;", "'")
        .replace("&amp;", "&")
}

fn collapse(s: &str) -> String {
    s.split_whitespace().collect::<Vec<_>>().join(" ")
}

#[cfg(test)]
mod tests {
    use super::*;

    const BASE: &str = "https://www.example.de/Datenportal/Anwendungen/Kaufpreis/Kaufpreis.html";

    #[test]
    fn resolves_relative_download_link_and_decodes_amp() {
        // The shape of the BMF page whose .xlsx link Exa's text dropped.
        let html = r#"<p><a href="Kaufpreis-Arbeitshilfe.xlsx?__blob=publicationFile&amp;v=9" class="dl">
            <span>Arbeitshilfe</span> (xlsx)</a></p>"#;
        assert_eq!(
            html_links(html, BASE),
            vec![(
                "Arbeitshilfe (xlsx)".to_string(),
                "https://www.example.de/Datenportal/Anwendungen/Kaufpreis/Kaufpreis-Arbeitshilfe.xlsx?__blob=publicationFile&v=9"
                    .to_string()
            )]
        );
    }

    #[test]
    fn drops_fragments_scripts_mail_and_duplicates() {
        let html = r##"<a href="#top">Top</a><A HREF='/a.pdf'>A</A><a href="/a.pdf#p2">A again</a>
            <a href="javascript:void(0)">x</a><a href="mailto:x@y.de">m</a><a name="anchor">no href</a>"##;
        assert_eq!(
            html_links(html, BASE),
            vec![("A".to_string(), "https://www.example.de/a.pdf".to_string())]
        );
    }

    #[test]
    fn ignores_abbr_and_area_and_reads_bare_href() {
        let html = "<abbr title=x>EStG</abbr><area href=/map.html><a href=/b.html>B</a>";
        assert_eq!(
            html_links(html, BASE),
            vec![("B".to_string(), "https://www.example.de/b.html".to_string())]
        );
    }

    #[test]
    fn base_href_wins_over_page_url() {
        let html = r#"<head><base href="https://cdn.example.org/files/"></head><a href="c.xlsx">C</a>"#;
        assert_eq!(html_links(html, BASE)[0].1, "https://cdn.example.org/files/c.xlsx");
    }

    #[test]
    fn data_href_attribute_is_not_href() {
        let html = r#"<a data-href="/wrong" href="/right">R</a>"#;
        assert_eq!(html_links(html, BASE)[0].1, "https://www.example.de/right");
    }

    #[test]
    fn non_ascii_text_survives() {
        let html = "<a href=\"/g.pdf\">Grundstücke – Anleitung</a>";
        assert_eq!(html_links(html, BASE)[0].0, "Grundstücke – Anleitung");
    }

    #[test]
    fn section_caps_and_strips_brackets() {
        assert_eq!(links_section(&[]), "");
        let one = links_section(&[("[x] y".into(), "https://e.de/".into())]);
        assert!(one.contains("- [x y](https://e.de/)"));
        let many: Vec<_> = (0..MAX_LINKS + 2)
            .map(|i| (String::new(), format!("https://e.de/{i}")))
            .collect();
        let s = links_section(&many);
        assert_eq!(s.matches("\n- <").count(), MAX_LINKS);
        assert!(s.contains("[2 more links not shown"));
    }
}
