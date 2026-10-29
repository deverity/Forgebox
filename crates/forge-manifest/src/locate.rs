//! Best-effort line/column of a validator field path in the manifest text.
//!
//! The semantic validator reports dotted paths (`spec.service.daemon.logs[0].access.ts`);
//! the YAML parser only gives positions for syntax and schema errors. [`locate`] walks the
//! text of a manifest written in the style of docs/04 (block maps, `- ` lists and one-line
//! `{ a: b }` flow maps) and returns the position of the deepest segment it can find, so
//! lint issues can be printed as `file:line:column`.

use crate::yaml::Location;

/// One line of the manifest: 1-based number, indentation (in characters) and text.
struct Line<'a> {
    number: usize,
    text: &'a str,
}

/// A window of the text that the next segment is searched in.
struct Scope {
    /// First line index (0-based) of the window.
    start: usize,
    /// One past the last line index.
    end: usize,
    /// Column (0-based) from which to search on the first line.
    from_col: usize,
}

fn indent(text: &str) -> usize {
    text.chars().take_while(|c| *c == ' ').count()
}

fn is_blank(text: &str) -> bool {
    let t = text.trim_start();
    t.is_empty() || t.starts_with('#')
}

/// Column (0-based, in characters) of `key:` in `text` at or after `from`, as a map key
/// (preceded by line start, spaces, `{`, `,` or `- `).
fn find_key(text: &str, key: &str, from: usize) -> Option<usize> {
    let chars: Vec<char> = text.chars().collect();
    let key_chars: Vec<char> = key.chars().collect();
    let mut i = from;
    while i + key_chars.len() <= chars.len() {
        if chars[i..i + key_chars.len()] == key_chars[..] {
            let before_ok = i == 0 || matches!(chars[i - 1], ' ' | '{' | ',' | '-' | '[');
            let mut j = i + key_chars.len();
            let after_ok = chars.get(j) == Some(&':') && {
                j += 1;
                chars.get(j).is_none_or(|c| *c == ' ')
            };
            if before_ok && after_ok {
                return Some(i);
            }
        }
        i += 1;
    }
    None
}

/// Position of `field` (`spec.service.daemon.logs[0].access.ts.pointer`) in `text`, or of
/// its deepest ancestor that exists. `None` when not even the first segment is found.
pub fn locate(text: &str, field: &str) -> Option<Location> {
    let text = text.strip_prefix('\u{feff}').unwrap_or(text);
    let lines: Vec<Line<'_>> = text
        .lines()
        .enumerate()
        .map(|(i, text)| Line {
            number: i + 1,
            text,
        })
        .collect();
    let mut scope = Scope {
        start: 0,
        end: lines.len(),
        from_col: 0,
    };
    let mut found = None;
    for segment in field.split('.') {
        let (key, indexes) = match segment.find('[') {
            Some(p) => (&segment[..p], &segment[p..]),
            None => (segment, ""),
        };
        let Some((line_idx, col)) = (scope.start..scope.end).find_map(|i| {
            if is_blank(lines[i].text) {
                return None;
            }
            let from = if i == scope.start { scope.from_col } else { 0 };
            find_key(lines[i].text, key, from).map(|c| (i, c))
        }) else {
            return found;
        };
        found = Some(Location {
            line: lines[line_idx].number,
            column: col + 1,
        });
        // Window of the value: the rest of this line plus the deeper-indented lines below.
        let key_indent = indent(lines[line_idx].text);
        let mut end = line_idx + 1;
        while end < scope.end && (is_blank(lines[end].text) || indent(lines[end].text) > key_indent)
        {
            end += 1;
        }
        scope = Scope {
            start: line_idx,
            end,
            from_col: col + key.chars().count() + 1,
        };
        for index in indexes.split('[').filter(|s| !s.is_empty()) {
            let Ok(n) = index.trim_end_matches(']').parse::<usize>() else {
                return found;
            };
            match nth_item(&lines, &scope, n) {
                Some(next) => {
                    found = Some(Location {
                        line: lines[next.start].number,
                        column: next.from_col + 1,
                    });
                    scope = next;
                }
                None => return found,
            }
        }
    }
    found
}

/// The `n`-th `- ` item of the list that starts in `scope` (block list, one item per
/// `-` at the shallowest indentation).
fn nth_item(lines: &[Line<'_>], scope: &Scope, n: usize) -> Option<Scope> {
    let items: Vec<(usize, usize, usize)> = (scope.start + 1..scope.end)
        .filter_map(|i| {
            let t = lines[i].text;
            let ind = indent(t);
            let rest = &t[ind..];
            (rest.starts_with("- ") || rest == "-").then_some((i, ind, ind))
        })
        .collect();
    let shallowest = items.iter().map(|(_, ind, _)| *ind).min()?;
    let items: Vec<_> = items
        .into_iter()
        .filter(|(_, ind, _)| *ind == shallowest)
        .collect();
    let (start, ind, _) = *items.get(n)?;
    let mut end = start + 1;
    while end < scope.end && (is_blank(lines[end].text) || indent(lines[end].text) > ind) {
        end += 1;
    }
    Some(Scope {
        start,
        end,
        from_col: ind + 2,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    const TEXT: &str = "\
spec:
  service:
    daemon:
      logs:
        - { label: Error log, path: a.log }
        - label: Access
          path: b.json
          access:
            ts: { pointer: /t, unit: s }
            rotate:
              strategy: rename-reopen
";

    fn at(field: &str) -> (usize, usize) {
        let l = locate(TEXT, field).unwrap_or_else(|| panic!("{field}"));
        (l.line, l.column)
    }

    #[test]
    fn finds_block_keys_and_list_items() {
        assert_eq!(at("spec.service"), (2, 3));
        assert_eq!(at("spec.service.daemon.logs"), (4, 7));
        assert_eq!(at("spec.service.daemon.logs[0]"), (5, 11));
        assert_eq!(at("spec.service.daemon.logs[0].path"), (5, 31));
        assert_eq!(at("spec.service.daemon.logs[1].access"), (8, 11));
        assert_eq!(
            at("spec.service.daemon.logs[1].access.rotate.strategy"),
            (11, 15)
        );
    }

    #[test]
    fn finds_keys_inside_flow_maps() {
        assert_eq!(at("spec.service.daemon.logs[1].access.ts.unit"), (9, 32));
    }

    #[test]
    fn falls_back_to_the_deepest_existing_ancestor() {
        assert_eq!(
            at("spec.service.daemon.logs[1].access.rotate.reopen"),
            (10, 13)
        );
        assert_eq!(at("spec.service.daemon.logs[7].access"), (4, 7));
        assert!(locate(TEXT, "nothing").is_none());
    }
}
