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
use std::sync::{Arc, Mutex};
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
    #[arg(short = 'd', long = "days", conflicts_with = "since")]
    days: Option<u64>,

    /// Keep only files modified at or after this instant (dates are UTC).
    /// Accepts YYYY-MM-DD, "YYYY-MM-DD HH:MM[:SS]" or a relative age (30m, 12h, 7d, 2w)
    #[arg(short = 'S', long = "since", visible_alias = "newer-than", value_name = "WHEN")]
    since: Option<String>,

    /// Keep only files modified strictly before this instant (dates are UTC).
    /// Accepts YYYY-MM-DD, "YYYY-MM-DD HH:MM[:SS]" or a relative age (30m, 12h, 7d, 2w)
    #[arg(short = 'U', long = "until", visible_alias = "older-than", value_name = "WHEN")]
    until: Option<String>,

    /// Only detail the first N levels below PATH. Deeper content is still scanned
    /// and its size is rolled up into its level-N ancestor
    #[arg(short = 'L', long = "max-depth", value_name = "N",
          value_parser = clap::value_parser!(u32).range(1..))]
    max_depth: Option<u32>,

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
    /// Number of files kept under this path, recursively (accumulated during the scan)
    file_count: u64,
}

impl Default for Entry {
    fn default() -> Self {
        Self {
            size: 0,
            is_dir: false,
            mtime: None,
            file_count: 0,
        }
    }
}

/// Bounded "N biggest" collection: memory stays O(cap) whatever the input size.
struct TopN {
    cap: usize,
    items: Vec<(PathBuf, Entry)>,
    min_idx: usize,
}

impl TopN {
    fn new(cap: usize) -> Self {
        Self {
            cap,
            items: Vec::with_capacity(cap),
            min_idx: 0,
        }
    }

    fn is_full(&self) -> bool {
        self.items.len() >= self.cap
    }

    fn accepts(&self, size: u64) -> bool {
        self.cap > 0 && (!self.is_full() || size > self.items[self.min_idx].1.size)
    }

    /// `make` is only called when the item actually enters the top, so the path
    /// is cloned for a handful of candidates rather than for every entry.
    fn offer(&mut self, size: u64, make: impl FnOnce() -> (PathBuf, Entry)) {
        if !self.accepts(size) {
            return;
        }
        let item = make();
        if self.is_full() {
            self.items[self.min_idx] = item;
        } else {
            self.items.push(item);
        }
        if self.is_full() {
            self.min_idx = self
                .items
                .iter()
                .enumerate()
                .min_by_key(|(_, (_, e))| e.size)
                .map(|(i, _)| i)
                .unwrap_or(0);
        }
    }

    fn into_sorted(mut self) -> Vec<(PathBuf, Entry)> {
        self.items.sort_unstable_by(|a, b| b.1.size.cmp(&a.1.size));
        self.items
    }
}

/// Thread-safe top-N of files, with a lock-free fast path for the (vast
/// majority of) files too small to ever make it in.
struct TopFiles {
    floor: AtomicU64,
    top: Mutex<TopN>,
}

impl TopFiles {
    fn new(cap: usize) -> Self {
        Self {
            floor: AtomicU64::new(0),
            top: Mutex::new(TopN::new(cap)),
        }
    }

    fn offer(&self, size: u64, make: impl FnOnce() -> (PathBuf, Entry)) {
        if size < self.floor.load(Ordering::Relaxed) {
            return;
        }
        let mut top = self.top.lock().unwrap();
        top.offer(size, make);
        if top.is_full() {
            let min = top.items.get(top.min_idx).map_or(u64::MAX, |(_, e)| e.size);
            self.floor.store(min.saturating_add(1), Ordering::Relaxed);
        }
    }

    fn snapshot(&self) -> Vec<(PathBuf, Entry)> {
        self.top.lock().unwrap().items.clone()
    }
}

/// Scan state: every directory with its aggregated size, but only the biggest
/// files. Storing each file path used to cost tens of GiB on large trees.
struct Store {
    dirs: DashMap<PathBuf, Entry>,
    files: TopFiles,
}

impl Store {
    fn new(count: usize) -> Self {
        Self {
            dirs: DashMap::new(),
            files: TopFiles::new(count),
        }
    }

    /// Add `size`/`files` to `start` and each of its ancestors up to `root`.
    fn add_to_ancestors(&self, start: Option<&Path>, root: &Path, size: u64, files: u64) {
        let mut cur = start;
        while let Some(p) = cur {
            if !p.starts_with(root) {
                break;
            }
            match self.dirs.get_mut(p) {
                Some(mut e) => {
                    e.size += size;
                    e.file_count += files;
                    e.is_dir = true;
                }
                None => {
                    let mut e = self.dirs.entry(p.to_path_buf()).or_default();
                    e.size += size;
                    e.file_count += files;
                    e.is_dir = true;
                }
            }
            if p == root {
                break;
            }
            cur = p.parent();
        }
    }

    fn root_size(&self, root: &Path) -> u64 {
        self.dirs.get(root).map(|r| r.size).unwrap_or(0)
    }

    /// The `count` biggest entries (directories and files), excluding the root.
    fn top(&self, root: &Path, count: usize) -> Vec<(PathBuf, Entry)> {
        let mut top = TopN::new(count);
        for r in self.dirs.iter() {
            if r.key().as_path() == root {
                continue;
            }
            top.offer(r.value().size, || (r.key().clone(), r.value().clone()));
        }
        for (path, entry) in self.files.snapshot() {
            top.offer(entry.size, || (path, entry));
        }
        top.into_sorted()
    }
}

type SharedStore = Arc<Store>;

/// Number of levels `path` lies below `root` (the root itself is 0).
fn depth_below(path: &Path, root: &Path) -> usize {
    path.strip_prefix(root).map_or(0, |r| r.components().count())
}

/// The ancestor of `path` (possibly itself) lying at most `max` levels below `root`.
fn clamp_depth<'a>(path: &'a Path, root: &Path, max: Option<usize>) -> &'a Path {
    match max {
        Some(max) => path
            .ancestors()
            .nth(depth_below(path, root).saturating_sub(max))
            .unwrap_or(path),
        None => path,
    }
}

#[derive(Default, Debug)]
struct TreeNode {
    size: u64,
    is_dir: bool,
    file_count: u64,
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
        node.file_count = info.file_count;
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

/// " (1 234 files, 2026-07-01 14:22)" — file count for directories, mtime when known.
fn meta_suffix(node: &TreeNode) -> String {
    let mut parts = Vec::new();
    if node.is_dir {
        parts.push(format!(
            "{} file{}",
            group_digits(node.file_count),
            if node.file_count == 1 { "" } else { "s" }
        ));
    }
    if let Some(ref mtime) = node.mtime {
        parts.push(mtime.clone());
    }
    if parts.is_empty() {
        String::new()
    } else {
        format!(" ({})", parts.join(", "))
    }
}

/// Thin-space digit grouping: 1234567 -> "1 234 567".
fn group_digits(n: u64) -> String {
    let digits = n.to_string();
    let mut out = String::with_capacity(digits.len() + digits.len() / 3);
    for (i, c) in digits.chars().enumerate() {
        if i > 0 && (digits.len() - i) % 3 == 0 {
            out.push('\u{202f}');
        }
        out.push(c);
    }
    out
}

fn render_node(
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
        ansi(&human_size(current.size), size_color(current.size), true)
    } else {
        format!("{:>10}", "")
    };

    let meta_part = if current.in_top {
        ansi(&meta_suffix(current), Color::DarkGrey, false)
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
        render_node(child, &cname.to_string_lossy(), &new_prefix, last, out);
    }
}

// ─── Local filesystem walker (optimized with DashMap) ───────────────────────

fn walker_local(
    root: PathBuf,
    store: SharedStore,
    running: Arc<AtomicBool>,
    scanned: Arc<AtomicU64>,
    apparent: bool,
    range: TimeRange,
    max_depth: Option<usize>,
) {
    let store_cb = Arc::clone(&store);
    let scanned_cb = Arc::clone(&scanned);
    let running_cb = Arc::clone(&running);
    let root_cb = root.clone();

    let walker = WalkDir::new(&root)
        .skip_hidden(false)
        .follow_links(false)
        .parallelism(jwalk::Parallelism::RayonNewPool(num_cpus()))
        .process_read_dir(move |_depth, dir_path, _state, children| {
            if !running_cb.load(Ordering::Relaxed) {
                children.clear();
                return;
            }
            // jwalk also reads the root's parent to yield the root entry itself.
            if !dir_path.starts_with(&root_cb) {
                return;
            }
            let child_depth = depth_below(dir_path, &root_cb) + 1;
            let files_in_top = max_depth.map_or(true, |m| child_depth <= m);
            let mut dir_size: u64 = 0;
            let mut dir_files: u64 = 0;
            let mut seen: u64 = 0;
            for child_result in children.iter() {
                let Ok(child) = child_result else { continue };
                if child.file_type().is_dir() {
                    seen += 1;
                    continue;
                }
                let Ok(meta) = child.metadata() else { continue };

                if range.is_active() {
                    match mtime_secs(&meta) {
                        Some(secs) if range.contains(secs) => {}
                        _ => continue,
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

                seen += 1;
                dir_size += size;
                dir_files += 1;
                if !files_in_top {
                    continue;
                }
                store_cb.files.offer(size, || {
                    (
                        child.path(),
                        Entry {
                            size,
                            ..Entry::default()
                        },
                    )
                });
            }
            scanned_cb.fetch_add(seen, Ordering::Relaxed);

            let anchor = clamp_depth(dir_path, &root_cb, max_depth);
            {
                let mut e = store_cb.dirs.entry(anchor.to_path_buf()).or_default();
                e.is_dir = true;
                e.size += dir_size;
                e.file_count += dir_files;
            }
            if dir_files > 0 && anchor != root_cb.as_path() {
                store_cb.add_to_ancestors(anchor.parent(), &root_cb, dir_size, dir_files);
            }

            children.retain(|c| matches!(c, Ok(c) if c.file_type().is_dir()));
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
    store: SharedStore,
    running: Arc<AtomicBool>,
    scanned: Arc<AtomicU64>,
    range: TimeRange,
    max_depth: Option<usize>,
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
        
        if !is_dir && range.is_active() {
            match parse_datetime(&mtime_str) {
                Ok(secs) if range.contains(secs) => {}
                _ => continue,
            }
        }
        
        scanned.fetch_add(1, Ordering::Relaxed);

        let in_depth = max_depth.map_or(true, |m| depth_below(&entry_path, &root) <= m);
        if is_dir {
            if in_depth {
                let mut e = store.dirs.entry(entry_path).or_default();
                e.is_dir = true;
                e.mtime = Some(mtime_str);
            }
        } else {
            let parent = entry_path.parent().map(|p| clamp_depth(p, &root, max_depth));
            store.add_to_ancestors(parent, &root, size, 1);
            if !in_depth {
                continue;
            }
            store.files.offer(size, || {
                (
                    entry_path,
                    Entry {
                        size,
                        mtime: Some(mtime_str),
                        ..Entry::default()
                    },
                )
            });
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

/// After scan completes, stamp the final top-N with their mtime.
/// Remote listings already carry it; local entries are stat'ed here.
fn enrich_top_entries(top: &mut [(PathBuf, Entry)], is_remote: bool) {
    if is_remote {
        return;
    }
    for (path, e) in top.iter_mut() {
        if let Some(secs) = std::fs::metadata(path).ok().as_ref().and_then(mtime_secs) {
            e.mtime = Some(format_timestamp(secs));
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

fn days_from_civil(y: i64, m: u32, d: u32) -> i64 {
    // Inverse of days_to_date, same source
    let y = if m <= 2 { y - 1 } else { y };
    let era = if y >= 0 { y } else { y - 399 } / 400;
    let yoe = (y - era * 400) as u32;
    let mp = if m > 2 { m - 3 } else { m + 9 };
    let doy = (153 * mp + 2) / 5 + d - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    era * 146097 + doe as i64 - 719468
}

// ─── Time filtering ─────────────────────────────────────────────────────────

/// Half-open modification-time window `[min, max)`, in seconds since the epoch.
#[derive(Clone, Copy, Default, Debug)]
struct TimeRange {
    min: Option<i64>,
    max: Option<i64>,
}

impl TimeRange {
    fn is_active(&self) -> bool {
        self.min.is_some() || self.max.is_some()
    }

    fn contains(&self, secs: i64) -> bool {
        self.min.map_or(true, |m| secs >= m) && self.max.map_or(true, |m| secs < m)
    }

    fn label(&self) -> Option<String> {
        match (self.min, self.max) {
            (None, None) => None,
            (Some(a), None) => Some(format!("since {}", format_timestamp(a))),
            (None, Some(b)) => Some(format!("before {}", format_timestamp(b))),
            (Some(a), Some(b)) => Some(format!(
                "{} -> {}",
                format_timestamp(a),
                format_timestamp(b)
            )),
        }
    }
}

fn now_secs() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs() as i64
}

fn mtime_secs(meta: &std::fs::Metadata) -> Option<i64> {
    let modified = meta.modified().ok()?;
    Some(match modified.duration_since(std::time::UNIX_EPOCH) {
        Ok(d) => d.as_secs() as i64,
        Err(e) => -(e.duration().as_secs() as i64),
    })
}

/// Parse `YYYY-MM-DD`, `YYYY-MM-DD HH:MM[:SS]` or `YYYY-MM-DDTHH:MM[:SS]` (UTC).
fn parse_datetime(s: &str) -> Result<i64, String> {
    let s = s.trim();
    let (date_part, time_part) = match s.split_once(|c| c == 'T' || c == ' ') {
        Some((d, t)) => (d, t.trim()),
        None => (s, ""),
    };

    let d: Vec<&str> = date_part.split('-').collect();
    if d.len() != 3 {
        return Err(format!("`{}` is not a YYYY-MM-DD date", s));
    }
    let year: i64 = d[0]
        .parse()
        .map_err(|_| format!("`{}`: bad year", date_part))?;
    let month: u32 = d[1]
        .parse()
        .map_err(|_| format!("`{}`: bad month", date_part))?;
    let day: u32 = d[2]
        .parse()
        .map_err(|_| format!("`{}`: bad day", date_part))?;
    if !(1..=12).contains(&month) || !(1..=31).contains(&day) {
        return Err(format!("`{}`: month/day out of range", date_part));
    }

    let mut secs = days_from_civil(year, month, day) * 86400;

    if !time_part.is_empty() {
        let t: Vec<&str> = time_part.split(':').collect();
        if t.len() < 2 || t.len() > 3 {
            return Err(format!("`{}`: bad time, expected HH:MM[:SS]", time_part));
        }
        let hours: i64 = t[0]
            .parse()
            .map_err(|_| format!("`{}`: bad hours", time_part))?;
        let minutes: i64 = t[1]
            .parse()
            .map_err(|_| format!("`{}`: bad minutes", time_part))?;
        let seconds: i64 = if t.len() == 3 {
            t[2].parse()
                .map_err(|_| format!("`{}`: bad seconds", time_part))?
        } else {
            0
        };
        if !(0..24).contains(&hours) || !(0..60).contains(&minutes) || !(0..61).contains(&seconds) {
            return Err(format!("`{}`: time out of range", time_part));
        }
        secs += hours * 3600 + minutes * 60 + seconds;
    }

    Ok(secs)
}

/// Parse a relative age such as `30m`, `12h`, `7d`, `2w` into seconds.
fn parse_relative(s: &str) -> Option<i64> {
    let s = s.trim();
    let (num, unit) = s.split_at(s.len().checked_sub(1)?);
    let mult = match unit {
        "s" => 1,
        "m" => 60,
        "h" => 3600,
        "d" => 86400,
        "w" => 604800,
        _ => return None,
    };
    num.trim().parse::<i64>().ok().map(|n| n * mult)
}

/// Parse an absolute date or a relative age (interpreted as "N ago").
fn parse_time_spec(s: &str) -> Result<i64, String> {
    if let Some(age) = parse_relative(s) {
        return Ok(now_secs() - age);
    }
    parse_datetime(s)
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

fn build_tree(top: &[(PathBuf, Entry)], root: &Path) -> TreeNode {
    let mut tree = TreeNode::default();
    for (path, info) in top {
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
    tree
}

fn render_screen(
    top: &[(PathBuf, Entry)],
    root: &Path,
    root_size: u64,
    scanned: u64,
    last_height: &mut u16,
    finished: bool,
    filter_label: Option<&str>,
) {
    let tree = build_tree(top, root);

    let status = if finished { "done " } else { "scan " };
    let mut status_line = format!(
        "{} {} entries  total: {}",
        ansi(
            status,
            if finished { Color::Green } else { Color::Yellow },
            true
        ),
        scanned,
        ansi(&human_size(root_size), size_color(root_size), true)
    );
    if let Some(f) = filter_label {
        status_line.push_str(&format!("  {}", ansi(f, Color::Cyan, false)));
    }

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
        render_node(child, &cname.to_string_lossy(), "", last, &mut lines);
    }

    let (term_width, term_height) = crossterm::terminal::size()
        .map(|(w, h)| (w as usize, h as usize))
        .unwrap_or((200, usize::MAX));

    // While scanning, the live view must fit on screen: the cursor cannot move
    // above the top row, so a taller redraw would pile up instead of replacing
    // the previous frame. The final frame is printed in full.
    if !finished {
        let max = term_height.saturating_sub(1).max(3);
        if lines.len() + 1 > max {
            let keep = max - 2;
            let hidden = lines.len() - keep;
            lines.truncate(keep);
            lines.push(format!(
                "{:>10}  {}",
                "",
                ansi(
                    &format!("… {} more lines, full tree at the end of the scan", hidden),
                    Color::DarkGrey,
                    false
                )
            ));
        }
    }

    lines.push(status_line);

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
    *last_height = lines.len().min(u16::MAX as usize) as u16;
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
        meta_suffix(current)
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
    top: &[(PathBuf, Entry)],
    root: &Path,
    root_size: u64,
    scanned: u64,
    filter_label: Option<&str>,
) -> String {
    let tree = build_tree(top, root);


    let mut lines = Vec::new();
    lines.push(format!("📂 {}", root.display()));
    if let Some(f) = filter_label {
        lines.push(format!("🔎 {}", f));
    }

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
    
    let mut range = TimeRange::default();
    if let Some(d) = cli.days {
        range.min = Some(now_secs() - (d as i64) * 86400);
    }
    if let Some(ref spec) = cli.since {
        range.min = Some(parse_time_spec(spec).map_err(|e| format!("invalid --since: {}", e))?);
    }
    if let Some(ref spec) = cli.until {
        range.max = Some(parse_time_spec(spec).map_err(|e| format!("invalid --until: {}", e))?);
    }
    if let (Some(a), Some(b)) = (range.min, range.max) {
        if a >= b {
            return Err(format!(
                "empty time window: --since {} is not before --until {}",
                format_timestamp(a),
                format_timestamp(b)
            )
            .into());
        }
    }
    let max_depth = cli.max_depth.map(|d| d as usize);
    let filter_label = {
        let parts: Vec<String> = range
            .label()
            .into_iter()
            .chain(max_depth.map(|d| format!("depth <= {}", d)))
            .collect();
        (!parts.is_empty()).then(|| parts.join(", "))
    };

    let store: SharedStore = Arc::new(Store::new(cli.count));
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
        let store = Arc::clone(&store);
        let running = Arc::clone(&running);
        let scanned = Arc::clone(&scanned);
        let done = Arc::clone(&walker_done);
        let error = Arc::clone(&walker_error);
        let path = path_str.clone();
        let root_clone = root.clone();
        let apparent = cli.apparent_size;

        thread::spawn(move || {
            let result = if is_remote_path(&path) {
                walker_hdfs_cli(&path, store, running, scanned, range, max_depth)
            } else {
                walker_local(root_clone, store, running, scanned, apparent, range, max_depth);
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
            if walker_done.load(Ordering::Relaxed) {
                break;
            }
            let cur_scanned = scanned.load(Ordering::Relaxed);
            if cur_scanned != last_scanned {
                render_screen(
                    &store.top(&root, cli.count),
                    &root,
                    store.root_size(&root),
                    cur_scanned,
                    &mut last_height,
                    false,
                    filter_label.as_deref(),
                );
                last_scanned = cur_scanned;
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

    let mut top = store.top(&root, cli.count);
    enrich_top_entries(&mut top, is_remote);
    let root_size = store.root_size(&root);

    if interactive || !slack_mode {
        render_screen(
            &top,
            &root,
            root_size,
            scanned.load(Ordering::Relaxed),
            &mut last_height,
            true,
            filter_label.as_deref(),
        );
        println!();
    }

    if slack_mode {
        let tree_text = render_slack_text(
            &top,
            &root,
            root_size,
            scanned.load(Ordering::Relaxed),
            filter_label.as_deref(),
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
