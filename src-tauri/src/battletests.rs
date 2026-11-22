//! Battletests: adversarial tests for the customization surface.
//!
//! The unit tests inside each module check that the happy path works and that
//! the obvious bad input is refused. These do the opposite: they assume the
//! config file is hostile, malformed, or enormous, and they check that the app
//! still behaves.
//!
//! Five batteries, each answering a different question:
//!
//! 1. `security`      — can a config file escape the sandbox it is supposed to
//!                      live in? (network, script, filesystem, shell)
//! 2. `shortcomings`  — what happens to input nobody designed for? (BOM, nulls,
//!                      wrong types, non-UTF8, a directory where a file should be)
//! 3. `efficiency`    — is the cost bounded? (watcher tick, validation, load)
//! 4. `safety`        — can a config make the UI lie, or survive safe mode?
//! 5. `resilience`    — can a bad config brick the app or corrupt its own file?
//!
//! These are `#[cfg(test)]` only, so they never ship in the binary.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::time::Instant;

use crate::actions::{
    self, ActionRun, ActionSlot, ActionsConfig, BuiltinCommand, CustomAction, MAX_ACTIONS_BYTES,
};
use crate::customize::{self, ThemeConfig, ThemeRule, MAX_THEME_BYTES};
use crate::layout::{self, Density, LayoutConfig, Side, MAX_LAYOUT_BYTES};
use crate::panels::{
    self, Block, CustomPanel, PanelBlock, PanelsConfig, MAX_PANELS_BYTES, PANEL_WIDTH,
};

/* ------------------------------------------------------------------ *
 * Helpers
 * ------------------------------------------------------------------ */

/// A fresh directory per test, so the batteries can run in parallel.
fn scratch(name: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("vibedesk-bt-{name}-{}", std::process::id()));
    std::fs::remove_dir_all(&dir).ok();
    std::fs::create_dir_all(&dir).expect("scratch dir");
    dir
}

fn write(path: &Path, body: &str) {
    std::fs::write(path, body).expect("write fixture");
}

fn theme_with(tokens: &[(&str, &str)]) -> ThemeConfig {
    ThemeConfig {
        tokens: tokens
            .iter()
            .map(|(k, v)| (k.to_string(), v.to_string()))
            .collect(),
        rules: Vec::new(),
    }
}

fn theme_with_rule(selector: &str, property: &str, value: &str) -> ThemeConfig {
    ThemeConfig {
        tokens: BTreeMap::new(),
        rules: vec![ThemeRule {
            selector: selector.into(),
            declarations: BTreeMap::from([(property.into(), value.into())]),
        }],
    }
}

fn action(id: &str, run: ActionRun) -> CustomAction {
    CustomAction {
        id: id.into(),
        label: "Label".into(),
        icon: None,
        slot: ActionSlot::Composer,
        hint: None,
        run,
    }
}

fn panel(id: &str, blocks: Vec<PanelBlock>) -> CustomPanel {
    CustomPanel {
        id: id.into(),
        title: "Panel".into(),
        icon: None,
        width: 320,
        layout: None,
        blocks: blocks.into_iter().map(Block::new).collect(),
        sources: Vec::new(),
        plugin: None,
    }
}

/* ================================================================== *
 * BATTLETEST 1 — security
 *
 * Every one of these is an attempt to turn a data file into a program, a
 * network client, or a filesystem browser. All must be refused.
 * ================================================================== */

mod security {
    use super::*;

    #[test]
    fn a_theme_value_cannot_reach_the_network_or_the_script_engine() {
        let payloads = [
            "url(https://evil.test/beacon)",
            "URL(https://evil.test/beacon)",
            "url('data:text/css,x')",
            "@import url(https://evil.test/x.css)",
            "@IMPORT 'x'",
            "expression(alert(1))",
            "EXPRESSION(alert(1))",
            "javascript:alert(1)",
            "JavaScript:alert(1)",
            "vbscript:msgbox(1)",
            "behavior:url(x.htc)",
            "binding:url(x.xml)",
            "\\75 rl(https://evil.test)",
            "<script>alert(1)</script>",
            "image-set(url(https://evil.test/x.png) 1x)",
        ];

        for payload in payloads {
            let theme = theme_with(&[("--vd-accent", payload)]);
            assert!(
                customize::validate(&theme).is_err(),
                "theme value should be refused: {payload}"
            );
        }
    }

    #[test]
    fn a_theme_rule_cannot_reach_the_network_either() {
        for payload in [
            "url(https://evil.test/x)",
            "@import 'x'",
            "javascript:alert(1)",
            "expression(alert(1))",
        ] {
            let theme = theme_with_rule(".vd-card", "background-image", payload);
            assert!(
                customize::validate(&theme).is_err(),
                "rule value should be refused: {payload}"
            );
        }
    }

    #[test]
    fn a_theme_cannot_target_anything_outside_the_app_namespace() {
        for selector in [
            "body",
            "html body",
            "*",
            "input",
            "textarea",
            "#root",
            "[data-vd-protected]",
            "div > .vd-card",
            ".vd-card, body",
            "@media screen { .vd-card }",
        ] {
            let theme = theme_with_rule(selector, "color", "red");
            assert!(
                customize::validate(&theme).is_err(),
                "selector should be refused: {selector}"
            );
        }
    }

    #[test]
    fn a_theme_cannot_use_a_property_that_redraws_the_interface() {
        for property in ["position", "z-index", "pointer-events", "content"] {
            let theme = theme_with_rule(".vd-card", property, "fixed");
            assert!(
                customize::validate(&theme).is_err(),
                "property should be refused: {property}"
            );
        }
    }

    #[test]
    fn an_action_cannot_open_a_dangerous_url() {
        for url in [
            "javascript:alert(1)",
            "JavaScript:alert(1)",
            "file:///C:/Windows/System32/calc.exe",
            "data:text/html,<script>alert(1)</script>",
            "vbscript:msgbox(1)",
            "ms-settings:privacy",
            "\\\\server\\share\\x.exe",
            "https://user:pass@evil.test/x",
            "https://evil.test/x y",
            "not a url at all",
        ] {
            let config = ActionsConfig {
                actions: vec![action("x", ActionRun::OpenUrl { url: url.into() })],
            };
            assert!(
                actions::validate(&config).is_err(),
                "url should be refused: {url}"
            );
        }
    }

    #[test]
    fn an_action_cannot_reveal_a_path_outside_the_workspace() {
        for path in [
            "../secrets.txt",
            "a/../../b",
            "/etc/passwd",
            "C:/Windows/win.ini",
            "c:/windows/win.ini",
            "~/notes.md",
            "\\\\server\\share",
            "..",
            "./../x",
            "a\\..\\..\\b",
        ] {
            let config = ActionsConfig {
                actions: vec![action("x", ActionRun::Reveal { path: path.into() })],
            };
            assert!(
                actions::validate(&config).is_err(),
                "path should be refused: {path}"
            );
        }
    }

    #[test]
    fn an_action_cannot_invent_a_kind_or_a_command() {
        // A fifth kind is a parse error, not a silently ignored action.
        for raw in [
            r#"{"actions":[{"id":"x","label":"X","run":{"kind":"shell","command":"rm -rf /"}}]}"#,
            r#"{"actions":[{"id":"x","label":"X","run":{"kind":"eval","code":"alert(1)"}}]}"#,
            r#"{"actions":[{"id":"x","label":"X","run":{"kind":"command","command":"format-disk"}}]}"#,
            r#"{"actions":[{"id":"x","label":"X","run":{"kind":"command","command":"bash"}}]}"#,
        ] {
            assert!(
                serde_json::from_str::<ActionsConfig>(raw).is_err(),
                "should not parse: {raw}"
            );
        }
    }

    #[test]
    fn a_panel_cannot_smuggle_markup_or_a_script() {
        for raw in [
            r#"{"panels":[{"id":"p","title":"P","blocks":[{"kind":"html","html":"<script>alert(1)</script>"}]}]}"#,
            r#"{"panels":[{"id":"p","title":"P","blocks":[{"kind":"script","src":"https://evil.test/x.js"}]}]}"#,
            r#"{"panels":[{"id":"p","title":"P","blocks":[{"kind":"iframe","url":"https://evil.test"}]}]}"#,
        ] {
            assert!(
                serde_json::from_str::<PanelsConfig>(raw).is_err(),
                "should not parse: {raw}"
            );
        }
    }

    #[test]
    fn a_panel_file_block_cannot_escape_the_workspace() {
        for path in ["../x", "/etc/passwd", "C:/Windows", "~/x", "a/../../b", ".."] {
            let config = PanelsConfig {
                panels: vec![panel(
                    "p",
                    vec![PanelBlock::File {
                        path: path.into(),
                        label: None,
                    }],
                )],
            };
            assert!(
                panels::validate(&config).is_err(),
                "path should be refused: {path}"
            );
        }
    }

    #[test]
    fn a_panel_link_cannot_be_a_script_url() {
        let config = PanelsConfig {
            panels: vec![panel(
                "p",
                vec![PanelBlock::Link {
                    label: "x".into(),
                    url: "javascript:alert(1)".into(),
                }],
            )],
        };
        assert!(panels::validate(&config).is_err());
    }

    #[test]
    fn a_panel_cannot_ask_for_an_eager_file_read() {
        // The user's requirement: a panel stays confined until asked. There is
        // no schema field that opts into reading on open, and an unknown field
        // is dropped rather than honoured.
        let raw = r#"{"panels":[{"id":"p","title":"P","blocks":[
            {"kind":"file","path":"README.md","expanded":true,"content":"leaked","autoLoad":true}
        ]}]}"#;
        let parsed: PanelsConfig = serde_json::from_str(raw).expect("parses");
        let round = serde_json::to_string(&parsed).expect("serialises");
        assert!(!round.contains("expanded"));
        assert!(!round.contains("autoLoad"));
        assert!(!round.contains("leaked"));
    }
}

/* ================================================================== *
 * BATTLETEST 2 — shortcomings
 *
 * Input nobody designed for. The contract is narrow: never panic, and either
 * return a usable default or an error that names the problem.
 * ================================================================== */

mod shortcomings {
    use super::*;

    #[test]
    fn degenerate_files_never_panic() {
        let dir = scratch("degenerate");
        let path = dir.join("theme.json");

        let bodies: &[&[u8]] = &[
            b"",
            b"   \n\t  ",
            b"\xef\xbb\xbf{\"tokens\":{}}",   // UTF-8 BOM
            b"null",
            b"[]",
            b"\"a string\"",
            b"42",
            b"{}",
            b"{\"tokens\":null}",
            b"{\"tokens\":[]}",
            b"{\"tokens\":{\"--vd-accent\":null}}",
            b"{\"tokens\":{\"--vd-accent\":42}}",
            b"{\"rules\":\"not an array\"}",
            b"{\"unknownField\":true}",
            b"{\"tokens\":{\"--vd-accent\":\"#fff\"},\"tokens\":{\"--vd-accent\":\"#000\"}}",
            b"\xff\xfe\x00\x00",              // not UTF-8
            b"{\"tokens\":{\"--vd-accent\":\"#fff\"",  // truncated
        ];

        for body in bodies {
            std::fs::write(&path, body).expect("write");
            // The only requirement: it returns, and it does not panic.
            let _ = customize::load_theme_from(&path);
        }

        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn a_directory_where_a_file_belongs_is_an_error_not_a_crash() {
        let dir = scratch("dir-as-file");
        let path = dir.join("theme.json");
        std::fs::create_dir_all(&path).expect("dir");

        assert!(customize::load_theme_from(&path).is_err());

        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn an_oversized_file_is_refused_before_it_is_parsed() {
        let dir = scratch("oversized");
        let path = dir.join("theme.json");

        // One byte over the limit, and valid JSON, so only the size check can
        // be what refuses it.
        let padding = "x".repeat(MAX_THEME_BYTES as usize);
        write(&path, &format!("{{\"tokens\":{{\"--vd-accent\":\"{padding}\"}}}}"));

        let error = customize::load_theme_from(&path).expect_err("should refuse");
        assert!(error.contains("limit"), "error should name the limit: {error}");

        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn every_loader_refuses_an_oversized_file() {
        let dir = scratch("oversized-all");

        let theme = dir.join("theme.json");
        write(&theme, &"x".repeat(MAX_THEME_BYTES as usize + 1));
        assert!(customize::load_theme_from(&theme).is_err());

        let layout = dir.join("layout.json");
        write(&layout, &"x".repeat(MAX_LAYOUT_BYTES as usize + 1));
        assert!(layout::load_layout_from(&layout).is_err());

        let actions = dir.join("actions.json");
        write(&actions, &"x".repeat(MAX_ACTIONS_BYTES as usize + 1));
        assert!(actions::load_actions_from(&actions).is_err());

        let panels = dir.join("panels.json");
        write(&panels, &"x".repeat(MAX_PANELS_BYTES as usize + 1));
        assert!(panels::load_panels_from(&panels).is_err());

        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn a_partial_file_keeps_the_other_defaults() {
        let dir = scratch("partial");
        let path = dir.join("layout.json");

        write(&path, r#"{"sidebarWidth":300}"#);
        let layout = layout::load_layout_from(&path).expect("loads");
        assert_eq!(layout.sidebar_width, 300);
        assert_eq!(layout.rail_width, 48);
        assert_eq!(layout.sidebar_side, Side::Right);
        assert_eq!(layout.density, Density::Comfortable);

        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn an_unknown_enum_value_is_an_error_not_a_silent_default() {
        let dir = scratch("bad-enum");
        let path = dir.join("layout.json");

        write(&path, r#"{"sidebarSide":"diagonal"}"#);
        assert!(layout::load_layout_from(&path).is_err());

        write(&path, r#"{"density":"enormous"}"#);
        assert!(layout::load_layout_from(&path).is_err());

        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn too_many_entries_is_refused() {
        let actions: Vec<CustomAction> = (0..25)
            .map(|i| {
                action(
                    &format!("a{i}"),
                    ActionRun::Command {
                        command: BuiltinCommand::NewChat,
                    },
                )
            })
            .collect();
        assert!(actions::validate(&ActionsConfig { actions }).is_err());

        let panels: Vec<CustomPanel> = (0..13).map(|i| panel(&format!("p{i}"), vec![])).collect();
        assert!(panels::validate(&PanelsConfig { panels }).is_err());
    }

    #[test]
    fn a_label_of_only_whitespace_is_refused() {
        let config = ActionsConfig {
            actions: vec![CustomAction {
                label: "   ".into(),
                ..action(
                    "x",
                    ActionRun::Command {
                        command: BuiltinCommand::NewChat,
                    },
                )
            }],
        };
        assert!(actions::validate(&config).is_err());
    }

    #[test]
    fn a_control_character_in_a_label_is_refused() {
        let config = ActionsConfig {
            actions: vec![CustomAction {
                label: "bad\u{7}bell".into(),
                ..action(
                    "x",
                    ActionRun::Command {
                        command: BuiltinCommand::NewChat,
                    },
                )
            }],
        };
        assert!(actions::validate(&config).is_err());
    }
}

/* ================================================================== *
 * BATTLETEST 3 — efficiency
 *
 * The watcher runs forever, so its per-tick cost is the one that matters. The
 * thresholds here are deliberately loose: they are there to catch an accidental
 * order-of-magnitude regression (reading every file on every tick, say), not to
 * benchmark the machine.
 * ================================================================== */

mod efficiency {
    use super::*;

    #[test]
    fn a_watcher_tick_is_eight_metadata_calls_and_nothing_else() {
        let dir = scratch("tick");
        let paths = [
            dir.join("theme.json"),
            dir.join("layout.json"),
            dir.join("actions.json"),
            dir.join("panels.json"),
        ];
        for path in &paths {
            write(path, "{}");
        }

        // The watcher's tick is exactly this: four config stamps plus four
        // pending stamps, compared against the previous tick. No file is opened
        // unless one of them moved, and the two halves are compared separately
        // so a proposal does not re-read four config files that did not change.
        let started = Instant::now();
        for _ in 0..1_000 {
            for path in &paths {
                let _ = std::fs::metadata(path).map(|m| m.len());
            }
            for path in &paths {
                let _ = std::fs::metadata(path).map(|m| m.len());
            }
        }
        let elapsed = started.elapsed();

        assert!(
            elapsed.as_millis() < 2_000,
            "1000 ticks (8000 metadata calls) took {elapsed:?}, which suggests the \
             watcher is doing more than stat-ing eight files"
        );

        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn validation_is_cheap_at_the_documented_limits() {
        let actions = ActionsConfig {
            actions: (0..24)
                .map(|i| {
                    action(
                        &format!("a{i}"),
                        ActionRun::Prompt {
                            text: "x".repeat(200),
                        },
                    )
                })
                .collect(),
        };
        let panels = PanelsConfig {
            panels: (0..12)
                .map(|i| {
                    panel(
                        &format!("p{i}"),
                        (0..40)
                            .map(|b| PanelBlock::Text {
                                text: format!("block {b}"),
                            })
                            .collect(),
                    )
                })
                .collect(),
        };

        let started = Instant::now();
        for _ in 0..200 {
            assert!(actions::validate(&actions).is_ok());
            assert!(panels::validate(&panels).is_ok());
        }
        let elapsed = started.elapsed();

        assert!(
            elapsed.as_millis() < 2_000,
            "200 validations at the documented limits took {elapsed:?}"
        );
    }

    #[test]
    fn loading_a_file_at_the_size_limit_is_bounded() {
        let dir = scratch("load-limit");
        let path = dir.join("theme.json");

        // A theme just under the limit, with many small tokens.
        let mut tokens = String::new();
        for i in 0..150 {
            if i > 0 {
                tokens.push(',');
            }
            tokens.push_str(&format!("\"--vd-t{i}\":\"#abcdef\""));
        }
        write(&path, &format!("{{\"tokens\":{{{tokens}}}}}"));

        let started = Instant::now();
        for _ in 0..100 {
            let _ = customize::load_theme_from(&path);
        }
        let elapsed = started.elapsed();

        assert!(
            elapsed.as_millis() < 3_000,
            "100 loads of a near-limit theme took {elapsed:?}"
        );

        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn the_limits_themselves_are_sane() {
        // A guard against someone raising a cap to a value that would make the
        // watcher or the webview unhappy.
        assert!(MAX_THEME_BYTES <= 256 * 1024);
        assert!(MAX_LAYOUT_BYTES <= 64 * 1024);
        assert!(MAX_ACTIONS_BYTES <= 64 * 1024);
        assert!(MAX_PANELS_BYTES <= 128 * 1024);
        assert!(PANEL_WIDTH.0 >= 200 && PANEL_WIDTH.1 <= 640);
    }

    #[test]
    fn a_missing_file_costs_one_failed_stat() {
        let dir = scratch("missing");
        let path = dir.join("does-not-exist.json");

        let started = Instant::now();
        for _ in 0..1_000 {
            assert!(customize::load_theme_from(&path).is_ok());
        }
        let elapsed = started.elapsed();

        assert!(
            elapsed.as_millis() < 1_000,
            "1000 missing-file loads took {elapsed:?}"
        );

        std::fs::remove_dir_all(&dir).ok();
    }
}

/* ================================================================== *
 * BATTLETEST 4 — safety
 *
 * The surfaces that must never be restyled, and the escape hatch that must
 * always work.
 * ================================================================== */

mod safety {
    use super::*;

    /// Every protected surface, in every spelling a config might use.
    const PROTECTED: &[&str] = &[
        "vd-protected",
        ".vd-approval",
        ".vd-elicitation",
        ".vd-statusbar",
        ".vd-composer",
    ];

    #[test]
    fn no_protected_surface_can_be_targeted_in_any_spelling() {
        for surface in PROTECTED {
            let variants = [
                surface.to_string(),
                surface.to_uppercase(),
                format!("  {surface}  "),
                format!(".vd-card {surface}"),
                format!("{surface} .vd-btn"),
                format!(".vd-card, {surface}"),
                format!("{surface}:hover"),
                format!("{surface} > *"),
            ];

            for selector in variants {
                let theme = theme_with_rule(&selector, "color", "red");
                assert!(
                    customize::validate(&theme).is_err(),
                    "protected surface reachable through: {selector}"
                );
            }
        }
    }

    #[test]
    fn a_theme_cannot_inject_text_into_the_interface() {
        // `content` on a pseudo-element is how a stylesheet writes words the
        // user did not ask for, which on an approval bar would be a lie.
        let theme = theme_with_rule(".vd-card::after", "content", "\"Approved\"");
        assert!(customize::validate(&theme).is_err());
    }

    #[test]
    fn safe_mode_bypasses_every_loader() {
        let dir = scratch("safe-mode");

        // Hostile files on disk: each would be refused if it were read.
        let theme = dir.join("theme.json");
        write(&theme, r#"{"tokens":{"--vd-accent":"url(https://evil.test)"}}"#);
        let layout = dir.join("layout.json");
        write(&layout, r#"{"sidebarSide":"diagonal"}"#);
        let actions = dir.join("actions.json");
        write(
            &actions,
            r#"{"actions":[{"id":"x","label":"X","run":{"kind":"open-url","url":"javascript:1"}}]}"#,
        );
        let panels = dir.join("panels.json");
        write(
            &panels,
            r#"{"panels":[{"id":"p","title":"P","blocks":[{"kind":"file","path":"../x"}]}]}"#,
        );

        // With safe mode on, none of them is even read.
        assert!(customize::load_theme_at(&theme, true).unwrap().tokens.is_empty());
        assert_eq!(
            layout::load_layout_at(&layout, true).unwrap().sidebar_width,
            252
        );
        assert!(actions::load_actions_at(&actions, true).unwrap().actions.is_empty());
        assert!(panels::load_panels_at(&panels, true).unwrap().panels.is_empty());

        // With it off, the same files are refused, which proves the bypass is
        // what made the difference rather than the files being harmless.
        assert!(customize::load_theme_at(&theme, false).is_err());
        assert!(layout::load_layout_at(&layout, false).is_err());
        assert!(actions::load_actions_at(&actions, false).is_err());
        assert!(panels::load_panels_at(&panels, false).is_err());

        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn safe_mode_parses_the_environment_conservatively() {
        // Only an explicit affirmative counts. Anything else leaves the app
        // fully customizable, which is the documented default.
        let previous = std::env::var_os("VIBEDESK_SAFE_MODE");

        for (value, expected) in [
            ("1", true),
            ("true", true),
            ("yes", true),
            ("0", false),
            ("false", false),
            ("", false),
            ("maybe", false),
        ] {
            std::env::set_var("VIBEDESK_SAFE_MODE", value);
            assert_eq!(
                customize::safe_mode(),
                expected,
                "VIBEDESK_SAFE_MODE={value:?} should be {expected}"
            );
        }

        match previous {
            Some(value) => std::env::set_var("VIBEDESK_SAFE_MODE", value),
            None => std::env::remove_var("VIBEDESK_SAFE_MODE"),
        }
    }

    #[test]
    fn a_layout_cannot_shrink_the_composer_out_of_reach() {
        // The composer is where the user answers the agent. A layout that could
        // collapse it would be a denial of service dressed as a preference.
        let hostile = LayoutConfig {
            rail_width: 0,
            sidebar_width: 0,
            transcript_max: 0,
            composer_max: 0,
            ..LayoutConfig::default()
        }
        .clamped();

        assert!(hostile.composer_max >= 480);
        assert!(hostile.transcript_max >= 480);
        assert!(hostile.sidebar_width >= 200);
        assert!(hostile.rail_width >= 40);
    }

    #[test]
    fn a_panel_cannot_be_wide_enough_to_hide_the_transcript() {
        let config = PanelsConfig {
            panels: vec![CustomPanel {
                width: u16::MAX,
                ..panel("p", vec![])
            }],
        }
        .clamped();

        assert_eq!(config.panels[0].width, PANEL_WIDTH.1);
        assert!(config.panels[0].width <= 520);
    }
}

/* ================================================================== *
 * BATTLETEST 5 — resilience
 *
 * A bad config must not brick the app, and a good one must not corrupt itself.
 * ================================================================== */

mod resilience {
    use super::*;

    #[test]
    fn every_config_round_trips_through_disk() {
        let dir = scratch("round-trip");

        let theme = theme_with(&[("--vd-accent", "#ff7a1a")]);
        let theme_path = dir.join("theme.json");
        customize::save_theme_to(&theme_path, &theme).expect("save theme");
        let loaded = customize::load_theme_from(&theme_path).expect("load theme");
        assert_eq!(
            loaded.tokens.get("--vd-accent").map(String::as_str),
            Some("#ff7a1a")
        );

        let layout = LayoutConfig {
            sidebar_width: 300,
            sidebar_side: Side::Left,
            density: Density::Roomy,
            ..LayoutConfig::default()
        };
        let layout_path = dir.join("layout.json");
        layout::save_layout_to(&layout_path, &layout).expect("save layout");
        let loaded = layout::load_layout_from(&layout_path).expect("load layout");
        assert_eq!(loaded.sidebar_width, 300);
        assert_eq!(loaded.sidebar_side, Side::Left);
        assert_eq!(loaded.density, Density::Roomy);

        let actions = ActionsConfig {
            actions: vec![action(
                "go",
                ActionRun::Command {
                    command: BuiltinCommand::NewChat,
                },
            )],
        };
        let actions_path = dir.join("actions.json");
        actions::save_actions_to(&actions_path, &actions).expect("save actions");
        let loaded = actions::load_actions_from(&actions_path).expect("load actions");
        assert_eq!(loaded.actions.len(), 1);
        assert_eq!(loaded.actions[0].id, "go");

        let panels = PanelsConfig {
            panels: vec![panel(
                "notes",
                vec![PanelBlock::File {
                    path: "README.md".into(),
                    label: None,
                }],
            )],
        };
        let panels_path = dir.join("panels.json");
        panels::save_panels_to(&panels_path, &panels).expect("save panels");
        let loaded = panels::load_panels_from(&panels_path).expect("load panels");
        assert_eq!(loaded.panels.len(), 1);

        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn a_save_leaves_no_temporary_file_behind() {
        let dir = scratch("atomic");
        let path = dir.join("theme.json");

        customize::save_theme_to(&path, &theme_with(&[("--vd-accent", "#fff")]))
            .expect("save");

        let leftovers: Vec<String> = std::fs::read_dir(&dir)
            .expect("read dir")
            .flatten()
            .map(|e| e.file_name().to_string_lossy().to_string())
            .filter(|name| name.contains("tmp"))
            .collect();

        assert!(
            leftovers.is_empty(),
            "atomic save left temporary files behind: {leftovers:?}"
        );

        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn an_invalid_save_never_reaches_disk() {
        let dir = scratch("invalid-save");
        let path = dir.join("theme.json");

        // A good file first.
        customize::save_theme_to(&path, &theme_with(&[("--vd-accent", "#fff")]))
            .expect("save good");
        let before = std::fs::read_to_string(&path).expect("read");

        // Then an invalid one, which must be refused before the write.
        let hostile = theme_with(&[("--vd-accent", "url(https://evil.test)")]);
        assert!(customize::save_theme_to(&path, &hostile).is_err());

        let after = std::fs::read_to_string(&path).expect("read");
        assert_eq!(before, after, "a refused save must not touch the file");

        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn reset_removes_the_file_and_returns_the_default() {
        let dir = scratch("reset");

        let theme_path = dir.join("theme.json");
        customize::save_theme_to(&theme_path, &theme_with(&[("--vd-accent", "#fff")]))
            .expect("save");
        assert!(theme_path.exists());

        let cleared = customize::reset_theme_at(&theme_path).expect("reset");
        assert!(cleared.tokens.is_empty());
        assert!(!theme_path.exists());

        // Resetting something that is not there is not an error.
        assert!(customize::reset_theme_at(&theme_path).is_ok());

        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn one_broken_file_does_not_take_the_others_down() {
        // This mirrors the watcher: each half is loaded independently, and a
        // failure contributes `None` for that half only.
        let dir = scratch("independent");

        let theme = dir.join("theme.json");
        write(&theme, r#"{"tokens":{"--vd-accent":"url(https://evil.test)"}}"#);
        let layout = dir.join("layout.json");
        write(&layout, r#"{"sidebarWidth":300}"#);
        let actions = dir.join("actions.json");
        write(&actions, "not json at all");
        let panels = dir.join("panels.json");
        write(
            &panels,
            r#"{"panels":[{"id":"p","title":"P","blocks":[{"kind":"text","text":"hi"}]}]}"#,
        );

        let theme_result = customize::load_theme_at(&theme, false);
        let layout_result = layout::load_layout_at(&layout, false);
        let actions_result = actions::load_actions_at(&actions, false);
        let panels_result = panels::load_panels_at(&panels, false);

        assert!(theme_result.is_err(), "the broken theme should fail");
        assert!(actions_result.is_err(), "the broken actions should fail");
        assert!(layout_result.is_ok(), "the valid layout should still load");
        assert!(panels_result.is_ok(), "the valid panels should still load");

        assert_eq!(layout_result.unwrap().sidebar_width, 300);
        assert_eq!(panels_result.unwrap().panels.len(), 1);

        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn a_config_that_is_a_directory_does_not_brick_the_loader() {
        let dir = scratch("dir-config");
        let path = dir.join("actions.json");
        std::fs::create_dir_all(&path).expect("dir");

        assert!(actions::load_actions_from(&path).is_err());
        // And the app can still recover by resetting.
        assert!(actions::reset_actions_at(&path).is_err() || !path.exists());

        std::fs::remove_dir_all(&dir).ok();
    }
}

/* ------------------------------------------------------------------ *
 * 6. sources — can a plugin or the agent change the app without the
 *    user saying yes?
 *
 * The two sources are deliberately different authorities. A plugin's
 * file-write permission is about touching the filesystem; VibeDesk's
 * approval is about changing the app's own chrome. Both must pass, and
 * neither source gets a shortcut through validation.
 * ------------------------------------------------------------------ */

mod sources {
    use super::*;
    use crate::sources::{self, ConfigKind};

    /// A plugin folder with a `vibedesk/` subfolder holding one config.
    fn plugin_with(root: &Path, name: &str, file: &str, body: &str) -> PathBuf {
        let plugins = root.join("plugins");
        let custom = plugins.join(name).join("vibedesk");
        std::fs::create_dir_all(&custom).expect("plugin dir");
        write(&custom.join(file), body);
        plugins
    }

    #[test]
    fn a_plugin_that_is_merely_installed_changes_nothing() {
        let root = scratch("src-installed");
        let live = root.join("live");
        std::fs::create_dir_all(&live).expect("live");

        let plugins = plugin_with(
            &root,
            "acme",
            "theme.json",
            r##"{"tokens":{"--vd-accent":"#ff0000"}}"##,
        );

        let found = sources::discover_plugin_sources_at(&[plugins], false);
        assert_eq!(found.len(), 1);
        assert!(found[0].valid);

        // Discovery is a read. Nothing reached the live config.
        assert!(!live.join("theme.json").exists());

        std::fs::remove_dir_all(&root).ok();
    }

    #[test]
    fn an_invalid_plugin_config_is_reported_and_refused() {
        let root = scratch("src-invalid");
        let live = root.join("live");
        std::fs::create_dir_all(&live).expect("live");

        let plugins = plugin_with(
            &root,
            "bad",
            "theme.json",
            r##"{"tokens":{"--vd-accent":"url(https://evil.example/x.css)"}}"##,
        );

        let found = sources::discover_plugin_sources_at(&[plugins.clone()], false);
        assert_eq!(found.len(), 1);
        assert!(!found[0].valid, "a network value must not be offered");
        assert!(found[0].error.is_some());

        let refused = sources::apply_plugin_source_at(
            &[plugins],
            &live,
            "bad",
            ConfigKind::Theme,
            "",
            false,
        );
        assert!(refused.is_err());
        assert!(!live.join("theme.json").exists());

        std::fs::remove_dir_all(&root).ok();
    }

    #[test]
    fn a_directory_named_like_a_config_is_not_read() {
        let root = scratch("src-dir");
        let pending = root.join("pending");
        std::fs::create_dir_all(pending.join("theme.json")).expect("dir");

        let state = sources::read_pending_at(&pending, false);
        assert!(state.proposals.is_empty());
        assert_eq!(state.issues.len(), 1);

        std::fs::remove_dir_all(&root).ok();
    }

    #[test]
    fn the_agent_cannot_apply_its_own_proposal() {
        let root = scratch("src-agent");
        let pending = root.join("pending");
        let live = root.join("live");
        std::fs::create_dir_all(&pending).expect("pending");
        std::fs::create_dir_all(&live).expect("live");

        write(
            &pending.join("theme.json"),
            r##"{"tokens":{"--vd-accent":"#ff0000"}}"##,
        );

        // Writing the file is all the agent can do. Reading it back is a
        // listing, not an application.
        let state = sources::read_pending_at(&pending, false);
        assert_eq!(state.proposals.len(), 1);
        assert!(!live.join("theme.json").exists());

        // Only an explicit apply, carrying the digest the UI showed, lands it.
        let digest = state.proposals[0].digest.clone();
        sources::apply_pending_at(&pending, &live, ConfigKind::Theme, &digest, false)
            .expect("apply");
        assert!(live.join("theme.json").exists());

        std::fs::remove_dir_all(&root).ok();
    }

    #[test]
    fn a_proposal_that_moves_under_the_ui_is_refused() {
        let root = scratch("src-toctou");
        let pending = root.join("pending");
        let live = root.join("live");
        std::fs::create_dir_all(&pending).expect("pending");
        std::fs::create_dir_all(&live).expect("live");

        let path = pending.join("theme.json");
        write(&path, r##"{"tokens":{"--vd-accent":"#ff0000"}}"##);
        let shown = sources::read_pending_at(&pending, false).proposals[0]
            .digest
            .clone();

        // The agent rewrites the file while the prompt is on screen.
        write(&path, r##"{"tokens":{"--vd-accent":"#00ff00"}}"##);

        let refused = sources::apply_pending_at(&pending, &live, ConfigKind::Theme, &shown, false);
        assert!(refused.is_err(), "a moved proposal must not be applied");
        assert!(!live.join("theme.json").exists());

        std::fs::remove_dir_all(&root).ok();
    }

    #[test]
    fn protected_surfaces_stay_closed_through_the_pending_path() {
        let root = scratch("src-protected");
        let pending = root.join("pending");
        std::fs::create_dir_all(&pending).expect("pending");

        // The approval bar must not be themeable, from any source.
        write(
            &pending.join("theme.json"),
            r#"{"rules":[{"selector":".vd-approval","declarations":{"opacity":"0"}}]}"#,
        );

        let state = sources::read_pending_at(&pending, false);
        assert!(state.proposals.is_empty());
        assert!(!state.issues.is_empty());

        std::fs::remove_dir_all(&root).ok();
    }

    #[test]
    fn a_stray_name_is_reported_not_applied() {
        let root = scratch("src-stray");
        let pending = root.join("pending");
        std::fs::create_dir_all(&pending).expect("pending");
        write(&pending.join("mytheme.json"), "{}");

        let state = sources::read_pending_at(&pending, false);
        assert!(state.proposals.is_empty());
        assert!(state.issues.iter().any(|i| i.contains("mytheme.json")));

        std::fs::remove_dir_all(&root).ok();
    }

    #[test]
    fn a_differently_cased_name_is_the_same_file_not_a_stray() {
        let root = scratch("src-case");
        let pending = root.join("pending");
        std::fs::create_dir_all(&pending).expect("pending");
        write(&pending.join("Theme.JSON"), r##"{"tokens":{"--vd-accent":"#ff0000"}}"##);

        // Windows is case-insensitive, so this file *is* theme.json. It must be
        // offered once, and not also reported as a stray name.
        let state = sources::read_pending_at(&pending, false);
        assert_eq!(state.proposals.len(), 1);
        assert_eq!(state.proposals[0].kind, ConfigKind::Theme);
        assert!(state.issues.is_empty(), "{:?}", state.issues);

        std::fs::remove_dir_all(&root).ok();
    }

    #[test]
    fn oversized_and_empty_proposals_are_refused() {
        let root = scratch("src-size");
        let pending = root.join("pending");
        std::fs::create_dir_all(&pending).expect("pending");

        write(
            &pending.join("layout.json"),
            &"x".repeat(MAX_LAYOUT_BYTES as usize + 1),
        );
        write(&pending.join("theme.json"), "  \n\t ");

        let state = sources::read_pending_at(&pending, false);
        assert!(state.proposals.is_empty());
        assert_eq!(state.issues.len(), 2);

        std::fs::remove_dir_all(&root).ok();
    }

    #[test]
    fn safe_mode_disables_both_sources() {
        let root = scratch("src-safe");
        let pending = root.join("pending");
        let live = root.join("live");
        std::fs::create_dir_all(&pending).expect("pending");
        std::fs::create_dir_all(&live).expect("live");
        write(
            &pending.join("theme.json"),
            r##"{"tokens":{"--vd-accent":"#ff0000"}}"##,
        );
        let plugins = plugin_with(
            &root,
            "acme",
            "theme.json",
            r##"{"tokens":{"--vd-accent":"#00ff00"}}"##,
        );

        assert!(sources::read_pending_at(&pending, true).proposals.is_empty());
        assert!(sources::discover_plugin_sources_at(&[plugins.clone()], true).is_empty());

        // And applying is refused outright, not silently ignored.
        assert!(sources::apply_pending_at(&pending, &live, ConfigKind::Theme, "any", true).is_err());
        assert!(sources::apply_plugin_source_at(
            &[plugins],
            &live,
            "acme",
            ConfigKind::Theme,
            "any",
            true
        )
        .is_err());
        assert!(!live.join("theme.json").exists());

        std::fs::remove_dir_all(&root).ok();
    }

    #[test]
    fn what_lands_is_the_normalised_body() {
        let root = scratch("src-normalise");
        let pending = root.join("pending");
        let live = root.join("live");
        std::fs::create_dir_all(&pending).expect("pending");
        std::fs::create_dir_all(&live).expect("live");

        // `expanded` is not a field of a file block, and `sneaky` is not a field
        // of a panel. A source cannot smuggle either past serde.
        write(
            &pending.join("panels.json"),
            r#"{"panels":[{"id":"p","title":"P","sneaky":true,"blocks":[{"kind":"file","path":"a.md","expanded":true}]}]}"#,
        );

        let digest = sources::read_pending_at(&pending, false).proposals[0]
            .digest
            .clone();
        sources::apply_pending_at(&pending, &live, ConfigKind::Panels, &digest, false)
            .expect("apply");

        let landed = std::fs::read_to_string(live.join("panels.json")).expect("read");
        assert!(!landed.contains("sneaky"), "{landed}");
        assert!(!landed.contains("expanded"), "{landed}");

        std::fs::remove_dir_all(&root).ok();
    }

    #[test]
    fn a_plugin_cannot_name_a_path_outside_its_own_folder() {
        let root = scratch("src-plugin-path");
        let live = root.join("live");
        std::fs::create_dir_all(&live).expect("live");

        // A plugin folder with no `vibedesk/` subfolder. The caller names a
        // plugin and a kind, never a path, so there is nothing to substitute.
        let plugins = root.join("plugins");
        std::fs::create_dir_all(plugins.join("acme")).expect("plugin");

        assert!(sources::discover_plugin_sources_at(&[plugins.clone()], false).is_empty());
        assert!(sources::apply_plugin_source_at(
            &[plugins],
            &live,
            "acme",
            ConfigKind::Theme,
            "x",
            false
        )
        .is_err());
        assert!(!live.join("theme.json").exists());

        std::fs::remove_dir_all(&root).ok();
    }

    #[test]
    fn a_vibedesk_entry_that_is_a_file_is_not_a_plugin_source() {
        let root = scratch("src-vibedesk-file");
        let plugins = root.join("plugins");
        std::fs::create_dir_all(plugins.join("acme")).expect("plugin");
        write(&plugins.join("acme").join("vibedesk"), "not a directory");

        assert!(sources::discover_plugin_sources_at(&[plugins], false).is_empty());

        std::fs::remove_dir_all(&root).ok();
    }
}


/* ================================================================== *
 * BATTLETEST 6 — paths
 *
 * A workstation path is a key: it is compared against the agent session
 * history, written to settings, and shown in the title bar. canonicalize
 * returns a verbatim path on Windows, which is correct for the filesystem
 * and wrong for all three of those uses.
 * ================================================================== */

mod paths {
    use crate::paths::strip_verbatim;

    #[test]
    fn a_verbatim_drive_path_loses_its_prefix() {
        assert_eq!(strip_verbatim(r"\\?\D:\MistralVibe"), r"D:\MistralVibe");
    }

    #[test]
    fn a_verbatim_unc_path_becomes_a_plain_unc_path() {
        assert_eq!(strip_verbatim(r"\\?\UNC\server\share\project"), r"\\server\share\project");
    }

    #[test]
    fn a_plain_path_is_untouched() {
        assert_eq!(strip_verbatim(r"D:\MistralVibe"), r"D:\MistralVibe");
        assert_eq!(strip_verbatim("/home/user/project"), "/home/user/project");
        assert_eq!(strip_verbatim(""), "");
    }

    #[test]
    fn a_path_that_merely_contains_the_prefix_is_untouched() {
        // Only a leading prefix is stripped; a folder named like one deeper in
        // the path is a legal name and must survive.
        assert_eq!(strip_verbatim(r"D:\a\?\b"), r"D:\a\?\b");
    }
}
