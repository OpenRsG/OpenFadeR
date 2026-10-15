//! The minimal XML reader the smart lists, XML libraries, sampler packs and iTunes plists share.

use std::collections::HashMap;

/// Whether a [`Tag`] opens, closes or is self-closing.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Edge {
    Open,
    Close,
    Empty,
}

pub struct Tag {
    pub name: String,
    pub attrs: HashMap<String, String>,
    pub edge: Edge,
    /// Unescaped text between this tag and the next one; empty when blank.
    pub text: String,
}

/// Every element tag in document order: comments and `<?…?>` / `<!…>` are skipped; attribute
/// values and the text after a tag are unescaped.
pub fn tags(mut s: &str) -> Result<Vec<Tag>, String> {
    let mut out = Vec::new();
    while let Some(i) = s.find('<') {
        s = &s[i + 1..];
        // `>` ends the tag unless it sits inside a quoted attribute value.
        let (mut quote, mut end) = (None, None);
        for (j, ch) in s.char_indices() {
            match (quote, ch) {
                (None, '>') => {
                    end = Some(j);
                    break;
                }
                (None, '"' | '\'') => quote = Some(ch),
                (Some(q), c) if c == q => quote = None,
                _ => {}
            }
        }
        let end = end.ok_or("unterminated tag")?;
        let body = &s[..end];
        s = &s[end + 1..];
        if body.starts_with(['?', '!']) {
            continue;
        }
        let text = &s[..s.find('<').unwrap_or(s.len())];
        let text = if text.trim().is_empty() {
            String::new()
        } else {
            unescape(text)
        };
        if let Some(name) = body.strip_prefix('/') {
            out.push(Tag {
                name: name.trim().to_owned(),
                attrs: HashMap::new(),
                edge: Edge::Close,
                text,
            });
            continue;
        }
        let (body, edge) = body
            .strip_suffix('/')
            .map_or((body, Edge::Open), |b| (b, Edge::Empty));
        let (name, mut rest) = body.split_once(char::is_whitespace).unwrap_or((body, ""));
        let mut attrs = HashMap::new();
        while !rest.trim().is_empty() {
            let (k, r) = rest
                .split_once('=')
                .ok_or_else(|| format!("bad attribute in <{name}>"))?;
            let r = r.trim_start();
            let q = r
                .chars()
                .next()
                .filter(|c| matches!(c, '"' | '\''))
                .ok_or_else(|| format!("unquoted attribute {}", k.trim()))?;
            let r = &r[1..];
            let e = r.find(q).ok_or("unterminated attribute value")?;
            attrs.insert(k.trim().to_string(), unescape(&r[..e]));
            rest = &r[e + 1..];
        }
        out.push(Tag {
            name: name.to_string(),
            attrs,
            edge,
            text,
        });
    }
    Ok(out)
}

/// The five XML entities plus `&#N;` / `&#xH;`; anything else is kept verbatim.
fn unescape(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut r = s;
    while let Some(i) = r.find('&') {
        out.push_str(&r[..i]);
        r = &r[i..];
        let Some(j) = r.find(';') else { break };
        let ent = &r[1..j];
        let c = match ent {
            "lt" => Some('<'),
            "gt" => Some('>'),
            "amp" => Some('&'),
            "quot" => Some('"'),
            "apos" => Some('\''),
            _ => ent
                .strip_prefix('#')
                .and_then(|n| match n.strip_prefix(['x', 'X']) {
                    Some(h) => u32::from_str_radix(h, 16).ok(),
                    None => n.parse().ok(),
                })
                .and_then(char::from_u32),
        };
        match c {
            Some(c) => out.push(c),
            None => out.push_str(&r[..=j]),
        }
        r = &r[j + 1..];
    }
    out.push_str(r);
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn xml_tags_and_entities() {
        let t = tags(
            "<?xml version=\"1.0\"?><!-- c --><A x='1 > 0' y=\"&lt;&amp;\"><B/><C z=\"q\" /></A>",
        )
        .unwrap();
        let got: Vec<_> = t.iter().map(|t| (t.name.as_str(), t.edge)).collect();
        assert_eq!(
            got,
            [
                ("A", Edge::Open),
                ("B", Edge::Empty),
                ("C", Edge::Empty),
                ("A", Edge::Close)
            ]
        );
        assert_eq!(
            (t[0].attrs["x"].as_str(), t[0].attrs["y"].as_str()),
            ("1 > 0", "<&")
        );
        assert!(tags("<A").is_err() && tags("<A b=1>").is_err());
        assert_eq!(unescape("&#65;&#x42;&bogus;&"), "AB&bogus;&");
    }
}
