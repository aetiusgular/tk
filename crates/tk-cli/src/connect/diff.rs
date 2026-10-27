//! A unified diff of two texts, line by line, for the dry run. The common
//! head and tail are trimmed first, so a one-entry change in a large
//! `~/.claude.json` costs only the changed lines; the middle is diffed by
//! longest common subsequence, capped so a huge rewrite degrades to
//! "replace these lines" instead of using quadratic memory.

const CONTEXT: usize = 3;
/// Middle sizes past which the LCS table is not built (cells).
const MAX_CELLS: usize = 4_000_000;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Op {
    Keep,
    Del,
    Add,
}

fn split_lines(t: &str) -> Vec<&str> {
    if t.is_empty() {
        return Vec::new();
    }
    t.split_inclusive('\n').collect()
}

fn ops(a: &[&str], b: &[&str]) -> Vec<(Op, usize, usize)> {
    // (op, index in a, index in b)
    let mut head = 0;
    while head < a.len() && head < b.len() && a[head] == b[head] {
        head += 1;
    }
    let mut tail = 0;
    while tail < a.len() - head
        && tail < b.len() - head
        && a[a.len() - 1 - tail] == b[b.len() - 1 - tail]
    {
        tail += 1;
    }
    let am = &a[head..a.len() - tail];
    let bm = &b[head..b.len() - tail];
    let mut out: Vec<(Op, usize, usize)> = (0..head).map(|i| (Op::Keep, i, i)).collect();
    if am.len().saturating_mul(bm.len()) <= MAX_CELLS {
        let (n, m) = (am.len(), bm.len());
        let mut t = vec![0u32; (n + 1) * (m + 1)];
        for i in (0..n).rev() {
            for j in (0..m).rev() {
                t[i * (m + 1) + j] = if am[i] == bm[j] {
                    t[(i + 1) * (m + 1) + j + 1] + 1
                } else {
                    t[(i + 1) * (m + 1) + j].max(t[i * (m + 1) + j + 1])
                };
            }
        }
        let (mut i, mut j) = (0, 0);
        while i < n || j < m {
            if i < n && j < m && am[i] == bm[j] {
                out.push((Op::Keep, head + i, head + j));
                i += 1;
                j += 1;
            } else if i < n && (j == m || t[(i + 1) * (m + 1) + j] >= t[i * (m + 1) + j + 1]) {
                // Deletions first, as `diff -u` prints them.
                out.push((Op::Del, head + i, head + j));
                i += 1;
            } else {
                out.push((Op::Add, head + i, head + j));
                j += 1;
            }
        }
    } else {
        out.extend((0..am.len()).map(|i| (Op::Del, head + i, head)));
        out.extend((0..bm.len()).map(|j| (Op::Add, head + am.len(), head + j)));
    }
    let (al, bl) = (a.len(), b.len());
    out.extend((0..tail).map(|k| (Op::Keep, al - tail + k, bl - tail + k)));
    out
}

fn show(line: &str) -> String {
    match line.strip_suffix('\n') {
        Some(l) => l.to_string(),
        None => format!("{line}\n\\ No newline at end of file"),
    }
}

/// `diff -u` style output. Empty when the texts are equal.
pub fn unified(old: &str, new: &str, old_label: &str, new_label: &str) -> String {
    if old == new {
        return String::new();
    }
    let a = split_lines(old);
    let b = split_lines(new);
    let ops = ops(&a, &b);
    let changed: Vec<usize> = ops
        .iter()
        .enumerate()
        .filter(|(_, o)| o.0 != Op::Keep)
        .map(|(i, _)| i)
        .collect();
    let mut out = format!("--- {old_label}\n+++ {new_label}\n");
    let mut k = 0;
    while k < changed.len() {
        let start = changed[k].saturating_sub(CONTEXT);
        let mut end = changed[k];
        while k < changed.len() && changed[k] <= end + 2 * CONTEXT + 1 {
            end = changed[k];
            k += 1;
        }
        let end = (end + CONTEXT + 1).min(ops.len());
        let hunk = &ops[start..end];
        let a_start = hunk.first().map_or(0, |o| o.1);
        let b_start = hunk.first().map_or(0, |o| o.2);
        let a_len = hunk.iter().filter(|o| o.0 != Op::Add).count();
        let b_len = hunk.iter().filter(|o| o.0 != Op::Del).count();
        let a_from = if a_len == 0 { a_start } else { a_start + 1 };
        let b_from = if b_len == 0 { b_start } else { b_start + 1 };
        out.push_str(&format!("@@ -{a_from},{a_len} +{b_from},{b_len} @@\n"));
        for (op, i, j) in hunk {
            match op {
                Op::Keep => out.push_str(&format!(" {}\n", show(a[*i]))),
                Op::Del => out.push_str(&format!("-{}\n", show(a[*i]))),
                Op::Add => out.push_str(&format!("+{}\n", show(b[*j]))),
            }
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::unified;

    #[test]
    fn equal_texts_have_no_diff() {
        assert_eq!(unified("a\n", "a\n", "x", "y"), "");
    }

    #[test]
    fn insertion_in_the_middle_with_context() {
        let old = "1\n2\n3\n4\n5\n6\n7\n8\n";
        let new = "1\n2\n3\n4\nnew\n5\n6\n7\n8\n";
        assert_eq!(
            unified(old, new, "f", "f (tk)"),
            "--- f\n+++ f (tk)\n@@ -2,6 +2,7 @@\n 2\n 3\n 4\n+new\n 5\n 6\n 7\n"
        );
    }

    #[test]
    fn new_file_and_missing_final_newline() {
        assert_eq!(
            unified("", "{}", "f", "g"),
            "--- f\n+++ g\n@@ -0,0 +1,1 @@\n+{}\n\\ No newline at end of file\n"
        );
    }

    #[test]
    fn two_far_changes_make_two_hunks() {
        let old: String = (1..=30).map(|i| format!("{i}\n")).collect();
        let new: String = (1..=30)
            .map(|i| match i {
                3 => "three\n".to_string(),
                27 => "x\n".to_string(),
                _ => format!("{i}\n"),
            })
            .collect();
        let d = unified(&old, &new, "a", "b");
        assert_eq!(d.matches("@@ -").count(), 2, "{d}");
        assert!(d.contains("-3\n+three\n"));
        assert!(d.contains("-27\n+x\n"));
    }

    #[test]
    fn huge_rewrite_degrades_without_quadratic_memory() {
        let old: String = (0..5000).map(|i| format!("a{i}\n")).collect();
        let new: String = (0..5000).map(|i| format!("b{i}\n")).collect();
        let d = unified(&old, &new, "a", "b");
        assert_eq!(
            d.lines()
                .filter(|l| l.starts_with('-') && !l.starts_with("---"))
                .count(),
            5000
        );
    }
}
