use super::*;

fn ch(s: &str) -> Key {
    Key::Character(s.into())
}
fn named(n: NamedKey) -> Key {
    Key::Named(n)
}

fn page_with(values: Value) -> SettingsPage {
    SettingsPage::new(default_settings(), values)
}

fn commands() -> Vec<CommandRow> {
    vec![
        CommandRow { id: "QuickOpen".into(), label: "Open file".into(), default: Some("ctrl+p".into()) },
        CommandRow { id: "Palette".into(), label: "Command palette".into(), default: Some("ctrl+shift+p".into()) },
        CommandRow { id: "CloseTab".into(), label: "Close tab".into(), default: Some("ctrl+w".into()) },
        CommandRow { id: "TestFile".into(), label: "Test file".into(), default: None },
        CommandRow { id: "Sidebar".into(), label: "Toggle sidebar".into(), default: Some("ctrl+b".into()) },
    ]
}

fn modal() -> Modal {
    Modal::new(page_with(json!({})), KeybindsPage::new(commands(), HashMap::new()), Page::Settings)
}

fn kb() -> KeybindsPage {
    KeybindsPage::new(commands(), HashMap::new())
}

/// Apply events to a plain map exactly like the app would, for round-trip checks.
fn apply(map: &mut HashMap<String, String>, ev: Event) {
    for e in ev.flatten() {
        match e {
            Event::Bind { combo, command_id } => {
                map.insert(combo, command_id);
            }
            Event::Unbind { combo } => {
                map.remove(&combo);
            }
            Event::ResetAll => map.clear(),
            _ => {}
        }
    }
}

fn press(m: &mut Modal, key: Key) -> Event {
    m.key(&key, None, false, false, false)
}
fn typing(m: &mut Modal, s: &str) {
    for c in s.chars() {
        if c == ' ' {
            m.key(&named(NamedKey::Space), Some(" "), false, false, false);
        } else {
            m.key(&ch(&c.to_string()), Some(&c.to_string()), false, false, false);
        }
    }
}
fn record(p: &mut KeybindsPage, ctrl: bool, shift: bool, alt: bool, k: &str) -> Event {
    p.record_key(ctrl, shift, alt, &ch(k))
}
fn select(p: &mut KeybindsPage, id: &str) {
    p.filter = TextField::default();
    p.refilter();
    let k = p.row_ids().iter().position(|x| *x == id).unwrap();
    p.select_row(k);
}
fn def_idx(p: &SettingsPage, key: &str) -> usize {
    p.defs.iter().position(|d| d.key == key).unwrap()
}
fn keys_of(p: &SettingsPage) -> Vec<&'static str> {
    p.visible().iter().map(|&i| p.defs[i].key).collect()
}

#[test]
fn default_aliases_count_as_defaults() {
    let rows = vec![
        CommandRow { id: "Redo".into(), label: "Redo".into(), default: Some("ctrl+shift+z".into()) },
        CommandRow { id: "Save".into(), label: "Save".into(), default: Some("ctrl+s".into()) },
    ];
    let mut p = KeybindsPage::new(rows, HashMap::new());
    p.add_default_aliases([("ctrl+y".to_string(), "Redo".to_string()), ("Ctrl+K S".to_string(), "Save".to_string())]);
    assert_eq!(p.bindings_of("Redo"), vec!["ctrl+shift+z", "ctrl+y"], "main default first");
    assert_eq!(p.bindings_of("Save"), vec!["ctrl+s", "ctrl+k s"]);
    assert!(!p.is_modified("Redo"), "an alias is not a modification");
    assert_eq!(p.owner_of("ctrl+y"), Some("Redo"));
    // Removing the alias disables it explicitly and marks the row as modified; reset undoes it.
    let ev = p.remove_binding("Redo", "ctrl+y");
    assert!(matches!(ev, Some(Event::Bind { ref combo, ref command_id }) if combo == "ctrl+y" && command_id == "none"));
    assert!(p.is_modified("Redo"));
    assert_eq!(p.to_json(), r#"{"ctrl+y":"none"}"#);
    p.reset_row(0);
    assert!(!p.is_modified("Redo"));
    assert_eq!(p.to_json(), "{}");
}

// --- defaults / model ---

#[test]
fn default_settings_have_unique_real_keys() {
    let defs = default_settings();
    let mut keys: Vec<_> = defs.iter().map(|d| d.key).collect();
    keys.sort();
    let n = keys.len();
    keys.dedup();
    assert_eq!(keys.len(), n);
    for k in [
        "theme",
        "appearance",
        "buffer_font_size",
        "format_on_save",
        "auto_import",
        "lsp_narrow",
        "code_lens",
        "tsgo_source",
        "theme_light",
        "theme_dark",
    ] {
        assert!(defs.iter().any(|d| d.key == k), "{k}");
    }
    for k in PENDING_KEYS {
        assert!(defs.iter().any(|d| d.key == *k), "{k}");
    }
    assert!(
        matches!(defs.iter().find(|d| d.key == "buffer_font_size").unwrap().kind, SettingKind::Number { min, max, .. } if min == 8.0 && max == 40.0)
    );
}

#[test]
fn bottom_bar_has_one_toggle_per_optional_item_and_none_for_the_mandatory_one() {
    let defs = default_settings();
    let vals = default_values();
    for (id, _, required) in crate::config::STATUS_ITEMS {
        let key = format!("status_{id}");
        let def = defs.iter().find(|d| d.key == key);
        if *required {
            assert!(def.is_none(), "{id} is mandatory: no switch");
        } else {
            let def = def.unwrap_or_else(|| panic!("missing switch {key}"));
            assert_eq!(def.section, "Status bar");
            assert_eq!(def.kind, SettingKind::Bool);
            assert_eq!(vals[&key], json!(true), "{key} defaults to visible");
        }
    }
    for k in ["breadcrumbs", "git_blame", "ram_display"] {
        assert!(defs.iter().any(|d| d.key == k), "{k}");
        assert!(vals.get(k).is_some(), "{k} default");
    }
    assert!(
        matches!(&defs.iter().find(|d| d.key == "ram_display").unwrap().kind, SettingKind::Choice(o) if *o == ["long", "short"])
    );
}

#[test]
fn bottom_bar_order_has_one_stepper_per_right_hand_item() {
    let defs = default_settings();
    let vals = default_values();
    assert_eq!(crate::config::STATUS_RIGHT.len(), 7);
    for (slot, id) in crate::config::STATUS_RIGHT.iter().enumerate() {
        let key = format!("status_pos_{id}");
        let def = defs.iter().find(|d| d.key == key).unwrap_or_else(|| panic!("missing {key}"));
        assert!(matches!(def.kind, SettingKind::Number { min, max, step } if min == 1.0 && max == 7.0 && step == 1.0));
        assert_eq!(vals[&key], json!(slot + 1), "{key} default slot");
    }
}

#[test]
fn set_values_replaces_what_the_rows_show() {
    let mut p = page_with(json!({"status_pos_git": 6}));
    let def = p.defs.iter().position(|d| d.key == "status_pos_git").unwrap();
    assert_eq!(p.current(def), json!(6));
    p.set_values(json!({"status_pos_git": 2}));
    assert_eq!(p.current(def), json!(2));
}

#[test]
fn sections_in_order_and_visible_by_section() {
    let mut p = page_with(json!({}));
    assert_eq!(
        p.sections(),
        vec!["General", "Editor", "Appearance", "TypeScript", "Git", "Status bar", "Fonts", "Editor display", "Build"]
    );
    assert_eq!(p.visible().len(), 2);
    p.set_section(3);
    assert_eq!(
        keys_of(&p),
        vec![
            "auto_import",
            "lsp_narrow",
            "code_lens",
            "code_lens_refs",
            "code_lens_impls",
            "inlay_hints",
            "inlay_hints_params",
            "inlay_hints_types",
            "tsgo_source"
        ]
    );
    p.set_section(99);
    assert_eq!(p.section, 3, "out of range ignored");
}

#[test]
fn search_filters_label_and_help_case_insensitive_across_sections() {
    let mut m = modal();
    typing(&mut m, "MINIMAP");
    assert_eq!(keys_of(&m.settings), vec!["minimap", "minimap_style"]);
    m.settings.search = TextField::with("server");
    let keys = keys_of(&m.settings);
    assert!(
        keys.contains(&"format_on_save") && keys.contains(&"lsp_narrow") && keys.contains(&"code_lens"),
        "{keys:?}"
    );
    m.settings.search = TextField::with("zzzz");
    assert!(m.settings.visible().is_empty());
    assert_eq!(m.settings.selected_def(), None);
    // multiple terms are ANDed (label or help)
    m.settings.search = TextField::with("dark theme");
    let keys = keys_of(&m.settings);
    assert!(keys.contains(&"theme_dark") && !keys.contains(&"buffer_font_size"), "{keys:?}");
    m.settings.search = TextField::with("   ");
    assert_eq!(keys_of(&m.settings), vec!["format_on_save", "updates"], "blank search = section list");
}

#[test]
fn typing_resets_selection_and_backspace_restores_list() {
    let mut m = modal();
    typing(&mut m, "theme");
    assert!(m.settings.visible().len() >= 3);
    press(&mut m, named(NamedKey::ArrowDown));
    assert_eq!(m.settings.sel, 1);
    typing(&mut m, "x");
    assert_eq!(m.settings.sel, 0);
    for _ in 0..6 {
        press(&mut m, named(NamedKey::Backspace));
    }
    assert!(m.settings.search.text.is_empty());
    assert_eq!(m.settings.visible().len(), 2, "back to the section list");
}

#[test]
fn bool_toggle_emits_events_via_enter_space_and_arrows() {
    let mut m = modal();
    let ev = press(&mut m, named(NamedKey::Enter));
    assert_eq!(ev, Event::Setting { key: "format_on_save".into(), value: json!(true) });
    let ev = press(&mut m, named(NamedKey::Space));
    assert_eq!(ev, Event::Setting { key: "format_on_save".into(), value: json!(false) });
    assert_eq!(
        press(&mut m, named(NamedKey::ArrowRight)),
        Event::Setting { key: "format_on_save".into(), value: json!(true) }
    );
    assert_eq!(press(&mut m, named(NamedKey::ArrowRight)), Event::None, "already on");
    assert_eq!(
        press(&mut m, named(NamedKey::ArrowLeft)),
        Event::Setting { key: "format_on_save".into(), value: json!(false) }
    );
    assert_eq!(m.settings.values()["format_on_save"], json!(false));
}

#[test]
fn space_goes_to_search_when_search_is_not_empty() {
    let mut m = modal();
    typing(&mut m, "format on");
    assert_eq!(m.settings.search.text, "format on");
    assert_eq!(m.settings.values().get("format_on_save"), None, "nothing toggled");
}

#[test]
fn choice_cycles_forward_and_backward_with_wrap() {
    let mut p = page_with(json!({"appearance": "fixed"}));
    let d = def_idx(&p, "appearance");
    assert_eq!(p.cycle(d, 1), Event::Setting { key: "appearance".into(), value: json!("system") });
    assert_eq!(p.cycle(d, 1), Event::Setting { key: "appearance".into(), value: json!("fixed") });
    assert_eq!(p.cycle(d, -1), Event::Setting { key: "appearance".into(), value: json!("system") });
    let t = def_idx(&p, "tsgo_source");
    assert_eq!(p.cycle(t, 1), Event::Setting { key: "tsgo_source".into(), value: json!("project") }, "auto -> project");
    assert_eq!(p.pick(t, 2), Event::Setting { key: "tsgo_source".into(), value: json!("bundled") });
    assert_eq!(p.cycle(t, 1), Event::Setting { key: "tsgo_source".into(), value: json!("auto") }, "wraps");
    assert_eq!(p.pick(t, 0), Event::None, "same value");
    assert_eq!(p.pick(t, 9), Event::None);
}

#[test]
fn dynamic_choices_and_dropdown_pick() {
    let mut m = modal();
    let themes: Vec<String> = (0..20).map(|i| format!("theme-{i}")).collect();
    m.settings.set_choices("theme", themes.clone());
    m.settings.set_section(2);
    let d = def_idx(&m.settings, "theme");
    assert_eq!(m.settings.choices_of(d), themes);
    press(&mut m, named(NamedKey::ArrowDown));
    assert_eq!(m.settings.selected_def(), Some(d));
    assert_eq!(press(&mut m, named(NamedKey::Enter)), Event::None);
    assert!(m.settings.dropdown_open());
    press(&mut m, named(NamedKey::ArrowDown));
    press(&mut m, named(NamedKey::ArrowDown));
    let ev = press(&mut m, named(NamedKey::Enter));
    assert_eq!(
        ev,
        Event::Setting { key: "theme".into(), value: json!("theme-2") },
        "list starts at 0 without a current value"
    );
    assert!(!m.settings.dropdown_open());
    // Esc closes the dropdown only, a second Esc closes the modal.
    press(&mut m, named(NamedKey::Enter));
    assert!(m.settings.dropdown_open());
    assert_eq!(m.settings.drop.as_ref().unwrap().sel, 2, "opens on the current value");
    assert_eq!(press(&mut m, named(NamedKey::Escape)), Event::None);
    assert!(!m.settings.dropdown_open());
    assert_eq!(press(&mut m, named(NamedKey::Escape)), Event::Close);
}

#[test]
fn dropdown_selection_is_clamped_and_scrolls() {
    let mut m = modal();
    m.settings.set_choices("theme", (0..30).map(|i| i.to_string()).collect());
    m.settings.set_section(2);
    press(&mut m, named(NamedKey::ArrowDown));
    press(&mut m, named(NamedKey::Enter));
    for _ in 0..50 {
        press(&mut m, named(NamedKey::ArrowDown));
    }
    let d = m.settings.drop.clone().unwrap();
    assert_eq!(d.sel, 29);
    assert_eq!(d.scroll, 30 - DROP_VIS);
    press(&mut m, named(NamedKey::Home));
    let d = m.settings.drop.clone().unwrap();
    assert_eq!((d.sel, d.scroll), (0, 0));
    m.wheel(40.0);
    assert_eq!(m.settings.drop.as_ref().unwrap().scroll, 1);
}

#[test]
fn number_step_clamps_to_range() {
    let mut p = page_with(json!({"buffer_font_size": 39}));
    let d = def_idx(&p, "buffer_font_size");
    assert_eq!(p.step(d, 1), Event::Setting { key: "buffer_font_size".into(), value: json!(40) });
    assert_eq!(p.step(d, 1), Event::None, "already at max");
    assert_eq!(p.set_number(d, 8.0), Event::Setting { key: "buffer_font_size".into(), value: json!(8) });
    assert_eq!(p.step(d, -1), Event::None, "already at min");
    assert_eq!(p.set_number(d, 1000.0), Event::Setting { key: "buffer_font_size".into(), value: json!(40) });
    assert_eq!(p.set_number(d, -5.0), Event::Setting { key: "buffer_font_size".into(), value: json!(8) });
}

#[test]
fn number_out_of_range_value_is_pulled_in_and_fractions_survive() {
    let mut p = SettingsPage::new(
        vec![SettingDef {
            key: "scale",
            section: "General",
            label: "Scale",
            help: "",
            kind: SettingKind::Number { min: 0.5, max: 2.0, step: 0.25 },
        }],
        json!({"scale": 9}),
    );
    assert_eq!(p.step(0, -1), Event::Setting { key: "scale".into(), value: json!(2) });
    assert_eq!(p.step(0, -1), Event::Setting { key: "scale".into(), value: json!(1.75) });
    assert_eq!(p.cur_num(0), 1.75);
    assert_eq!(fmt_num(1.75), "1.75");
    assert_eq!(fmt_num(14.0), "14");
}

#[test]
fn number_via_keyboard_arrows() {
    let mut m = modal();
    m.settings.set_section(6); // Fonts
    let fs = def_idx(&m.settings, "buffer_font_size");
    m.settings.select_def(fs);
    assert_eq!(m.settings.selected_def(), Some(fs));
    assert_eq!(
        press(&mut m, named(NamedKey::ArrowRight)),
        Event::Setting { key: "buffer_font_size".into(), value: json!(15) }
    );
    assert_eq!(
        press(&mut m, named(NamedKey::ArrowLeft)),
        Event::Setting { key: "buffer_font_size".into(), value: json!(14) }
    );
    assert_eq!(press(&mut m, named(NamedKey::Enter)), Event::None);
}

#[test]
fn text_setting_edit_commit_and_cancel() {
    let defs = vec![SettingDef {
        key: "shell",
        section: "General",
        label: "Shell",
        help: "Terminal program",
        kind: SettingKind::Text,
    }];
    let mut m = Modal::new(
        SettingsPage::new(defs, json!({"shell": "bash"})),
        KeybindsPage::new(vec![], HashMap::new()),
        Page::Settings,
    );
    press(&mut m, named(NamedKey::Enter));
    assert!(m.settings.is_editing());
    typing(&mut m, "ç");
    assert_eq!(press(&mut m, named(NamedKey::Enter)), Event::Setting { key: "shell".into(), value: json!("bashç") });
    assert!(!m.settings.is_editing());
    // cancel keeps the value
    press(&mut m, named(NamedKey::Enter));
    typing(&mut m, "x");
    assert_eq!(press(&mut m, named(NamedKey::Escape)), Event::None);
    assert!(!m.settings.is_editing());
    assert_eq!(m.settings.values()["shell"], json!("bashç"));
    // committing an unchanged text emits nothing
    press(&mut m, named(NamedKey::Enter));
    assert_eq!(press(&mut m, named(NamedKey::Enter)), Event::None);
}

#[test]
fn reset_row_restores_default_and_flags_modified() {
    let mut p = page_with(json!({"lsp_narrow": true, "buffer_font_size": 14}));
    let d = def_idx(&p, "lsp_narrow");
    assert!(p.is_modified(d));
    assert!(!p.is_modified(def_idx(&p, "buffer_font_size")));
    assert_eq!(p.reset_def(d), Event::Setting { key: "lsp_narrow".into(), value: json!(false) });
    assert!(!p.is_modified(d));
    assert_eq!(p.reset_def(d), Event::None);
}

#[test]
fn settings_navigation_bounds_and_tab() {
    let mut m = modal();
    m.settings.set_section(3); // 7 rows
    press(&mut m, named(NamedKey::ArrowUp));
    assert_eq!(m.settings.sel, 0);
    for _ in 0..10 {
        press(&mut m, named(NamedKey::ArrowDown));
    }
    assert_eq!(m.settings.sel, 8);
    press(&mut m, named(NamedKey::Home));
    assert_eq!(m.settings.sel, 0);
    press(&mut m, named(NamedKey::End));
    assert_eq!(m.settings.sel, 8);
    press(&mut m, named(NamedKey::PageUp));
    assert_eq!(m.settings.sel, 2);
    press(&mut m, named(NamedKey::PageDown));
    assert_eq!(m.settings.sel, 8);
    // Tab walks sections and wraps
    let last = m.settings.sections().len() - 1;
    m.settings.set_section(last);
    press(&mut m, named(NamedKey::Tab));
    assert_eq!((m.settings.section, m.settings.sel), (0, 0));
    m.key(&named(NamedKey::Tab), None, false, true, false);
    assert_eq!(m.settings.section, last);
}

#[test]
fn selection_keeps_row_in_view_and_wheel_clamps() {
    let defs: Vec<SettingDef> = (0..30)
        .map(|i| SettingDef {
            key: Box::leak(format!("k{i}").into_boxed_str()),
            section: "General",
            label: "Item",
            help: "",
            kind: SettingKind::Bool,
        })
        .collect();
    let mut p = SettingsPage::new(defs, json!({}));
    p.view_h = 240;
    for _ in 0..10 {
        p.move_sel(1);
    }
    assert_eq!(p.sel, 10);
    assert_eq!(p.scroll, (11 * SET_ROW - 240) as f64);
    p.wheel(10_000.0);
    assert_eq!(p.scroll, (30 * SET_ROW - 240) as f64, "wheel clamps at the end");
    p.wheel(-10_000.0);
    assert_eq!(p.scroll, 0.0);
}

// --- keybindings ---

#[test]
fn combo_chips_and_parts() {
    assert_eq!(combo_chips("ctrl+shift+p"), vec!["Ctrl", "Shift", "P"]);
    assert_eq!(combo_chips("alt+left"), vec!["Alt", "←"]);
    assert_eq!(combo_chips("f5"), vec!["F5"]);
    assert_eq!(combo_chips("ctrl++"), vec!["Ctrl", "+"]);
    assert_eq!(combo_chips("ctrl+pageup"), vec!["Ctrl", "PgUp"]);
    assert_eq!(combo_parts("ctrl+shift+alt+x"), vec!["ctrl", "shift", "alt", "x"]);
    assert_eq!(combo_chips("ctrl+k r"), vec!["Ctrl", "K", "›", "R"]);
    assert_eq!(combo_chips("explorer:shift+a"), vec!["Explorer", "Shift", "A"]);
    assert_eq!(combo_chips("explorer:delete"), vec!["Explorer", "Del"]);
}

#[test]
fn keybinds_filter_by_label_id_and_combo() {
    let mut p = kb();
    assert_eq!(p.rows().len(), 5);
    p.filter = TextField::with("OPEN");
    p.refilter();
    assert_eq!(p.row_ids(), vec!["QuickOpen"]);
    p.filter = TextField::with("closetab");
    p.refilter();
    assert_eq!(p.row_ids(), vec!["CloseTab"]);
    p.filter = TextField::with("ctrl shift");
    p.refilter();
    assert_eq!(p.row_ids(), vec!["Palette"]);
    p.filter = TextField::with("nada");
    p.refilter();
    assert!(p.rows().is_empty() && p.selected().is_none());
}

#[test]
fn record_free_combo_binds_and_replaces_default() {
    let mut p = kb();
    select(&mut p, "QuickOpen");
    p.begin_record();
    assert!(p.is_recording());
    let ev = record(&mut p, true, false, false, "K");
    assert_eq!(
        ev,
        Event::Batch(vec![
            Event::Bind { combo: "ctrl+p".into(), command_id: "none".into() },
            Event::Bind { combo: "ctrl+k".into(), command_id: "QuickOpen".into() },
        ])
    );
    assert!(!p.is_capturing());
    assert_eq!(p.bindings_of("QuickOpen"), vec!["ctrl+k"]);
    assert_eq!(p.owner_of("ctrl+p"), None);
    assert!(p.is_modified("QuickOpen"));
}

#[test]
fn record_unbound_command_gets_single_bind_event() {
    let mut p = kb();
    select(&mut p, "TestFile");
    p.begin_record();
    let ev = p.record_key(true, true, false, &ch("T"));
    assert_eq!(ev, Event::Bind { combo: "ctrl+shift+t".into(), command_id: "TestFile".into() });
    assert_eq!(p.to_json(), r#"{"ctrl+shift+t":"TestFile"}"#);
}

#[test]
fn record_rejects_bare_keys_ignores_modifiers_and_escape_cancels() {
    let mut p = kb();
    select(&mut p, "TestFile");
    p.begin_record();
    assert_eq!(record(&mut p, false, false, false, "a"), Event::None);
    assert_eq!(record(&mut p, false, true, false, "a"), Event::None);
    assert!(p.is_recording() && p.note().is_some(), "still recording with a hint");
    assert_eq!(p.record_key(true, false, false, &named(NamedKey::Control)), Event::None);
    assert_eq!(p.record_key(false, true, false, &named(NamedKey::Shift)), Event::None);
    assert!(p.is_recording());
    assert!(p.user().is_empty());
    // F-keys are fine without modifiers
    let ev = p.record_key(false, false, false, &named(NamedKey::F5));
    assert_eq!(ev, Event::Bind { combo: "f5".into(), command_id: "TestFile".into() });
    // Esc cancels
    p.begin_record();
    assert_eq!(p.record_key(false, false, false, &named(NamedKey::Escape)), Event::None);
    assert!(!p.is_capturing());
    assert_eq!(p.bindings_of("TestFile"), vec!["f5"], "unchanged");
}

#[test]
fn conflict_requires_confirmation_then_replaces() {
    let mut p = kb();
    select(&mut p, "TestFile");
    p.begin_record();
    // ctrl+w belongs to CloseTab by default
    assert_eq!(record(&mut p, true, false, false, "w"), Event::None);
    assert_eq!(p.pending_conflict(), Some(("ctrl+w", "CloseTab")));
    assert!(p.is_capturing());
    assert!(p.user().is_empty(), "nothing applied yet");
    let ev = p.confirm();
    assert_eq!(ev, Event::Bind { combo: "ctrl+w".into(), command_id: "TestFile".into() });
    assert_eq!(p.owner_of("ctrl+w"), Some("TestFile"));
    assert!(p.bindings_of("CloseTab").is_empty());
    assert!(!p.is_capturing());
}

#[test]
fn conflict_with_user_binding_cancel_and_enter_confirm() {
    let mut user = HashMap::new();
    user.insert("ctrl+shift+t".to_string(), "TestFile".to_string());
    let mut p = KeybindsPage::new(commands(), user);
    select(&mut p, "Sidebar");
    p.begin_record();
    assert_eq!(p.record_key(true, true, false, &ch("t")), Event::None);
    assert_eq!(p.pending_conflict(), Some(("ctrl+shift+t", "TestFile")));
    // Esc cancels the question without changes
    assert_eq!(p.record_key(false, false, false, &named(NamedKey::Escape)), Event::None);
    assert!(!p.is_capturing());
    assert_eq!(p.owner_of("ctrl+shift+t"), Some("TestFile"));
    // again, confirm with Enter: Sidebar leaves ctrl+b (masked) and takes ctrl+shift+t
    p.begin_record();
    p.record_key(true, true, false, &ch("t"));
    let ev = p.record_key(false, false, false, &named(NamedKey::Enter));
    assert_eq!(
        ev,
        Event::Batch(vec![
            Event::Bind { combo: "ctrl+b".into(), command_id: "none".into() },
            Event::Bind { combo: "ctrl+shift+t".into(), command_id: "Sidebar".into() },
        ])
    );
    assert!(p.bindings_of("TestFile").is_empty());
}

#[test]
fn rebinding_to_own_combo_is_a_noop() {
    let mut p = kb();
    select(&mut p, "CloseTab");
    p.begin_record();
    assert_eq!(record(&mut p, true, false, false, "w"), Event::None);
    assert!(!p.is_capturing() && p.pending_conflict().is_none());
    assert!(p.user().is_empty());
}

#[test]
fn delete_unbinds_default_with_none_and_user_binding_with_unbind() {
    let mut p = kb();
    select(&mut p, "CloseTab");
    assert_eq!(p.unbind_selected(), Event::Bind { combo: "ctrl+w".into(), command_id: "none".into() });
    assert_eq!(p.to_json(), r#"{"ctrl+w":"none"}"#);
    assert_eq!(p.unbind_selected(), Event::None, "nothing left to remove");
    select(&mut p, "TestFile");
    p.begin_record();
    record(&mut p, true, true, false, "t");
    assert_eq!(p.unbind_selected(), Event::Unbind { combo: "ctrl+shift+t".into() });
    assert_eq!(p.to_json(), r#"{"ctrl+w":"none"}"#);
}

#[test]
fn delete_and_backspace_keys_through_modal() {
    let mut m = modal();
    m.switch_page();
    press(&mut m, named(NamedKey::ArrowDown));
    press(&mut m, named(NamedKey::ArrowDown));
    assert_eq!(m.keybinds.selected_id(), Some("CloseTab"));
    assert_eq!(
        press(&mut m, named(NamedKey::Delete)),
        Event::Bind { combo: "ctrl+w".into(), command_id: "none".into() }
    );
    // With text in the filter, Backspace edits the filter instead.
    typing(&mut m, "a");
    assert_eq!(press(&mut m, named(NamedKey::Backspace)), Event::None);
    assert!(m.keybinds.filter.text.is_empty());
    // With an empty filter, Backspace removes bindings of the selected row (CloseTab has none now).
    assert_eq!(press(&mut m, named(NamedKey::Backspace)), Event::None);
    press(&mut m, named(NamedKey::ArrowUp));
    assert_eq!(m.keybinds.selected_id(), Some("Palette"));
    assert_eq!(
        press(&mut m, named(NamedKey::Backspace)),
        Event::Bind { combo: "ctrl+shift+p".into(), command_id: "none".into() }
    );
}

#[test]
fn reset_row_restores_default_and_drops_user_entries() {
    let mut p = kb();
    select(&mut p, "QuickOpen");
    p.begin_record();
    record(&mut p, true, false, false, "k");
    assert!(p.is_modified("QuickOpen"));
    let k = p.row_ids().iter().position(|x| *x == "QuickOpen").unwrap();
    let ev = p.reset_row(k);
    assert_eq!(ev.flatten().len(), 2, "unbind ctrl+k and ctrl+p none");
    assert!(!p.is_modified("QuickOpen"));
    assert_eq!(p.bindings_of("QuickOpen"), vec!["ctrl+p"]);
    assert_eq!(p.to_json(), "{}");
    assert_eq!(p.reset_row(k), Event::None);
}

#[test]
fn reset_all_clears_user_map_and_needs_confirmation_via_keyboard() {
    let mut m = modal();
    m.switch_page();
    press(&mut m, named(NamedKey::Delete)); // QuickOpen: ctrl+p -> none
    assert!(!m.keybinds.user().is_empty());
    m.key(&ch("r"), None, true, true, false); // Ctrl+Shift+R arms
    assert!(m.keybinds.is_capturing());
    assert_eq!(press(&mut m, named(NamedKey::Escape)), Event::None, "Esc cancels the question");
    assert!(!m.keybinds.is_capturing());
    assert!(!m.keybinds.user().is_empty());
    m.key(&ch("r"), None, true, true, false);
    assert_eq!(press(&mut m, named(NamedKey::Enter)), Event::ResetAll);
    assert!(m.keybinds.user().is_empty());
    assert_eq!(m.keybinds.to_json(), "{}");
    assert_eq!(m.keybinds.bindings_of("QuickOpen"), vec!["ctrl+p"]);
}

#[test]
fn to_json_is_minimal_sorted_and_round_trips_through_keymap() {
    let mut user = HashMap::new();
    user.insert("Ctrl+Shift+T".to_string(), "TestFile".to_string());
    user.insert("ctrl+w".to_string(), "none".to_string());
    user.insert("ctrl+p".to_string(), "QuickOpen".to_string()); // equals default -> dropped
    user.insert("ctrl+9".to_string(), "none".to_string()); // none without a default -> dropped
    let p = KeybindsPage::new(commands(), user);
    let js = p.to_json();
    assert_eq!(js, r#"{"ctrl+shift+t":"TestFile","ctrl+w":"none"}"#);
    let km = keymap::Keymap::from_json(&js);
    assert_eq!(km.len(), 2);
    assert_eq!(km.lookup(true, true, false, &ch("t")), Some("TestFile"));
    assert_eq!(km.lookup(true, false, false, &ch("w")), Some("none"));
    assert_eq!(km.lookup(true, false, false, &ch("p")), Some("QuickOpen")); // not in the file: the built-in default answers
    // and feeding it back into a page gives the same document
    let p2 = KeybindsPage::new(commands(), serde_json::from_str::<HashMap<String, String>>(&js).unwrap());
    assert_eq!(p2.to_json(), js);
}

#[test]
fn events_replayed_on_a_plain_map_match_the_page_state() {
    let mut p = kb();
    let mut map: HashMap<String, String> = HashMap::new();
    select(&mut p, "QuickOpen");
    p.begin_record();
    let ev = record(&mut p, true, false, false, "k");
    apply(&mut map, ev);
    select(&mut p, "TestFile");
    p.begin_record();
    record(&mut p, true, false, false, "b"); // conflict with Sidebar
    let ev = p.confirm();
    apply(&mut map, ev);
    select(&mut p, "CloseTab");
    let ev = p.unbind_selected();
    apply(&mut map, ev);
    assert_eq!(&map, p.user());
    select(&mut p, "QuickOpen");
    let k = p.sel;
    let ev = p.reset_row(k);
    apply(&mut map, ev);
    assert_eq!(&map, p.user());
    let ev = p.reset_all();
    apply(&mut map, ev);
    assert!(map.is_empty() && p.user().is_empty());
}

#[test]
fn keybinds_navigation_bounds_and_enter_starts_recording() {
    let mut m = modal();
    m.switch_page();
    press(&mut m, named(NamedKey::ArrowUp));
    assert_eq!(m.keybinds.sel, 0);
    for _ in 0..20 {
        press(&mut m, named(NamedKey::ArrowDown));
    }
    assert_eq!(m.keybinds.sel, 4);
    press(&mut m, named(NamedKey::Home));
    assert_eq!(m.keybinds.sel, 0);
    press(&mut m, named(NamedKey::End));
    assert_eq!(m.keybinds.sel, 4);
    press(&mut m, named(NamedKey::PageUp));
    assert_eq!(m.keybinds.sel, 0);
    press(&mut m, named(NamedKey::Enter));
    assert!(m.keybinds.is_recording());
    // while recording, keys are captured by the page, not navigated
    let ev = m.key(&ch("j"), Some("j"), true, false, false);
    assert!(matches!(ev, Event::Batch(_)), "{ev:?}");
    assert!(!m.keybinds.is_recording());
    assert_eq!(m.keybinds.bindings_of("QuickOpen"), vec!["ctrl+j"]);
}

// --- modal shell ---

#[test]
fn ctrl_tab_switches_pages_and_esc_cancels_recording_before_closing() {
    let mut m = modal();
    assert_eq!(m.page, Page::Settings);
    assert_eq!(m.key(&named(NamedKey::Tab), None, true, false, false), Event::None);
    assert_eq!(m.page, Page::Keybinds);
    m.key(&named(NamedKey::Tab), None, true, false, false);
    assert_eq!(m.page, Page::Settings);
    m.switch_page();
    press(&mut m, named(NamedKey::Enter));
    assert!(m.keybinds.is_recording());
    assert_eq!(press(&mut m, named(NamedKey::Escape)), Event::None);
    assert!(!m.keybinds.is_recording());
    assert_eq!(press(&mut m, named(NamedKey::Escape)), Event::Close);
    // Ctrl+Tab is a valid combo while recording (captured, not a page switch)
    press(&mut m, named(NamedKey::Enter));
    m.key(&named(NamedKey::Tab), None, true, false, false);
    assert_eq!(m.page, Page::Keybinds);
    assert_eq!(m.keybinds.bindings_of("QuickOpen"), vec!["ctrl+tab"]);
}

#[test]
fn rect_is_centered_and_sized() {
    let r = Modal::rect_for(Rect::new(0, 0, 1920, 1080));
    assert_eq!((r.w, r.h), (860, 560));
    assert_eq!((r.x, r.y), ((1920 - 860) / 2, (1080 - 560) / 2));
    let r = Modal::rect_for(Rect::new(0, 0, 600, 400));
    assert_eq!((r.w, r.h), (520, 320));
    let r = Modal::rect_for(Rect::new(10, 20, 600, 400));
    assert_eq!((r.x, r.y), (10 + 40, 20 + 40));
}

#[test]
fn event_flatten() {
    let e = Event::Batch(vec![Event::None, Event::Batch(vec![Event::ResetAll]), Event::Close]);
    assert_eq!(e.flatten(), vec![Event::ResetAll, Event::Close]);
    assert!(Event::None.flatten().is_empty());
}

// --- drawing (needs fonts of the machine, not the GPU) ---

fn test_theme() -> Theme {
    Theme::load(&crate::config::assets_dir().join("themes/dark-plus.json")).unwrap()
}

const BG: u32 = 0x010203;

fn center(r: Rect) -> (f64, f64) {
    ((r.x + r.w / 2) as f64, (r.y + r.h / 2) as f64)
}

fn hit_rect(m: &Modal, h: Hit) -> Rect {
    m.hits.iter().find(|(_, x)| *x == h).map(|(r, _)| *r).unwrap_or_else(|| panic!("no hit {h:?}"))
}

#[test]
fn draw_smoke_stays_inside_modal_and_hits_work() {
    let (w, h) = (1000i32, 700i32);
    let mut px = vec![BG; (w * h) as usize];
    let theme = test_theme();
    let mut tx = TextCtx::new(None);
    let window = Rect::new(0, 0, w, h);
    let mut m = modal();
    m.settings.set_choices("theme", vec!["a".into(), "b".into(), "c".into(), "d".into()]);
    {
        let mut cv = Canvas { px: &mut px, w, h };
        m.draw(&mut cv, &mut tx, &theme, window);
    }
    let r = m.rect();
    assert_eq!(r, Modal::rect_for(window));
    let near = r.inset(-26); // the shadow reaches ~24px
    let mut inside_changed = 0i64;
    for y in 0..h {
        for x in 0..w {
            let v = px[(y * w + x) as usize];
            if r.contains(x as f64, y as f64) {
                inside_changed += (v != BG) as i64;
            } else if !near.contains(x as f64, y as f64) {
                assert_eq!(v, BG, "({x},{y}) outside the modal+shadow must be untouched");
            }
        }
    }
    assert!(inside_changed > (r.w as i64 * r.h as i64) / 2, "modal body painted");
    // Tabs are clickable: click the second tab, then draw the keybinds page.
    let tab = hit_rect(&m, Hit::Tab(Page::Keybinds));
    assert_eq!(m.mouse_down((tab.x + 5) as f64, (tab.y + 5) as f64), Event::None);
    assert_eq!(m.page, Page::Keybinds);
    {
        let mut cv = Canvas { px: &mut px, w, h };
        m.draw(&mut cv, &mut tx, &theme, window);
    }
    // Clicking a row starts recording; a conflicting combo shows the confirm buttons.
    let row = hit_rect(&m, Hit::KbRow(2));
    m.mouse_down((row.x + 20) as f64, (row.y + 5) as f64);
    assert!(m.keybinds.is_recording());
    assert_eq!(m.keybinds.selected_id(), Some("CloseTab"));
    assert_eq!(m.key(&ch("p"), None, true, false, false), Event::None);
    assert!(m.keybinds.pending_conflict().is_some());
    {
        let mut cv = Canvas { px: &mut px, w, h };
        m.draw(&mut cv, &mut tx, &theme, window);
    }
    let (x, y) = center(hit_rect(&m, Hit::KbConfirm));
    let ev = m.mouse_down(x, y);
    assert_eq!(
        ev,
        Event::Batch(vec![
            Event::Bind { combo: "ctrl+w".into(), command_id: "none".into() },
            Event::Bind { combo: "ctrl+p".into(), command_id: "CloseTab".into() },
        ])
    );
    // Outside click closes.
    assert_eq!(m.mouse_down(1.0, 1.0), Event::Close);
}

#[test]
fn settings_clicks_toggle_step_and_dropdown_via_hits() {
    let (w, h) = (1000i32, 700i32);
    let mut px = vec![BG; (w * h) as usize];
    let theme = test_theme();
    let mut tx = TextCtx::new(None);
    let window = Rect::new(0, 0, w, h);
    let mut m = modal();
    let mut redraw = |m: &mut Modal| {
        let mut cv = Canvas { px: &mut px, w, h };
        m.draw(&mut cv, &mut tx, &theme, window);
    };
    redraw(&mut m);
    // General: toggle format_on_save
    let d = def_idx(&m.settings, "format_on_save");
    let (x, y) = center(hit_rect(&m, Hit::Toggle(d)));
    assert_eq!(m.mouse_down(x, y), Event::Setting { key: "format_on_save".into(), value: json!(true) });
    // Fonts section: "+" on the buffer font size
    let (x, y) = center(hit_rect(&m, Hit::Section(6)));
    m.mouse_down(x, y);
    redraw(&mut m);
    let d = def_idx(&m.settings, "buffer_font_size");
    let (x, y) = center(hit_rect(&m, Hit::Step(d, 1)));
    assert_eq!(m.mouse_down(x, y), Event::Setting { key: "buffer_font_size".into(), value: json!(15) });
    // Appearance: segmented option click, then dropdown open + pick
    m.settings.set_choices("theme", vec!["x".into(), "y".into(), "z".into(), "w".into(), "v".into()]);
    let (x, y) = center(hit_rect(&m, Hit::Section(2)));
    m.mouse_down(x, y);
    redraw(&mut m);
    let d = def_idx(&m.settings, "appearance");
    let (x, y) = center(hit_rect(&m, Hit::Seg(d, 1)));
    assert_eq!(m.mouse_down(x, y), Event::Setting { key: "appearance".into(), value: json!("system") });
    let d = def_idx(&m.settings, "theme");
    let (x, y) = center(hit_rect(&m, Hit::DropOpen(d)));
    assert_eq!(m.mouse_down(x, y), Event::None);
    assert!(m.settings.dropdown_open());
    redraw(&mut m);
    let (x, y) = center(hit_rect(&m, Hit::DropItem(3)));
    assert_eq!(m.mouse_down(x, y), Event::Setting { key: "theme".into(), value: json!("w") });
    assert!(!m.settings.dropdown_open());
}
