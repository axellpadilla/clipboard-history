//! The keyboard shortcut list, in one place so the `?` view and `README.md`
//! cannot drift apart: [`tests::readme_lists_every_shortcut`] fails if a row is
//! missing from the README.

/// One row of the shortcut list: the keys as they are pressed, and what they
/// do.
pub struct Shortcut {
    pub keys: &'static str,
    pub description: &'static str,
}

pub const SHORTCUTS: &[Shortcut] = &[
    Shortcut {
        keys: "Type",
        description: "search the clipboard history",
    },
    Shortcut {
        keys: "Enter / Ctrl+V",
        description: "paste the highlighted entry",
    },
    Shortcut {
        keys: "Ctrl+Shift+V",
        description: "paste it as plain text",
    },
    Shortcut {
        keys: "Ctrl+0-9",
        description: "paste the Nth recent entry",
    },
    Shortcut {
        keys: "Ctrl+Shift+0-9",
        description: "paste the Nth favorite",
    },
    Shortcut {
        keys: "Up / Down",
        description: "move the highlight",
    },
    Shortcut {
        keys: "Home / End",
        description: "jump to the first or last entry",
    },
    Shortcut {
        keys: "PgUp / PgDn",
        description: "move by a page",
    },
    Shortcut {
        keys: "Left / Right",
        description: "collapse or expand Favorites, or open or close the details of the \
                      highlighted entry",
    },
    Shortcut {
        keys: "Ctrl+Shift+Up/Down",
        description: "move a favorite earlier or later",
    },
    Shortcut {
        keys: "Ctrl+D",
        description: "toggle details",
    },
    Shortcut {
        keys: "Delete",
        description: "delete the highlighted entry",
    },
    Shortcut {
        keys: "Ctrl+R",
        description: "reload the database",
    },
    Shortcut {
        keys: "Ctrl+Tab / Ctrl+Shift+Tab",
        description: "cycle tabs",
    },
    Shortcut {
        keys: "Alt+1-5",
        description: "jump to a tab: All, Text, Images, Favorites, Settings",
    },
    Shortcut {
        keys: "Alt+X / Alt+M",
        description: "cycle the search kind: plain, RegEx, MIME",
    },
    Shortcut {
        keys: "?",
        description: "toggle this list",
    },
    Shortcut {
        keys: "Esc",
        description: "clear the search, close details, then close the window",
    },
];

#[cfg(test)]
mod tests {
    use super::SHORTCUTS;

    /// Compares the README's shortcut section against the table as plain text:
    /// `<kbd>` markup and whitespace are stripped from both sides, so the two
    /// only have to list the same keys and descriptions.
    #[test]
    fn readme_lists_every_shortcut() {
        let readme = include_str!("../README.md");
        let (_, after_heading) = readme
            .split_once("## Keyboard shortcuts")
            .expect("README.md has a `## Keyboard shortcuts` section");
        let (section, _) = after_heading
            .split_once("\n## ")
            .unwrap_or((after_heading, ""));
        let flat = flatten(&section.replace("<kbd>", "").replace("</kbd>", ""));

        for shortcut in SHORTCUTS {
            for text in [shortcut.keys, shortcut.description] {
                assert!(
                    flat.contains(&flatten(text)),
                    "README.md's `## Keyboard shortcuts` section is missing: {text}"
                );
            }
        }
    }

    fn flatten(text: &str) -> String {
        text.chars().filter(|c| !c.is_whitespace()).collect()
    }
}
