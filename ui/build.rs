use std::{env, fmt::Write as _, fs, path::PathBuf};

/// The globals the split-screen window (`SplitWindow`, src/app/split/) mirrors from the main window, and whether their properties
/// are copied too (false: only the callbacks are forwarded to the main window's handlers).
const MIRRORED: [(&str, bool); 7] = [
    ("BrowserState", true),
    ("AutomixState", true),
    ("TrackInfoState", true),
    ("PaneState", true),
    ("DeviceState", false),
    ("LibraryState", false),
    ("DragState", false),
];

fn main() {
    println!("cargo:rerun-if-changed=.");
    // @tr strings without a context: one msgid per English text, shared with the app's `tr!` (src/app/i18n.rs).
    let config = slint_build::CompilerConfiguration::new()
        .with_bundled_translations("translations")
        .with_default_translation_context(slint_build::DefaultTranslationContext::None);
    slint_build::compile_with_config("app.slint", config).unwrap();
    let out = PathBuf::from(env::var_os("OUT_DIR").unwrap());
    fs::write(out.join("mirror.rs"), mirror()).unwrap();
    fs::write(out.join("skin_tokens.rs"), skin_tokens()).unwrap();
}

enum Kind {
    In,
    InOut,
    Callback { args: usize, pure: bool },
}

/// One declaration of a global: its Rust name, kind and, for a property, its Slint type (`color`, `length`, …; empty for a callback).
struct Decl {
    name: String,
    kind: Kind,
    ty: String,
}

/// The body of `export global <name> { … }` (comments dropped, nested blocks cut out, a `;` where one closed), found in `*.slint`.
fn body(name: &str) -> String {
    let head = format!("export global {name} {{");
    for file in fs::read_dir(".").unwrap() {
        let path = file.unwrap().path();
        if path.extension().is_none_or(|e| e != "slint") {
            continue;
        }
        let src = fs::read_to_string(path).unwrap_or_default();
        let Some(at) = src.find(&head) else { continue };
        let (mut depth, mut quoted, mut escaped) = (1, false, false);
        let (mut top, mut line_comment) = (String::new(), false);
        let mut prev = ' ';
        for c in src[at + head.len()..].chars() {
            if line_comment {
                line_comment = c != '\n';
                prev = c;
                continue;
            }
            if quoted {
                quoted = escaped || c != '"';
                escaped = !escaped && c == '\\';
                if depth == 1 {
                    top.push(c);
                }
                continue;
            }
            match c {
                '/' if prev == '/' => {
                    line_comment = true;
                    if depth == 1 {
                        top.pop();
                    }
                }
                '"' => {
                    quoted = true;
                    if depth == 1 {
                        top.push(c);
                    }
                }
                '{' => depth += 1,
                '}' => {
                    depth -= 1;
                    if depth == 0 {
                        return top;
                    }
                    if depth == 1 {
                        top.push(';');
                    }
                }
                _ if depth == 1 => top.push(c),
                _ => {}
            }
            prev = c;
        }
        panic!("unterminated global {name}");
    }
    panic!("no `export global {name}` in ui/*.slint");
}

/// The leading identifier of `s` as a Rust name (`foo-bar` → `foo_bar`).
fn ident(s: &str) -> String {
    s.chars()
        .take_while(|c| c.is_alphanumeric() || *c == '-')
        .collect::<String>()
        .replace('-', "_")
}

/// Properties and callbacks of a global, by Rust name.
fn decls(name: &str) -> Vec<Decl> {
    let mut out = Vec::new();
    for stmt in body(name).split(';') {
        let words: Vec<&str> = stmt.split_whitespace().collect();
        let (pure, words) = match words.as_slice() {
            ["pure", rest @ ..] => (true, rest),
            w => (false, w),
        };
        match words {
            ["in", "property", ..] | ["in-out", "property", ..] => {
                let kind = if words[0] == "in" {
                    Kind::In
                } else {
                    Kind::InOut
                };
                // `property <type> name`: the type holds no `>` (models are `[T]`).
                let (head, after) = stmt.split_once('>').unwrap_or((stmt, ""));
                let ty = head.rsplit_once('<').map_or("", |(_, t)| t.trim());
                out.push(Decl {
                    name: ident(after.trim_start()),
                    kind,
                    ty: ty.to_owned(),
                });
            }
            ["callback", ..] => {
                let after = stmt.split_once("callback").unwrap().1.trim_start();
                let args = match after.split_once('(') {
                    Some((_, rest)) => {
                        let inner = rest.split_once(')').unwrap().0;
                        if inner.trim().is_empty() {
                            0
                        } else {
                            inner.matches(',').count() + 1
                        }
                    }
                    None => 0,
                };
                out.push(Decl {
                    name: ident(after),
                    kind: Kind::Callback { args, pure },
                    ty: String::new(),
                });
            }
            _ => {}
        }
    }
    out
}

/// Rust for [`mirror`](lib.rs) (used by `src/app/split/`): `sync` copies the main window's state into the split window and keeps them in step (properties
/// the UI sets in either window follow the one that changed last), `forward` makes every callback of the split window's globals call the
/// main window's handler. Written from the declarations, so a new property or callback needs no edit here.
fn mirror() -> String {
    let globals: Vec<_> = MIRRORED
        .iter()
        .map(|&(n, props)| (n, props, decls(n)))
        .collect();
    let names: Vec<&str> = MIRRORED.iter().map(|g| g.0).collect();
    let mut s = format!(
        "// @generated by ui/build.rs from the globals in ui/*.slint\nuse crate::{{App, SplitWindow, {}}};\nuse slint::ComponentHandle;\n\n",
        names.join(", ")
    );
    // Every in-out property starts equal to the main window's, and remembers the value both windows last agreed on.
    s += "pub fn sync(a: slint::Weak<App>, b: slint::Weak<SplitWindow>) -> Box<dyn FnMut()> {\n    let (a0, b0) = (a.upgrade().unwrap(), b.upgrade().unwrap());\n";
    for (g, props, ds) in &globals {
        for d in ds
            .iter()
            .filter(|d| *props && matches!(d.kind, Kind::InOut))
        {
            let n = &d.name;
            let l = g.to_lowercase();
            let _ = writeln!(
                s,
                "    b0.global::<{g}>().set_{n}(a0.global::<{g}>().get_{n}());\n    let mut last_{l}_{n} = a0.global::<{g}>().get_{n}();"
            );
        }
    }
    s += "    Box::new(move || {\n        let (Some(a), Some(b)) = (a.upgrade(), b.upgrade()) else { return };\n";
    for (g, _, ds) in globals.iter().filter(|g| g.1) {
        let l = g.to_lowercase();
        let _ = writeln!(
            s,
            "        {{\n            let (x, y) = (a.global::<{g}>(), b.global::<{g}>());"
        );
        for Decl { name: n, kind, .. } in ds {
            match kind {
                Kind::In => {
                    let _ = writeln!(s, "            y.set_{n}(x.get_{n}());");
                }
                Kind::InOut => {
                    let _ = writeln!(
                        s,
                        "            {{\n                let (p, q) = (x.get_{n}(), y.get_{n}());\n                if q != last_{l}_{n} {{\n                    if p != q {{\n                        x.set_{n}(q.clone());\n                    }}\n                    last_{l}_{n} = q;\n                }} else if p != q {{\n                    y.set_{n}(p.clone());\n                    last_{l}_{n} = p;\n                }}\n            }}"
                    );
                }
                Kind::Callback { .. } => {}
            }
        }
        s += "        }\n";
    }
    s += "    })\n}\n\n";
    // `s()` runs `sync`: the split window's in-out values reach the main window before its handler reads them, and what the handler
    // changed comes back after. Pure callbacks run while the UI evaluates, so they must not set properties.
    s += "pub fn forward(a: &slint::Weak<App>, b: &SplitWindow, sync: &std::rc::Rc<dyn Fn()>) {\n";
    for (g, _, ds) in &globals {
        if !ds.iter().any(|d| matches!(d.kind, Kind::Callback { .. })) {
            continue; // nothing to forward (PaneState)
        }
        let _ = writeln!(s, "    {{\n        let g = b.global::<{g}>();");
        for Decl { name: n, kind, .. } in ds {
            let Kind::Callback { args, pure } = kind else {
                continue;
            };
            let ps = (0..*args)
                .map(|i| format!("p{i}"))
                .collect::<Vec<_>>()
                .join(", ");
            let (bind, run) = if *pure {
                ("let a = a.clone();", "")
            } else {
                ("let (a, s) = (a.clone(), sync.clone());", "s();")
            };
            let _ = writeln!(
                s,
                "        {{\n            {bind}\n            g.on_{n}(move |{ps}| {{ {run} let r = a.upgrade().map(|a| a.global::<{g}>().invoke_{n}({ps})).unwrap_or_default(); {run} r }});\n        }}"
            );
        }
        s += "    }\n";
    }
    s += "}\n";
    s
}

/// Rust for [`skin`](lib.rs) (used by `src/app/skin/`): the skin tokens (every `in-out` property of the `Theme` global) with their types, and typed
/// `get` / `set` over a window's `Theme`. A new token in ui/theme.slint needs no edit here or in Rust.
fn skin_tokens() -> String {
    let tokens: Vec<_> = decls("Theme")
        .into_iter()
        .filter(|d| matches!(d.kind, Kind::InOut))
        .collect();
    // (Ty variant, Value variant) by Slint type.
    let variant = |d: &Decl| match d.ty.as_str() {
        "color" => "Color",
        "length" => "Length",
        "string" => "Str",
        "int" => "Int",
        "float" => "Float",
        "duration" => "Duration",
        t => panic!(
            "Theme.{}: skin tokens of type `{t}` are not supported (src/app/skin)",
            d.name
        ),
    };
    let mut s = String::from(
        "// @generated by ui/build.rs from the in-out properties of `Theme` in ui/theme.slint\n\n/// Every skin token: its kebab-case name and type, in ui/theme.slint order.\npub const TOKENS: &[(&str, Ty)] = &[\n",
    );
    for d in &tokens {
        let _ = writeln!(
            s,
            "    (\"{}\", Ty::{}),",
            d.name.replace('_', "-"),
            variant(d)
        );
    }
    s += "];\n\n/// The current value of token `name` in `w`'s `Theme` (`None`: not a token).\npub fn get<'a, C: ComponentHandle>(w: &'a C, name: &str) -> Option<Value>\nwhere\n    Theme<'a>: slint::Global<'a, C>,\n{\n    let t = w.global::<Theme>();\n    Some(match name {\n";
    for d in &tokens {
        let (k, n) = (variant(d), &d.name);
        let conv = if k == "Str" {
            format!("t.get_{n}().to_string()")
        } else {
            format!("t.get_{n}()")
        };
        let _ = writeln!(
            s,
            "        \"{}\" => Value::{k}({conv}),",
            n.replace('_', "-")
        );
    }
    s += "        _ => return None,\n    })\n}\n\n/// Sets token `name` in `w`'s `Theme`; false when it is not a token of that value's type.\npub fn set<'a, C: ComponentHandle>(w: &'a C, name: &str, v: &Value) -> bool\nwhere\n    Theme<'a>: slint::Global<'a, C>,\n{\n    let t = w.global::<Theme>();\n    match (name, v) {\n";
    for d in &tokens {
        let (k, n) = (variant(d), &d.name);
        let arg = if k == "Str" {
            "x.as_str().into()"
        } else {
            "*x"
        };
        let _ = writeln!(
            s,
            "        (\"{}\", Value::{k}(x)) => t.set_{n}({arg}),",
            n.replace('_', "-")
        );
    }
    s += "        _ => return false,\n    }\n    true\n}\n";
    // The editor's group and hint of each token, its value rule and the token it follows: `// @group <name>`, `///` docs and
    // `// @values` / `// @range` of theme.slint, in token order.
    let info = token_info();
    assert_eq!(
        info.iter().map(|t| t.name.as_str()).collect::<Vec<_>>(),
        tokens
            .iter()
            .map(|d| d.name.replace('_', "-"))
            .collect::<Vec<_>>(),
        "theme.slint token scan out of step with the declarations"
    );
    s += "\n/// The theme editor's group (`// @group` in ui/theme.slint) and hint (the `///` doc) of each token of [`TOKENS`], in that order.\npub const TOKEN_INFO: &[(&str, &str)] = &[\n";
    for t in &info {
        let _ = writeln!(s, "    ({:?}, {:?}),", t.group, t.doc);
    }
    s += "];\n\n/// The value rule of each token of [`TOKENS`] (`// @values`, `// @range`), in that order.\npub const TOKEN_RULES: &[Rule] = &[\n";
    for (t, d) in info.iter().zip(&tokens) {
        match t.rule.split_once(' ') {
            None => s += "    Rule::Any,\n",
            Some(("@values", v)) => {
                assert_eq!(d.ty, "int", "Theme.{}: `@values` is for int tokens", t.name);
                let names: Vec<String> = v.split('|').map(|n| format!("{:?}", n.trim())).collect();
                let _ = writeln!(s, "    Rule::Choice(&[{}]),", names.join(", "));
            }
            Some(("@range", v)) => {
                assert_eq!(
                    d.ty, "float",
                    "Theme.{}: `@range` is for float tokens",
                    t.name
                );
                let (lo, hi) = v.split_once("..").expect("`@range lo..hi`");
                let _ = writeln!(
                    s,
                    "    Rule::Range({:?}f32, {:?}f32),",
                    lo.trim().parse::<f32>().unwrap(),
                    hi.trim().parse::<f32>().unwrap()
                );
            }
            Some(_) => panic!("Theme.{}: unknown rule `{}`", t.name, t.rule),
        }
    }
    s += "];\n\n/// Tokens whose default is another token (`name: root.other;` in ui/theme.slint): (token, the token it follows). A skin that does not\n/// set the token gives it the value the skin gives the other one.\npub const FOLLOWS: &[(&str, &str)] = &[\n";
    for t in &info {
        if let Some(f) = &t.follows {
            assert!(
                info.iter().any(|o| &o.name == f),
                "Theme.{}: follows `{f}`, which is not a token",
                t.name
            );
            let _ = writeln!(s, "    ({:?}, {f:?}),", t.name);
        }
    }
    s += "];\n";
    s
}
/// One `in-out` property of `Theme` as theme.slint describes it.
struct Info {
    name: String,
    group: String,
    doc: String,
    /// The `// @values …` or `// @range …` line before it (empty: none).
    rule: String,
    /// The token its default names (`root.<token>`).
    follows: Option<String>,
}

/// Every `in-out` property of `Theme`, scanned from ui/theme.slint line by line.
fn token_info() -> Vec<Info> {
    let src = fs::read_to_string("theme.slint").expect("ui/theme.slint");
    let (mut group, mut doc, mut rule) = (String::new(), Vec::new(), String::new());
    let mut out = Vec::new();
    for line in src.lines().map(str::trim) {
        if let Some(g) = line.strip_prefix("// @group ") {
            group = g.trim().to_owned();
        } else if let Some(d) = line.strip_prefix("///") {
            doc.push(d.trim().to_owned());
        } else if line.starts_with("// @values ") || line.starts_with("// @range ") {
            rule = line[3..].trim().to_owned();
        } else if let Some((_, rest)) = line
            .strip_prefix("in-out property")
            .and_then(|r| r.split_once('>'))
        {
            let rest = rest.trim_start();
            let name = ident(rest).replace('_', "-");
            let follows = rest
                .split_once(':')
                .and_then(|(_, d)| d.trim().strip_suffix(';')?.trim().strip_prefix("root."))
                .filter(|t| !t.is_empty() && t.chars().all(|c| c.is_alphanumeric() || c == '-'))
                .filter(|t| *t != "light")
                .map(str::to_owned);
            out.push(Info {
                name,
                group: group.clone(),
                doc: doc.join(" "),
                rule: std::mem::take(&mut rule),
                follows,
            });
            doc.clear();
        } else if !line.starts_with("//") {
            doc.clear();
            rule.clear();
        }
    }
    out
}
