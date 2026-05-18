use clap::Parser;
use crossterm::{
    cursor::{Hide, MoveToColumn, MoveUp, Show},
    execute, queue,
    style::{Color, Print},
    terminal::{Clear, ClearType},
};
use dashmap::DashMap;
use jwalk::WalkDir;
use std::collections::BTreeMap;
use std::ffi::OsString;
use std::io::{stdout, IsTerminal, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::Arc;
use std::thread;
use std::time::Duration;

#[derive(Parser, Debug)]
#[command(
    name = "topfs",
    version,
    about = "Live top-N biggest filesystem entries with tree display"
)]
struct Cli {
    /// Number of top entries to display
    #[arg(short = 'n', long = "count", default_value_t = 20)]
    count: usize,

    /// Path to scan (local, hdfs://host:port/path, or abfs://container@account/path)
    #[arg(default_value = ".")]
    path: String,

    /// Refresh interval in milliseconds
    #[arg(short = 'r', long = "refresh-ms", default_value_t = 100)]
    refresh_ms: u64,

    /// Use apparent size instead of disk usage
    #[arg(short = 'a', long = "apparent-size", default_value_t = false)]
    apparent_size: bool,

    /// Filter to files modified in the last N days (excludes older files from accumulation)
    #[arg(short = 'd', long = "days")]
    days: Option<u64>,    

    /// Send results to a Slack webhook URL (disables real-time display).
    /// If URL is empty, outputs Slack-compatible format to stdout.
    #[arg(long = "slack", num_args = 0..=1, default_missing_value = "")]
    slack: Option<String>,

    /// Optional message to include as header in Slack output
    #[arg(short = 'm', long = "message")]
    message: Option<String>,
}

#[derive(Clone, Debug)]
struct Entry {
    size: u64,
    is_dir: bool,
    /// Last modification time as "YYYY-MM-DD HH:MM" string (populated at end for top entries)
    mtime: Option<String>,
    /// Number of direct children (populated at end for top entries)
    child_count: Option<u64>,
}

impl Default for Entry {
    fn default() -> Self {
        Self {
            size: 0,
            is_dir: false,
            mtime: None,
            child_count: None,
        }
    }
}

type SharedEntryMap = Arc<DashMap<PathBuf, Entry>>;

#[derive(Default, Debug)]
struct TreeNode {
    size: u64,
    is_dir: bool,
    child_count: Option<u64>,
    mtime: Option<String>,
    in_top: bool,
    sort_key: u64,
    children: BTreeMap<OsString, TreeNode>,
}

fn human_size(bytes: u64) -> String {
    const KIB: f64 = 1024.0;
    const UNITS: [&str; 6] = ["B  ", "KiB", "MiB", "GiB", "TiB", "PiB"];
    let mut v = bytes as f64;
    let mut u = 0;
    while v >= KIB && u < UNITS.len() - 1 {
        v /= KIB;
        u += 1;
    }
    if u == 0 {
        format!("{:>6} {}", bytes, UNITS[0])
    } else {
        format!("{:>6.1} {}", v, UNITS[u])
    }
}

fn size_color(bytes: u64) -> Color {
    const GIB: u64 = 1 << 30;
    const MIB: u64 = 1 << 20;
    const KIB: u64 = 1 << 10;
    if bytes >= 10 * GIB {
        Color::Red
    } else if bytes >= GIB {
        Color::Magenta
    } else if bytes >= 100 * MIB {
        Color::Yellow
    } else if bytes >= MIB {
        Color::Green
    } else if bytes >= KIB {
        Color::Cyan
    } else {
        Color::DarkGrey
    }
}

fn ansi(s: &str, color: Color, bold: bool) -> String {
    let code = match color {
        Color::Red => "31",
        Color::Green => "32",
        Color::Yellow => "33",
        Color::Blue => "34",
        Color::Magenta => "35",
        Color::Cyan => "36",
        Color::DarkGrey => "90",
        _ => "37",
    };
    if bold {
        format!("\x1b[1;{}m{}\x1b[0m", code, s)
    } else {
        format!("\x1b[{}m{}\x1b[0m", code, s)
    }
}

fn insert_path(node: &mut TreeNode, components: &[OsString], info: &Entry) {
    if components.is_empty() {
        node.size = info.size;
        node.is_dir = info.is_dir;
        node.child_count = info.child_count;
        node.mtime = info.mtime.clone();
        node.in_top = true;
        return;
    }
    let head = components[0].clone();
    let child = node.children.entry(head).or_default();
    insert_path(child, &components[1..], info);
}

fn compute_sort_keys(node: &mut TreeNode) -> u64 {
    let max_child: u64 = node
        .children
        .values_mut()
        .map(compute_sort_keys)
        .max()
        .unwrap_or(0);
    node.sort_key = node.size.max(max_child);
    node.sort_key
}

fn render_node(
    node: &TreeNode,
    name: &str,
    prefix: &str,
    is_last: bool,
    finished: bool,
    out: &mut Vec<String>,
) {
    let mut display_name = name.to_string();
    let mut current = node;
    while !current.in_top && current.children.len() == 1 {
        let (k, v) = current.children.iter().next().unwrap();
        display_name = format!("{}/{}", display_name, k.to_string_lossy());
        current = v;
    }

    let connector = if is_last { "└── " } else { "├── " };

    let size_part = if current.in_top {
        ansi(&human_size(current.size), size_color(current.size), true)
    } else {
        format!("{:>10}", "")
    };

    let meta_part = if finished && current.in_top {
        let mut parts = Vec::new();
        if let Some(count) = current.child_count {
            if current.is_dir {
                parts.push(format!("{}", count));
            }
        }
        if let Some(ref mtime) = current.mtime {
            parts.push(mtime.clone());
        }
        if parts.is_empty() {
            String::new()
        } else {
            ansi(&format!(" ({})", parts.join(" ")), Color::DarkGrey, false)
        }
    } else {
        String::new()
    };

    let name_colored = if current.in_top && current.is_dir {
        ansi(&display_name, Color::Blue, true)
    } else {
        display_name
    };

    out.push(format!(
        "{}  {}{}{}{}",
        size_part, prefix, connector, name_colored, meta_part
    ));

    let mut entries: Vec<_> = current.children.iter().collect();
    entries.sort_by(|a, b| b.1.sort_key.cmp(&a.1.sort_key));

    let new_prefix = if is_last {
        format!("{}    ", prefix)
    } else {
        format!("{}│   ", prefix)
    };

    let n = entries.len();
    for (i, (cname, child)) in entries.iter().enumerate() {
        let last = i == n - 1;
        render_node(child, &cname.to_string_lossy(), &new_prefix, last, finished, out);
    }
}

// ─── Local filesystem walker (optimized with DashMap) ───────────────────────

fn walker_local(
    root: PathBuf,
    entries: SharedEntryMap,
    running: Arc<AtomicBool>,
    scanned: Arc<AtomicU64>,
    apparent: bool,
    cutoff: Option<std::time::SystemTime>,
) {
    let entries_cb = Arc::clone(&entries);
    let scanned_cb = Arc::clone(&scanned);
    let running_cb = Arc::clone(&running);
    let root_cb = root.clone();

    let walker = WalkDir::new(&root)
        .skip_hidden(false)
        .follow_links(false)
        .parallelism(jwalk::Parallelism::RayonNewPool(num_cpus()))
        .process_read_dir(move |_depth, _dir_path, _state, children| {
            if !running_cb.load(Ordering::Relaxed) {
                children.clear();
                return;
            }
            for child_result in children.iter() {
                let Ok(child) = child_result else { continue };
                let Ok(meta) = child.metadata() else { continue };

                let path = child.path();
                let is_dir = meta.is_dir();

                if !is_dir {
                    if let Some(c) = cutoff {
                        match meta.modified() {
                            Ok(m) if m >= c => {}
                            _ => continue,
                        }
                    }
                }

                let size = if meta.is_file() {
                    if apparent {
                        meta.len()
                    } else {
                        #[cfg(unix)]
                        {
                            use std::os::unix::fs::MetadataExt;
                            meta.blocks() * 512
                        }
                        #[cfg(not(unix))]
                        {
                            meta.len()
                        }
                    }
                } else {
                    0
                };

                scanned_cb.fetch_add(1, Ordering::Relaxed);

                {
                    let mut e = entries_cb.entry(path.clone()).or_default();
                    e.is_dir = is_dir;
                    if !is_dir {
                        e.size = size;
                    }
                }

                if !is_dir && size > 0 {
                    let mut cur = path.parent();
                    while let Some(p) = cur {
                        if p != root_cb.as_path() && !p.starts_with(&root_cb) {
                            break;
                        }
                        {
                            let mut e = entries_cb.entry(p.to_path_buf()).or_default();
                            e.size += size;
                            e.is_dir = true;
                        }
                        if p == root_cb.as_path() {
                            break;
                        }
                        cur = p.parent();
                    }
                }
            }
        });

    for entry in walker {
        if !running.load(Ordering::Relaxed) {
            break;
        }
        let _ = entry;
    }
}
// ─── Remote walker using `hdfs dfs` CLI (Kerberos-aware, supports hdfs:// and abfs://) ──

fn walker_hdfs_cli(
    url: &str,
    entries: SharedEntryMap,
    running: Arc<AtomicBool>,
    scanned: Arc<AtomicU64>,
    cutoff_str: Option<String>,
) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    use std::io::BufRead;
    use std::process::{Command, Stdio};

    let root = PathBuf::from(url.trim_end_matches('/'));

    // Use `hdfs dfs -ls -R` which leverages the Java Hadoop client with Kerberos
    let mut child = Command::new("hdfs")
        .args(["dfs", "-ls", "-R", url])
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|e| -> Box<dyn std::error::Error + Send + Sync> {
            format!("Failed to spawn 'hdfs dfs': {}. Is 'hdfs' in PATH?", e).into()
        })?;

    let stdout = child.stdout.take().unwrap();
    let reader = std::io::BufReader::with_capacity(256 * 1024, stdout);

    for line in reader.lines() {
        if !running.load(Ordering::Relaxed) {
            let _ = child.kill();
            break;
        }

        let line = match line {
            Ok(l) => l,
            Err(_) => continue,
        };

        // Parse hdfs ls -R output format:
        // drwxr-xr-x   - user group          0 2024-01-01 12:00 /path/to/dir
        // -rw-r--r--   3 user group    1234567 2024-01-01 12:00 /path/to/file
        let line = line.trim();
        if line.is_empty() {
            continue;
        }

        let fields: Vec<&str> = line.split_whitespace().collect();
        if fields.len() < 8 {
            continue;
        }

        let perms = fields[0];
        let is_dir = perms.starts_with('d');
        let size: u64 = fields[4].parse().unwrap_or(0);
        let mtime_str = format!("{} {}", fields[5], fields[6]);
        // Path is the last field (may contain spaces, so rejoin from field 7)
        let path_str = fields[7..].join(" ");
        let entry_path = PathBuf::from(&path_str);
        
        if !is_dir {
            if let Some(ref c) = cutoff_str {
                if mtime_str.as_str() < c.as_str() {
                    continue;
                }
            }
        }
        
        scanned.fetch_add(1, Ordering::Relaxed);

        // Update entry
        {
            let mut e = entries.entry(entry_path.clone()).or_default();
            e.is_dir = is_dir;
            if !is_dir {
                e.size = size;
            }
            e.mtime = Some(mtime_str);
        }

        // Accumulate size to parents
        if !is_dir && size > 0 {
            let mut cur = entry_path.parent();
            while let Some(p) = cur {
                if p != root.as_path() && !p.starts_with(&root) {
                    break;
                }
                let mut e = entries.entry(p.to_path_buf()).or_default();
                e.size += size;
                e.is_dir = true;
                if p == root.as_path() {
                    break;
                }
                cur = p.parent();
            }
        }
    }

    let status = child.wait()?;
    if !status.success() {
        // Read stderr for error message
        let stderr = child.stderr.take();
        let err_msg = if let Some(stderr) = stderr {
            use std::io::BufRead as _;
            let mut err = String::new();
            std::io::BufReader::new(stderr)
                .lines()
                .take(5)
                .for_each(|l| {
                    if let Ok(l) = l {
                        err.push_str(&l);
                        err.push('\n');
                    }
                });
            err
        } else {
            format!("hdfs dfs exited with code: {}", status)
        };
        return Err(err_msg.into());
    }

    Ok(())
}

fn prune_to_top_n(entries: &SharedEntryMap, root: &Path, count: usize) {
    let mut snapshot: Vec<(PathBuf, u64)> = entries
        .iter()
        .filter(|r| r.key().as_path() != root)
        .map(|r| (r.key().clone(), r.value().size))
        .collect();

    let take = (count * 2).min(snapshot.len());
    if snapshot.len() > take {
        snapshot.select_nth_unstable_by(take, |a, b| b.1.cmp(&a.1));
        snapshot.truncate(take);
    }

    let mut to_keep: std::collections::HashSet<PathBuf> =
        std::collections::HashSet::with_capacity(take * 6);
    to_keep.insert(root.to_path_buf());

    for (path, _) in &snapshot {
        to_keep.insert(path.clone());
        let mut cur = path.parent();
        while let Some(p) = cur {
            if !p.starts_with(root) && p != root {
                break;
            }
            to_keep.insert(p.to_path_buf());
            if p == root {
                break;
            }
            cur = p.parent();
        }
    }

    entries.retain(|k, _| to_keep.contains(k));
}


/// After scan completes, enrich the top-N entries with child_count and mtime.
/// For local paths: stat each directory to get mtime, count immediate children.
/// For remote paths: child_count is computed from the entries map, mtime already stored.
fn enrich_top_entries(entries: &SharedEntryMap, root: &Path, count: usize, is_remote: bool) {
    // Get top-N entries
    let mut snapshot: Vec<(PathBuf, u64, bool)> = entries
        .iter()
        .filter(|r| r.key().as_path() != root)
        .map(|r| (r.key().clone(), r.value().size, r.value().is_dir))
        .collect();
    snapshot.sort_unstable_by(|a, b| b.1.cmp(&a.1));
    snapshot.truncate(count);

    for (path, _, is_dir) in &snapshot {
        if *is_dir {
            // Count direct children from the entries map
            let prefix = path.clone();
            let child_count = entries
                .iter()
                .filter(|r| {
                    let k = r.key();
                    k.as_path() != prefix.as_path() && k.parent() == Some(prefix.as_path())
                })
                .count() as u64;
            if let Some(mut e) = entries.get_mut(path) {
                e.child_count = Some(child_count);
            }
        }

        // For local paths, get mtime from filesystem
        if !is_remote {
            if let Ok(meta) = std::fs::metadata(path) {
                if let Ok(modified) = meta.modified() {
                    let duration = modified
                        .duration_since(std::time::UNIX_EPOCH)
                        .unwrap_or_default();
                    let secs = duration.as_secs() as i64;
                    let mtime_str = format_timestamp(secs);
                    if let Some(mut e) = entries.get_mut(path) {
                        e.mtime = Some(mtime_str);
                    }
                }
            }
        }
    }

    // Also enrich root
    if !is_remote {
        if let Ok(meta) = std::fs::metadata(root) {
            if let Ok(modified) = meta.modified() {
                let duration = modified
                    .duration_since(std::time::UNIX_EPOCH)
                    .unwrap_or_default();
                let secs = duration.as_secs() as i64;
                let mtime_str = format_timestamp(secs);
                if let Some(mut e) = entries.get_mut(&root.to_path_buf()) {
                    e.mtime = Some(mtime_str);
                }
            }
        }
    }
}

fn format_timestamp(secs: i64) -> String {
    // Simple UTC timestamp formatting without external crate
    let days = secs / 86400;
    let time_of_day = secs % 86400;
    let hours = time_of_day / 3600;
    let minutes = (time_of_day % 3600) / 60;

    // Calculate year/month/day from days since epoch
    let (year, month, day) = days_to_date(days);
    format!(
        "{:04}-{:02}-{:02} {:02}:{:02}",
        year, month, day, hours, minutes
    )
}

fn days_to_date(days_since_epoch: i64) -> (i64, u32, u32) {
    // Algorithm from http://howardhinnant.github.io/date_algorithms.html
    let z = days_since_epoch + 719468;
    let era = if z >= 0 { z } else { z - 146096 } / 146097;
    let doe = (z - era * 146097) as u32;
    let yoe = (doe - doe / 1460 + doe / 36524 - doe / 146096) / 365;
    let y = yoe as i64 + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let y = if m <= 2 { y + 1 } else { y };
    (y, m, d)
}

// ─── Rendering ──────────────────────────────────────────────────────────────

struct CursorGuard;
impl CursorGuard {
    fn new() -> Self {
        let _ = execute!(stdout(), Hide);
        Self
    }
}
impl Drop for CursorGuard {
    fn drop(&mut self) {
        let _ = execute!(stdout(), Show);
    }
}

fn render_screen(
    entries: &SharedEntryMap,
    root: &Path,
    count: usize,
    scanned: u64,
    last_height: &mut u16,
    finished: bool,
) {
    let mut snapshot: Vec<(PathBuf, Entry)> = entries
        .iter()
        .filter(|r| r.key().as_path() != root)
        .map(|r| (r.key().clone(), r.value().clone()))
        .collect();
    if snapshot.len() > count {
        snapshot.select_nth_unstable_by(count, |a, b| b.1.size.cmp(&a.1.size));
        snapshot.truncate(count);
    }
    snapshot.sort_unstable_by(|a, b| b.1.size.cmp(&a.1.size));
    let root_size = entries
        .get(&root.to_path_buf())
        .map(|r| r.size)
        .unwrap_or(0);

    let mut tree = TreeNode::default();
    for (path, info) in &snapshot {
        let rel = path.strip_prefix(root).unwrap_or(path);
        let comps: Vec<OsString> = rel
            .components()
            .map(|c| c.as_os_str().to_os_string())
            .collect();
        if !comps.is_empty() {
            insert_path(&mut tree, &comps, info);
        }
    }
    compute_sort_keys(&mut tree);

    let status = if finished { "done " } else { "scan " };
    let status_line = format!(
        "{} {} entries  total: {}",
        ansi(
            status,
            if finished { Color::Green } else { Color::Yellow },
            true
        ),
        scanned,
        ansi(&human_size(root_size), size_color(root_size), true)
    );

    let mut lines = Vec::new();

    let root_display = format!(
        "{}  {}",
        format!("{:>10}", ""),
        ansi(&root.display().to_string(), Color::Blue, true)
    );
    lines.push(root_display);

    let mut top_entries: Vec<_> = tree.children.iter().collect();
    top_entries.sort_by(|a, b| b.1.sort_key.cmp(&a.1.sort_key));
    let n = top_entries.len();
    for (i, (cname, child)) in top_entries.iter().enumerate() {
        let last = i == n - 1;
        render_node(child, &cname.to_string_lossy(), "", last, finished, &mut lines);
    }

    lines.push(status_line);

    let term_width = crossterm::terminal::size().map(|(w, _)| w as usize).unwrap_or(200);

    let mut out = stdout();
    if *last_height > 0 {
        let n = last_height.saturating_sub(1);
        if n > 0 {
            let _ = queue!(out, MoveUp(n));
        }
        let _ = queue!(out, MoveToColumn(0), Clear(ClearType::FromCursorDown));
    }
    for (i, line) in lines.iter().enumerate() {
        if i > 0 {
            let _ = queue!(out, Print("\n"));
        }
        let _ = queue!(out, Print(truncate_ansi(line, term_width)));
    }

    let _ = out.flush();
    *last_height = lines.len() as u16;
}

/// Truncate a string containing ANSI escape codes to `max_visible` visible characters.
fn truncate_ansi(s: &str, max_visible: usize) -> String {
    let mut result = String::with_capacity(s.len());
    let mut visible = 0;
    let mut in_escape = false;
    for ch in s.chars() {
        if in_escape {
            result.push(ch);
            if ch.is_ascii_alphabetic() {
                in_escape = false;
            }
        } else if ch == '\x1b' {
            in_escape = true;
            result.push(ch);
        } else {
            if visible >= max_visible {
                break;
            }
            result.push(ch);
            visible += 1;
        }
    }
    // Reset formatting if we truncated mid-escape
    if visible >= max_visible {
        result.push_str("\x1b[0m");
    }
    result
}

// ─── Slack formatting ───────────────────────────────────────────────────────

fn render_node_plain(
    node: &TreeNode,
    name: &str,
    prefix: &str,
    is_last: bool,
    out: &mut Vec<String>,
) {
    let mut display_name = name.to_string();
    let mut current = node;
    while !current.in_top && current.children.len() == 1 {
        let (k, v) = current.children.iter().next().unwrap();
        display_name = format!("{}/{}", display_name, k.to_string_lossy());
        current = v;
    }

    let connector = if is_last { "└── " } else { "├── " };

    let size_part = if current.in_top {
        human_size(current.size)
    } else {
        format!("{:>10}", "")
    };

    let meta_part = if current.in_top {
        let mut parts = Vec::new();
        if let Some(count) = current.child_count {
            if current.is_dir {
                parts.push(format!("{}", count));
            }
        }
        if let Some(ref mtime) = current.mtime {
            parts.push(mtime.clone());
        }
        if parts.is_empty() {
            String::new()
        } else {
            format!(" ({})", parts.join(" "))
        }
    } else {
        String::new()
    };

    let dir_marker = if current.in_top && current.is_dir {
        "/"
    } else {
        ""
    };

    out.push(format!(
        "{}  {}{}{}{}{}",
        size_part, prefix, connector, display_name, dir_marker, meta_part
    ));

    let mut entries: Vec<_> = current.children.iter().collect();
    entries.sort_by(|a, b| b.1.sort_key.cmp(&a.1.sort_key));

    let new_prefix = if is_last {
        format!("{}    ", prefix)
    } else {
        format!("{}│   ", prefix)
    };

    let n = entries.len();
    for (i, (cname, child)) in entries.iter().enumerate() {
        let last = i == n - 1;
        render_node_plain(child, &cname.to_string_lossy(), &new_prefix, last, out);
    }
}

fn render_slack_text(
    entries: &SharedEntryMap,
    root: &Path,
    count: usize,
    scanned: u64,
) -> String {
    let mut snapshot: Vec<(PathBuf, Entry)> = entries
        .iter()
        .filter(|r| r.key().as_path() != root)
        .map(|r| (r.key().clone(), r.value().clone()))
        .collect();
    snapshot.sort_unstable_by(|a, b| b.1.size.cmp(&a.1.size));
    snapshot.truncate(count);

    let root_size = entries
        .get(&root.to_path_buf())
        .map(|r| r.size)
        .unwrap_or(0);

    let mut tree = TreeNode::default();
    for (path, info) in &snapshot {
        let rel = path.strip_prefix(root).unwrap_or(path);
        let comps: Vec<OsString> = rel
            .components()
            .map(|c| c.as_os_str().to_os_string())
            .collect();
        if !comps.is_empty() {
            insert_path(&mut tree, &comps, info);
        }
    }
    compute_sort_keys(&mut tree);

    let mut lines = Vec::new();
    lines.push(format!("📂 {}", root.display()));

    let mut top_entries: Vec<_> = tree.children.iter().collect();
    top_entries.sort_by(|a, b| b.1.sort_key.cmp(&a.1.sort_key));
    let n = top_entries.len();
    for (i, (cname, child)) in top_entries.iter().enumerate() {
        let last = i == n - 1;
        render_node_plain(child, &cname.to_string_lossy(), "", last, &mut lines);
    }

    lines.push(format!(
        "✅ {} entries scanned  total: {}",
        scanned,
        human_size(root_size)
    ));

    lines.join("\n")
}

fn send_to_slack(
    webhook_url: &str,
    tree_text: &str,
    message: Option<&str>,
) -> Result<(), Box<dyn std::error::Error>> {
    let mut blocks = Vec::new();

    // Optional header message block
    if let Some(msg) = message {
        blocks.push(serde_json::json!({
            "type": "section",
            "text": {
                "type": "mrkdwn",
                "text": format!("*{}*", msg)
            }
        }));
    }

    // Code block with tree output
    blocks.push(serde_json::json!({
        "type": "section",
        "text": {
            "type": "mrkdwn",
            "text": format!("```\n{}\n```", tree_text)
        }
    }));

    let payload = serde_json::json!({ "blocks": blocks });

    let client = reqwest::blocking::Client::new();
    let resp = client
        .post(webhook_url)
        .header("Content-Type", "application/json")
        .json(&payload)
        .send()?;

    if !resp.status().is_success() {
        let status = resp.status();
        let body = resp.text().unwrap_or_default();
        return Err(format!("Slack webhook returned {}: {}", status, body).into());
    }

    Ok(())
}

fn num_cpus() -> usize {
    thread::available_parallelism()
        .map(|n| n.get())
        .unwrap_or(4)
}

fn is_remote_path(path: &str) -> bool {
    path.starts_with("hdfs://")
        || path.starts_with("abfs://")
        || path.starts_with("abfss://")
        || path.starts_with("///")
}

fn normalize_remote_url(url: &str) -> String {
    if let Some(rest) = url.strip_prefix("///") {
        format!("hdfs:///{}", rest)
    } else if url.starts_with("hdfs://") && !url.starts_with("hdfs:///") {
        format!("hdfs:///{}", &url[7..])
    } else {
        url.to_string()
    }
}
/// Extract the root path from a remote URL.
/// `hdfs dfs -ls -R` returns full URLs in its output, so root must be the full URL.
fn remote_root_path(url_str: &str) -> PathBuf {
    PathBuf::from(url_str.trim_end_matches('/'))
}

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let cli = Cli::parse();
    let slack_mode = cli.slack.is_some();
    let interactive = stdout().is_terminal();
    let is_remote = is_remote_path(&cli.path);

    let path_str: String = if is_remote {
        normalize_remote_url(&cli.path)
    } else {
        cli.path.clone()
    };
    
    let cutoff_local: Option<std::time::SystemTime> = cli.days.map(|d| {
        std::time::SystemTime::now() - Duration::from_secs(d * 86400)
    });

    let cutoff_str: Option<String> = cli.days.map(|d| {
        let now_secs = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs() as i64;
        format_timestamp(now_secs - (d as i64 * 86400))
    });

    let entries: SharedEntryMap = Arc::new(DashMap::new());
    let running = Arc::new(AtomicBool::new(true));
    let scanned = Arc::new(AtomicU64::new(0));
    let walker_done = Arc::new(AtomicBool::new(false));
    let walker_error: Arc<std::sync::Mutex<Option<String>>> =
        Arc::new(std::sync::Mutex::new(None));

    {
        let running = Arc::clone(&running);
        let _ = ctrlc::set_handler(move || {
            running.store(false, Ordering::Relaxed);
            let _ = execute!(std::io::stdout(), Show);
            std::thread::sleep(Duration::from_millis(200));
            std::process::exit(130);
        });
    }

    let root: PathBuf = if is_remote {
        remote_root_path(&path_str)
    } else {
        std::fs::canonicalize(&path_str)?
    };

    let walker_handle = {
        let entries = Arc::clone(&entries);
        let running = Arc::clone(&running);
        let scanned = Arc::clone(&scanned);
        let done = Arc::clone(&walker_done);
        let error = Arc::clone(&walker_error);
        let path = path_str.clone();
        let root_clone = root.clone();
        let apparent = cli.apparent_size;
        let cutoff_local = cutoff_local;
        let cutoff_str = cutoff_str.clone();

        thread::spawn(move || {
            let result = if is_remote_path(&path) {
                walker_hdfs_cli(&path, entries, running, scanned, cutoff_str)
            } else {
                walker_local(root_clone, entries, running, scanned, apparent, cutoff_local);
                Ok(())
            };
            if let Err(e) = result {
                *error.lock().unwrap() = Some(format!("{}", e));
            }
            done.store(true, Ordering::Relaxed);
        })
    };

    let mut last_height: u16 = 0;

    if interactive {
        let _guard = CursorGuard::new();
        let mut last_scanned: u64 = 0;
        loop {
            let cur_scanned = scanned.load(Ordering::Relaxed);
            let done = walker_done.load(Ordering::Relaxed);
            if cur_scanned != last_scanned || done {
                render_screen(
                    &entries,
                    &root,
                    cli.count,
                    cur_scanned,
                    &mut last_height,
                    done,
                );
                last_scanned = cur_scanned;
            }
            if done {
                break;
            }
            thread::sleep(Duration::from_millis(cli.refresh_ms));
        }
    } else {
        while !walker_done.load(Ordering::Relaxed) {
            thread::sleep(Duration::from_millis(200));
        }
    }

    walker_handle.join().ok();
    let _ = execute!(stdout(), Show);

    let walker_err = walker_error.lock().unwrap().take();

    prune_to_top_n(&entries, &root, cli.count);
    enrich_top_entries(&entries, &root, cli.count, is_remote);

    if interactive || !slack_mode {
        render_screen(
            &entries,
            &root,
            cli.count,
            scanned.load(Ordering::Relaxed),
            &mut last_height,
            true,
        );
        println!();
    }

    if slack_mode {
        let tree_text = render_slack_text(
            &entries,
            &root,
            cli.count,
            scanned.load(Ordering::Relaxed),
        );

        if let Some(ref webhook_url) = cli.slack {
            if !webhook_url.is_empty() {
                match send_to_slack(webhook_url, &tree_text, cli.message.as_deref()) {
                    Ok(()) => eprintln!("✅ Results sent to Slack"),
                    Err(e) => {
                        eprintln!("❌ Slack send failed: {}", e);
                        if walker_err.is_none() {
                            return Err(e);
                        }
                    }
                }
            } else {
                if let Some(ref msg) = cli.message {
                    println!("{}\n", msg);
                }
                println!("{}", tree_text);
            }
        }
    }

    if let Some(err) = walker_err {
        eprintln!("{}", ansi(&format!("error: {}", err), Color::Red, true));
        if !slack_mode {
            return Err(err.into());
        }
    }

    Ok(())
}
