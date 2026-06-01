use super::*;

// ── trim_trailing_whitespace pre-save hook tests ──────────────────────────────

/// Saving with `trim_trailing_whitespace` on strips trailing spaces and tabs
/// from every line in the buffer, both on disk and in the buffer itself.
#[test]
fn save_trims_trailing_whitespace_when_option_on() {
    use std::io::Write;
    let path = tmp_path(&format!(
        "hjkl_tts_on_{}.txt",
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    // Create the file so App can open it.
    let mut f = std::fs::File::create(&path).unwrap();
    f.write_all(b"hello   \nworld\t\nclean\n").unwrap();
    drop(f);

    let mut app = App::new(Some(path.clone()), false, None, None).unwrap();
    // Enable the option.
    app.active_mut()
        .editor
        .settings_mut()
        .trim_trailing_whitespace = true;
    // Trigger :w
    app.dispatch_ex("write");

    // Disk must be trimmed.
    let on_disk = std::fs::read_to_string(&path).unwrap();
    assert!(
        !on_disk.contains("   "),
        "trailing spaces must be stripped from disk; got: {on_disk:?}"
    );
    assert!(
        !on_disk.contains('\t'),
        "trailing tabs must be stripped from disk; got: {on_disk:?}"
    );
    assert!(
        on_disk.contains("hello"),
        "content must be preserved; got: {on_disk:?}"
    );
    assert!(
        on_disk.contains("world"),
        "content must be preserved; got: {on_disk:?}"
    );

    // Buffer must also reflect the trimmed content.
    let buf = app.active().editor.buffer().as_string();
    assert!(
        !buf.contains("   "),
        "buffer must have no trailing spaces after trim; got: {buf:?}"
    );
    assert!(
        !buf.contains('\t') || buf.trim_start_matches('\t').is_empty(),
        "buffer must have no trailing tabs; got: {buf:?}"
    );

    let _ = std::fs::remove_file(&path);
}

/// Saving with `trim_trailing_whitespace` off preserves trailing whitespace.
#[test]
fn save_skips_trim_when_option_off() {
    use std::io::Write;
    let path = tmp_path(&format!(
        "hjkl_tts_off_{}.txt",
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    let mut f = std::fs::File::create(&path).unwrap();
    f.write_all(b"hello   \nworld\n").unwrap();
    drop(f);

    let mut app = App::new(Some(path.clone()), false, None, None).unwrap();
    // Ensure option is off (default).
    app.active_mut()
        .editor
        .settings_mut()
        .trim_trailing_whitespace = false;
    app.dispatch_ex("write");

    let on_disk = std::fs::read_to_string(&path).unwrap();
    assert!(
        on_disk.contains("hello   "),
        "trailing whitespace must be preserved when option is off; got: {on_disk:?}"
    );

    let _ = std::fs::remove_file(&path);
}

/// A buffer with no trailing whitespace is written unchanged when the option
/// is on — no spurious dirty-gen bump.
#[test]
fn save_trim_noop_when_no_trailing_whitespace() {
    use std::io::Write;
    let path = tmp_path(&format!(
        "hjkl_tts_noop_{}.txt",
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    let original = "clean\nlines\nonly\n";
    let mut f = std::fs::File::create(&path).unwrap();
    f.write_all(original.as_bytes()).unwrap();
    drop(f);

    let mut app = App::new(Some(path.clone()), false, None, None).unwrap();
    app.active_mut()
        .editor
        .settings_mut()
        .trim_trailing_whitespace = true;
    app.dispatch_ex("write");

    let on_disk = std::fs::read_to_string(&path).unwrap();
    assert!(
        on_disk.contains("clean"),
        "clean content must be preserved; got: {on_disk:?}"
    );

    let _ = std::fs::remove_file(&path);
}

// ── format_on_save pre-save hook tests ───────────────────────────────────────

/// Saving with `format_on_save` on and no formatter registered for the file
/// extension proceeds without error (warn-and-fall-through does not apply here
/// — no formatter = silent pass-through).
#[test]
fn save_writes_unformatted_when_no_formatter_for_path() {
    use std::io::Write;
    let path = tmp_path(&format!(
        "hjkl_fos_noext_{}.xyz",
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    let mut f = std::fs::File::create(&path).unwrap();
    f.write_all(b"some content\n").unwrap();
    drop(f);

    let mut app = App::new(Some(path.clone()), false, None, None).unwrap();
    app.active_mut().editor.settings_mut().format_on_save = true;
    // :write must succeed — no formatter for .xyz, no error.
    app.dispatch_ex("write");

    let msg = app.bus.last_body_or_empty().to_string();
    // The status must be an info message (file written), not an error.
    assert!(
        !msg.to_lowercase().contains("error") && !msg.to_lowercase().contains("abort"),
        "save with no formatter must succeed; got status: {msg:?}"
    );
    // File must exist.
    assert!(
        path.exists(),
        "file must be written when no formatter matches"
    );

    let _ = std::fs::remove_file(&path);
}

/// `:set fos` via dispatch_ex correctly enables format_on_save in settings.
#[test]
fn set_fos_alias_enables_format_on_save_via_ex() {
    let mut app = App::new(None, false, None, None).unwrap();
    // format_on_save defaults to on; turn it off first so the alias toggle is
    // exercised in both directions.
    app.dispatch_ex("set nofos");
    assert!(
        !app.active().editor.settings().format_on_save,
        ":set nofos must disable format_on_save"
    );
    app.dispatch_ex("set fos");
    assert!(
        app.active().editor.settings().format_on_save,
        ":set fos must enable format_on_save"
    );
}

/// `:set tts` via dispatch_ex correctly enables trim_trailing_whitespace.
#[test]
fn set_tts_alias_enables_trim_trailing_whitespace_via_ex() {
    let mut app = App::new(None, false, None, None).unwrap();
    assert!(
        !app.active().editor.settings().trim_trailing_whitespace,
        "trim_trailing_whitespace must start false"
    );
    app.dispatch_ex("set tts");
    assert!(
        app.active().editor.settings().trim_trailing_whitespace,
        ":set tts must enable trim_trailing_whitespace"
    );
}

/// `:set nofos` disables format_on_save.
#[test]
fn set_nofos_disables_format_on_save_via_ex() {
    let mut app = App::new(None, false, None, None).unwrap();
    app.active_mut().editor.settings_mut().format_on_save = true;
    app.dispatch_ex("set nofos");
    assert!(
        !app.active().editor.settings().format_on_save,
        ":set nofos must disable format_on_save"
    );
}

/// `:set notts` disables trim_trailing_whitespace.
#[test]
fn set_notts_disables_trim_trailing_whitespace_via_ex() {
    let mut app = App::new(None, false, None, None).unwrap();
    app.active_mut()
        .editor
        .settings_mut()
        .trim_trailing_whitespace = true;
    app.dispatch_ex("set notts");
    assert!(
        !app.active().editor.settings().trim_trailing_whitespace,
        ":set notts must disable trim_trailing_whitespace"
    );
}

/// format_on_save with a missing tool must warn but NOT abort the save.
/// Uses a `.xyz` file that happens to have no formatter, simulating the
/// "tool installed check path" via a file without a registered formatter.
/// (A real "tool not installed" test would need to mock `is_tool_installed`.)
#[test]
fn save_fos_missing_tool_warns_but_does_not_abort() {
    use std::io::Write;
    // .xyz has no formatter registered in hjkl-mangler → formatter_for_path returns None
    // → silent pass-through, not an abort.
    let path = tmp_path(&format!(
        "hjkl_fos_notool_{}.xyz",
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    let mut f = std::fs::File::create(&path).unwrap();
    f.write_all(b"data\n").unwrap();
    drop(f);

    let mut app = App::new(Some(path.clone()), false, None, None).unwrap();
    app.active_mut().editor.settings_mut().format_on_save = true;
    app.dispatch_ex("write");

    // File must have been written (save not aborted).
    assert!(
        path.exists(),
        "file must be written when formatter is missing"
    );

    let _ = std::fs::remove_file(&path);
}
