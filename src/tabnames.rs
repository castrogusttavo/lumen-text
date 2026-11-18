//! Names for the tabs: the file name, with as many parent folders in front as it takes to tell
//! apart open files that share a name (`workspace/route.ts` and `project/route.ts`).

use std::{collections::HashMap, path::Path};

/// How many parent folders may be added at most.
const MAX_PARENTS: usize = 6;

/// The last `k` folders above `path`, joined with `/` (fewer when the path is shorter).
fn parents(path: &Path, k: usize) -> String {
    let mut parts: Vec<String> = path
        .parent()
        .map(|p| {
            p.components()
                .filter_map(|c| match c {
                    std::path::Component::Normal(n) => Some(n.to_string_lossy().into_owned()),
                    _ => None,
                })
                .collect()
        })
        .unwrap_or_default();
    let keep = parts.len().min(k);
    parts.drain(..parts.len() - keep);
    parts.join("/")
}

/// One label per item `(file name, path)`. A name that only one open file has stays as it is; files
/// that share a name get the parent folders in front, using the fewest folders that make every
/// label of that group different (the same count for the whole group, so they read alike).
pub fn disambiguate(items: &[(&str, Option<&Path>)]) -> Vec<String> {
    let mut groups: HashMap<&str, Vec<usize>> = HashMap::new();
    for (i, (name, path)) in items.iter().enumerate() {
        if path.is_some() {
            groups.entry(name).or_default().push(i);
        }
    }
    let mut out: Vec<String> = items.iter().map(|(n, _)| (*n).to_string()).collect();
    for (name, idx) in groups.into_iter().filter(|(_, v)| v.len() > 1) {
        let paths: Vec<&Path> = idx.iter().filter_map(|&i| items[i].1).collect();
        let label = |p: &Path, k: usize| {
            let pre = parents(p, k);
            if pre.is_empty() { name.to_string() } else { format!("{pre}/{name}") }
        };
        let k = (1..=MAX_PARENTS)
            .find(|&k| {
                let labels: Vec<String> = paths.iter().map(|p| label(p, k)).collect();
                labels.iter().enumerate().all(|(a, l)| !labels[..a].contains(l))
            })
            .unwrap_or(MAX_PARENTS);
        for (&i, p) in idx.iter().zip(&paths) {
            out[i] = label(p, k);
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn run(list: &[(&str, &str)]) -> Vec<String> {
        let paths: Vec<std::path::PathBuf> = list.iter().map(|(_, p)| p.into()).collect();
        let items: Vec<(&str, Option<&Path>)> =
            list.iter().zip(&paths).map(|((n, _), p)| (*n, Some(p.as_path()))).collect();
        disambiguate(&items)
    }

    #[test]
    fn unique_names_stay_as_they_are() {
        assert_eq!(run(&[("a.ts", "/p/src/a.ts"), ("b.ts", "/p/src/b.ts")]), ["a.ts", "b.ts"]);
    }

    #[test]
    fn same_names_get_the_parent_folder_in_front() {
        let got = run(&[
            ("route.ts", "/p/app/api/workspace/route.ts"),
            ("route.ts", "/p/app/api/project/route.ts"),
            ("page.tsx", "/p/app/page.tsx"),
        ]);
        assert_eq!(got, ["workspace/route.ts", "project/route.ts", "page.tsx"]);
    }

    #[test]
    fn more_folders_are_added_until_the_labels_differ() {
        let got =
            run(&[("index.ts", "/p/a/x/index.ts"), ("index.ts", "/p/b/x/index.ts"), ("index.ts", "/p/b/y/index.ts")]);
        assert_eq!(got, ["a/x/index.ts", "b/x/index.ts", "b/y/index.ts"]);
    }

    #[test]
    fn a_file_with_no_path_keeps_its_name_and_does_not_count() {
        let p: &Path = Path::new("/p/a/route.ts");
        let got = disambiguate(&[("route.ts", Some(p)), ("route.ts", None)]);
        assert_eq!(got, ["route.ts", "route.ts"]);
    }

    #[test]
    fn a_short_path_never_panics() {
        assert_eq!(run(&[("a.ts", "/a.ts"), ("a.ts", "/x/a.ts")]), ["a.ts", "x/a.ts"]);
    }
}
