//! SSH hosts for an ADE that imports them from the user's SSH config (Orca). InstaADE writes
//! one block per sandbox to `~/.ssh/instabox/config`, and `~/.ssh/config` includes that file
//! from its first line, before any `Host *`, so the block's options win. InstaADE changes only
//! the blocks it marked and that one `Include` line. Every other line stays as it is, with its
//! own line end, and new blocks go after the last line InstaADE wrote.
//!
//! A block keeps its alias once written: the ADE knows the host by it. A new token of the
//! sandbox rewrites the block's `User`, and deleting the sandbox in InstaADE removes it. The
//! app renews the token of every sandbox with a block before it runs out
//! (`Core::renew_ssh_hosts`).

use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::{Mutex, PoisonError};

use anyhow::{Context, Result, bail};

use crate::api::SshToken;
use crate::ssh::SshTokenStore;

/// The line InstaADE puts first in `~/.ssh/config`.
pub const INCLUDE: &str = "Include ~/.ssh/instabox/config";
/// The file `INCLUDE` names, as it names it.
const HOSTS_ARG: &str = "~/.ssh/instabox/config";
/// Starts each block InstaADE wrote, followed by the sandbox id.
const MARKER: &str = "# instabox-sandbox ";
/// The first line of a new hosts file. It keeps the app's old name: new blocks go right after
/// it, so a file older releases wrote must still match it.
const HEADER: &str =
    "# InstaBox writes the blocks that start with \"# instabox-sandbox\". It keeps all else.";
const ALIAS_PREFIX: &str = "instabox-";
/// The longest alias, before any suffix, so a long sandbox name stays readable.
const MAX_SLUG: usize = 40;
/// The length of the id suffix that tells two sandboxes with the same name apart.
const SHORT_ID: usize = 6;
/// The copy of `~/.ssh/config` made before InstaADE first changes it.
const BACKUP: &str = "config.instabox.bak";

/// One change at a time to the two files, across every flow.
static LOCK: Mutex<()> = Mutex::new(());

/// `~/.ssh/instabox/config` under `home`.
pub fn hosts_path(home: &Path) -> PathBuf {
    home.join(".ssh").join("instabox").join("config")
}

/// `~/.ssh/config` under `home`.
pub fn config_path(home: &Path) -> PathBuf {
    home.join(".ssh").join("config")
}

/// Writes the host of sandbox `id`, named `name`, with the login `t`, and makes sure
/// `~/.ssh/config` includes the hosts file. Returns the host's alias. A sandbox that has a host
/// keeps its alias; a new one gets `instabox-<name>`, with a short id suffix when another
/// sandbox has that alias.
pub fn write_host(home: &Path, id: &str, name: &str, t: &SshToken) -> Result<String> {
    check(id, t)?;
    check_home(home)?;
    let _g = LOCK.lock().unwrap_or_else(PoisonError::into_inner);
    ensure_include(home)?;
    let path = hosts_path(home);
    let mut lines = read_lines(&path)?;
    let blocks = blocks(&lines);
    let sep = separator(&lines);
    match blocks.iter().find(|b| b.id == id) {
        Some(b) => {
            let alias = b.alias.clone();
            replace_block(&mut lines, b, render(id, &alias, t));
            write_hosts(home, &lines)?;
            Ok(alias)
        }
        None => {
            let alias = new_alias(name, id, &blocks);
            if lines.is_empty() {
                lines.push(Line::new(HEADER, sep));
            }
            // After the last line InstaADE wrote, so lines the user added below stay as they
            // are, down to the end of the file.
            let at = match blocks.last() {
                Some(b) => b.end,
                None if lines[0].text == HEADER => 1,
                None => lines.len(),
            };
            // The line before gets an end if it has none, to start a new line after it.
            if lines[at - 1].end.is_empty() {
                lines[at - 1].end = sep;
            }
            let mut new = Vec::new();
            if !lines[at - 1].text.trim().is_empty() {
                new.push(Line::new("", sep));
            }
            new.extend(render(id, &alias, t).iter().map(|l| Line::new(l, sep)));
            lines.splice(at..at, new);
            write_hosts(home, &lines)?;
            Ok(alias)
        }
    }
}

/// Rewrites the login of sandbox `id`'s host with `t`, keeping its alias. False when the
/// sandbox has no host (or there is no home, so no hosts) or the host already has this login.
/// An error only when the hosts file cannot be read or written, or `t` cannot go in it.
pub fn update_login(home: &Path, id: &str, t: &SshToken) -> Result<bool> {
    if !home.is_absolute() {
        return Ok(false);
    }
    let _g = LOCK.lock().unwrap_or_else(PoisonError::into_inner);
    let mut lines = read_lines(&hosts_path(home))?;
    let Some(b) = blocks(&lines).into_iter().find(|b| b.id == id) else {
        return Ok(false);
    };
    check(id, t)?;
    let block = render(id, &b.alias, t);
    if lines[b.start..b.end]
        .iter()
        .map(|l| &l.text)
        .eq(block.iter())
    {
        return Ok(false);
    }
    replace_block(&mut lines, &b, block);
    write_hosts(home, &lines)?;
    Ok(true)
}

/// Removes the host of sandbox `id`. False when it has none.
pub fn remove_host(home: &Path, id: &str) -> Result<bool> {
    remove_where(home, |b| b.id == id).map(|n| n > 0)
}

/// Removes every host InstaADE wrote. Returns how many.
pub fn remove_all(home: &Path) -> Result<usize> {
    remove_where(home, |_| true)
}

fn remove_where(home: &Path, gone: impl Fn(&Block) -> bool) -> Result<usize> {
    check_home(home)?;
    let _g = LOCK.lock().unwrap_or_else(PoisonError::into_inner);
    let mut lines = read_lines(&hosts_path(home))?;
    let doomed: Vec<Block> = blocks(&lines).into_iter().filter(|b| gone(b)).collect();
    if doomed.is_empty() {
        return Ok(0);
    }
    // From the end, so the earlier ranges stay valid.
    for b in doomed.iter().rev() {
        let mut start = b.start;
        // The blank line written before the block goes with it.
        if start > 0 && lines[start - 1].text.trim().is_empty() {
            start -= 1;
        }
        lines.drain(start..b.end);
    }
    write_hosts(home, &lines)?;
    Ok(doomed.len())
}

/// The aliases of the hosts InstaADE wrote, by sandbox id.
pub fn aliases(home: &Path) -> Result<Vec<(String, String)>> {
    check_home(home)?;
    let _g = LOCK.lock().unwrap_or_else(PoisonError::into_inner);
    let lines = read_lines(&hosts_path(home))?;
    Ok(blocks(&lines)
        .into_iter()
        .map(|b| (b.id, b.alias))
        .collect())
}

/// A block InstaADE wrote: its lines `start..end`, the sandbox id and the alias.
#[derive(Debug, Clone, PartialEq, Eq)]
struct Block {
    start: usize,
    end: usize,
    id: String,
    alias: String,
}

/// A line of the hosts file and the end it had: `\n`, `\r\n`, or none for a last line without
/// one. A line InstaADE did not write is written back with its own end.
#[derive(Debug, Clone, PartialEq, Eq)]
struct Line {
    text: String,
    end: &'static str,
}

impl Line {
    fn new(text: &str, end: &'static str) -> Line {
        Line {
            text: text.to_string(),
            end,
        }
    }
}

/// The line end most lines of the file have, for the lines InstaADE writes: `\r\n` when most
/// do, else `\n`.
fn separator(lines: &[Line]) -> &'static str {
    let crlf = lines.iter().filter(|l| l.end == "\r\n").count();
    let lf = lines.iter().filter(|l| l.end == "\n").count();
    if crlf > lf { "\r\n" } else { "\n" }
}

/// Puts `block` in place of the block `b`. Each line keeps the end the line in its place had,
/// so a new token changes only that text.
fn replace_block(lines: &mut Vec<Line>, b: &Block, block: Vec<String>) {
    let sep = separator(lines);
    let ends: Vec<&'static str> = lines[b.start..b.end].iter().map(|l| l.end).collect();
    let new: Vec<Line> = block
        .iter()
        .enumerate()
        .map(|(i, text)| Line::new(text, ends.get(i).copied().unwrap_or(sep)))
        .collect();
    lines.splice(b.start..b.end, new);
}

/// The blocks InstaADE wrote: a marker line, then its `Host` line, then the indented lines
/// under it. The first line that is blank or not indented ends a block, so a block the user
/// added right after one of these stays theirs.
fn blocks(lines: &[Line]) -> Vec<Block> {
    let mut out = Vec::new();
    let mut i = 0;
    while i < lines.len() {
        let id = lines[i].text.trim_end().strip_prefix(MARKER).map(str::trim);
        let alias = lines
            .get(i + 1)
            .and_then(|l| l.text.trim_end().strip_prefix("Host "))
            .map(str::trim);
        let (Some(id), Some(alias)) = (id, alias) else {
            i += 1;
            continue;
        };
        if id.is_empty() || alias.is_empty() || alias.contains(char::is_whitespace) {
            i += 1;
            continue;
        }
        let mut end = i + 2;
        while end < lines.len()
            && lines[end].text.starts_with([' ', '\t'])
            && !lines[end].text.trim().is_empty()
        {
            end += 1;
        }
        out.push(Block {
            start: i,
            end,
            id: id.to_string(),
            alias: alias.to_string(),
        });
        i = end;
    }
    out
}

/// A block's lines. `ForwardAgent no` because the gateway refuses agent forwarding, and a
/// refused request fails the ADE's whole connection.
fn render(id: &str, alias: &str, t: &SshToken) -> Vec<String> {
    vec![
        format!("{MARKER}{id}"),
        format!("Host {alias}"),
        format!("  HostName {}", t.host),
        format!("  Port {}", t.port),
        format!("  User {}", t.token),
        "  StrictHostKeyChecking accept-new".to_string(),
        "  ForwardAgent no".to_string(),
    ]
}

/// Refuses a home that is not a full path, such as the empty one of a user without a home:
/// the files would land in whatever directory the app runs in.
fn check_home(home: &Path) -> Result<()> {
    if !home.is_absolute() {
        bail!("No home directory for the SSH config");
    }
    Ok(())
}

/// Refuses a value that could break out of its line or option in an SSH config.
fn check(id: &str, t: &SshToken) -> Result<()> {
    let word = |s: &str, extra: &[char]| {
        !s.is_empty()
            && s.chars()
                .all(|c| c.is_ascii_alphanumeric() || extra.contains(&c))
    };
    if !word(id, &['-', '_']) {
        bail!("The sandbox id {id:?} cannot go into an SSH config");
    }
    if !word(&t.host, &['-', '.']) || t.port == 0 || !word(&t.token, &['-', '_', '.']) {
        bail!("The SSH login of the sandbox cannot go into an SSH config");
    }
    Ok(())
}

/// Lowercase letters and digits, runs of anything else as one `-`, at most `MAX_SLUG` long.
/// Lowercase because ssh matches a host alias against the name lowercased.
fn slug(s: &str) -> String {
    let mut out = String::new();
    for c in s.chars() {
        if c.is_ascii_alphanumeric() {
            out.push(c.to_ascii_lowercase());
        } else if !out.is_empty() && !out.ends_with('-') {
            out.push('-');
        }
    }
    out.truncate(MAX_SLUG);
    out.trim_end_matches('-').to_string()
}

/// The alias for a new host: `instabox-<name>`, then with the end of the id when another host
/// has that alias, then with the whole id.
fn new_alias(name: &str, id: &str, blocks: &[Block]) -> String {
    let id_slug = slug(id);
    let name_slug = slug(name);
    let base = format!(
        "{ALIAS_PREFIX}{}",
        if name_slug.is_empty() {
            &id_slug
        } else {
            &name_slug
        }
    );
    let taken = |a: &str| blocks.iter().any(|b| b.alias == a);
    if !taken(&base) {
        return base;
    }
    let short = &id_slug[id_slug.len().saturating_sub(SHORT_ID)..];
    [format!("{base}-{short}"), format!("{base}-{id_slug}")]
        .into_iter()
        .find(|a| !taken(a))
        .unwrap_or_else(|| format!("{base}-{}", uuid::Uuid::new_v4().simple()))
}

/// The file's lines, each with the end it had; none when it does not exist.
fn read_lines(path: &Path) -> Result<Vec<Line>> {
    match fs::read_to_string(path) {
        Ok(s) => Ok(split_lines(&s)),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(Vec::new()),
        Err(e) => Err(e).with_context(|| format!("could not read {}", path.display())),
    }
}

/// `text` as lines with their ends, so joining them gives `text` back byte for byte.
fn split_lines(text: &str) -> Vec<Line> {
    text.split_inclusive('\n')
        .map(|piece| match piece.strip_suffix("\r\n") {
            Some(t) => Line::new(t, "\r\n"),
            None => match piece.strip_suffix('\n') {
                Some(t) => Line::new(t, "\n"),
                None => Line::new(piece, ""),
            },
        })
        .collect()
}

/// Writes the hosts file, only readable by the user, in a directory only they can open. Each
/// line goes back with its own end.
fn write_hosts(home: &Path, lines: &[Line]) -> Result<()> {
    let path = hosts_path(home);
    let dir = path.parent().context("no directory for the SSH hosts")?;
    create_private_dirs(dir)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(dir, fs::Permissions::from_mode(0o700))
            .with_context(|| format!("could not protect {}", dir.display()))?;
    }
    let text: String = lines
        .iter()
        .flat_map(|l| [l.text.as_str(), l.end])
        .collect();
    replace(&path, text.as_bytes(), None)
}

/// Creates `dir` and its missing parents, such as `~/.ssh`, only the user can open (0700 on
/// Unix). A directory that exists keeps its permissions.
fn create_private_dirs(dir: &Path) -> Result<()> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::DirBuilderExt;
        fs::DirBuilder::new()
            .recursive(true)
            .mode(0o700)
            .create(dir)
            .with_context(|| format!("could not create {}", dir.display()))
    }
    #[cfg(not(unix))]
    fs::create_dir_all(dir).with_context(|| format!("could not create {}", dir.display()))
}

/// Replaces `path` with `bytes` through a temporary file in the same directory and a rename,
/// so a reader sees the old file or the new one, never part of it. The new file gets `mode`,
/// else 0600 on Unix.
fn replace(path: &Path, bytes: &[u8], mode: Option<fs::Permissions>) -> Result<()> {
    let dir = path.parent().context("no directory")?;
    let name = path.file_name().context("no file name")?.to_string_lossy();
    let tmp = dir.join(format!(
        ".{name}.instabox-{}",
        uuid::Uuid::new_v4().simple()
    ));
    let written = (|| -> std::io::Result<()> {
        let mut options = fs::OpenOptions::new();
        options.write(true).create_new(true);
        #[cfg(unix)]
        std::os::unix::fs::OpenOptionsExt::mode(&mut options, 0o600);
        let mut f = options.open(&tmp)?;
        f.write_all(bytes)?;
        f.sync_all()?;
        drop(f);
        if let Some(m) = mode {
            fs::set_permissions(&tmp, m)?;
        }
        fs::rename(&tmp, path)
    })();
    if let Err(e) = written {
        let _ = fs::remove_file(&tmp);
        return Err(e).with_context(|| format!("could not write {}", path.display()));
    }
    Ok(())
}

/// Puts `INCLUDE` first in `~/.ssh/config`. A missing file is created with that one line. A
/// file that already includes the hosts file before its first `Host` or `Match` is left as it
/// is. Else the line goes on top, after a copy of the file to `~/.ssh/config.instabox.bak`
/// (made once, never overwritten). A `~/.ssh/config` that is a link is changed where it points,
/// and the file keeps its permissions and line ends.
fn ensure_include(home: &Path) -> Result<()> {
    let link = config_path(home);
    let path = match fs::symlink_metadata(&link) {
        Ok(m) if m.file_type().is_symlink() => fs::canonicalize(&link)
            .with_context(|| format!("{} links to a file that does not exist", link.display()))?,
        _ => link.clone(),
    };
    let old = match fs::read(&path) {
        Ok(b) => Some(b),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => None,
        Err(e) => return Err(e).with_context(|| format!("could not read {}", path.display())),
    };
    let Some(old) = old else {
        create_private_dirs(path.parent().context("no directory for the SSH config")?)?;
        return replace(&path, format!("{INCLUDE}\n").as_bytes(), None);
    };
    let text = String::from_utf8_lossy(&old);
    if includes_hosts(&text, home) {
        return Ok(());
    }
    let backup = home.join(".ssh").join(BACKUP);
    if !backup.exists() {
        let mut options = fs::OpenOptions::new();
        options.write(true).create_new(true);
        #[cfg(unix)]
        std::os::unix::fs::OpenOptionsExt::mode(&mut options, 0o600);
        let mut f = options
            .open(&backup)
            .with_context(|| format!("could not back up to {}", backup.display()))?;
        f.write_all(&old)?;
        f.sync_all()?;
    }
    const BOM: &[u8] = b"\xEF\xBB\xBF";
    let (bom, rest) = match old.strip_prefix(BOM) {
        Some(rest) => (BOM, rest),
        None => (&[][..], &old[..]),
    };
    let eol: &[u8] = if old.windows(2).any(|w| w == b"\r\n") {
        b"\r\n"
    } else {
        b"\n"
    };
    let mut new = Vec::with_capacity(old.len() + INCLUDE.len() + 3);
    new.extend_from_slice(bom);
    new.extend_from_slice(INCLUDE.as_bytes());
    new.extend_from_slice(eol);
    new.extend_from_slice(rest);
    let mode = fs::metadata(&path).ok().map(|m| m.permissions());
    replace(&path, &new, mode)
}

/// True when `text` (an SSH config) includes the hosts file before its first `Host` or `Match`
/// line, where the include applies to every host.
fn includes_hosts(text: &str, home: &Path) -> bool {
    let absolute = hosts_path(home);
    for line in text.lines() {
        let line = line.trim_start_matches('\u{feff}').trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        let (key, args) = split_option(line);
        match key.to_ascii_lowercase().as_str() {
            "host" | "match" => return false,
            "include" => {
                let named = args.split_whitespace().any(|a| {
                    let a = a.trim_matches('"');
                    a == HOSTS_ARG || Path::new(a) == absolute
                });
                if named {
                    return true;
                }
            }
            _ => {}
        }
    }
    false
}

/// The keyword and the rest of a config line: `Key value` or `Key=value`.
fn split_option(line: &str) -> (&str, &str) {
    let end = line
        .find(|c: char| c.is_whitespace() || c == '=')
        .unwrap_or(line.len());
    let rest = line[end..].trim_start();
    let rest = rest.strip_prefix('=').unwrap_or(rest).trim_start();
    (&line[..end], rest)
}

/// A token store that keeps the hosts in step: a new token of a sandbox that has a host
/// becomes the host's `User`, and clearing every token (an account boundary) removes every
/// host InstaADE wrote.
pub struct SyncedStore<S> {
    inner: S,
    home: PathBuf,
}

impl<S> SyncedStore<S> {
    pub fn new(inner: S, home: PathBuf) -> SyncedStore<S> {
        SyncedStore { inner, home }
    }
}

impl<S: SshTokenStore> SshTokenStore for SyncedStore<S> {
    fn load(&self, id: &str) -> std::result::Result<Option<SshToken>, String> {
        self.inner.load(id)
    }

    /// A host that cannot take the new token keeps the stored token behind too, so the next
    /// renewal makes another and tries the host again. The token in hand still works.
    fn save(&self, id: &str, token: &SshToken) -> std::result::Result<(), String> {
        update_login(&self.home, id, token)
            .map_err(|e| format!("could not update the SSH host of sandbox {id}: {e:#}"))?;
        self.inner.save(id, token)
    }

    fn clear_all(&self) -> std::result::Result<(), String> {
        let hosts = remove_all(&self.home);
        self.inner.clear_all()?;
        hosts.map(drop).map_err(|e| format!("{e:#}"))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ssh::MemorySshStore;

    fn tok(token: &str) -> SshToken {
        SshToken {
            host: "ssh.instabox.sh".into(),
            port: 2222,
            token: token.into(),
            command: format!("ssh -p 2222 {token}@ssh.instabox.sh"),
            expires_at: None,
            host_key_fingerprint: None,
        }
    }

    fn read(p: &Path) -> String {
        fs::read_to_string(p).unwrap()
    }

    const BLOCK_M1: &str = "# instabox-sandbox m1
Host instabox-dev-1
  HostName ssh.instabox.sh
  Port 2222
  User ibssh_a
  StrictHostKeyChecking accept-new
  ForwardAgent no
";

    #[test]
    fn a_new_ssh_config_gets_the_include_and_the_host() {
        let home = tempfile::tempdir().unwrap();
        let alias = write_host(home.path(), "m1", "dev 1", &tok("ibssh_a")).unwrap();
        assert_eq!(alias, "instabox-dev-1");
        assert_eq!(read(&config_path(home.path())), format!("{INCLUDE}\n"));
        assert_eq!(
            read(&hosts_path(home.path())),
            format!("{HEADER}\n\n{BLOCK_M1}")
        );
        assert!(
            !home.path().join(".ssh").join(BACKUP).exists(),
            "nothing to back up"
        );
    }

    #[test]
    fn the_include_goes_above_host_star_and_no_other_line_changes() {
        let home = tempfile::tempdir().unwrap();
        let cfg = config_path(home.path());
        fs::create_dir_all(cfg.parent().unwrap()).unwrap();
        let before = "# mine\nHost *\n  ForwardAgent yes\n\nHost work\n  HostName w.example\n";
        fs::write(&cfg, before).unwrap();
        write_host(home.path(), "m1", "dev 1", &tok("ibssh_a")).unwrap();
        assert_eq!(read(&cfg), format!("{INCLUDE}\n{before}"));
        assert_eq!(
            read(&home.path().join(".ssh").join(BACKUP)),
            before,
            "the original is kept"
        );
        // A second sandbox, and the same one again: the config is left alone.
        write_host(home.path(), "m2", "web", &tok("ibssh_b")).unwrap();
        write_host(home.path(), "m1", "dev 1", &tok("ibssh_c")).unwrap();
        assert_eq!(read(&cfg), format!("{INCLUDE}\n{before}"));
    }

    #[test]
    fn a_config_without_host_star_gets_the_include_on_top() {
        let home = tempfile::tempdir().unwrap();
        let cfg = config_path(home.path());
        fs::create_dir_all(cfg.parent().unwrap()).unwrap();
        fs::write(&cfg, "Host work\n  User me").unwrap();
        write_host(home.path(), "m1", "dev 1", &tok("ibssh_a")).unwrap();
        assert_eq!(read(&cfg), format!("{INCLUDE}\nHost work\n  User me"));
    }

    #[test]
    fn an_include_already_before_any_host_is_kept_as_it_is() {
        for before in [
            format!("{INCLUDE}\nHost *\n  User me\n"),
            "# top\ninclude ~/.ssh/other ~/.ssh/instabox/config\nHost *\n".to_string(),
            "Include=\"~/.ssh/instabox/config\"\n".to_string(),
        ] {
            let home = tempfile::tempdir().unwrap();
            let cfg = config_path(home.path());
            fs::create_dir_all(cfg.parent().unwrap()).unwrap();
            fs::write(&cfg, &before).unwrap();
            write_host(home.path(), "m1", "dev 1", &tok("ibssh_a")).unwrap();
            assert_eq!(read(&cfg), before);
            assert!(!home.path().join(".ssh").join(BACKUP).exists(), "{before}");
        }
    }

    #[test]
    fn an_include_inside_a_host_block_does_not_count() {
        let home = tempfile::tempdir().unwrap();
        let cfg = config_path(home.path());
        fs::create_dir_all(cfg.parent().unwrap()).unwrap();
        let before = format!("Host work\n  {INCLUDE}\n");
        fs::write(&cfg, &before).unwrap();
        write_host(home.path(), "m1", "dev 1", &tok("ibssh_a")).unwrap();
        assert_eq!(read(&cfg), format!("{INCLUDE}\n{before}"));
    }

    #[test]
    fn the_backup_is_made_once() {
        let home = tempfile::tempdir().unwrap();
        let cfg = config_path(home.path());
        fs::create_dir_all(cfg.parent().unwrap()).unwrap();
        fs::write(&cfg, "Host a\n").unwrap();
        write_host(home.path(), "m1", "dev 1", &tok("ibssh_a")).unwrap();
        // The user takes the line out; the next write puts it back, keeping the first backup.
        fs::write(&cfg, "Host b\n").unwrap();
        write_host(home.path(), "m1", "dev 1", &tok("ibssh_a")).unwrap();
        assert_eq!(read(&cfg), format!("{INCLUDE}\nHost b\n"));
        assert_eq!(read(&home.path().join(".ssh").join(BACKUP)), "Host a\n");
    }

    #[test]
    fn crlf_line_ends_and_a_bom_are_kept() {
        let home = tempfile::tempdir().unwrap();
        let cfg = config_path(home.path());
        fs::create_dir_all(cfg.parent().unwrap()).unwrap();
        fs::write(&cfg, b"\xEF\xBB\xBFHost a\r\n  User me\r\n").unwrap();
        write_host(home.path(), "m1", "dev 1", &tok("ibssh_a")).unwrap();
        assert_eq!(
            fs::read(&cfg).unwrap(),
            format!("\u{feff}{INCLUDE}\r\nHost a\r\n  User me\r\n").into_bytes()
        );
    }

    #[cfg(unix)]
    #[test]
    fn a_linked_config_is_changed_where_it_points() {
        let home = tempfile::tempdir().unwrap();
        let dotfiles = tempfile::tempdir().unwrap();
        let real = dotfiles.path().join("ssh_config");
        fs::write(&real, "Host a\n").unwrap();
        let cfg = config_path(home.path());
        fs::create_dir_all(cfg.parent().unwrap()).unwrap();
        std::os::unix::fs::symlink(&real, &cfg).unwrap();
        write_host(home.path(), "m1", "dev 1", &tok("ibssh_a")).unwrap();
        assert!(fs::symlink_metadata(&cfg).unwrap().file_type().is_symlink());
        assert_eq!(read(&real), format!("{INCLUDE}\nHost a\n"));
        // A link to nothing is an error, and nothing is written.
        let broken = tempfile::tempdir().unwrap();
        let cfg = config_path(broken.path());
        fs::create_dir_all(cfg.parent().unwrap()).unwrap();
        std::os::unix::fs::symlink(dotfiles.path().join("gone"), &cfg).unwrap();
        assert!(write_host(broken.path(), "m1", "dev 1", &tok("ibssh_a")).is_err());
        assert!(!hosts_path(broken.path()).exists());
    }

    #[cfg(unix)]
    #[test]
    fn the_hosts_file_and_its_directory_are_private() {
        use std::os::unix::fs::PermissionsExt;
        let home = tempfile::tempdir().unwrap();
        write_host(home.path(), "m1", "dev 1", &tok("ibssh_a")).unwrap();
        let mode = |p: &Path| fs::metadata(p).unwrap().permissions().mode() & 0o777;
        let hosts = hosts_path(home.path());
        assert_eq!(mode(&hosts), 0o600);
        assert_eq!(mode(hosts.parent().unwrap()), 0o700);
        assert_eq!(mode(&home.path().join(".ssh")), 0o700, "a new ~/.ssh");
        assert_eq!(mode(&config_path(home.path())), 0o600, "a new config");
        // An existing config keeps its permissions; the backup is private.
        let home = tempfile::tempdir().unwrap();
        let cfg = config_path(home.path());
        fs::create_dir_all(cfg.parent().unwrap()).unwrap();
        fs::write(&cfg, "Host a\n").unwrap();
        fs::set_permissions(&cfg, fs::Permissions::from_mode(0o644)).unwrap();
        write_host(home.path(), "m1", "dev 1", &tok("ibssh_a")).unwrap();
        assert_eq!(mode(&cfg), 0o644);
        assert_eq!(mode(&home.path().join(".ssh").join(BACKUP)), 0o600);
        // No temporary file is left behind.
        let left: Vec<_> = fs::read_dir(home.path().join(".ssh"))
            .unwrap()
            .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
            .filter(|n| n.contains(".instabox-"))
            .collect();
        assert!(left.is_empty(), "{left:?}");
    }

    #[test]
    fn two_sandboxes_with_one_name_get_a_short_id_suffix_and_aliases_stay() {
        let home = tempfile::tempdir().unwrap();
        let a = write_host(home.path(), "mch_01ABCDEF", "Dev Box!", &tok("ibssh_a")).unwrap();
        let b = write_host(home.path(), "mch_02XYZ123", "dev box", &tok("ibssh_b")).unwrap();
        assert_eq!(a, "instabox-dev-box");
        assert_eq!(b, "instabox-dev-box-xyz123");
        // A rename on the server and a new token keep the alias: the ADE knows the host by it.
        assert_eq!(
            write_host(home.path(), "mch_02XYZ123", "renamed", &tok("ibssh_c")).unwrap(),
            b
        );
        // A name with nothing usable falls back to the id.
        assert_eq!(
            write_host(home.path(), "m3", "???", &tok("ibssh_d")).unwrap(),
            "instabox-m3"
        );
        let found = aliases(home.path()).unwrap();
        assert_eq!(
            found,
            [
                ("mch_01ABCDEF".to_string(), a),
                ("mch_02XYZ123".to_string(), b),
                ("m3".to_string(), "instabox-m3".to_string()),
            ]
        );
    }

    #[test]
    fn slugs_are_lowercase_short_and_safe_for_orca() {
        assert_eq!(slug("My  Box_2 (copy)"), "my-box-2-copy");
        assert_eq!(slug("--x--"), "x");
        assert_eq!(slug("ü*?!"), "");
        assert_eq!(slug(&"a".repeat(60)).len(), MAX_SLUG);
        assert_eq!(slug(&format!("{}-tail", "a".repeat(39))), "a".repeat(39));
    }

    #[test]
    fn a_new_token_rewrites_only_the_user() {
        let home = tempfile::tempdir().unwrap();
        write_host(home.path(), "m1", "dev 1", &tok("ibssh_a")).unwrap();
        write_host(home.path(), "m2", "web", &tok("ibssh_b")).unwrap();
        let before = read(&hosts_path(home.path()));
        assert!(
            !update_login(home.path(), "m1", &tok("ibssh_a")).unwrap(),
            "same token"
        );
        assert!(
            !update_login(home.path(), "m9", &tok("ibssh_z")).unwrap(),
            "no host"
        );
        assert_eq!(read(&hosts_path(home.path())), before);
        assert!(update_login(home.path(), "m1", &tok("ibssh_new")).unwrap());
        assert_eq!(
            read(&hosts_path(home.path())),
            before.replace("User ibssh_a", "User ibssh_new")
        );
    }

    #[test]
    fn removing_a_host_keeps_every_line_it_did_not_write() {
        let home = tempfile::tempdir().unwrap();
        write_host(home.path(), "m1", "dev 1", &tok("ibssh_a")).unwrap();
        // The user adds a block right under InstaADE's, and one of their own lower down.
        let path = hosts_path(home.path());
        let mut text = read(&path);
        text.push_str("Host mine\n  User me\n\n# keep me\n");
        fs::write(&path, &text).unwrap();
        write_host(home.path(), "m2", "web", &tok("ibssh_b")).unwrap();
        assert!(remove_host(home.path(), "m1").unwrap());
        assert!(!remove_host(home.path(), "m1").unwrap(), "already gone");
        let left = read(&path);
        assert!(
            !left.contains("m1") && !left.contains("instabox-dev-1"),
            "{left}"
        );
        assert!(
            left.contains("Host mine\n  User me\n\n# keep me\n"),
            "{left}"
        );
        assert!(left.contains("Host instabox-web\n"), "{left}");
        assert_eq!(remove_all(home.path()).unwrap(), 1);
        assert_eq!(
            read(&path),
            format!("{HEADER}\nHost mine\n  User me\n\n# keep me\n")
        );
        assert_eq!(remove_all(home.path()).unwrap(), 0);
    }

    /// Runs a token update, an add and a removal on a hosts file that holds InstaADE's `m1`
    /// followed by `user`, and checks after each that `user` is still its last bytes.
    fn user_part_survives(user: &str) {
        let home = tempfile::tempdir().unwrap();
        write_host(home.path(), "m1", "dev 1", &tok("ibssh_a")).unwrap();
        let path = hosts_path(home.path());
        let mut bytes = fs::read(&path).unwrap();
        bytes.extend_from_slice(user.as_bytes());
        fs::write(&path, &bytes).unwrap();
        let kept = |step: &str| {
            let now = fs::read(&path).unwrap();
            assert!(
                now.ends_with(user.as_bytes()),
                "{step}: {:?}",
                String::from_utf8_lossy(&now)
            );
        };
        assert!(update_login(home.path(), "m1", &tok("ibssh_new")).unwrap());
        kept("token update");
        write_host(home.path(), "m2", "web", &tok("ibssh_b")).unwrap();
        kept("add");
        assert!(read(&path).contains("Host instabox-web"));
        assert!(remove_host(home.path(), "m1").unwrap());
        kept("removal");
        assert_eq!(remove_all(home.path()).unwrap(), 1);
        kept("removal of all");
        assert!(!read(&path).contains("instabox-sandbox m"));
    }

    #[test]
    fn a_user_block_with_crlf_is_kept_byte_for_byte() {
        user_part_survives("Host mine\r\n  User me\r\n");
    }

    #[test]
    fn a_user_part_without_a_final_newline_keeps_it() {
        user_part_survives("\nHost mine\n  User me");
        user_part_survives("Host mine\r\n  User me");
    }

    #[test]
    fn mixed_line_ends_are_kept() {
        user_part_survives("# a\r\nHost mixed\n  User x\r\n\n# end");
    }

    #[test]
    fn instabox_lines_take_the_line_end_most_lines_have() {
        let home = tempfile::tempdir().unwrap();
        let path = hosts_path(home.path());
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(&path, "Host a\r\n  User a\r\n").unwrap();
        write_host(home.path(), "m1", "dev 1", &tok("ibssh_a")).unwrap();
        let text = read(&path);
        assert!(
            text.starts_with("Host a\r\n  User a\r\n\r\n# instabox-sandbox m1\r\n"),
            "{text:?}"
        );
        assert!(!text.replace("\r\n", "").contains('\n'), "{text:?}");
        // A token update keeps the ends the block has.
        update_login(home.path(), "m1", &tok("ibssh_new")).unwrap();
        assert!(read(&path).contains("  User ibssh_new\r\n"));
    }

    #[test]
    fn a_file_of_only_user_lines_without_a_final_newline_gets_one_to_add_after() {
        // The one change to a user line: a new line needs the line before it to end.
        let home = tempfile::tempdir().unwrap();
        let path = hosts_path(home.path());
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(&path, "Host a\r\n  User a").unwrap();
        write_host(home.path(), "m1", "dev 1", &tok("ibssh_a")).unwrap();
        assert!(read(&path).starts_with("Host a\r\n  User a\r\n\r\n# instabox-sandbox m1\r\n"));
    }

    #[test]
    fn lines_split_and_join_back_byte_for_byte() {
        for text in ["", "a", "a\n", "a\r\nb", "a\r\n\r\nb\n", "a\rb\n", "\n\n"] {
            let joined: String = split_lines(text)
                .iter()
                .flat_map(|l| [l.text.as_str(), l.end])
                .collect();
            assert_eq!(joined, text);
        }
    }

    #[test]
    fn a_marker_without_its_host_line_is_not_a_block() {
        let lines = split_lines("# instabox-sandbox m1\n\nHost x\n# instabox-sandbox");
        assert!(blocks(&lines).is_empty());
    }

    #[test]
    fn a_login_that_could_break_the_config_is_refused() {
        let home = tempfile::tempdir().unwrap();
        for (id, t) in [
            ("m1", tok("ibssh_a\n  ProxyCommand calc")),
            ("m1", tok("ibssh a")),
            ("m1\nHost *", tok("ibssh_a")),
            ("", tok("ibssh_a")),
            (
                "m1",
                SshToken {
                    host: "ssh.instabox.sh -oProxyCommand=x".into(),
                    ..tok("ibssh_a")
                },
            ),
            (
                "m1",
                SshToken {
                    port: 0,
                    ..tok("ibssh_a")
                },
            ),
        ] {
            assert!(
                write_host(home.path(), id, "dev", &t).is_err(),
                "{id:?} {t:?}"
            );
        }
        assert!(!hosts_path(home.path()).exists());
        assert!(!config_path(home.path()).exists());
    }

    #[test]
    fn no_home_writes_nothing() {
        assert!(write_host(Path::new(""), "m1", "dev", &tok("ibssh_a")).is_err());
        // No home, no hosts: a new token has no host to update, and that is no failure.
        assert!(!update_login(Path::new(".ssh"), "m1", &tok("ibssh_a")).unwrap());
        assert!(remove_all(Path::new("")).is_err());
    }

    #[test]
    fn a_failed_host_update_keeps_the_stored_token_behind() {
        let home = tempfile::tempdir().unwrap();
        let store = SyncedStore::new(MemorySshStore::default(), home.path().to_path_buf());
        write_host(home.path(), "m1", "dev 1", &tok("ibssh_a")).unwrap();
        store.save("m1", &tok("ibssh_a")).unwrap();
        // The hosts file cannot be read for a while: it is a folder.
        let path = hosts_path(home.path());
        let aside = path.with_extension("aside");
        fs::rename(&path, &aside).unwrap();
        fs::create_dir(&path).unwrap();
        let e = store.save("m1", &tok("ibssh_b")).unwrap_err();
        assert!(
            e.contains("could not update the SSH host of sandbox m1"),
            "{e}"
        );
        assert_eq!(
            store.load("m1").unwrap().unwrap().token,
            "ibssh_a",
            "not advanced"
        );
        // A token the host cannot hold is refused the same way.
        fs::remove_dir(&path).unwrap();
        fs::rename(&aside, &path).unwrap();
        assert!(store.save("m1", &tok("ibssh_b c")).is_err());
        assert_eq!(store.load("m1").unwrap().unwrap().token, "ibssh_a");
        // Once the file is back, the next token goes into both.
        store.save("m1", &tok("ibssh_b")).unwrap();
        assert_eq!(store.load("m1").unwrap().unwrap().token, "ibssh_b");
        assert!(read(&path).contains("  User ibssh_b\n"));
        // A sandbox with no host saves any token: there is no host to keep in step.
        store.save("m2", &tok("ibssh_x y")).unwrap();
    }

    #[test]
    fn options_split_on_space_or_equals() {
        assert_eq!(split_option("Include ~/a"), ("Include", "~/a"));
        assert_eq!(split_option("Include=~/a"), ("Include", "~/a"));
        assert_eq!(split_option("Include = ~/a b"), ("Include", "~/a b"));
        assert_eq!(split_option("Host"), ("Host", ""));
    }

    #[test]
    fn the_synced_store_moves_the_host_with_the_token_and_clears_it_on_sign_out() {
        let home = tempfile::tempdir().unwrap();
        let store = SyncedStore::new(MemorySshStore::default(), home.path().to_path_buf());
        write_host(home.path(), "m1", "dev 1", &tok("ibssh_a")).unwrap();
        store.save("m1", &tok("ibssh_new")).unwrap();
        assert_eq!(store.load("m1").unwrap().unwrap().token, "ibssh_new");
        assert!(read(&hosts_path(home.path())).contains("  User ibssh_new\n"));
        // A token of a sandbox without a host writes no host.
        store.save("m2", &tok("ibssh_b")).unwrap();
        assert_eq!(aliases(home.path()).unwrap().len(), 1);
        store.clear_all().unwrap();
        assert_eq!(store.load("m1").unwrap(), None);
        assert!(aliases(home.path()).unwrap().is_empty());
    }

    #[test]
    fn the_synced_store_with_no_hosts_file_touches_no_file() {
        let home = tempfile::tempdir().unwrap();
        let store = SyncedStore::new(MemorySshStore::default(), home.path().to_path_buf());
        store.save("m1", &tok("ibssh_a")).unwrap();
        store.clear_all().unwrap();
        assert!(!home.path().join(".ssh").exists());
    }
}
