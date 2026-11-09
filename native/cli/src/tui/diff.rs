//! The diff an edit made (its old text against its new), shown under the
//! edit as the TypeScript TUI does: removed lines, added lines, and a little
//! context around them.

/// One line of a diff: `'-'`, `'+'` or `' '`, and the text.
pub type DiffLine = (char, String);

/// Lines of context kept around each change.
const CONTEXT: usize = 2;

/// The line diff of `old` → `new` (longest common subsequence), with only
/// [`CONTEXT`] unchanged lines around each change.
pub fn diff(old: &str, new: &str) -> Vec<DiffLine> {
    let a: Vec<&str> = old.lines().collect();
    let b: Vec<&str> = new.lines().collect();
    // Edits are small; a full table is fine, but very large ones are
    // shown whole as removed and added.
    if a.len() * b.len() > 4_000_000 {
        return a.iter().map(|l| ('-', l.to_string())).chain(b.iter().map(|l| ('+', l.to_string()))).collect();
    }
    let mut lcs = vec![vec![0u32; b.len() + 1]; a.len() + 1];
    for i in (0..a.len()).rev() {
        for j in (0..b.len()).rev() {
            lcs[i][j] = if a[i] == b[j] { lcs[i + 1][j + 1] + 1 } else { lcs[i + 1][j].max(lcs[i][j + 1]) };
        }
    }
    let mut all = Vec::new();
    let (mut i, mut j) = (0, 0);
    while i < a.len() || j < b.len() {
        if i < a.len() && j < b.len() && a[i] == b[j] {
            all.push((' ', a[i].to_string()));
            i += 1;
            j += 1;
        } else if i < a.len() && (j == b.len() || lcs[i + 1][j] >= lcs[i][j + 1]) {
            // Removals before additions, as diffs read.
            all.push(('-', a[i].to_string()));
            i += 1;
        } else {
            all.push(('+', b[j].to_string()));
            j += 1;
        }
    }
    // Keep the context near changes only.
    let near = |k: usize| {
        let lo = k.saturating_sub(CONTEXT);
        let hi = (k + CONTEXT + 1).min(all.len());
        all[lo..hi].iter().any(|(m, _)| *m != ' ')
    };
    all.iter().enumerate().filter(|(k, (m, _))| *m != ' ' || near(*k)).map(|(_, l)| l.clone()).collect()
}

/// `(added, removed)`.
pub fn counts(lines: &[DiffLine]) -> (usize, usize) {
    (lines.iter().filter(|(m, _)| *m == '+').count(), lines.iter().filter(|(m, _)| *m == '-').count())
}

/// The diff of an `edit` call, from its arguments.
pub fn of_call(call: &bubble_core::ToolCall) -> Vec<DiffLine> {
    if call.name != "edit" {
        return Vec::new();
    }
    let args = call.arguments_json();
    match (args["old_string"].as_str(), args["new_string"].as_str()) {
        (Some(old), Some(new)) => diff(old, new),
        _ => Vec::new(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn diffs_show_changes_with_a_little_context() {
        let old = "a\nb\nc\nd\ne\nf\ng\nh";
        let new = "a\nb\nc\nD\ne\nf\ng\nh\ni";
        let lines = diff(old, new);
        let shown: Vec<String> = lines.iter().map(|(m, l)| format!("{m}{l}")).collect();
        assert_eq!(shown, [" b", " c", "-d", "+D", " e", " f", " g", " h", "+i"]);
        assert_eq!(counts(&lines), (2, 1));
        assert!(diff("same", "same").is_empty());
    }
}
