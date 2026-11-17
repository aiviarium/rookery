//! The spellings of rookery 0.5 that mean something else now, or nothing.
//! Each fails with the spelling to use instead, so that a script or a
//! habit does not do something it did not mean.

use super::fail::Fail;

/// The flags of every command that take a value, which come before the
/// first word of the command.
const VALUED: &[&str] = &["--server", "--token-file", "-o", "--output", "--color"];

/// Old command words and what to type now.
const OLD: &[(&[&str], &str)] = &[
    (&["list"], "rookery host list, or rookery ls"),
    (&["remove"], "rookery host remove NAME"),
    (
        &["profile"],
        "rookery host export NAME, rookery host import NAME FILE or rookery host set NAME KEY=VALUE",
    ),
    (&["credentials"], "rookery host credentials NAME"),
    (&["adopt"], "rookery host adopt NAME [ADDRESS]"),
    (&["iso"], "rookery host iso NAME"),
    (&["seed"], "rookery host seed NAME"),
    (&["inventory"], "rookery host inventory [NAME…]"),
    (&["provisioners"], "rookery provisioner list"),
    (
        &["provision"],
        "rookery host create NAME --provisioner PROVISIONER",
    ),
    (
        &["instance"],
        "rookery host start NAME, rookery host stop NAME or rookery host remove NAME --destroy",
    ),
    (
        &["deploy"],
        "rookery resource create NAME --provisioner PROVISIONER --spec FILE",
    ),
    (&["actions"], "rookery action list HOST"),
    (&["manage"], "rookery run HOST ACTION"),
    (&["attach"], "rookery session attach HOST [ID]"),
    (&["sessions"], "rookery session list HOST"),
    (&["output"], "rookery session logs HOST ID"),
    (&["call"], "rookery service call HOST SERVICE METHOD"),
    (&["plugins"], "rookery plugin list, add, remove or update"),
    (&["connect"], "rookery login URL"),
    (&["setup-key"], "rookery server setup-key"),
    (&["store"], "rookery server store"),
    (&["openapi"], "rookery server api openapi"),
    (&["asyncapi"], "rookery server api asyncapi"),
    (&["operations"], "rookery server api operations"),
    (
        &["publish"],
        "rookery registry publish DESTINATION --host HOST",
    ),
    (&["image"], "rookery registry push-image DESTINATION"),
    (
        &["host", "install"],
        "rookery host apply NAME for a managed host, or rookery run HOST install-controller",
    ),
    (&["host", "type"], "rookery host set NAME type=TYPE"),
    (&["key", "new"], "rookery key create NAME"),
    (&["key", "add"], "rookery key import NAME FILE"),
    (&["key", "host"], "rookery host verify NAME [--accept]"),
    (
        &["registry", "repo"],
        "rookery registry add-repo NAME FORMAT REPOSITORY",
    ),
    (&["apply", "--behind"], "rookery rollout"),
    (&["apply", "--all"], "rookery rollout --all"),
    (&["channel"], "rookery channel listen HOST"),
];

/// The words of the command, without its flags.
fn words(args: &[String]) -> Vec<&str> {
    let mut words = vec![];
    let mut rest = args.iter().skip(1);
    while let Some(arg) = rest.next() {
        if VALUED.contains(&arg.as_str()) {
            rest.next();
            continue;
        }
        // What comes after `--` is a command for a host, not rookery's words.
        if arg == "--" {
            break;
        }
        // The flags the old spellings name are kept; the others are left out.
        if arg.starts_with('-') && !matches!(arg.as_str(), "--behind" | "--all") {
            continue;
        }
        words.push(arg.as_str());
    }
    words
}

/// Why these arguments are refused, when they are a spelling of before.
pub fn refuse(args: &[String]) -> Option<Fail> {
    if args
        .iter()
        .skip(1)
        .any(|arg| arg == "--token" || arg.starts_with("--token="))
    {
        return Some(Fail::usage(
            "--token puts the token where other users can read it. Use --token-file FILE or --token-stdin, or keep it with rookery login URL",
        ));
    }
    let words = words(args);
    let first = *words.first()?;
    for (old, new) in OLD {
        let matches = old
            .iter()
            .enumerate()
            .all(|(index, word)| words.get(index) == Some(word));
        if !matches {
            continue;
        }
        // `rookery channel listen` is the new spelling of the old `channel`.
        if old == &["channel"] && words.get(1) == Some(&"listen") {
            continue;
        }
        // A word that is a command now as well, with its new grammar.
        if old.len() == 1 && first == "channel" && words.len() < 2 {
            continue;
        }
        return Some(Fail::usage(format!(
            "rookery {} is {new} now",
            old.join(" ")
        )));
    }
    // `rookery resource ID remove` and the like: the action goes first now.
    if let [
        noun @ ("resource" | "provisioner" | "host"),
        name,
        action,
        ..,
    ] = words[..]
        && !name.starts_with('-')
        && !is_action(noun, name)
        && is_action(noun, action)
    {
        return Some(Fail::usage(format!(
            "The action comes before the name now: rookery {noun} {action} {name}"
        )));
    }
    None
}

fn is_action(noun: &str, word: &str) -> bool {
    let command = super::command();
    command
        .find_subcommand(noun)
        .is_some_and(|thing| thing.find_subcommand(word).is_some())
}

/// The table of the old spellings on the page about upgrading, between its
/// markers, as a test writes it.
#[cfg(test)]
fn table() -> String {
    let mut out = String::from("| rookery 0.5 | Now |\n| --- | --- |\n");
    for (old, new) in OLD {
        out += &format!("| `rookery {}` | `{new}` |\n", old.join(" "));
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The page about upgrading from 0.5, from the repository's root.
    const PAGE: &str = "docs/content/docs/(guide)/use/upgrading.mdx";
    const START: &str = "{/* old-spellings:start */}\n";
    const END: &str = "{/* old-spellings:end */}";

    #[test]
    fn the_upgrade_page_lists_the_old_spellings() {
        let file = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../..")
            .join(PAGE);
        let text = std::fs::read_to_string(&file).unwrap();
        let (before, rest) = text.split_once(START).expect("the start marker");
        let (_, after) = rest.split_once(END).expect("the end marker");
        // Blank lines around it, so that MDX reads it as a table.
        let page = format!("{before}{START}\n{}\n{END}{after}", table());
        if std::env::var_os("ROOKERY_UPDATE_DOCS").is_some() {
            std::fs::write(&file, &page).unwrap();
            return;
        }
        assert!(
            text == page,
            "{PAGE} is stale: write it again with ROOKERY_UPDATE_DOCS=1 cargo test -p rookery"
        );
    }

    fn said(line: &str) -> Option<String> {
        let args: Vec<String> = std::iter::once("rookery")
            .chain(line.split_whitespace())
            .map(String::from)
            .collect();
        refuse(&args).map(|fail| fail.message)
    }

    #[test]
    fn old_spellings_name_the_new_ones() {
        assert_eq!(
            said("remove studio").as_deref(),
            Some("rookery remove is rookery host remove NAME now")
        );
        assert_eq!(
            said("--server https://rookery.example apply --behind").as_deref(),
            Some("rookery apply --behind is rookery rollout now")
        );
        assert_eq!(
            said("resource r1 remove").as_deref(),
            Some("The action comes before the name now: rookery resource remove r1")
        );
        assert!(said("channel studio").is_some());
        assert!(said("--token s3cret ls").unwrap().contains("--token-file"));
        for fine in [
            "host remove studio",
            "apply studio",
            "channel listen studio",
            "ls",
            "resource show r1",
            "channel --help",
            "rollout --all",
        ] {
            assert_eq!(said(fine), None, "{fine}");
        }
    }
}
