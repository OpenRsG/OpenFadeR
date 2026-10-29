//! A small XML tree: parse a file tolerantly (declaration, comments, CDATA and text are skipped, entities in attribute values
//! are decoded, attribute order is kept) and write it back, so a collection written by Traktor itself survives a merge with
//! everything this plugin does not touch unchanged.

/// One element: name, attributes in file order, child elements (text is not kept: an NML has none).
#[derive(Clone, Debug, PartialEq)]
pub struct El {
    pub name: String,
    pub attrs: Vec<(String, String)>,
    pub kids: Vec<El>,
}

impl El {
    pub fn new(name: &str) -> Self {
        Self {
            name: name.into(),
            attrs: Vec::new(),
            kids: Vec::new(),
        }
    }

    /// Builder: `self` with attribute `key` (replaced when present).
    pub fn attr(mut self, key: &str, value: impl ToString) -> Self {
        self.set(key, value);
        self
    }

    /// Builder: `self` with `kid` appended.
    pub fn with(mut self, kid: El) -> Self {
        self.kids.push(kid);
        self
    }

    pub fn get(&self, key: &str) -> Option<&str> {
        self.attrs
            .iter()
            .find(|(k, _)| k == key)
            .map(|(_, v)| v.as_str())
    }

    pub fn set(&mut self, key: &str, value: impl ToString) {
        let value = value.to_string();
        match self.attrs.iter_mut().find(|(k, _)| k == key) {
            Some(a) => a.1 = value,
            None => self.attrs.push((key.into(), value)),
        }
    }

    pub fn child(&self, name: &str) -> Option<&El> {
        self.kids.iter().find(|k| k.name == name)
    }

    /// The child `name`, appended empty when there is none.
    pub fn child_mut(&mut self, name: &str) -> &mut El {
        let at = self.kids.iter().position(|k| k.name == name);
        let at = at.unwrap_or_else(|| {
            self.kids.push(El::new(name));
            self.kids.len() - 1
        });
        &mut self.kids[at]
    }

    /// Serialized as a document: XML declaration, then the element tree indented by two spaces.
    pub fn document(&self) -> String {
        let mut out =
            String::from("<?xml version=\"1.0\" encoding=\"UTF-8\" standalone=\"no\" ?>\n");
        self.emit(0, &mut out);
        out
    }

    fn emit(&self, depth: usize, out: &mut String) {
        let pad = "  ".repeat(depth);
        out.push_str(&format!("{pad}<{}", self.name));
        for (k, v) in &self.attrs {
            out.push_str(&format!(" {k}=\"{}\"", escape(v)));
        }
        if self.kids.is_empty() {
            out.push_str("/>\n");
            return;
        }
        out.push_str(">\n");
        for k in &self.kids {
            k.emit(depth + 1, out);
        }
        out.push_str(&format!("{pad}</{}>\n", self.name));
    }
}

/// `s` as an attribute value: the five entities, line breaks and tabs as character references, other control characters
/// (illegal in XML 1.0) dropped.
fn escape(s: &str) -> String {
    let mut o = String::with_capacity(s.len());
    for c in s.chars() {
        match c {
            '&' => o.push_str("&amp;"),
            '<' => o.push_str("&lt;"),
            '>' => o.push_str("&gt;"),
            '"' => o.push_str("&quot;"),
            '\'' => o.push_str("&apos;"),
            '\n' | '\r' | '\t' => o.push_str(&format!("&#{};", c as u32)),
            c if (c as u32) < 0x20 => {}
            c => o.push(c),
        }
    }
    o
}

/// The five entities plus `&#N;` / `&#xH;`; anything else is kept verbatim.
fn unescape(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut rest = s;
    while let Some(i) = rest.find('&') {
        out.push_str(&rest[..i]);
        rest = &rest[i..];
        let decoded = rest.find(';').and_then(|e| {
            let name = &rest[1..e];
            let c = match name {
                "amp" => Some('&'),
                "lt" => Some('<'),
                "gt" => Some('>'),
                "quot" => Some('"'),
                "apos" => Some('\''),
                _ => name.strip_prefix('#').and_then(|n| {
                    match n.strip_prefix(['x', 'X']) {
                        Some(h) => u32::from_str_radix(h, 16),
                        None => n.parse(),
                    }
                    .ok()
                    .and_then(char::from_u32)
                }),
            };
            c.map(|c| (c, e + 1))
        });
        match decoded {
            Some((c, n)) => {
                out.push(c);
                rest = &rest[n..];
            }
            None => {
                out.push('&');
                rest = &rest[1..];
            }
        }
    }
    out.push_str(rest);
    out
}

/// Index of the `>` that ends the tag at the start of `s`, ignoring any inside a quoted attribute value.
fn tag_end(s: &str) -> Result<usize, String> {
    let mut quote = None;
    for (i, c) in s.char_indices() {
        match (quote, c) {
            (None, '>') => return Ok(i),
            (None, '"' | '\'') => quote = Some(c),
            (Some(q), c) if c == q => quote = None,
            _ => {}
        }
    }
    Err("unterminated tag".into())
}

/// The start of an element (`name a="1" b='2'`, no angle brackets, no trailing slash).
fn open(body: &str) -> Result<El, String> {
    let body = body.trim();
    let (name, mut rest) = body.split_once(char::is_whitespace).unwrap_or((body, ""));
    if name.is_empty() {
        return Err("tag without a name".into());
    }
    let mut el = El::new(name);
    loop {
        rest = rest.trim_start();
        if rest.is_empty() {
            return Ok(el);
        }
        let bad = || format!("bad attribute in <{name}>");
        let (key, value) = rest.split_once('=').ok_or_else(bad)?;
        let value = value.trim_start();
        let quote = value
            .chars()
            .next()
            .filter(|c| matches!(c, '"' | '\''))
            .ok_or_else(bad)?;
        let value = &value[1..];
        let end = value.find(quote).ok_or_else(bad)?;
        el.attrs.push((key.trim().into(), unescape(&value[..end])));
        rest = &value[end + 1..];
    }
}

/// The root element of `s`; `Err` on a document that is not well-formed (unterminated or unbalanced tags).
pub fn parse(s: &str) -> Result<El, String> {
    let (mut stack, mut root): (Vec<El>, Option<El>) = (Vec::new(), None);
    let mut rest = s;
    'tags: while let Some(i) = rest.find('<') {
        rest = &rest[i + 1..];
        for (start, end) in [("!--", "-->"), ("![CDATA[", "]]>")] {
            if let Some(r) = rest.strip_prefix(start) {
                let e = r.find(end).ok_or("unterminated comment or CDATA")?;
                rest = &r[e + end.len()..];
                continue 'tags;
            }
        }
        let end = tag_end(rest)?;
        let body = &rest[..end];
        rest = &rest[end + 1..];
        if body.starts_with(['?', '!']) {
            continue;
        }
        let done = if let Some(name) = body.strip_prefix('/') {
            let el = stack.pop().ok_or("closing tag without an opening one")?;
            if el.name != name.trim() {
                return Err(format!("<{}> closed by </{}>", el.name, name.trim()));
            }
            el
        } else if let Some(body) = body.strip_suffix('/') {
            open(body)?
        } else {
            stack.push(open(body)?);
            continue;
        };
        match stack.last_mut() {
            Some(parent) => parent.kids.push(done),
            None => root = Some(done),
        }
    }
    match (stack.last(), root) {
        (Some(el), _) => Err(format!("<{}> is never closed", el.name)),
        (None, root) => root.ok_or_else(|| "no root element".into()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn round_trips_entities_comments_and_order() {
        let src = "<?xml version=\"1.0\"?>\n<!-- c --><A z=\"1\" a='x &amp; &quot;y&quot; &#13;&#10;&#x41;'>\n<B/><![CDATA[<x>]]><C k=\"a/b>\"></C></A>";
        let el = parse(src).unwrap();
        assert_eq!(el.attrs[0], ("z".into(), "1".into()));
        assert_eq!(el.get("a"), Some("x & \"y\" \r\nA"));
        assert_eq!(el.kids.len(), 2);
        assert_eq!(el.kids[1].get("k"), Some("a/b>"));
        assert_eq!(parse(&el.document()).unwrap(), el);
        assert!(el
            .document()
            .contains("a=\"x &amp; &quot;y&quot; &#13;&#10;A\""));
    }

    #[test]
    fn rejects_broken_documents() {
        for bad in ["<A><B></A>", "<A>", "</A>", "<A b=1/>", "<A", "text"] {
            assert!(parse(bad).is_err(), "{bad}");
        }
    }
}
