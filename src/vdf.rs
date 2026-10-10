//! Steam's text VDF (config.vdf, localconfig.vdf, appmanifest_*.acf): nested "key" "value"
//! pairs and "key" { ... } blocks. Strings are kept in their escaped form, exactly as Steam
//! wrote them, and written back in Steam's layout, so an unchanged file round-trips byte for byte.

#[derive(Clone, PartialEq, Debug)]
pub enum Node {
    /// Still escaped (`\"`, `\\`).
    Str(String),
    Map(Vec<(String, Node)>),
}

pub type Doc = Vec<(String, Node)>;

pub fn escape(s: &str) -> String {
    s.replace('\\', "\\\\").replace('"', "\\\"")
}

pub fn unescape(s: &str) -> String {
    let mut out = String::new();
    let mut chars = s.chars();
    while let Some(c) = chars.next() {
        match c {
            '\\' => match chars.next() {
                Some('n') => out.push('\n'),
                Some('t') => out.push('\t'),
                Some(other) => out.push(other),
                None => {}
            },
            _ => out.push(c),
        }
    }
    out
}

enum Token {
    Str(String),
    Open,
    Close,
}

fn tokens(text: &str) -> Result<Vec<Token>, String> {
    let mut out = Vec::new();
    let mut chars = text.char_indices().peekable();
    while let Some((i, c)) = chars.next() {
        match c {
            '{' => out.push(Token::Open),
            '}' => out.push(Token::Close),
            '"' => {
                let mut escaped = false;
                let mut end = None;
                for (j, d) in chars.by_ref() {
                    if escaped {
                        escaped = false;
                    } else if d == '\\' {
                        escaped = true;
                    } else if d == '"' {
                        end = Some(j);
                        break;
                    }
                }
                let end = end.ok_or("unterminated string")?;
                out.push(Token::Str(text[i + 1..end].to_string()));
            }
            c if c.is_whitespace() => {}
            // Only in files gamefix just reads (GE-Proton's compatibilitytool.vdf): writing
            // would drop them.
            '/' if chars.peek().is_some_and(|&(_, d)| d == '/') => {
                for (_, d) in chars.by_ref() {
                    if d == '\n' {
                        break;
                    }
                }
            }
            // Unquoted tokens never appear in the files gamefix edits.
            c => return Err(format!("unexpected '{c}'")),
        }
    }
    Ok(out)
}

fn parse_items(tokens: &mut std::iter::Peekable<std::vec::IntoIter<Token>>, nested: bool) -> Result<Doc, String> {
    let mut items = Vec::new();
    loop {
        match tokens.next() {
            None if !nested => return Ok(items),
            None => return Err("a block isn't closed".into()),
            Some(Token::Close) if nested => return Ok(items),
            Some(Token::Str(key)) => match tokens.next() {
                Some(Token::Str(value)) => items.push((key, Node::Str(value))),
                Some(Token::Open) => items.push((key, Node::Map(parse_items(tokens, true)?))),
                _ => return Err(format!("\"{key}\" has no value")),
            },
            _ => return Err("unexpected brace".into()),
        }
    }
}

pub fn parse(text: &str) -> Result<Doc, String> {
    parse_items(&mut tokens(text)?.into_iter().peekable(), false)
}

fn write_items(items: &[(String, Node)], depth: usize, out: &mut String) {
    let indent = "\t".repeat(depth);
    for (key, node) in items {
        match node {
            Node::Str(v) => out.push_str(&format!("{indent}\"{key}\"\t\t\"{v}\"\n")),
            Node::Map(m) => {
                out.push_str(&format!("{indent}\"{key}\"\n{indent}{{\n"));
                write_items(m, depth + 1, out);
                out.push_str(&format!("{indent}}}\n"));
            }
        }
    }
}

pub fn write(doc: &[(String, Node)]) -> String {
    let mut out = String::new();
    write_items(doc, 0, &mut out);
    out
}

/// The block at this path of keys (compared without case, as Steam does), created if missing.
pub fn block<'a>(doc: &'a mut Doc, path: &[&str]) -> Result<&'a mut Doc, String> {
    let mut current = doc;
    for key in path {
        let pos = match current.iter().position(|(k, _)| k.eq_ignore_ascii_case(key)) {
            Some(pos) => pos,
            None => {
                current.push((key.to_string(), Node::Map(Vec::new())));
                current.len() - 1
            }
        };
        current = match &mut current[pos].1 {
            Node::Map(m) => m,
            Node::Str(_) => return Err(format!("\"{key}\" is a value, not a block")),
        };
    }
    Ok(current)
}

pub fn get<'a>(doc: &'a Doc, path: &[&str]) -> Option<&'a Node> {
    let (last, parents) = path.split_last()?;
    let mut current = doc;
    for key in parents {
        current = match current.iter().find(|(k, _)| k.eq_ignore_ascii_case(key))? {
            (_, Node::Map(m)) => m,
            _ => return None,
        };
    }
    current.iter().find(|(k, _)| k.eq_ignore_ascii_case(last)).map(|(_, n)| n)
}

/// Sets or (with None) removes `key` in `block`, returning what it was.
pub fn replace(block: &mut Doc, key: &str, node: Option<Node>) -> Option<Node> {
    let pos = block.iter().position(|(k, _)| k.eq_ignore_ascii_case(key));
    match (pos, node) {
        (Some(p), Some(n)) => Some(std::mem::replace(&mut block[p].1, n)),
        (Some(p), None) => Some(block.remove(p).1),
        (None, Some(n)) => {
            block.push((key.to_string(), n));
            None
        }
        (None, None) => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn round_trip() {
        let text = "\"Root\"\n{\n\t\"a\"\t\t\"x \\\"q\\\" \\\\\"\n\t\"b\"\n\t{\n\t\t\"c\"\t\t\"\"\n\t}\n}\n";
        assert_eq!(write(&parse(text).unwrap()), text);
        assert_eq!(unescape("x \\\"q\\\" \\\\"), "x \"q\" \\");
        assert_eq!(escape("x \"q\" \\"), "x \\\"q\\\" \\\\");
    }

    /// Real files, if given: GAMEFIX_TEXT_VDF=a.vdf:b.vdf cargo test
    #[test]
    fn comments_are_skipped() {
        let text = "\"compat_tools\"\n{\n  \"GE-Proton11-7-x86_64\" // Internal name of this tool\n  {\n    // a // comment\n    \"install_path\" \".\"\n  }\n}\n";
        let doc = parse(text).unwrap();
        assert!(matches!(get(&doc, &["compat_tools", "GE-Proton11-7-x86_64", "install_path"]), Some(Node::Str(p)) if p == "."));
    }

    #[test]
    fn real_files_round_trip() {
        let Some(paths) = std::env::var_os("GAMEFIX_TEXT_VDF") else { return };
        for path in std::env::split_paths(&paths) {
            let text = std::fs::read_to_string(&path).unwrap();
            assert_eq!(write(&parse(&text).unwrap()), text, "{}", path.display());
        }
    }
}
