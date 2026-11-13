//! Finding Ashita v4, Windower 4 and the game host (pol.exe), generating Ashita's per-account boot
//! ini, and building the injector command lines. Nothing here carries a secret: the pipe
//! name and launcher PID are the only per-launch values, and both are public by design.

use crate::i18n::t;
use std::{
    fs,
    path::{Path, PathBuf},
    process::{Command, Stdio},
};

/// The loader ships as `pol.exe`, the process name FFXI tools (Windower's updater, Ashita,
/// EliteAPI tools, bots) look for. It lives in a `crystal` subfolder of the launcher's folder so
/// it never collides with a real PlayOnline install.
pub(crate) const LOADER_EXE: &str = r"crystal\pol.exe";
/// The boot ini the user's base defaults to, when Ashita has one.
pub(crate) const DEFAULT_BOOT_INI: &str = "default.ini";

#[derive(Clone, Copy, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub(crate) enum Tool {
    Ashita,
    Windower,
    Vanilla,
}

impl Tool {
    pub(crate) fn name(self) -> &'static str {
        match self {
            Tool::Ashita => "Ashita v4",
            Tool::Windower => "Windower 4",
            Tool::Vanilla => t("Vanilla"),
        }
    }
    /// The program in the install folder that the launcher runs.
    pub(crate) fn exe(self) -> &'static str {
        match self {
            Tool::Ashita => "Ashita-cli.exe",
            Tool::Windower => "Windower.exe",
            Tool::Vanilla => "pol.exe",
        }
    }
    fn product(self) -> Option<crate::discovery::Product> {
        match self {
            Tool::Ashita => Some(crate::discovery::Product::Ashita),
            Tool::Windower => Some(crate::discovery::Product::Windower),
            Tool::Vanilla => None,
        }
    }
}

/// Validate a discovered hint without starting a new inventory scan.
pub(crate) fn validated_folder(tool: Tool, folder: &Path) -> Option<PathBuf> {
    crate::discovery::validated_folder(tool.product()?, folder)
}

/// A valid configured folder wins; a stale path copied from another PC falls
/// back to local discovery. The stored preference remains available to edit.
pub(crate) fn detect(tool: Tool, configured: &str) -> Option<PathBuf> {
    crate::discovery::find(tool.product()?, Some(Path::new(configured.trim())))
}

#[path = "embedded.rs"]
pub(crate) mod embedded;

/// Prefer the installed host when it matches this launcher's embedded copy. Portable launches
/// and updates with an older installed host fall back to extracting the matching embedded one.
/// The returned path is the one the pipe handshake verifies the client against.
pub(crate) fn loader_path(configured: &str) -> Option<PathBuf> {
    let launcher = std::env::current_exe().ok();
    resolve_loader(
        configured.trim(),
        launcher.as_deref().and_then(Path::parent),
        embedded::expected_hash(),
        embedded::extracted,
    )
}

fn resolve_loader(
    configured: &str,
    launcher_dir: Option<&Path>,
    expected_hash: Option<&str>,
    extract: impl FnOnce() -> Option<PathBuf>,
) -> Option<PathBuf> {
    if !configured.is_empty() {
        let path = PathBuf::from(configured);
        return path.is_file().then_some(path);
    }
    launcher_dir.and_then(|dir| loader_in(dir, expected_hash)).or_else(|| extract().filter(|p| p.is_file()))
}

/// An installed host must match the embedded host, so replacing only the GUI during a portable
/// self-update cannot accidentally pair it with the previous installation's native code.
fn loader_in(launcher_dir: &Path, expected_hash: Option<&str>) -> Option<PathBuf> {
    let path = launcher_dir.join(LOADER_EXE);
    (path.is_file() && expected_hash.is_none_or(|hash| embedded::has_hash(&path, hash))).then_some(path)
}

/// Boot inis the user can base the generated one on (Ashita's own config\boot, minus ours).
pub(crate) fn boot_inis(ashita: &Path) -> Vec<String> {
    let mut names: Vec<String> = std::fs::read_dir(ashita.join(r"config\boot"))
        .into_iter()
        .flatten()
        .flatten()
        .filter(|e| e.file_type().is_ok_and(|t| t.is_file()))
        .filter_map(|e| e.file_name().into_string().ok())
        .filter(|n| n.to_ascii_lowercase().ends_with(".ini") && !n.to_ascii_lowercase().starts_with("crystal-"))
        .collect();
    names.sort_by_key(|n| n.to_ascii_lowercase());
    names
}

/// A boot config the launcher may read: a plain `.ini` file name inside `config\boot`.
pub(crate) fn valid_boot_ini_name(name: &str) -> bool {
    !name.is_empty()
        && name.len() <= 255
        && name.trim() == name
        && !name.contains(['/', '\\', ':'])
        && !name.chars().any(char::is_control)
        && name.to_ascii_lowercase().ends_with(".ini")
}

/// Profile names from Windower's settings.xml. Only `name` attributes are read; the file can
/// hold other tools' plaintext passwords, so nothing else is kept.
pub(crate) fn windower_profiles(windower: &Path) -> Vec<String> {
    use quick_xml::events::Event;
    let Ok(bytes) = std::fs::read(windower.join("settings.xml")) else {
        return Vec::new();
    };
    if bytes.len() > 4 * 1024 * 1024 {
        return Vec::new();
    }
    let mut reader = quick_xml::Reader::from_reader(bytes.as_slice());
    let mut names = Vec::new();
    let mut buffer = Vec::new();
    // Exactly the profiles a launch can select (`ffxi_settings::windower_profile`).
    let mut depth = 0usize;
    loop {
        let profile = match reader.read_event_into(&mut buffer) {
            Ok(Event::Start(e)) => {
                depth += 1;
                super::ffxi_settings::is_root_profile(depth, &e).then(|| super::ffxi_settings::profile_name(&e))
            }
            Ok(Event::Empty(e)) => super::ffxi_settings::is_root_profile(depth + 1, &e).then(|| super::ffxi_settings::profile_name(&e)),
            Ok(Event::End(_)) => {
                depth = depth.saturating_sub(1);
                None
            }
            Ok(Event::Eof) | Err(_) => break,
            _ => None,
        };
        // The unnamed profile is Windower's default, which "no profile" already means.
        if let Some(name) = profile.filter(|name| !name.is_empty() && valid_profile(name) && !names.contains(name)) {
            names.push(name);
        }
        buffer.clear();
    }
    names
}

pub(crate) fn valid_profile(name: &str) -> bool {
    name.len() <= 128 && !name.contains('"') && !name.chars().any(char::is_control)
}

/// The loader's own arguments: the pipe and our PID. Never a secret.
pub(crate) fn loader_args(pipe: &str, launcher_pid: u32) -> String {
    format!("--crystal-pipe {pipe} --crystal-launcher-pid {launcher_pid}")
}

/// The generated ini's file name inside Ashita's `config\boot`.
pub(crate) fn boot_ini_name(account_key: &str) -> String {
    format!("crystal-{account_key}.ini")
}

struct Edit<'a> {
    section: &'a str,
    key: &'a str,
    value: String,
    /// Only add the key when the base has none (the user's own choice wins).
    only_if_missing: bool,
}

/// The per-account Ashita boot ini (`file =` is the resolved loader path, whatever its name): the user's base with the boot keys pointed at the loader.
/// Comments, other sections, `[ashita.launcher]`, `script` and `[ffxi.registry]` come over as
/// the user had them. Retail is US only (`launch`): `--lang US`, `playonline = 2` (EN).
pub(crate) fn ashita_boot_ini(base: Option<&[u8]>, loader: &Path, pipe: &str, launcher_pid: u32) -> Result<Vec<u8>, String> {
    // GetPrivateProfileStringA reads the file in the ANSI code page.
    let loader = loader.to_str().filter(|s| s.is_ascii()).ok_or(
        t("Ashita reads its boot file as ANSI text, so the loader (pol.exe) must be in a folder whose path uses only plain (ASCII) characters."),
    )?;
    let command = format!("{} --lang US", loader_args(pipe, launcher_pid));
    let edits = [
        Edit { section: "ashita.boot", key: "file", value: loader.to_owned(), only_if_missing: false },
        Edit { section: "ashita.boot", key: "command", value: command, only_if_missing: false },
        Edit { section: "ashita.boot", key: "gamemodule", value: "ffximain.dll".into(), only_if_missing: false },
        Edit { section: "ashita.boot", key: "script", value: "default.txt".into(), only_if_missing: true },
        Edit { section: "ashita.language", key: "playonline", value: "2".into(), only_if_missing: true },
        Edit { section: "ashita.polplugins", key: "sandbox", value: "0".into(), only_if_missing: false },
    ];
    let base = base.unwrap_or(MINIMAL_BOOT_INI.as_bytes());
    let mut ini = edit_ini(base, &edits);
    if !ini_has_section(&ini, "ffxi.registry") {
        let newline: &[u8] = if ini.windows(2).any(|w| w == b"\r\n") { b"\r\n" } else { b"\n" };
        if !ini.ends_with(b"\n") {
            ini.extend_from_slice(newline);
        }
        ini.extend_from_slice(newline);
        ini.extend_from_slice(b"[ffxi.registry]");
        ini.extend_from_slice(newline);
    }
    Ok(ini)
}

/// The boot ini for a private-server launch through Ashita: the user's base with the boot
/// keys pointed at the server's xiloader and its arguments (`command`). Unlike the retail ini,
/// the language is left as the user set it: xiloader takes its own `--lang`.
pub(crate) fn ashita_private_boot_ini(base: Option<&[u8]>, loader: &Path, command: &str) -> Result<Vec<u8>, String> {
    // GetPrivateProfileStringA reads the file in the ANSI code page.
    let loader = loader.to_str().filter(|s| s.is_ascii()).ok_or_else(|| {
        "Ashita reads its boot file as ANSI text, so xiloader must be in a folder whose path uses only plain (ASCII) characters.".to_owned()
    })?;
    if command.contains(['\r', '\n']) {
        return Err("The xiloader command cannot contain line breaks.".into());
    }
    let edits = [
        Edit { section: "ashita.boot", key: "file", value: loader.to_owned(), only_if_missing: false },
        Edit { section: "ashita.boot", key: "command", value: command.to_owned(), only_if_missing: false },
        Edit { section: "ashita.boot", key: "gamemodule", value: "ffximain.dll".into(), only_if_missing: false },
        Edit { section: "ashita.boot", key: "script", value: "default.txt".into(), only_if_missing: true },
        Edit { section: "ashita.polplugins", key: "sandbox", value: "0".into(), only_if_missing: false },
    ];
    Ok(edit_ini(base.unwrap_or(MINIMAL_BOOT_INI.as_bytes()), &edits))
}

/// POL plugins that crash when two game hosts load them at once ("Failed to load POL plugin
/// 'pivot'"). An Ashita start whose boot ini loads one of these takes the machine-wide turn;
/// every other Ashita start goes at once. Add a name here if another plugin proves to race.
pub(crate) const SERIAL_POLPLUGINS: [&str; 1] = ["pivot"];

/// Whether an Ashita start with the base boot ini at `path` must take the turn. A missing file
/// means Crystal's own minimal ini (no plugins); a file that can't be read is the safe case.
pub(crate) fn boot_ini_needs_turn(path: &Path) -> bool {
    match fs::read(path) {
        Ok(ini) => loads_serial_polplugin(&ini),
        Err(e) => e.kind() != std::io::ErrorKind::NotFound,
    }
}

/// Whether `[ashita.polplugins]` has an enabled entry for a `SERIAL_POLPLUGINS` name:
/// `name = 1` (any value but 0/off/false/no) or a bare `name`. Names are case-insensitive;
/// `;` and `#` comment out a line, and `;` also ends a value.
fn loads_serial_polplugin(ini: &[u8]) -> bool {
    let mut in_section = false;
    for line in ini.split(|&b| b == b'\n') {
        let line = String::from_utf8_lossy(line);
        let text = line.trim().trim_start_matches('\u{feff}').trim();
        if let Some(section) = section_of(text) {
            in_section = section == "ashita.polplugins";
            continue;
        }
        if !in_section || text.is_empty() || text.starts_with(';') || text.starts_with('#') {
            continue;
        }
        let (name, value) = text.split_once('=').unwrap_or((text, "1"));
        let value = value.split(';').next().unwrap_or("").trim();
        let disabled = ["0", "off", "false", "no"].iter().any(|d| value.eq_ignore_ascii_case(d));
        if !disabled && SERIAL_POLPLUGINS.iter().any(|p| p.eq_ignore_ascii_case(name.trim())) {
            return true;
        }
    }
    false
}
/// Used when the user has no base boot ini at all.
const MINIMAL_BOOT_INI: &str = "; Generated by Crystal Launcher for a POL-less retail launch through crystal-loader.\r\n\
[ashita.launcher]\r\nautoclose = 1\r\nname = Crystal Launcher\r\n\r\n\
[ashita.boot]\r\nfile =\r\ncommand =\r\ngamemodule = ffximain.dll\r\nscript = default.txt\r\nargs =\r\n\r\n\
[ashita.language]\r\nplayonline = 2\r\nashita = 2\r\n\r\n\
[ashita.polplugins]\r\nsandbox = 0\r\n\r\n[ashita.polplugins.args]\r\n\r\n[ffxi.registry]\r\n";

fn section_of(line: &str) -> Option<String> {
    let t = line.trim().trim_start_matches('\u{feff}').trim();
    (t.starts_with('[') && t.ends_with(']') && t.len() >= 2).then(|| t[1..t.len() - 1].trim().to_ascii_lowercase())
}

fn ini_has_section(ini: &[u8], section: &str) -> bool {
    ini.split(|&b| b == b'\n')
        .any(|line| section_of(&String::from_utf8_lossy(line)).as_deref() == Some(section))
}

/// Rewrite or add `key = value` lines, keeping every other byte of the base as it was (the
/// base may be in the ANSI code page, so unchanged lines are copied as raw bytes).
fn edit_ini(base: &[u8], edits: &[Edit<'_>]) -> Vec<u8> {
    let newline: &[u8] = if base.is_empty() || base.windows(2).any(|w| w == b"\r\n") { b"\r\n" } else { b"\n" };
    let mut done = vec![false; edits.len()];
    let mut present = vec![false; edits.len()];
    // First pass: which only-if-missing keys the base already has.
    let mut section = String::new();
    for line in base.split(|&b| b == b'\n') {
        let text = String::from_utf8_lossy(line);
        if let Some(s) = section_of(&text) {
            section = s;
            continue;
        }
        if let Some(key) = key_of(&text) {
            for (i, e) in edits.iter().enumerate() {
                if e.section == section && key.eq_ignore_ascii_case(e.key) {
                    present[i] = true;
                }
            }
        }
    }
    let mut out = Vec::with_capacity(base.len() + 1024);
    let flush = |out: &mut Vec<u8>, section: &str, done: &mut Vec<bool>| {
        for (i, e) in edits.iter().enumerate() {
            if !done[i] && e.section == section && !(e.only_if_missing && present[i]) {
                if !out.is_empty() && !out.ends_with(b"\n") {
                    out.extend_from_slice(newline);
                }
                out.extend_from_slice(format!("{} = {}", e.key, e.value).as_bytes());
                out.extend_from_slice(newline);
                done[i] = true;
            }
        }
    };
    let mut current: Option<String> = None;
    let mut blanks: Vec<&[u8]> = Vec::new();
    for line in base.split_inclusive(|&b| b == b'\n') {
        let text = String::from_utf8_lossy(line);
        if let Some(s) = section_of(&text) {
            if let Some(previous) = &current {
                flush(&mut out, previous, &mut done);
            }
            for blank in blanks.drain(..) {
                out.extend_from_slice(blank);
            }
            current = Some(s);
            out.extend_from_slice(line);
            continue;
        }
        if text.trim().is_empty() {
            blanks.push(line);
            continue;
        }
        for blank in blanks.drain(..) {
            out.extend_from_slice(blank);
        }
        if let (Some(section), Some(key)) = (&current, key_of(&text)) {
            if let Some(i) = edits
                .iter()
                .position(|e| e.section == section && key.eq_ignore_ascii_case(e.key) && !e.only_if_missing)
            {
                let ending: &[u8] = if line.ends_with(b"\r\n") { b"\r\n" } else if line.ends_with(b"\n") { b"\n" } else { b"" };
                out.extend_from_slice(format!("{} = {}", key, edits[i].value).as_bytes());
                out.extend_from_slice(ending);
                done[i] = true;
                continue;
            }
        }
        out.extend_from_slice(line);
    }
    if let Some(previous) = &current {
        flush(&mut out, previous, &mut done);
    }
    for blank in blanks.drain(..) {
        out.extend_from_slice(blank);
    }
    // Sections the base lacks, in edit order.
    let mut missing: Vec<&str> = Vec::new();
    for (i, e) in edits.iter().enumerate() {
        if !done[i] && !(e.only_if_missing && present[i]) && !missing.contains(&e.section) {
            missing.push(e.section);
        }
    }
    for section in missing {
        if !out.is_empty() && !out.ends_with(b"\n") {
            out.extend_from_slice(newline);
        }
        if !out.is_empty() {
            out.extend_from_slice(newline);
        }
        out.extend_from_slice(format!("[{section}]").as_bytes());
        out.extend_from_slice(newline);
        flush(&mut out, section, &mut done);
    }
    out
}

/// The key of a `key = value` line; comments and section headers have none.
fn key_of(line: &str) -> Option<&str> {
    let t = line.trim_start_matches('\u{feff}').trim();
    if t.starts_with(';') || t.starts_with('#') || t.starts_with('[') {
        return None;
    }
    let (key, _) = t.split_once('=')?;
    let key = key.trim();
    (!key.is_empty()).then_some(key)
}

/// `Ashita-cli.exe crystal-<id>.ini`, run from Ashita's folder with no console window.
pub(crate) fn ashita_command(ashita: &Path, ini_name: &str) -> Command {
    let mut command = Command::new(ashita.join(Tool::Ashita.exe()));
    command.arg(ini_name).current_dir(ashita);
    private_stdio(&mut command);
    no_window(&mut command);
    command
}

/// No std handle of the spawning worker reaches the child: std's `Command` would otherwise
/// duplicate the worker's stdout pipe into it, and the window would never see the worker's
/// EOF while the game runs. Callers may still redirect stdout/stderr to a log file.
fn private_stdio(command: &mut Command) {
    command.stdin(Stdio::null()).stdout(Stdio::null()).stderr(Stdio::null());
}

/// Run the game host itself, with no console window. Credentials are supplied later over
/// its verified pipe.
pub(crate) fn vanilla_command(loader: &Path, install: &Path, pipe: &str, launcher_pid: u32) -> Command {
    let mut command = Command::new(loader);
    command.arg("--crystal-pipe").arg(pipe)
        .arg("--crystal-launcher-pid").arg(launcher_pid.to_string())
        .current_dir(install);
    private_stdio(&mut command);
    no_window(&mut command);
    command
}

fn no_window(command: &mut Command) {
    use std::os::windows::process::CommandExt;
    command.creation_flags(windows_sys::Win32::System::Threading::CREATE_NO_WINDOW);
}

/// `Windower.exe --hide --profile=<p> --executable=<loader> --args=<loader args>`, run from
/// Windower's folder (it loads settings.xml relative to the working directory).
pub(crate) fn windower_command(
    windower: &Path,
    profile: &str,
    loader: &Path,
    pipe: &str,
    launcher_pid: u32,
) -> Result<Command, String> {
    let profile = profile.trim();
    if !valid_profile(profile) {
        return Err(t("The Windower profile name can't contain quotes or control characters.").into());
    }
    let mut command = Command::new(windower.join(Tool::Windower.exe()));
    // Long option forms only: Windower's -h means both --hide and --height.
    command.arg("--hide");
    // Keep the choice explicit even for the unnamed default, so Windower and
    // the settings snapshot use the same profile selection.
    command.arg(format!("--profile={profile}"));
    command
        .arg(format!("--executable={}", loader.display()))
        .arg(format!("--args={}", loader_args(pipe, launcher_pid)))
        .current_dir(windower);
    private_stdio(&mut command);
    Ok(command)
}

/// `Windower.exe --hide --profile=<p> --executable=<program> --args=<arguments>` for a program
/// that is not Crystal's loader (a private server's xiloader), run from Windower's folder.
pub(crate) fn windower_command_with_args(windower: &Path, profile: &str, executable: &Path, arguments: &str) -> Result<Command, String> {
    let profile = profile.trim();
    if !valid_profile(profile) {
        return Err(t("The Windower profile name can't contain quotes or control characters.").into());
    }
    let mut command = Command::new(windower.join(Tool::Windower.exe()));
    // Long option forms only: Windower's -h means both --hide and --height.
    command.arg("--hide");
    command.arg(format!("--profile={profile}"));
    command
        .arg(format!("--executable={}", executable.display()))
        .arg(format!("--args={arguments}"))
        .current_dir(windower);
    private_stdio(&mut command);
    Ok(command)
}

/// What Windower's update chain may rewrite before it injects (pol-spec 07d §4.6; 08 §1
/// row 2): its update records, the add-on tree, and its own binaries and plugins.
const WINDOWER_UPDATED: [&str; 6] = [r"updates\manifest.xml", r"updates\resources.xml", r"updates\addons.json", r"updates\addons", "Hook.dll", "Windower.exe"];

/// (relative name, size and mtime in ns; `None` when missing) for each file Windower's update
/// chain touches, plugins included, in a fixed order. Local metadata only.
pub(crate) fn windower_files(windower: &Path) -> Vec<(String, Option<(u64, u128)>)> {
    let stamp = |path: &Path| fs::metadata(path).ok().map(|m| (m.len(),
        m.modified().ok().and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok()).map_or(0, |d| d.as_nanos())));
    let mut files: Vec<_> = WINDOWER_UPDATED.iter().map(|name| (name.to_string(), stamp(&windower.join(name)))).collect();
    let mut plugins: Vec<_> = fs::read_dir(windower.join("plugins")).into_iter().flatten().flatten()
        .filter(|entry| entry.path().extension().is_some_and(|e| e.eq_ignore_ascii_case("dll")))
        .map(|entry| (format!(r"plugins\{}", entry.file_name().to_string_lossy()), stamp(&entry.path())))
        .collect();
    plugins.sort();
    files.extend(plugins);
    files
}

/// The names whose size or mtime differ (added and removed included), in snapshot order.
pub(crate) fn windower_changes(before: &[(String, Option<(u64, u128)>)], after: &[(String, Option<(u64, u128)>)]) -> Vec<String> {
    let mut changed: Vec<String> = after.iter().filter(|file| !before.contains(file)).map(|(name, _)| name.clone()).collect();
    changed.extend(before.iter().filter(|(name, _)| !after.iter().any(|(other, _)| other == name)).map(|(name, _)| name.clone()));
    changed
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_a_boot_ini_that_loads_pivot_takes_the_turn() {
        let root = std::env::temp_dir().join(format!("crystal-turn-ini-{}", crate::backend::unique_id().unwrap()));
        fs::create_dir_all(&root).unwrap();
        let needs = |body: &str| {
            let path = root.join("x.ini");
            fs::write(&path, body).unwrap();
            boot_ini_needs_turn(&path)
        };
        assert!(needs("[ashita.polplugins]\r\nsandbox = 0\r\npivot = 1\r\nquicky = 1\r\n"));
        assert!(needs("[ashita.polplugins]\r\nPIVOT=1\r\n"));
        assert!(needs("[Ashita.PolPlugins]\r\npivot\r\n"));
        assert!(!needs("[ashita.polplugins]\r\nsandbox = 0\r\nquicky = 1\r\n"));
        assert!(!needs("[ashita.polplugins]\r\npivot = 0\r\n"));
        assert!(!needs("[ashita.polplugins]\r\n;pivot = 1\r\n# pivot\r\n"));
        assert!(!needs("[ashita.polplugins.args]\r\npivot = 1\r\n[ashita.boot]\r\npivot = 1\r\n"));
        assert!(!needs(""));
        // No base ini: Crystal's minimal one has no plugins. A path that can't be read takes the turn.
        assert!(!boot_ini_needs_turn(&root.join("missing.ini")));
        assert!(boot_ini_needs_turn(&root));
        let _ = fs::remove_dir_all(&root);
    }

    /// 07d §4.6: only what changed between the spawn and the hello is named.
    #[test]
    fn windower_update_snapshot_names_changed_files_only() {
        let root = std::env::temp_dir().join(format!("crystal-windower-files-{}", crate::backend::unique_id().unwrap()));
        fs::create_dir_all(root.join(r"updates\addons")).unwrap();
        fs::create_dir_all(root.join("plugins")).unwrap();
        for file in [r"updates\manifest.xml", "Hook.dll", "Windower.exe", r"plugins\Timestamp.dll", r"plugins\readme.txt"] {
            fs::write(root.join(file), b"v1").unwrap();
        }
        let before = windower_files(&root);
        assert_eq!(before.len(), WINDOWER_UPDATED.len() + 1, "plugins: DLLs only");
        assert!(windower_changes(&before, &windower_files(&root)).is_empty());
        std::thread::sleep(std::time::Duration::from_millis(20));
        fs::write(root.join("Hook.dll"), b"v2 longer").unwrap();
        fs::write(root.join(r"updates\addons.json"), b"{}").unwrap();
        fs::write(root.join(r"plugins\New.dll"), b"x").unwrap();
        fs::remove_file(root.join(r"plugins\Timestamp.dll")).unwrap();
        fs::write(root.join(r"plugins\readme.txt"), b"changed text").unwrap();
        assert_eq!(windower_changes(&before, &windower_files(&root)),
            vec![r"updates\addons.json", "Hook.dll", r"plugins\New.dll", r"plugins\Timestamp.dll"]);
        fs::remove_dir_all(root).unwrap();
    }

    const PIPE: &str = r"\\.\pipe\crystal-loader-00112233445566778899aabbccddeeff";

    #[test]
    fn vanilla_runs_only_the_host_with_separate_pipe_arguments_and_the_game_directory() {
        let loader = Path::new(r"C:\Users\試験 User\Crystal Launcher\pol.exe");
        let install = Path::new(r"C:\Games\SquareEnix\FINAL FANTASY XI");
        let command = vanilla_command(loader, install, PIPE, 127);
        assert_eq!(command.get_program(), loader.as_os_str());
        assert_eq!(command.get_current_dir(), Some(install));
        let args: Vec<_> = command.get_args().map(|a| a.to_str().unwrap()).collect();
        assert_eq!(args, ["--crystal-pipe", PIPE, "--crystal-launcher-pid", "127"]);
        assert_eq!(command.get_envs().count(), 0, "no credential environment variables");
        assert!(Tool::Vanilla.product().is_none(), "Vanilla has no install of its own");
    }

    fn text(bytes: &[u8]) -> String {
        String::from_utf8(bytes.to_vec()).unwrap()
    }

    fn value(ini: &str, section: &str, key: &str) -> Vec<String> {
        let mut current = String::new();
        let mut found = Vec::new();
        for line in ini.lines() {
            if let Some(s) = section_of(line) {
                current = s;
            } else if current == section {
                if let Some((k, v)) = line.split_once('=') {
                    if !k.trim().starts_with(';') && k.trim().eq_ignore_ascii_case(key) {
                        found.push(v.trim().to_owned());
                    }
                }
            }
        }
        found
    }

    const BASE: &str = "; My boot config\r\n[ashita.launcher]\r\nautoclose   = 0\r\nname        = Main\r\n\r\n\
[ashita.boot]\r\n; Retail Server Usage\r\nfile        =\r\ncommand     = /game eAZcFcB\r\ngamemodule  = ffximain.dll\r\nscript      = mine.txt\r\nargs        = \r\n\r\n\
[ashita.fonts]\r\nd3d8.family = Arial\r\n\r\n[ashita.polplugins]\r\nsandbox = 1\r\n\r\n[ashita.polplugins.args]\r\n;sandbox = \r\n\r\n\
[ffxi.registry]\r\n0001 = 2560\r\n0002 = 1440\r\n0042 = C:\\Program Files (x86)\\SquareEnix\\FINAL FANTASY XI\r\n";

    #[test]
    fn ashita_ini_rewrites_only_the_boot_keys() {
        let loader = Path::new(r"C:\Program Files\Crystal Launcher\crystal\pol.exe");
        let ini = text(&ashita_boot_ini(Some(BASE.as_bytes()), loader, PIPE, 4242).unwrap());
        assert_eq!(value(&ini, "ashita.boot", "file"), [loader.to_str().unwrap()]);
        assert_eq!(
            value(&ini, "ashita.boot", "command"),
            [format!("--crystal-pipe {PIPE} --crystal-launcher-pid 4242 --lang US")]
        );
        assert_eq!(value(&ini, "ashita.boot", "gamemodule"), ["ffximain.dll"]);
        // The user's own choices stay.
        assert_eq!(value(&ini, "ashita.boot", "script"), ["mine.txt"]);
        assert_eq!(value(&ini, "ashita.launcher", "autoclose"), ["0"]);
        assert_eq!(value(&ini, "ashita.launcher", "name"), ["Main"]);
        assert_eq!(value(&ini, "ashita.fonts", "d3d8.family"), ["Arial"]);
        assert_eq!(value(&ini, "ffxi.registry", "0001"), ["2560"]);
        assert_eq!(value(&ini, "ffxi.registry", "0042"), [r"C:\Program Files (x86)\SquareEnix\FINAL FANTASY XI"]);
        // Sandbox forced off; the commented-out arg is untouched.
        assert_eq!(value(&ini, "ashita.polplugins", "sandbox"), ["0"]);
        assert!(ini.contains(";sandbox = \r\n"));
        // Missing keys and sections are added, comments kept, CRLF throughout.
        assert_eq!(value(&ini, "ashita.language", "playonline"), ["2"]);
        assert!(ini.starts_with("; My boot config\r\n"));
        assert!(ini.contains("; Retail Server Usage\r\n"));
        assert!(!ini.replace("\r\n", "").contains('\n'));
        assert!(!ini.contains("eAZcFcB"));
    }

    #[test]
    fn ashita_ini_without_a_base_is_complete() {
        let ini = text(&ashita_boot_ini(None, Path::new(r"C:\L\pol.exe"), PIPE, 7).unwrap());
        assert_eq!(value(&ini, "ashita.boot", "file"), [r"C:\L\pol.exe"]);
        assert_eq!(value(&ini, "ashita.boot", "script"), ["default.txt"]);
        assert_eq!(value(&ini, "ashita.polplugins", "sandbox"), ["0"]);
        assert!(ini_has_section(ini.as_bytes(), "ffxi.registry"));
    }

    #[test]
    fn ashita_ini_adds_missing_sections() {
        let base = b"[ashita.boot]\nfile = x\n[ashita.input]\nmouse.unhook = 1";
        let ini = text(&ashita_boot_ini(Some(base), Path::new(r"C:\L\pol.exe"), PIPE, 7).unwrap());
        assert_eq!(value(&ini, "ashita.boot", "file"), [r"C:\L\pol.exe"]);
        assert_eq!(value(&ini, "ashita.boot", "command").len(), 1);
        assert_eq!(value(&ini, "ashita.input", "mouse.unhook"), ["1"]);
        assert_eq!(value(&ini, "ashita.polplugins", "sandbox"), ["0"]);
        assert_eq!(value(&ini, "ashita.language", "playonline"), ["2"]);
        assert!(ini_has_section(ini.as_bytes(), "ffxi.registry"));
        assert!(!ini.contains('\r'), "an LF base stays LF");
    }

    #[test]
    fn loader_lookup_finds_the_crystal_folder() {
        let dir = std::env::temp_dir().join(format!("crystal-loader-lookup-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(dir.join("crystal")).unwrap();
        assert_eq!(loader_in(&dir, None), None);
        // The name earlier builds used beside the launcher is no longer looked for.
        std::fs::write(dir.join("crystal-loader.exe"), b"").unwrap();
        assert_eq!(loader_in(&dir, None), None);
        let pol = dir.join("crystal").join("pol.exe");
        std::fs::write(&pol, b"").unwrap();
        assert_eq!(loader_in(&dir, None), Some(pol));
        assert!(LOADER_EXE.ends_with("pol.exe"));
        // An explicit override is used as given, whatever its file name.
        let custom = dir.join("anything.bin");
        std::fs::write(&custom, b"").unwrap();
        assert_eq!(loader_path(custom.to_str().unwrap()), Some(custom));
        assert_eq!(loader_path(dir.join("missing.exe").to_str().unwrap()), None);
        std::fs::remove_dir_all(&dir).unwrap();
    }
    #[test]
    fn loader_resolution_uses_matching_installed_host_without_extracting() {
        let dir = std::env::temp_dir().join(format!("crystal-loader-order-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let launcher = dir.join("launcher");
        std::fs::create_dir_all(launcher.join("crystal")).unwrap();
        let beside = launcher.join("crystal").join("pol.exe");
        let extracted = dir.join("data").join("crystal").join("pol.exe");
        let custom = dir.join("custom.exe");
        std::fs::create_dir_all(extracted.parent().unwrap()).unwrap();
        for p in [&beside, &extracted, &custom] {
            std::fs::write(p, b"x").unwrap();
        }
        let hash = embedded::sha256_hex(b"x");
        let unexpected_extract = || panic!("an installed or explicitly configured host must not be extracted");
        let custom_str = custom.to_str().unwrap();
        assert_eq!(resolve_loader(custom_str, Some(&launcher), Some(&hash), unexpected_extract), Some(custom.clone()));
        // A set but missing override is never replaced silently.
        assert_eq!(resolve_loader(dir.join("gone.exe").to_str().unwrap(), Some(&launcher), Some(&hash), unexpected_extract), None);
        assert_eq!(resolve_loader("", Some(&launcher), Some(&hash), unexpected_extract), Some(beside.clone()));
        // A portable GUI update must ignore a host from the previous release.
        std::fs::write(&beside, b"older host").unwrap();
        assert_eq!(resolve_loader("", Some(&launcher), Some(&hash), || Some(extracted.clone())), Some(extracted.clone()));
        assert_eq!(resolve_loader("", Some(&launcher), Some(&hash), || None), None);
        assert_eq!(resolve_loader("", None, Some(&hash), || Some(extracted.clone())), Some(extracted.clone()));
        std::fs::remove_file(&extracted).unwrap();
        assert_eq!(resolve_loader("", None, Some(&hash), || Some(extracted.clone())), None);
        std::fs::remove_dir_all(&dir).unwrap();
    }
    #[test]
    fn ashita_ini_keeps_ansi_bytes_and_refuses_non_ascii_loader_paths() {
        let base = b"[ashita.launcher]\r\nname = Caf\xe9\r\n[ashita.boot]\r\nfile =\r\n";
        let ini = ashita_boot_ini(Some(base), Path::new(r"C:\L\pol.exe"), PIPE, 7).unwrap();
        assert!(ini.windows(5).any(|w| w == b"Caf\xe9\r"));
        assert!(ashita_boot_ini(None, Path::new("C:\\Usu\u{e1}rio\\pol.exe"), PIPE, 7).is_err());
    }

    #[test]
    fn ashita_runs_the_generated_ini_from_its_folder() {
        let command = ashita_command(Path::new(r"C:\Games\AshitaV4"), &boot_ini_name("ab12"));
        assert_eq!(command.get_program(), Path::new(r"C:\Games\AshitaV4\Ashita-cli.exe").as_os_str());
        let args: Vec<_> = command.get_args().collect();
        assert_eq!(args, ["crystal-ab12.ini"]);
        assert_eq!(command.get_current_dir(), Some(Path::new(r"C:\Games\AshitaV4")));
    }

    #[test]
    fn windower_command_carries_only_the_pipe_and_pid() {
        let loader = Path::new(r"C:\Program Files\Crystal Launcher\crystal\pol.exe");
        let command = windower_command(Path::new(r"C:\Games\Windower 4"), "Main Mule", loader, PIPE, 4242).unwrap();
        assert_eq!(command.get_program(), Path::new(r"C:\Games\Windower 4\Windower.exe").as_os_str());
        assert_eq!(command.get_current_dir(), Some(Path::new(r"C:\Games\Windower 4")));
        let args: Vec<String> = command.get_args().map(|a| a.to_string_lossy().into_owned()).collect();
        assert_eq!(
            args,
            [
                "--hide".to_owned(),
                "--profile=Main Mule".to_owned(),
                format!("--executable={}", loader.display()),
                format!("--args=--crystal-pipe {PIPE} --crystal-launcher-pid 4242"),
            ]
        );
        assert!(command.get_envs().next().is_none(), "nothing is passed through the environment");
        // No session material can reach the command line: it is built only from these inputs.
        for arg in &args {
            for secret in ["v_key", "auth_code", "password", "session"] {
                assert!(!arg.contains(secret), "{arg}");
            }
        }
        let none = windower_command(Path::new(r"C:\W"), "", loader, PIPE, 1).unwrap();
        assert!(none.get_args().any(|a| a == "--profile="));
        assert!(windower_command(Path::new(r"C:\W"), "a\" --executable=evil", loader, PIPE, 1).is_err());
    }

    #[test]
    fn detection_prefers_a_valid_folder_and_falls_back_when_it_is_stale() {
        let root = std::env::temp_dir().join(format!("xi-tools-{}", crate::backend::unique_id().unwrap()));
        let ashita = root.join("AshitaV4");
        std::fs::create_dir_all(ashita.join(r"config\boot")).unwrap();
        std::fs::write(ashita.join("Ashita-cli.exe"), b"").unwrap();
        for name in ["default.ini", "Main.ini", "crystal-old.ini", "notes.txt"] {
            std::fs::write(ashita.join(r"config\boot").join(name), b"").unwrap();
        }
        let selected = crate::backend::plain_windows_path(&std::fs::canonicalize(&ashita).unwrap());
        assert_eq!(detect(Tool::Ashita, ashita.to_str().unwrap()), Some(selected));
        assert_eq!(detect(Tool::Windower, ashita.to_str().unwrap()), detect(Tool::Windower, ""));
        assert_eq!(detect(Tool::Ashita, root.join("missing").to_str().unwrap()), detect(Tool::Ashita, ""));
        assert_eq!(boot_inis(&ashita), ["default.ini", "Main.ini"]);
        use crate::discovery::{find, is_install, Product};
        assert!(find(Product::Ashita, None).iter().all(|folder| is_install(Product::Ashita, folder)));
        assert!(find(Product::Windower, None).iter().all(|folder| is_install(Product::Windower, folder)));
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn windower_profiles_read_names_only() {
        let root = std::env::temp_dir().join(format!("xi-windower-{}", crate::backend::unique_id().unwrap()));
        std::fs::create_dir_all(&root).unwrap();
        std::fs::write(
            root.join("settings.xml"),
            br#"<?xml version="1.0"?><settings><profile name="Main"><executable>pol.exe</executable><password>hunter2</password></profile><profile name="Mule &amp; Co"/><profile name="bad&quot;name"/></settings>"#,
        )
        .unwrap();
        assert_eq!(windower_profiles(&root), ["Main", "Mule & Co"]);
        // Windower's own layout: an unnamed default profile, nested elements, a duplicate.
        std::fs::write(
            root.join("settings.xml"),
            "<?xml version=\"1.1\" encoding=\"utf-8\" standalone=\"no\"?>\r\n<settings>\r\n  <launcher><branch>stable</branch></launcher>\r\n  <autoload><addon>gearswap</addon></autoload>\r\n  <profile name=\"\">\r\n    <consolekey>Insert</consolekey>\r\n  </profile>\r\n  <profile name=\"Box 2\">\r\n    <width>1280</width>\r\n  </profile>\r\n  <profile name=\"Box 2\" />\r\n</settings>\r\n",
        )
        .unwrap();
        assert_eq!(windower_profiles(&root), ["Box 2"]);
        // Finding: only what a launch can select is listed; a nested or differently cased
        // profile used to be listed and then refused at launch.
        let xml = br#"<settings><x><profile name="Nested"/></x><Profile name="Cased"/><profile name=" Main "/></settings>"#;
        std::fs::write(root.join("settings.xml"), xml).unwrap();
        assert_eq!(windower_profiles(&root), ["Main"]);
        for name in ["Nested", "Cased"] { assert!(super::super::ffxi_settings::windower_profile(Some(xml), name).is_err()); }
        assert!(super::super::ffxi_settings::windower_profile(Some(xml), "Main").is_ok());
        std::fs::remove_file(root.join("settings.xml")).unwrap();
        assert!(windower_profiles(&root).is_empty(), "no settings.xml, no profiles");
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn boot_ini_names_must_stay_inside_config_boot() {
        for good in ["default.ini", "Main Mule.INI", "box-2.ini"] {
            assert!(valid_boot_ini_name(good), "{good}");
        }
        for bad in ["", "notes.txt", r"..\default.ini", "sub/x.ini", "C:x.ini", " default.ini", "a\nb.ini"] {
            assert!(!valid_boot_ini_name(bad), "{bad:?}");
        }
    }
}
