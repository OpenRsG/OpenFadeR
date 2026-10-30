//! A small XML tree for `database.xml` and `.vdjfolder` files. Parsing is tolerant (BOM, CRLF, comments, processing
//! instructions, CDATA, single or double quoted attributes, any element a newer VirtualDJ adds); [`split`] cuts a database into
//! its top-level items so those this plugin does not touch are written back byte for byte.

/// A child of an element.
#[derive(Clone, Debug, PartialEq)]
pub enum Node {
    El(El),
    Text(String),
}

/// An element: attributes keep their order.
#[derive(Clone, Debug, PartialEq)]
pub struct El {
    pub name: String,
    pub attrs: Vec<(String, String)>,
    pub kids: Vec<Node>,
}

impl El {
    pub fn new(name: &str) -> Self {
        Self {
            name: name.into(),
            attrs: Vec::new(),
            kids: Vec::new(),
        }
    }

    pub fn attr(&self, key: &str) -> Option<&str> {
        self.attrs
            .iter()
            .find(|(k, _)| k == key)
            .map(|(_, v)| v.as_str())
    }

    /// Replaces the attribute, or appends it.
    pub fn set(&mut self, key: &str, value: impl Into<String>) {
        let value = value.into();
        match self.attrs.iter_mut().find(|(k, _)| k == key) {
            Some((_, v)) => *v = value,
            None => self.attrs.push((key.into(), value)),
        }
    }

    /// Sets the attribute unless it is there.
    pub fn set_new(&mut self, key: &str, value: impl Into<String>) {
        if self.attr(key).is_none() {
            self.set(key, value);
        }
    }

    pub fn children<'a>(&'a self, name: &'a str) -> impl Iterator<Item = &'a El> {
        self.kids.iter().filter_map(move |n| match n {
            Node::El(e) if e.name == name => Some(e),
            _ => None,
        })
    }

    /// The first child `name`, appended empty when there is none.
    pub fn child_mut(&mut self, name: &str) -> &mut El {
        let at = self
            .kids
            .iter()
            .position(|n| matches!(n, Node::El(e) if e.name == name))
            .unwrap_or_else(|| {
                self.kids.push(Node::El(El::new(name)));
                self.kids.len() - 1
            });
        match &mut self.kids[at] {
            Node::El(e) => e,
            Node::Text(_) => unreachable!("position found an element"),
        }
    }

    /// The text of the element (its text children joined).
    pub fn text(&self) -> String {
        self.kids
            .iter()
            .filter_map(|n| match n {
                Node::Text(t) => Some(t.as_str()),
                Node::El(_) => None,
            })
            .collect()
    }

    pub fn set_text(&mut self, text: &str) {
        self.kids = vec![Node::Text(text.into())];
    }

    /// `<name a="b"/>`, `<name>text</name>` or the children one per line, `depth` spaces deep.
    pub fn write(&self, depth: usize, out: &mut String) {
        out.push_str(&" ".repeat(depth));
        out.push('<');
        out.push_str(&self.name);
        for (k, v) in &self.attrs {
            out.push_str(&format!(" {k}=\"{}\"", escape(v, true)));
        }
        let kids: Vec<&Node> = self
            .kids
            .iter()
            .filter(|n| !matches!(n, Node::Text(t) if t.trim().is_empty()))
            .collect();
        if kids.is_empty() {
            out.push_str("/>");
        } else if kids.iter().all(|n| matches!(n, Node::Text(_))) {
            out.push('>');
            for n in kids {
                if let Node::Text(t) = n {
                    out.push_str(&escape(t, false));
                }
            }
            out.push_str(&format!("</{}>", self.name));
        } else {
            out.push('>');
            for n in kids {
                out.push('\n');
                match n {
                    Node::El(e) => e.write(depth + 1, out),
                    Node::Text(t) => {
                        out.push_str(&" ".repeat(depth + 1));
                        out.push_str(&escape(t, false));
                    }
                }
            }
            out.push('\n');
            out.push_str(&" ".repeat(depth));
            out.push_str(&format!("</{}>", self.name));
        }
    }
}

/// `s` for an attribute value (`attr`) or for text: the XML entities, line breaks and tabs of an attribute as character
/// references, other control characters (illegal in XML 1.0) dropped.
pub fn escape(s: &str, attr: bool) -> String {
    let mut o = String::with_capacity(s.len());
    for c in s.chars() {
        match c {
            '&' => o.push_str("&amp;"),
            '<' => o.push_str("&lt;"),
            '>' => o.push_str("&gt;"),
            '"' if attr => o.push_str("&quot;"),
            '\'' if attr => o.push_str("&apos;"),
            '\r' => o.push_str("&#13;"),
            '\n' | '\t' if attr => o.push_str(&format!("&#{};", c as u32)),
            '\n' | '\t' => o.push(c),
            c if (c as u32) < 0x20 => {}
            c => o.push(c),
        }
    }
    o
}

/// The entities and character references of `s` decoded; an unknown entity stays as written.
pub fn unescape(s: &str) -> String {
    let mut o = String::with_capacity(s.len());
    let mut rest = s;
    while let Some(at) = rest.find('&') {
        o.push_str(&rest[..at]);
        rest = &rest[at..];
        let decoded = rest.find(';').and_then(|end| {
            let name = &rest[1..end];
            let c = match name {
                "amp" => '&',
                "lt" => '<',
                "gt" => '>',
                "quot" => '"',
                "apos" => '\'',
                _ => {
                    let n = name.strip_prefix('#')?;
                    let code = match n.strip_prefix(['x', 'X']) {
                        Some(h) => u32::from_str_radix(h, 16).ok()?,
                        None => n.parse().ok()?,
                    };
                    char::from_u32(code)?
                }
            };
            Some((c, end + 1))
        });
        match decoded {
            Some((c, len)) => {
                o.push(c);
                rest = &rest[len..];
            }
            None => {
                o.push('&');
                rest = &rest[1..];
            }
        }
    }
    o.push_str(rest);
    o
}

struct Parser<'a> {
    s: &'a str,
    i: usize,
}

impl<'a> Parser<'a> {
    fn err<T>(&self, what: &str) -> Result<T, String> {
        let line = self.s[..self.i].matches('\n').count() + 1;
        Err(format!("line {line}: {what}"))
    }

    fn rest(&self) -> &'a str {
        &self.s[self.i..]
    }

    fn skip_ws(&mut self) {
        self.i += self.rest().len() - self.rest().trim_start().len();
    }

    /// Skips `<? … ?>`, `<!-- … -->` and `<!DOCTYPE … >` when one starts here; whether one did.
    fn skip_misc(&mut self) -> Result<bool, String> {
        let end = if self.rest().starts_with("<?") {
            "?>"
        } else if self.rest().starts_with("<!--") {
            "-->"
        } else if self.rest().starts_with("<!") && !self.rest().starts_with("<![CDATA[") {
            ">"
        } else {
            return Ok(false);
        };
        match self.rest().find(end) {
            Some(at) => {
                self.i += at + end.len();
                Ok(true)
            }
            None => self.err("unterminated markup"),
        }
    }

    fn name(&mut self) -> Result<String, String> {
        let n = self
            .rest()
            .find(|c: char| c.is_whitespace() || matches!(c, '/' | '>' | '='))
            .unwrap_or(self.rest().len());
        if n == 0 {
            return self.err("a name was expected");
        }
        let name = self.rest()[..n].to_owned();
        self.i += n;
        Ok(name)
    }

    /// `<name attr="v" …` up to the `>` or `/>`: the element and whether it is self-closed.
    fn start_tag(&mut self) -> Result<(El, bool), String> {
        self.i += 1; // '<'
        let mut el = El::new(&self.name()?);
        loop {
            self.skip_ws();
            if self.rest().starts_with("/>") {
                self.i += 2;
                return Ok((el, true));
            }
            if self.rest().starts_with('>') {
                self.i += 1;
                return Ok((el, false));
            }
            let key = self.name()?;
            self.skip_ws();
            if !self.rest().starts_with('=') {
                return self.err(&format!("attribute {key} has no value"));
            }
            self.i += 1;
            self.skip_ws();
            let Some(q) = self
                .rest()
                .chars()
                .next()
                .filter(|c| matches!(c, '"' | '\''))
            else {
                return self.err(&format!("attribute {key} is not quoted"));
            };
            let body = &self.rest()[1..];
            let Some(end) = body.find(q) else {
                return self.err("unterminated attribute value");
            };
            el.attrs.push((key, unescape(&body[..end])));
            self.i += end + 2;
        }
    }

    fn element(&mut self) -> Result<El, String> {
        let (mut el, closed) = self.start_tag()?;
        if closed {
            return Ok(el);
        }
        loop {
            if self.rest().is_empty() {
                return self.err(&format!("<{}> is never closed", el.name));
            }
            if self.rest().starts_with("</") {
                let close = self.rest().find('>').map(|e| &self.rest()[2..e]);
                if close.map(str::trim) != Some(el.name.as_str()) {
                    return self.err(&format!(
                        "</{}> does not close <{}>",
                        close.unwrap_or(""),
                        el.name
                    ));
                }
                self.i += self.rest().find('>').unwrap_or(0) + 1;
                return Ok(el);
            }
            if self.skip_misc()? {
                continue;
            }
            if let Some(cdata) = self.rest().strip_prefix("<![CDATA[") {
                let Some(end) = cdata.find("]]>") else {
                    return self.err("unterminated CDATA");
                };
                el.kids.push(Node::Text(cdata[..end].to_owned()));
                self.i += 9 + end + 3;
            } else if self.rest().starts_with('<') {
                el.kids.push(Node::El(self.element()?));
            } else {
                let n = self.rest().find('<').unwrap_or(self.rest().len());
                el.kids.push(Node::Text(unescape(&self.rest()[..n])));
                self.i += n;
            }
        }
    }
}

/// The element `s` holds (one element, anything before or after it is ignored).
pub fn parse(s: &str) -> Result<El, String> {
    let mut p = Parser { s, i: 0 };
    loop {
        p.skip_ws();
        if !p.skip_misc()? {
            break;
        }
    }
    if !p.rest().starts_with('<') {
        return p.err("no element");
    }
    p.element()
}

/// One top-level item of the database root: its text as written (with the white space before it), and for a `<Song>` the
/// `FilePath`.
pub struct Item {
    pub raw: String,
    pub file_path: Option<String>,
}

/// A database cut at its root: the text up to and including the root start tag, the items, the root's name.
pub struct Doc {
    pub head: String,
    pub items: Vec<Item>,
    pub root: String,
}

/// Cuts the document `s` into [`Doc`]; `Err` when it is not well formed XML below the root.
pub fn split(s: &str) -> Result<Doc, String> {
    let s = s.strip_prefix('\u{feff}').unwrap_or(s);
    let mut p = Parser { s, i: 0 };
    loop {
        p.skip_ws();
        if !p.skip_misc()? {
            break;
        }
    }
    if !p.rest().starts_with('<') {
        return p.err("no root element");
    }
    let (root, closed) = p.start_tag()?;
    let mut head = s[..p.i].to_owned();
    if closed {
        // `<Root a="b"/>`: reopen it so items can follow.
        head.truncate(head.len() - 2);
        head.push('>');
        return Ok(Doc {
            head,
            items: Vec::new(),
            root: root.name,
        });
    }
    let mut items = Vec::new();
    let mut from = p.i;
    loop {
        p.skip_ws();
        if p.rest().starts_with("</") {
            return Ok(Doc {
                head,
                items,
                root: root.name,
            });
        }
        if p.rest().is_empty() {
            return p.err(&format!("<{}> is never closed", root.name));
        }
        if p.skip_misc()? {
            items.push(Item {
                raw: s[from..p.i].to_owned(),
                file_path: None,
            });
        } else if p.rest().starts_with('<') {
            let el = p.element()?;
            let file_path = (el.name == "Song")
                .then(|| el.attr("FilePath").map(str::to_owned))
                .flatten();
            items.push(Item {
                raw: s[from..p.i].to_owned(),
                file_path,
            });
        } else {
            return p.err("text between the items of the database");
        }
        from = p.i;
    }
}
