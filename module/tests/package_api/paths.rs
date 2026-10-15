#[derive(Clone, Debug, Default)]
pub struct RootedPath {
    pub segments: Vec<String>,
    pub written: String,
    pub alias: Option<String>,
    offset: usize,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum UnsupportedSyntax {
    WholeCrateAlias,
    PathAttribute,
    IncludeMacro,
}

pub struct FilePaths {
    pub paths: Vec<(usize, RootedPath)>,
    pub unsupported: Vec<(usize, UnsupportedSyntax, String)>,
}

#[derive(Clone, Copy)]
pub struct Token<'a> {
    pub text: &'a str,
    start: usize,
}

pub fn tokens(stripped: &str) -> Vec<Token<'_>> {
    let mut tokens = Vec::new();
    let mut at = 0usize;
    while at < stripped.len() {
        let remaining = &stripped[at..];
        let first = remaining
            .chars()
            .next()
            .expect("at is before the end of source");
        if is_space(first) {
            at += first.len_utf8();
            continue;
        }
        let start = at;
        let raw_ident = remaining
            .strip_prefix("r#")
            .and_then(|text| text.chars().next())
            .is_some_and(ident_start);
        if raw_ident || ident_start(first) {
            if raw_ident {
                at += 2;
            }
            while let Some(c) = stripped[at..].chars().next().filter(|c| ident_char(*c)) {
                at += c.len_utf8();
            }
        } else if remaining.starts_with("::") {
            at += 2;
        } else {
            at += first.len_utf8();
        }
        tokens.push(Token {
            text: &stripped[start..at],
            start,
        });
    }
    tokens
}

pub fn is_ident(token: Token<'_>) -> bool {
    token.text.chars().next().is_some_and(ident_start)
}

fn is_space(c: char) -> bool {
    c.is_whitespace() || matches!(c, '\u{200e}' | '\u{200f}')
}

fn ident_char(c: char) -> bool {
    c.is_ascii_alphanumeric() || c == '_' || (!c.is_ascii() && !is_space(c))
}

fn ident_start(c: char) -> bool {
    ident_char(c) && !c.is_ascii_digit()
}

fn ident_name(token: Token<'_>) -> Option<&str> {
    is_ident(token).then(|| token.text.strip_prefix("r#").unwrap_or(token.text))
}

fn line_at(stripped: &str, offset: usize) -> usize {
    stripped[..offset].matches('\n').count() + 1
}

pub fn tree_leaves(
    tokens: &[Token<'_>],
    at: usize,
    parent: &RootedPath,
    out: &mut Vec<RootedPath>,
) -> usize {
    let mut leaf = parent.clone();
    let mut index = at;
    while let Some(token) = tokens.get(index).copied() {
        if token.text == "{" {
            return group_leaves(tokens, index, &leaf, out);
        }
        if token.text == "*" || is_ident(token) {
            leaf.written.push_str("::");
            leaf.written.push_str(token.text);
            if token.text != "self" {
                let name = ident_name(token).unwrap_or(token.text);
                leaf.segments.push(name.to_string());
                leaf.offset = token.start;
            }
            index += 1;
            if token.text != "*" && tokens.get(index).is_some_and(|next| next.text == "::") {
                index += 1;
                continue;
            }
        }
        break;
    }
    if tokens.get(index).is_some_and(|token| token.text == "as") {
        if let Some(alias) = tokens.get(index + 1) {
            leaf.alias = Some(alias.text.to_string());
            index += 2;
        }
    }
    if leaf.segments.len() > parent.segments.len() || leaf.written.ends_with("::self") {
        out.push(leaf);
    }
    index
}

fn group_leaves(
    tokens: &[Token<'_>],
    open: usize,
    parent: &RootedPath,
    out: &mut Vec<RootedPath>,
) -> usize {
    let mut index = open + 1;
    while let Some(token) = tokens.get(index) {
        match token.text {
            "}" => return index + 1,
            "," => index += 1,
            _ => {
                let next = tree_leaves(tokens, index, parent, out);
                index = next.max(index + 1);
            }
        }
    }
    index
}

fn inline_modules(tokens: &[Token<'_>]) -> Vec<(usize, usize)> {
    let mut open = Vec::new();
    let mut pairs = Vec::new();
    for token in tokens {
        match token.text {
            "{" => open.push(token.start),
            "}" => {
                if let Some(start) = open.pop() {
                    pairs.push((start, token.start));
                }
            }
            _ => {}
        }
    }
    tokens
        .windows(3)
        .filter(|w| w[0].text == "mod" && is_ident(w[1]) && w[2].text == "{")
        .filter_map(|w| pairs.iter().find(|(open, _)| *open == w[2].start).copied())
        .collect()
}

pub fn file_paths(stripped: &str, file_depth: usize) -> FilePaths {
    let tokens = tokens(stripped);
    let modules = inline_modules(&tokens);
    let mut leaves = Vec::new();
    let mut index = 0usize;
    while index < tokens.len() {
        let token = tokens[index];
        let after = |offset: usize, text: &str| {
            tokens
                .get(index + offset)
                .is_some_and(|token| token.text == text)
        };
        let (root, separator, required_supers) =
            if token.text == "$" && after(1, "crate") && after(2, "::") {
                ("$crate".to_string(), index + 2, None)
            } else if token.text == "crate"
                && after(1, "::")
                && (index == 0 || tokens[index - 1].text != "$")
            {
                ("crate".to_string(), index + 1, None)
            } else if token.text == "super" && (index == 0 || tokens[index - 1].text != "::") {
                let mut levels = 1usize;
                let mut end = index;
                while tokens.get(end + 1).is_some_and(|t| t.text == "::")
                    && tokens.get(end + 2).is_some_and(|t| t.text == "super")
                {
                    levels += 1;
                    end += 2;
                }
                let depth = file_depth
                    + modules
                        .iter()
                        .filter(|(open, close)| *open < token.start && token.start < *close)
                        .count();
                if tokens.get(end + 1).is_none_or(|t| t.text != "::") {
                    index = end + 1;
                    continue;
                }
                (vec!["super"; levels].join("::"), end + 1, Some(depth + 1))
            } else {
                index += 1;
                continue;
            };
        let parent = RootedPath {
            written: root,
            ..RootedPath::default()
        };
        let mut root_leaves = Vec::new();
        index = tree_leaves(&tokens, separator + 1, &parent, &mut root_leaves).max(separator + 1);
        for mut leaf in root_leaves {
            if let Some(required) = required_supers {
                let levels = leaf
                    .written
                    .split("::")
                    .take_while(|s| *s == "super")
                    .count();
                if levels != required {
                    continue;
                }
                let grouped_supers = leaf.segments.iter().take_while(|s| *s == "super").count();
                leaf.segments.drain(..grouped_supers);
            }
            leaves.push(leaf);
        }
    }
    let paths = leaves
        .into_iter()
        .filter(|leaf| !leaf.segments.is_empty())
        .map(|leaf| (line_at(stripped, leaf.offset), leaf))
        .collect();
    FilePaths {
        paths,
        unsupported: unsupported_syntax(&tokens, stripped),
    }
}

fn unsupported_syntax(
    tokens: &[Token<'_>],
    stripped: &str,
) -> Vec<(usize, UnsupportedSyntax, String)> {
    let mut found = Vec::new();
    let mut alias = |start: usize, name: &str| {
        found.push((
            start,
            UnsupportedSyntax::WholeCrateAlias,
            format!("crate alias `{name}`"),
        ));
    };
    for (index, token) in tokens.iter().enumerate() {
        if token.text != "crate" || !in_use_statement(tokens, index) {
            continue;
        }
        let renames = |at: usize| {
            tokens.get(at).is_some_and(|t| t.text == "as")
                && tokens.get(at + 1).is_some_and(|t| t.text != "_")
        };
        if renames(index + 1) {
            alias(token.start, tokens[index + 2].text);
        } else if tokens.get(index + 1).is_some_and(|t| t.text == "::") {
            let mut leaves = Vec::new();
            tree_leaves(tokens, index + 2, &RootedPath::default(), &mut leaves);
            for leaf in leaves {
                if leaf.segments.is_empty() {
                    if let Some(name) = leaf.alias.filter(|name| name != "_") {
                        alias(token.start, &name);
                    }
                }
            }
        }
    }
    for window in tokens.windows(5) {
        if window[0].text == "extern"
            && window[1].text == "crate"
            && window[2].text == "self"
            && window[3].text == "as"
            && window[4].text != "_"
        {
            alias(window[0].start, window[4].text);
        }
    }
    found.extend(path_attributes(tokens));
    for window in tokens.windows(2) {
        if ident_name(window[0]) == Some("include") && window[1].text == "!" {
            found.push((
                window[0].start,
                UnsupportedSyntax::IncludeMacro,
                "`include!`".to_string(),
            ));
        }
    }
    found.sort_by_key(|finding| finding.0);
    found.dedup_by(|left, right| left.0 == right.0 && left.1 == right.1);
    found
        .into_iter()
        .map(|(start, kind, written)| (line_at(stripped, start), kind, written))
        .collect()
}

fn in_use_statement(tokens: &[Token<'_>], index: usize) -> bool {
    tokens[..index]
        .iter()
        .rev()
        .take_while(|token| token.text != ";")
        .any(|token| token.text == "use")
}

fn path_attributes(tokens: &[Token<'_>]) -> Vec<(usize, UnsupportedSyntax, String)> {
    let mut found = Vec::new();
    for (index, window) in tokens.windows(2).enumerate() {
        if window[0].text != "#" || window[1].text != "[" {
            continue;
        }
        let Some(end) = tokens[index + 2..].iter().position(|t| t.text == "]") else {
            continue;
        };
        let attribute = &tokens[index + 2..index + 2 + end];
        let names_path =
            |pair: &[Token<'_>]| ident_name(pair[0]) == Some("path") && pair[1].text == "=";
        let written = match attribute.first().and_then(|t| ident_name(*t)) {
            Some("path") if attribute.len() > 1 && names_path(&attribute[..2]) => "`#[path]`",
            Some("cfg_attr") if attribute.windows(2).any(names_path) => {
                "`#[cfg_attr(..., path = ...)]`"
            }
            _ => continue,
        };
        found.push((
            window[0].start,
            UnsupportedSyntax::PathAttribute,
            written.to_string(),
        ));
    }
    found
}
