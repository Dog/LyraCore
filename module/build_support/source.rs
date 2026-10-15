use std::fs;
use std::path::{Path, PathBuf};

pub const DEBUG_REDUCERS_FILE_CFG: &str = "#![cfg(feature = \"debug_reducers\")]";
pub const TEST_FILE_CFG: &str = "#![cfg(test)]";

pub struct StrippedSource {
    pub code: String,
    pub line_comments: Vec<(usize, String)>,
}

pub fn strip_source(src: &str) -> StrippedSource {
    let b: Vec<char> = src.chars().collect();
    let mut out: Vec<char> = Vec::with_capacity(b.len());
    let mut line_comments = Vec::new();
    let blank = |c: char| if c == '\n' { '\n' } else { ' ' };
    let mut i = 0usize;
    let mut line = 1usize;
    let mut counted_until = 0usize;
    while i < b.len() {
        line += b[counted_until..i].iter().filter(|c| **c == '\n').count();
        counted_until = i;
        let c = b[i];
        if c == '/' && b.get(i + 1) == Some(&'/') {
            let comment_start = i;
            while i < b.len() && b[i] != '\n' {
                out.push(' ');
                i += 1;
            }
            line_comments.push((line, b[comment_start..i].iter().collect()));
            continue;
        }
        if c == '/' && b.get(i + 1) == Some(&'*') {
            let mut depth = 0usize;
            while i < b.len() {
                if b[i] == '/' && b.get(i + 1) == Some(&'*') {
                    depth += 1;
                    out.push(' ');
                    out.push(' ');
                    i += 2;
                } else if b[i] == '*' && b.get(i + 1) == Some(&'/') {
                    depth -= 1;
                    out.push(' ');
                    out.push(' ');
                    i += 2;
                    if depth == 0 {
                        break;
                    }
                } else {
                    out.push(blank(b[i]));
                    i += 1;
                }
            }
            continue;
        }
        let prev_is_ident = i > 0 && (b[i - 1].is_alphanumeric() || b[i - 1] == '_');
        if !prev_is_ident && (c == 'r' || (matches!(c, 'b' | 'c') && b.get(i + 1) == Some(&'r'))) {
            let r_at = if c == 'r' { i } else { i + 1 };
            let mut j = r_at + 1;
            while b.get(j) == Some(&'#') {
                j += 1;
            }
            if b.get(j) == Some(&'"') {
                let hashes = j - (r_at + 1);
                for c in &b[i..=j] {
                    out.push(blank(*c));
                }
                i = j + 1;
                'raw: while i < b.len() {
                    if b[i] == '"' {
                        let mut h = 0usize;
                        while h < hashes && b.get(i + 1 + h) == Some(&'#') {
                            h += 1;
                        }
                        if h == hashes {
                            for c in &b[i..=(i + hashes)] {
                                out.push(blank(*c));
                            }
                            i += hashes + 1;
                            break 'raw;
                        }
                    }
                    out.push(blank(b[i]));
                    i += 1;
                }
                continue;
            }
        }
        if c == '"' || (!prev_is_ident && matches!(c, 'b' | 'c') && b.get(i + 1) == Some(&'"')) {
            if c != '"' {
                out.push(' ');
                i += 1;
            }
            out.push(' ');
            i += 1;
            while i < b.len() {
                if b[i] == '\\' {
                    out.push(' ');
                    if i + 1 < b.len() {
                        out.push(blank(b[i + 1]));
                    }
                    i += 2;
                    continue;
                }
                if b[i] == '"' {
                    out.push(' ');
                    i += 1;
                    break;
                }
                out.push(blank(b[i]));
                i += 1;
            }
            continue;
        }
        if c == '\'' {
            let is_char_lit = match b.get(i + 1) {
                Some('\\') => true,
                Some(_) => b.get(i + 2) == Some(&'\''),
                None => false,
            };
            if is_char_lit {
                out.push(' ');
                i += 1;
                if b.get(i) == Some(&'\\') {
                    // Consume escaped quotes before looking for the character literal's terminator.
                    out.push(' ');
                    i += 1;
                    if i < b.len() {
                        out.push(blank(b[i]));
                        i += 1;
                    }
                    while i < b.len() && b[i] != '\'' {
                        out.push(blank(b[i]));
                        i += 1;
                    }
                } else {
                    out.push(blank(b[i]));
                    i += 1;
                }
                if b.get(i) == Some(&'\'') {
                    out.push(' ');
                    i += 1;
                }
                continue;
            }
        }
        out.push(c);
        i += 1;
    }
    StrippedSource {
        code: out.into_iter().collect(),
        line_comments,
    }
}

pub fn file_gate(source: &str) -> Option<&str> {
    source.lines().map(str::trim).find(|line| !line.is_empty())
}

pub fn collect_rs_files(dir: &Path, out: &mut Vec<PathBuf>) {
    let entries =
        fs::read_dir(dir).unwrap_or_else(|e| panic!("cannot read dir {}: {e}", dir.display()));
    for entry in entries {
        let path = entry
            .unwrap_or_else(|e| panic!("cannot read dir entry in {}: {e}", dir.display()))
            .path();
        if path.is_dir() {
            collect_rs_files(&path, out);
        } else if path.extension().is_some_and(|e| e == "rs") {
            out.push(path);
        }
    }
}
